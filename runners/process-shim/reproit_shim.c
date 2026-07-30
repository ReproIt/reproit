/*
 * ReproIt process shim: record and serve a program's reads of the outside
 * world at the dynamic-linking boundary (LD_PRELOAD on Linux,
 * DYLD_INSERT_LIBRARIES on macOS).
 *
 * RECORD  (REPROIT_RECORD=<log>): every intercepted call appends one line to
 *         the log. The host program's behavior is unchanged; a capture
 *         failure is dropped, never propagated.
 * REPLAY  (REPROIT_REPLAY_LOG=<log>): the same entry points serve the
 *         recorded results and never touch the real resource. A read of a
 *         resource the log does not carry is a DIVERGENCE: the call fails
 *         closed and one `REPROIT:DIVERGENCE ` line goes to stderr.
 *
 * Line format is tab separated so the C side never parses JSON:
 *   open    <path-b64> <fd|-errno>
 *   read    <fd> <data-b64>
 *   connect <fd> <endpoint-b64> <result>
 *   send    <fd> <data-b64>
 *   recv    <fd> <data-b64>
 *   clock   <clock_id> <sec> <nsec>
 *   time    <sec>
 *   random  <data-b64>
 *   env     <name-b64> <value-b64|->
 *
 * Divergence policy, stated so the numbers mean something:
 *   DIVERGENCE (fail closed) is reserved for an EXTERNAL RESOURCE the capsule
 *   cannot serve: a file path never opened at record time, a connect to an
 *   endpoint never dialed, or a socket stream read past its recorded bytes.
 *   Clock and RNG overruns are NOT divergences. They are policy served (the
 *   clock freezes at its last recorded value plus one nanosecond, the RNG
 *   continues from the capsule's replay seed) and counted separately, because
 *   a replayed run legitimately makes a different NUMBER of clock and random
 *   draws while depending on the same external inputs.
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/random.h>
#include <sys/sendfile.h>
#include <sys/syscall.h>
#endif
#include <arpa/inet.h>
#include <netinet/in.h>

#include "reproit_shim_capsule.h"

/* Real libc entry points, resolved lazily. */
static int (*real_open)(const char *, int, ...);
static int (*real_openat)(int, const char *, int, ...);
static ssize_t (*real_read)(int, void *, size_t);
static ssize_t (*real_pread)(int, void *, size_t, off_t);
static int (*real_close)(int);
static int (*real_connect)(int, const struct sockaddr *, socklen_t);
static ssize_t (*real_send)(int, const void *, size_t, int);
static ssize_t (*real_sendto)(int, const void *, size_t, int, const struct sockaddr *, socklen_t);
static ssize_t (*real_recv)(int, void *, size_t, int);
static ssize_t (*real_recvfrom)(int, void *, size_t, int, struct sockaddr *, socklen_t *);
static ssize_t (*real_write)(int, const void *, size_t);
static int (*real_clock_gettime)(clockid_t, struct timespec *);
static int (*real_gettimeofday)(struct timeval *, void *);
static time_t (*real_time)(time_t *);
static char *(*real_getenv)(const char *);
static FILE *(*real_fopen)(const char *, const char *);
static size_t (*real_fread)(void *, size_t, size_t, FILE *);
static int (*real_fclose)(FILE *);

#define RESOLVE(fn)                                                                                \
    do {                                                                                           \
        if (!real_##fn) {                                                                          \
            real_##fn = dlsym(RTLD_NEXT, #fn);                                                     \
        }                                                                                          \
    } while (0)

void shim_init(void) {
    if (G.ready) {
        return;
    }
    G.ready = 1;
    G.log_fd = -1;
    RESOLVE(getenv);
    const char *record = real_getenv ? real_getenv("REPROIT_RECORD") : NULL;
    const char *replay = real_getenv ? real_getenv("REPROIT_REPLAY_LOG") : NULL;
    const char *seed = real_getenv ? real_getenv("REPROIT_REPLAY_SEED") : NULL;
    G.rng_state = seed ? strtoull(seed, NULL, 16) : 0x9e3779b97f4a7c15ULL;
    if (!G.rng_state) {
        G.rng_state = 0x9e3779b97f4a7c15ULL;
    }
    if (replay && replay[0]) {
        G.mode = 2;
        load_replay(replay);
    } else if (record && record[0]) {
        G.mode = 1;
        reproit_open_log(record);
    }
}

__attribute__((destructor)) static void shim_report(void) { reproit_report(); }


