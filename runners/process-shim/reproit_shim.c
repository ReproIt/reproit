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
/* Only ever called with F_GETFL, which takes no third argument. */
static int (*real_fcntl_getfl)(int, int);
static int (*real_connect)(int, const struct sockaddr *, socklen_t);
static ssize_t (*real_send)(int, const void *, size_t, int);
static ssize_t (*real_sendto)(int, const void *, size_t, int, const struct sockaddr *, socklen_t);
static ssize_t (*real_recv)(int, void *, size_t, int);
static ssize_t (*real_recvfrom)(int, void *, size_t, int, struct sockaddr *, socklen_t *);
static ssize_t (*real_write)(int, const void *, size_t);
/* Only `shim_init` reads the environment here; the getenv INTERPOSER lives in
 * reproit_shim_time.c with the rest of the determinism envelope. */
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
    const char *pinned = real_getenv ? real_getenv("REPROIT_ENV_PINNED") : NULL;
    G.env_pinned = pinned && pinned[0] == '1';
    if (replay && replay[0]) {
        G.mode = 2;
        load_replay(replay);
    } else if (record && record[0]) {
        G.mode = 1;
        reproit_open_log(record);
    }
    /* Start the syscall completeness layer LAST, so it inherits the open log
     * and the loaded replay entries. When it comes up it owns files and path
     * metadata and the interposed file calls below become passthrough. */
    const char *seccomp_env = real_getenv ? real_getenv("REPROIT_SECCOMP") : NULL;
    int layered = reproit_seccomp_start(seccomp_env);
    const char *layerless_reason =
        seccomp_env && seccomp_env[0] == '0' ? "disabled by REPROIT_SECCOMP=0"
#ifdef __linux__
                                             : "seccomp user-notify unavailable on this host";
#else
                                             : "unsupported on this platform";
#endif
    if (G.mode == 1) {
        /* The capsule records WHICH layer captured it, and a layer-less
         * capture is a named event, never a silent downgrade. */
        record_blob(K_LAYER, layered ? "seccomp" : "libc", NULL, 0, 0, 0);
        if (!layered) {
            reproit_layer_note(layerless_reason);
        }
    } else if (G.mode == 2 && !layered) {
        /* A capsule captured by the completeness layer holds path metadata
         * only that layer can serve; replaying it on the libc boundary dies
         * confusingly mid-run with ZERO divergence lines (measured: OSError
         * Errno 9 inside CPython's getpath). Refuse by name, fail closed,
         * before the program runs at all. */
        for (size_t i = 0; i < G.entry_count; i++) {
            if (G.entries[i].kind == K_LAYER && strcmp(G.entries[i].key, "seccomp") == 0) {
                diverge("seccomp-required",
                        "capsule was captured by the seccomp completeness layer and this "
                        "replay cannot install it; refusing a layer-less replay");
                _exit(3);
            }
        }
        reproit_layer_note(layerless_reason);
    }
}

__attribute__((destructor)) static void shim_report(void) { reproit_report(); }


/* Resolve a (dirfd, path) pair to the capsule key for that file, the SAME way
 * in record and in replay. Doing it in only one of the two modes is what made
 * a relative open divergent at replay while two different files shared one
 * key at record; see reproit_path_key in the capsule header for the measured
 * evidence in both directions.
 *
 * A base that cannot be resolved falls back to the path as given. That is
 * symmetric between the two modes, so it costs a spurious divergence at worst
 * and never a wrong answer. */