static void track_path(int fd, const char *path) {
    if (fd >= 0 && fd < MAX_FDS) {
        snprintf(G.paths[fd], sizeof(G.paths[fd]), "%s", path ? path : "-");
    }
}

/* The file's size at record time, stored on the open entry so replay can
 * prove the capsule actually carries the data. */
static long file_size(int fd) {
    if (fd < 0) {
        return 0;
    }
    struct stat info;
    if (fstat(fd, &info) != 0 || !S_ISREG(info.st_mode)) {
        return 0;
    }
    return (long)info.st_size;
}

static int serve_open(const char *path) {
    /* Replay a file by building a memfd of its recorded content: the kernel
     * then serves read, pread, lseek, and fstat, so replay does not depend on
     * the program reading in the same sized chunks it did at record time. */
    unsigned char *content = NULL;
    size_t len = gather(K_READ, path, &content);
    entry_t *opened = next_entry(K_OPEN, path);
    if (!opened && !content) {
        return -2; /* unknown path: caller diverges */
    }
    if (opened && opened->a < 0 && !content) {
        errno = (int)(-opened->a);
        free(content);
        return -1; /* recorded failure, replayed faithfully */
    }
    /* COMPLETENESS ORACLE. The capsule saw the open but never the bytes, so
     * some data mover carried the content past this boundary (measured:
     * coreutils cat uses copy_file_range, CPython uses mmap). Serving an
     * empty file here is a SILENT WRONG REPLAY, the one outcome this project
     * refuses. Fail closed and name the reason instead. */
    if (opened && opened->b > 0 && len == 0) {
        free(content);
        return -3;
    }
#ifdef __linux__
    int fd = (int)syscall(SYS_memfd_create, "reproit-replay", 0);
#else
    char template[] = "/tmp/reproit-replay-XXXXXX";
    int fd = mkstemp(template);
    if (fd >= 0) {
        unlink(template);
    }
#endif
    if (fd < 0) {
        free(content);
        return -2;
    }
    if (content && len) {
        RESOLVE(write);
        ssize_t ignored = real_write(fd, content, len);
        (void)ignored;
        lseek(fd, 0, SEEK_SET);
    }
    /* Partial capture: the capsule holds fewer bytes than the file had. That
     * is only a problem if the program actually consumes past them, so the fd
     * is flagged and the divergence fires at the over-read, not here. */
    if (fd >= 0 && fd < MAX_FDS) {
        fdstate_t *state = &G.fds[fd];
        memset(state, 0, sizeof(*state));
        state->active = 1;
        state->is_socket = 0;
        state->incomplete = opened && opened->b > (long)len;
        snprintf(state->key, sizeof(state->key), "%s", path);
    }
    free(content);
    G.served++;
    return fd;
}

int open(const char *path, int flags, ...) {
    int return_real = 0;
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    RESOLVE(open);
    ENTER();
    if (return_real) {
        return real_open(path, flags, mode);
    }
    int fd;
    if (G.mode == 2) {
        fd = serve_open(path);
        if (fd == -2) {
            diverge("file", path);
            errno = ENOENT;
            fd = -1;
        } else if (fd == -3) {
            diverge("incomplete-file", path);
            errno = EIO;
            fd = -1;
        }
    } else {
        fd = real_open(path, flags, mode);
        track_path(fd, path);
        char key[256];
        snprintf(key, sizeof(key), "%s", path);
        record_blob(K_OPEN, key, NULL, 0, fd >= 0 ? fd : -errno, file_size(fd));
    }
    LEAVE();
    return fd;
}

int openat(int dirfd, const char *path, int flags, ...) {
    int return_real = 0;
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    RESOLVE(openat);
    ENTER();
    if (return_real) {
        return real_openat(dirfd, path, flags, mode);
    }
    int fd;
    if (G.mode == 2) {
        /* Never fall through to the real filesystem. Measured: falling
         * through for relative paths made a python3 replay read the live
         * disk, so a deleted input produced an empty result with ZERO
         * divergences, a false negative. A path the capsule does not carry
         * is a divergence, even when that makes an interpreted runtime
         * noisy: fail closed is the whole contract. */
        char absolute[MAX_PATH_LEN];
        if (path && path[0] == '/') {
            snprintf(absolute, sizeof(absolute), "%s", path);
        } else if (dirfd == AT_FDCWD && path) {
            char cwd[MAX_PATH_LEN / 2];
            if (getcwd(cwd, sizeof(cwd))) {
                snprintf(absolute, sizeof(absolute), "%s/%s", cwd, path);
            } else {
                snprintf(absolute, sizeof(absolute), "%s", path);
            }
        } else {
            snprintf(absolute, sizeof(absolute), "%s", path ? path : "-");
        }
        fd = serve_open(absolute);
        if (fd == -2) {
            diverge("file", absolute);
            errno = ENOENT;
            fd = -1;
        } else if (fd == -3) {
            diverge("incomplete-file", absolute);
            errno = EIO;
            fd = -1;
        }
    } else if (G.mode == 1) {
        fd = real_openat(dirfd, path, flags, mode);
        track_path(fd, path);
        char key[256];
        snprintf(key, sizeof(key), "%s", path);
        record_blob(K_OPEN, key, NULL, 0, fd >= 0 ? fd : -errno, file_size(fd));
    } else {
        fd = real_openat(dirfd, path, flags, mode);
    }
    LEAVE();
    return fd;
}

ssize_t read(int fd, void *buf, size_t count) {
    int return_real = 0;
    RESOLVE(read);
    ENTER();
    if (return_real) {
        return real_read(fd, buf, count);
    }
    ssize_t got;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active && !G.fds[fd].is_socket) {
        /* A replayed FILE: the kernel serves the memfd. An EOF on a fd whose
         * capture was partial means the program wanted bytes the capsule
         * never carried, so it fails closed rather than reading short. */
        got = real_read(fd, buf, count);
        if (got == 0 && count > 0 && G.fds[fd].incomplete) {
            diverge("truncated-file", G.fds[fd].key);
            errno = EIO;
            got = -1;
        } else if (got > 0) {
            G.served++;
        }
        LEAVE();
        return got;
    }
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        /* a replayed socket stream */
        fdstate_t *s = &G.fds[fd];
        size_t left = s->len > s->off ? s->len - s->off : 0;
        if (!left) {
            diverge(s->incomplete ? "incomplete-socket" : "socket-stream", s->key);
            errno = ECONNRESET;
            LEAVE();
            return -1;
        }
        size_t take = left < count ? left : count;
        memcpy(buf, s->buf + s->off, take);
        s->off += take;
        G.served++;
        LEAVE();
        return (ssize_t)take;
    }
    got = real_read(fd, buf, count);
    if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.paths[fd][0]) {
        record_blob(K_READ, G.paths[fd], (const unsigned char *)buf, (size_t)got, fd, 0);
    }
    LEAVE();
    return got;
}

ssize_t pread(int fd, void *buf, size_t count, off_t offset) {
    int return_real = 0;
    RESOLVE(pread);
    ENTER();
    if (return_real) {
        return real_pread(fd, buf, count, offset);
    }
    ssize_t got = real_pread(fd, buf, count, offset);
    if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.paths[fd][0]) {
        record_blob(K_READ, G.paths[fd], (const unsigned char *)buf, (size_t)got, fd, offset);
    }
    LEAVE();
    return got;
}

int close(int fd) {
    int return_real = 0;
    RESOLVE(close);
    ENTER();
    if (return_real) {
        return real_close(fd);
    }
    if (fd >= 0 && fd < MAX_FDS) {
        if (G.fds[fd].active) {
            free(G.fds[fd].buf);
            memset(&G.fds[fd], 0, sizeof(G.fds[fd]));
        }
        G.paths[fd][0] = 0;
    }
    int result = real_close(fd);
    LEAVE();
    return result;
}

static void endpoint_of(const struct sockaddr *addr, socklen_t len, char *out, size_t cap) {
    if (addr && addr->sa_family == AF_INET && len >= (socklen_t)sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *in4 = (const struct sockaddr_in *)addr;
        char ip[INET_ADDRSTRLEN] = {0};
        inet_ntop(AF_INET, &in4->sin_addr, ip, sizeof(ip));
        snprintf(out, cap, "%s:%u", ip, (unsigned)ntohs(in4->sin_port));
        return;
    }
    snprintf(out, cap, "unknown");
}