static void path_key(int dirfd, const char *path, char *out, size_t cap) {
    if (!path || !path[0]) {
        snprintf(out, cap, "-");
        return;
    }
    if (path[0] == '/') {
        reproit_path_key(path, out, cap);
        return;
    }
    char base[MAX_PATH_LEN / 2];
    base[0] = 0;
    if (dirfd == AT_FDCWD) {
        if (!getcwd(base, sizeof(base))) {
            base[0] = 0;
        }
    } else {
#ifdef __linux__
        char link[64];
        snprintf(link, sizeof(link), "/proc/self/fd/%d", dirfd);
        ssize_t n = readlink(link, base, sizeof(base) - 1);
        if (n > 0) {
            base[n] = 0;
        } else {
            base[0] = 0;
        }
#else
        if (fcntl(dirfd, F_GETPATH, base) != 0) {
            base[0] = 0;
        }
#endif
    }
    if (!base[0]) {
        snprintf(out, cap, "%s", path);
        return;
    }
    char absolute[MAX_PATH_LEN];
    snprintf(absolute, sizeof(absolute), "%s/%s", base, path);
    reproit_path_key(absolute, out, cap);
}

static void track_path(int fd, const char *path) {
    if (fd >= 0 && fd < MAX_FDS) {
        snprintf(G.paths[fd], sizeof(G.paths[fd]), "%s", path ? path : "-");
        G.mover_end[fd] = 0;
        G.mover_capped[fd] = 0;
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
    /* The open first: it bounds the reads that belong to this stream. A file
     * opened twice is two streams, and gathering across the whole log served
     * their bytes concatenated. */
    size_t at = 0;
    entry_t *opened = next_entry_at(K_OPEN, path, &at);
    size_t from = opened ? at + 1 : 0;
    size_t to = opened ? next_key_index(K_OPEN, path, at) : G.entry_count;
    size_t len = gather_span(K_READ, path, from, to, &content);
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
     * refuses. Fail closed and name the shortfall instead. */
    if (opened && opened->b > 0 && len == 0) {
        diverge_short("incomplete-file", path, opened->b, 0);
        free(content);
        return -3;
    }
    /* The capsule holds a PREFIX of the file: the source outgrew the inline
     * cap, or its reads were lost to the entry bound. Deferring this check to
     * read() is unsound, because mmap and glibc stdio consume the descriptor
     * without ever calling read (the phase 1 lesson), so a short memfd would
     * hand back zeros inside the last mapped page in silence. The shortfall
     * fails HERE, at the last point this layer can still see it. */
    if (opened && opened->b > 0 && len < (size_t)opened->b) {
        diverge_short("truncated-file", path, opened->b, (long)len);
        free(content);
        return -3;
    }
    /* The other direction is wrong too: MORE bytes than the recording
     * observed means the same range was recorded twice (a re-mapped file, a
     * re-read region) and a doubled serve is as silent and as wrong as a
     * short one. */
    if (opened && opened->b > 0 && len > (size_t)opened->b) {
        diverge_short("overlong-file", path, opened->b, (long)len);
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
    /* A shortfall can no longer reach this point (both cases fail above), so
     * incomplete is a backstop: if a future serving path reintroduces one,
     * the over-read still diverges with its counts rather than reading short. */
    if (fd >= 0 && fd < MAX_FDS) {
        fdstate_t *state = &G.fds[fd];
        memset(state, 0, sizeof(*state));
        state->active = 1;
        state->is_socket = 0;
        state->incomplete = opened && opened->b > (long)len;
        state->recorded = opened ? opened->b : (long)len;
        state->held = (long)len;
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
    /* The syscall layer owns files when it is live: this call still issues
     * the openat the supervisor sees, so serving it here too would record the
     * same read under two different keys. */
    if (return_real || G.seccomp_files) {
        LEAVE();
        return real_open(path, flags, mode);
    }
    int fd;
    char key[MAX_KEY_LEN];
    path_key(AT_FDCWD, path, key, sizeof(key));
    if (G.mode == 2) {
        fd = serve_open(key);
        if (fd == -2) {
            diverge("file", key);
            errno = ENOENT;
            fd = -1;
        } else if (fd == -3) {
            /* serve_open already named the shortfall and its counts */
            errno = EIO;
            fd = -1;
        }
    } else {
        fd = real_open(path, flags, mode);
        track_path(fd, key);
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
    if (return_real || G.seccomp_files) {
        LEAVE();
        return real_openat(dirfd, path, flags, mode);
    }
    int fd;
    /* Never fall through to the real filesystem at replay. Measured: falling
     * through for relative paths made a python3 replay read the live disk, so
     * a deleted input produced an empty result with ZERO divergences, a false
     * negative. A path the capsule does not carry is a divergence, even when
     * that makes an interpreted runtime noisy: fail closed is the whole
     * contract. */
    char key[MAX_KEY_LEN];
    path_key(dirfd, path, key, sizeof(key));
    if (G.mode == 2) {
        fd = serve_open(key);
        if (fd == -2) {
            diverge("file", key);
            errno = ENOENT;
            fd = -1;
        } else if (fd == -3) {
            /* serve_open already named the shortfall and its counts */
            errno = EIO;
            fd = -1;
        }
    } else if (G.mode == 1) {
        fd = real_openat(dirfd, path, flags, mode);
        track_path(fd, key);
        record_blob(K_OPEN, key, NULL, 0, fd >= 0 ? fd : -errno, file_size(fd));
    } else {
        fd = real_openat(dirfd, path, flags, mode);
    }
    LEAVE();
    return fd;
}

/* Phase 2: is this fd the program's input stream rather than a file it
 * opened? An inherited descriptor has no recorded path, and stdin is the
 * input source a headless interactive program is driven through. */
/* Consecutive input reads tolerated at one tick before the schedule gives up
 * and serves early. A tick driven loop advances its clock every frame, so it
 * never comes close; a program that blocks on input without a clock does. */
#define MAX_INPUT_HOLDS 4096

static int is_input_stream(int fd) {
    return fd == 0 && (fd >= MAX_FDS || !G.paths[fd][0]);
}

/* Serve one recorded input event, but only once the program has reached the
 * TICK it arrived on. A fixed timestep loop reads the clock once per frame,
 * so holding an event back until its tick reproduces the timing relationship
 * between input and frames instead of delivering the whole session at once.
 *
 * A blocking fd cannot be held back without hanging the program, so it is
 * served early and counted. Nothing is silently reordered. */
static ssize_t serve_input(int fd, void *buf, size_t count) {
    entry_t *e = next_entry(K_INPUT, "stdin");
    if (!e) {
        /* The recording's input is exhausted: end of stream, not a
         * divergence. A replayed loop may poll more often than the recorded
         * one did, and an extra poll finding nothing is not drift. */
        return 0;
    }
    if ((size_t)e->b > G.tick) {
        /* Hold it back until the program reaches the tick this input arrived
         * on. The real descriptor's flags are deliberately NOT consulted: at
         * replay the input comes from the capsule, not from that descriptor,
         * and an inherited stdin that happens to be blocking would otherwise
         * defeat the schedule entirely.
         *
         * The only real risk is a program that waits for input without ever
         * reading its clock, which would spin. That is bounded: after
         * MAX_INPUT_HOLDS consecutive asks with no tick advance, the input is
         * served early and COUNTED, so the schedule is never quietly dropped. */
        if (G.tick != G.input_hold_tick) {
            G.input_hold_tick = G.tick;
            G.input_holds = 0;
        }
        if (++G.input_holds <= MAX_INPUT_HOLDS) {
            e->consumed = 0; /* not yet: let the next frame ask again */
            errno = EAGAIN;
            return -1;
        }
        G.input_early++;
    }
    size_t take = e->blob_len < count ? e->blob_len : count;
    if (take) {
        memcpy(buf, e->blob, take);
    }
    G.input_served++;
    G.served++;
    return (ssize_t)take;
}

ssize_t read(int fd, void *buf, size_t count) {
    int return_real = 0;
    RESOLVE(read);
    ENTER();
    if (return_real) {
        return real_read(fd, buf, count);
    }
    ssize_t got;
    if (G.mode == 2 && is_input_stream(fd)) {
        got = serve_input(fd, buf, count);
        LEAVE();
        return got;
    }
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active && !G.fds[fd].is_socket) {
        /* A replayed FILE: the kernel serves the memfd. An EOF on a fd whose
         * capture was partial means the program wanted bytes the capsule
         * never carried, so it fails closed rather than reading short. */
        got = real_read(fd, buf, count);
        if (got == 0 && count > 0 && G.fds[fd].incomplete) {
            diverge_short("truncated-file", G.fds[fd].key, G.fds[fd].recorded, G.fds[fd].held);
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
    if (G.mode == 1 && got > 0 && is_input_stream(fd)) {
        /* b carries the tick this input arrived on, which is what replay
         * schedules against. */
        record_blob(K_INPUT, "stdin", (const unsigned char *)buf, (size_t)got, fd, (long)G.tick);
    } else if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.paths[fd][0]) {
        record_content(K_READ, G.paths[fd], (const unsigned char *)buf, (size_t)got, fd, 0);
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
        record_content(K_READ, G.paths[fd], (const unsigned char *)buf, (size_t)got, fd, offset);
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
        G.mover_end[fd] = 0;
        G.mover_capped[fd] = 0;
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

/* glibc's stdio calls its own internal __open and __read rather than the
 * public symbols, so fopen and fread are INVISIBLE to a libc-symbol boundary
 * unless the boundary also covers stdio. This was measured, not assumed: the
 * first run of validation/process/measure.sh recorded zero open and zero read
 * entries for the fopen path. Replay serves the recorded content through
 * fmemopen, so fread, fgets, fseek, and fclose then work natively. */
#define MAX_STREAMS 256
static struct {
    FILE *handle;
    char path[MAX_KEY_LEN];
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
    /* stdio opens a descriptor underneath, which the syscall layer already
     * sees and serves; recording here as well would store the same bytes
     * twice and replay them concatenated (measured: "boomboom"). */
    if (return_real || G.seccomp_files) {
        LEAVE();
        return real_fopen(path, mode);
    }
    FILE *handle;
    char key[MAX_KEY_LEN];
    path_key(AT_FDCWD, path, key, sizeof(key));
    if (G.mode == 2) {
        unsigned char *content = NULL;
        size_t at = 0;
        entry_t *opened = next_entry_at(K_OPEN, key, &at);
        size_t from = opened ? at + 1 : 0;
        size_t to = opened ? next_key_index(K_OPEN, key, at) : G.entry_count;
        size_t len = gather_span(K_READ, key, from, to, &content);
        if (!opened && !content) {
            diverge("file", key);
            errno = ENOENT;
            LEAVE();
            return NULL;
        }
        if (opened && opened->b > 0 && len == 0) {
            diverge_short("incomplete-file", key, opened->b, 0);
            free(content);
            errno = EIO;
            LEAVE();
            return NULL;
        }
        /* A fmemopen stream is consumed by glibc stdio internals this
         * boundary cannot see (fgets, fscanf, getline all bypass the fread
         * interposer), so a prefix cannot defer its check to the reads. A
         * capsule holding fewer bytes than the recording observed fails at
         * the serve, with both counts named. */
        if (opened && opened->b > 0 && len < (size_t)opened->b) {
            diverge_short("truncated-file", key, opened->b, (long)len);
            free(content);
            errno = EIO;
            LEAVE();
            return NULL;
        }
        if (opened && opened->b > 0 && len > (size_t)opened->b) {
            diverge_short("overlong-file", key, opened->b, (long)len);
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
        track_stream(handle, key);
        record_blob(K_OPEN, key, NULL, 0, handle ? 0 : -errno,
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
            record_content(K_READ, path, (const unsigned char *)buf, got * size, 0, 0);
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