int connect(int fd, const struct sockaddr *addr, socklen_t len) {
    int return_real = 0;
    RESOLVE(connect);
    ENTER();
    if (return_real) {
        return real_connect(fd, addr, len);
    }
    char endpoint[128];
    endpoint_of(addr, len, endpoint, sizeof(endpoint));
    int result;
    if (G.mode == 2) {
        entry_t *dialed = next_entry(K_CONNECT, endpoint);
        if (!dialed) {
            diverge("connect", endpoint);
            errno = ECONNREFUSED;
            LEAVE();
            return -1;
        }
        if (fd >= 0 && fd < MAX_FDS) {
            fdstate_t *s = &G.fds[fd];
            memset(s, 0, sizeof(*s));
            s->active = 1;
            s->is_socket = 1;
            snprintf(s->key, sizeof(s->key), "%s", endpoint);
            s->len = gather(K_RECV, endpoint, &s->buf);
            /* Same oracle for a stream: a recorded dial whose bytes never
             * reached the capsule must not read as a clean empty response. */
            s->incomplete = (s->len == 0);
        }
        G.served++;
        result = 0;
    } else {
        result = real_connect(fd, addr, len);
        record_blob(K_CONNECT, endpoint, NULL, 0, fd, result);
        if (fd >= 0 && fd < MAX_FDS) {
            snprintf(G.fds[fd].key, sizeof(G.fds[fd].key), "%s", endpoint);
        }
    }
    LEAVE();
    return result;
}

ssize_t send(int fd, const void *buf, size_t len, int flags) {
    int return_real = 0;
    RESOLVE(send);
    ENTER();
    if (return_real) {
        return real_send(fd, buf, len, flags);
    }
    ssize_t result;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        result = (ssize_t)len; /* the request is not replayed outward */
        G.served++;
    } else {
        result = real_send(fd, buf, len, flags);
        if (G.mode == 1 && fd >= 0 && fd < MAX_FDS && G.fds[fd].key[0]) {
            record_blob(K_SEND, G.fds[fd].key, (const unsigned char *)buf, len, fd, 0);
        }
    }
    LEAVE();
    return result;
}

ssize_t sendto(int fd, const void *buf, size_t len, int flags, const struct sockaddr *addr,
               socklen_t alen) {
    int return_real = 0;
    RESOLVE(sendto);
    ENTER();
    if (return_real) {
        return real_sendto(fd, buf, len, flags, addr, alen);
    }
    ssize_t result;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        result = (ssize_t)len;
        G.served++;
    } else {
        result = real_sendto(fd, buf, len, flags, addr, alen);
    }
    LEAVE();
    return result;
}

ssize_t serve_recv(int fd, void *buf, size_t len) {
    fdstate_t *s = &G.fds[fd];
    size_t left = s->len > s->off ? s->len - s->off : 0;
    if (!left) {
        /* The capsule recorded the dial but never the bytes: the boundary
         * missed the data mover, so an empty read would be a silent wrong
         * replay rather than an honest end of stream. */
        diverge(s->incomplete ? "incomplete-socket" : "socket-stream", s->key);
        errno = ECONNRESET;
        return -1;
    }
    size_t take = left < len ? left : len;
    memcpy(buf, s->buf + s->off, take);
    s->off += take;
    G.served++;
    return (ssize_t)take;
}

ssize_t recv(int fd, void *buf, size_t len, int flags) {
    int return_real = 0;
    RESOLVE(recv);
    ENTER();
    if (return_real) {
        return real_recv(fd, buf, len, flags);
    }
    ssize_t got;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        got = serve_recv(fd, buf, len);
    } else {
        got = real_recv(fd, buf, len, flags);
        if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.fds[fd].key[0]) {
            record_blob(K_RECV, G.fds[fd].key, (const unsigned char *)buf, (size_t)got, fd, 0);
        }
    }
    LEAVE();
    return got;
}

ssize_t recvfrom(int fd, void *buf, size_t len, int flags, struct sockaddr *addr,
                 socklen_t *alen) {
    int return_real = 0;
    RESOLVE(recvfrom);
    ENTER();
    if (return_real) {
        return real_recvfrom(fd, buf, len, flags, addr, alen);
    }
    ssize_t got;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        got = serve_recv(fd, buf, len);
    } else {
        got = real_recvfrom(fd, buf, len, flags, addr, alen);
        if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.fds[fd].key[0]) {
            record_blob(K_RECV, G.fds[fd].key, (const unsigned char *)buf, (size_t)got, fd, 0);
        }
    }
    LEAVE();
    return got;
}

int clock_gettime(clockid_t id, struct timespec *ts) {
    int return_real = 0;
    RESOLVE(clock_gettime);
    ENTER();
    if (return_real) {
        return real_clock_gettime(id, ts);
    }
    int result = 0;
    if (G.mode == 2) {
        char key[32];
        snprintf(key, sizeof(key), "%d", (int)id);
        entry_t *e = next_entry(K_CLOCK, key);
        if (e) {
            ts->tv_sec = e->a;
            ts->tv_nsec = e->b;
            G.last_sec = e->a;
            G.last_nsec = e->b;
            G.served++;
        } else {
            /* Policy, not divergence: a replayed run makes a different NUMBER
             * of clock reads while depending on the same external inputs. */
            G.last_nsec++;
            ts->tv_sec = G.last_sec;
            ts->tv_nsec = G.last_nsec;
            G.clock_overrun++;
        }
    } else {
        result = real_clock_gettime(id, ts);
        char key[32];
        snprintf(key, sizeof(key), "%d", (int)id);
        record_blob(K_CLOCK, key, NULL, 0, (long)ts->tv_sec, (long)ts->tv_nsec);
    }
    LEAVE();
    return result;
}

int gettimeofday(struct timeval *tv, void *tz) {
    int return_real = 0;
    RESOLVE(gettimeofday);
    ENTER();
    if (return_real) {
        return real_gettimeofday(tv, tz);
    }
    int result = 0;
    if (G.mode == 2) {
        entry_t *e = next_entry(K_TIME, "gettimeofday");
        if (e) {
            tv->tv_sec = e->a;
            tv->tv_usec = e->b;
            G.served++;
        } else {
            tv->tv_sec = G.last_sec;
            tv->tv_usec = 0;
            G.clock_overrun++;
        }
    } else {
        result = real_gettimeofday(tv, tz);
        record_blob(K_TIME, "gettimeofday", NULL, 0, (long)tv->tv_sec, (long)tv->tv_usec);
    }
    LEAVE();
    return result;
}

time_t time(time_t *out) {
    int return_real = 0;
    RESOLVE(time);
    ENTER();
    if (return_real) {
        return real_time(out);
    }
    time_t value;
    if (G.mode == 2) {
        entry_t *e = next_entry(K_TIME, "time");
        if (e) {
            value = (time_t)e->a;
            G.last_sec = e->a;
            G.served++;
        } else {
            value = (time_t)G.last_sec;
            G.clock_overrun++;
        }
    } else {
        value = real_time(out);
        record_blob(K_TIME, "time", NULL, 0, (long)value, 0);
    }
    if (out) {
        *out = value;
    }
    LEAVE();
    return value;
}

static uint64_t next_random(void) {
    /* xorshift64star, seeded from the capsule: replay determinism only. */
    G.rng_state ^= G.rng_state >> 12;
    G.rng_state ^= G.rng_state << 25;
    G.rng_state ^= G.rng_state >> 27;
    return G.rng_state * 0x2545f4914f6cdd1dULL;
}

#ifdef __linux__
ssize_t getrandom(void *buf, size_t len, unsigned int flags) {
    static ssize_t (*real_getrandom)(void *, size_t, unsigned int);
    int return_real = 0;
    if (!real_getrandom) {
        real_getrandom = dlsym(RTLD_NEXT, "getrandom");
    }
    ENTER();
    if (return_real) {
        return real_getrandom(buf, len, flags);
    }
    ssize_t got;
    if (G.mode == 2) {
        entry_t *e = next_entry(K_RANDOM, "getrandom");
        if (e && e->blob && e->blob_len >= len) {
            memcpy(buf, e->blob, len);
            G.served++;
        } else {
            unsigned char *out = buf;
            for (size_t i = 0; i < len; i++) {
                out[i] = (unsigned char)(next_random() & 0xff);
            }
            G.random_overrun++;
        }
        got = (ssize_t)len;
    } else {
        got = real_getrandom(buf, len, flags);
        if (got > 0) {
            record_blob(K_RANDOM, "getrandom", (const unsigned char *)buf, (size_t)got, 0, 0);
        }
    }
    LEAVE();
    return got;
}
#endif

char *getenv(const char *name) {
    int return_real = 0;
    RESOLVE(getenv);
    ENTER();
    if (return_real) {
        return real_getenv(name);
    }
    char *value;
    if (G.mode == 2) {
        entry_t *e = next_entry(K_ENV, name);
        if (e && e->blob) {
            static char stash[4096];
            size_t take = e->blob_len < sizeof(stash) - 1 ? e->blob_len : sizeof(stash) - 1;
            memcpy(stash, e->blob, take);
            stash[take] = 0;
            G.served++;
            value = stash;
        } else {
            /* The environment itself is pinned by the capsule at exec time,
             * so an unrecorded read falls through to the pinned environ
             * rather than diverging. Counted, never silent. */
            G.env_fallthrough++;
            value = real_getenv(name);
        }
    } else {
        value = real_getenv(name);
        record_blob(K_ENV, name, value ? (const unsigned char *)value : NULL,
                    value ? strlen(value) : 0, 0, 0);
    }
    LEAVE();
    return value;
}

/* glibc's stdio calls its own internal __open and __read rather than the
 * public symbols, so fopen and fread are INVISIBLE to a libc-symbol boundary
 * unless the boundary also covers stdio. This was measured, not assumed: the
 * first run of validation/process/measure.sh recorded zero open and zero read
 * entries for the fopen path. Replay serves the recorded content through
 * fmemopen, so fread, fgets, fseek, and fclose then work natively. */
#define MAX_STREAMS 256
static struct {
    FILE *handle;
    char path[256];
} G_streams[MAX_STREAMS];

static void track_stream(FILE *handle, const char *path) {
    if (!handle) {
        return;
    }
    for (int i = 0; i < MAX_STREAMS; i++) {
        if (!G_streams[i].handle) {
            G_streams[i].handle = handle;
            snprintf(G_streams[i].path, sizeof(G_streams[i].path), "%s", path ? path : "-");
            return;
        }
    }
}

static const char *stream_path(FILE *handle) {
    for (int i = 0; i < MAX_STREAMS; i++) {
        if (G_streams[i].handle == handle) {
            return G_streams[i].path;
        }
    }
    return NULL;
}

static void forget_stream(FILE *handle) {
    for (int i = 0; i < MAX_STREAMS; i++) {
        if (G_streams[i].handle == handle) {
            G_streams[i].handle = NULL;
            G_streams[i].path[0] = 0;
            return;
        }
    }
}

FILE *fopen(const char *path, const char *mode) {
    int return_real = 0;
    RESOLVE(fopen);
    ENTER();
    if (return_real) {
        return real_fopen(path, mode);
    }
    FILE *handle;
    if (G.mode == 2) {
        unsigned char *content = NULL;
        size_t len = gather(K_READ, path, &content);
        entry_t *opened = next_entry(K_OPEN, path);
        if (!opened && !content) {
            diverge("file", path);
            errno = ENOENT;
            LEAVE();
            return NULL;
        }
        if (opened && opened->b > 0 && len == 0) {
            diverge("incomplete-file", path);
            free(content);
            errno = EIO;
            LEAVE();
            return NULL;
        }
        if (!content) {
            /* a recorded open that yielded no bytes still replays as empty */
            content = malloc(1);
            len = 0;
        }
        handle = fmemopen(content, len, "r");
        G.served++;
    } else {
        handle = real_fopen(path, mode);
        track_stream(handle, path);
        record_blob(K_OPEN, path, NULL, 0, handle ? 0 : -errno,
                    handle ? file_size(fileno(handle)) : 0);
    }
    LEAVE();
    return handle;
}

size_t fread(void *buf, size_t size, size_t count, FILE *handle) {
    int return_real = 0;
    RESOLVE(fread);
    ENTER();
    if (return_real) {
        return real_fread(buf, size, count, handle);
    }
    size_t got = real_fread(buf, size, count, handle);
    if (G.mode == 1 && got > 0) {
        const char *path = stream_path(handle);
        if (path) {
            record_blob(K_READ, path, (const unsigned char *)buf, got * size, 0, 0);
        }
    }
    LEAVE();
    return got;
}

int fclose(FILE *handle) {
    int return_real = 0;
    RESOLVE(fclose);
    ENTER();
    if (return_real) {
        return real_fclose(handle);
    }
    forget_stream(handle);
    int result = real_fclose(handle);
    LEAVE();
    return result;
}

/* Large file offset aliases. Measured, not assumed: with only `open` and
 * `openat` interposed, a python3 run recorded 256 boundary entries and ZERO
 * file entries, and a /bin/cat replay produced empty output with no
 * divergence, the worst possible outcome. glibc compiles callers that define
 * _FILE_OFFSET_BITS=64 (CPython, coreutils, most of userspace) against
 * open64, openat64, and pread64, so a boundary that names only the plain
 * symbols silently sees no file I/O at all. */
int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    return open(path, flags, mode);
}

int openat64(int dirfd, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    return openat(dirfd, path, flags, mode);
}

ssize_t pread64(int fd, void *buf, size_t count, off_t offset) {
    return pread(fd, buf, count, offset);
}

FILE *fopen64(const char *path, const char *mode) { return fopen(path, mode); }

