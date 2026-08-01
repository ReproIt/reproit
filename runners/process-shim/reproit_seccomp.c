/*
 * The completeness layer: seccomp user-notify.
 *
 * WHY THIS EXISTS, measured rather than assumed. The LD_PRELOAD boundary in
 * reproit_shim.c only sees calls that cross the dynamic-linking boundary. A
 * libc that calls its OWN open/stat internally never crosses it, so a
 * python3 replay was measured serving 5 files from the capsule while the
 * loader, locale, and gconv paths fell through to the LIVE filesystem with
 * ZERO divergences. That silently violates the fail-closed contract: replay
 * was not hermetic and did not say so.
 *
 * Every one of those paths converges at the syscall layer, which is what this
 * file supervises. The division of labour:
 *
 *   libc shim (fast path): clock, randomness, environment, sockets.
 *   seccomp (completeness): files and path metadata, whoever called them.
 *
 * With this layer active the libc file interposition steps aside entirely
 * (see G.seccomp_files), so there is exactly one source of truth per class
 * and the two layers can never record the same read under two different keys.
 *
 * MECHANISM. The shim constructor forks a supervisor BEFORE installing the
 * filter, so the supervisor is never itself filtered. The target installs the
 * filter, hands the listener fd to the supervisor over a socketpair, and
 * proceeds into main.
 *
 *   record: the supervisor reads the path out of the target, performs the
 *           call itself, writes what it saw into the capsule, and answers
 *           CONTINUE so the kernel runs the real syscall.
 *   replay: the supervisor answers from the capsule alone. A file is served
 *           by materializing its recorded bytes as a REAL file in a scratch
 *           tree and injecting a descriptor to it with
 *           SECCOMP_IOCTL_NOTIF_ADDFD, so the target's later read, lseek,
 *           fstat, and mmap are served by the kernel and cannot diverge on
 *           chunk size. A path the capsule never recorded is a DIVERGENCE and
 *           an error return, never a fall through.
 *
 * A supervisor that cannot install stays out of the way: the shim keeps
 * working exactly as it did before, and the capsule records that the layer
 * was absent so a replay can never silently claim completeness it lacks.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stdio.h>
#include <stdio_ext.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SECCOMP_FILTER_FLAG_NEW_LISTENER
#define SECCOMP_FILTER_FLAG_NEW_LISTENER (1UL << 3)
#endif
#ifndef SECCOMP_USER_NOTIF_FLAG_CONTINUE
#define SECCOMP_USER_NOTIF_FLAG_CONTINUE (1UL << 0)
#endif
#ifndef SECCOMP_IOCTL_NOTIF_RECV
#define SECCOMP_IOCTL_NOTIF_RECV SECCOMP_IOR(0, struct seccomp_notif)
#endif

/* Per file inline cap: REPROIT_FILE_CAP, shared with the libc data movers so
 * a file bounds the same way whichever layer records it. Bigger than the
 * SDKs' 8 KiB body rule on purpose: a process input is a whole file, not an
 * HTTP body, and a locale archive is 350 KiB. A file past the cap records
 * its size, its bytes up to the cap, and a `trunc` marker naming the cap;
 * the completeness oracle in serve_open then refuses to serve the prefix
 * WITH the cap named. The check cannot defer to an over-read: the kernel
 * answers the injected descriptor's reads, so this layer never sees them
 * (the earlier claim that it fired there was measured false; a short scratch
 * file replayed a truncated cat with ZERO divergences). */
#define CHUNK MAX_BLOB

static pid_t sup_target;
static int sup_notify = -1;

/* Syscalls this layer owns. Everything else is ALLOWed outright by the
 * filter, so the kernel never wakes the supervisor for it and the overhead
 * stays proportional to path work, not to syscall count. */
static const int TRAPPED[] = {
#ifdef __NR_open
    __NR_open,
#endif
#ifdef __NR_openat
    __NR_openat,
#endif
#ifdef __NR_openat2
    __NR_openat2,
#endif
#ifdef __NR_stat
    __NR_stat,
#endif
#ifdef __NR_lstat
    __NR_lstat,
#endif
#ifdef __NR_newfstatat
    __NR_newfstatat,
#endif
#ifdef __NR_statx
    __NR_statx,
#endif
#ifdef __NR_access
    __NR_access,
#endif
#ifdef __NR_faccessat
    __NR_faccessat,
#endif
#ifdef __NR_faccessat2
    __NR_faccessat2,
#endif
#ifdef __NR_readlink
    __NR_readlink,
#endif
#ifdef __NR_readlinkat
    __NR_readlinkat,
#endif
#ifdef __NR_getcwd
    __NR_getcwd,
#endif
#ifdef __NR_getdents64
    __NR_getdents64,
#endif
};
#define TRAPPED_COUNT ((int)(sizeof(TRAPPED) / sizeof(TRAPPED[0])))

#if defined(__x86_64__)
#define REPROIT_AUDIT_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define REPROIT_AUDIT_ARCH AUDIT_ARCH_AARCH64
#else
#define REPROIT_AUDIT_ARCH 0
#endif

/* ---- target memory ---------------------------------------------------- */

static ssize_t peek(unsigned long remote, void *local, size_t len) {
    struct iovec l = {.iov_base = local, .iov_len = len};
    struct iovec r = {.iov_base = (void *)remote, .iov_len = len};
    return process_vm_readv(sup_target, &l, 1, &r, 1, 0);
}

static ssize_t poke(unsigned long remote, const void *local, size_t len) {
    struct iovec l = {.iov_base = (void *)local, .iov_len = len};
    struct iovec r = {.iov_base = (void *)remote, .iov_len = len};
    return process_vm_writev(sup_target, &l, 1, &r, 1, 0);
}

/* Read a NUL terminated path out of the target, one page-safe chunk at a
 * time. A path we cannot read is reported as empty and handled as unknown. */
static int peek_path(unsigned long remote, char *out, size_t cap) {
    size_t got = 0;
    while (got < cap - 1) {
        size_t want = 256;
        if (got + want > cap - 1) {
            want = cap - 1 - got;
        }
        if (peek(remote + got, out + got, want) < 0) {
            if (got == 0) {
                out[0] = 0;
                return -1;
            }
            break;
        }
        for (size_t i = got; i < got + want; i++) {
            if (out[i] == 0) {
                return 0;
            }
        }
        got += want;
    }
    out[cap - 1] = 0;
    return 0;
}

/* Absolute path for a (dirfd, path) pair, resolved against the TARGET's cwd
 * and fd table through /proc, never the supervisor's own.
 *
 * The result is both the capsule KEY and the path this layer opens, so it is
 * normalized with the identity-preserving rewrites only (see
 * reproit_normalize_path). Without that, `open("./x")` and `open("/d/x")`
 * keyed apart and the second DIVERGED on a file the capsule already held. */
static void absolutize(int dirfd, const char *path, char *out, size_t cap) {
    if (!path || !path[0]) {
        snprintf(out, cap, "-");
        return;
    }
    if (path[0] == '/') {
        snprintf(out, cap, "%s", path);
        reproit_normalize_path(out);
        return;
    }
    char base[MAX_PATH_LEN];
    char link[64];
    if (dirfd == AT_FDCWD) {
        snprintf(link, sizeof(link), "/proc/%d/cwd", (int)sup_target);
    } else {
        snprintf(link, sizeof(link), "/proc/%d/fd/%d", (int)sup_target, dirfd);
    }
    ssize_t n = readlink(link, base, sizeof(base) - 1);
    if (n <= 0) {
        snprintf(out, cap, "%s", path);
        return;
    }
    base[n] = 0;
    snprintf(out, cap, "%s/%s", base, path);
    reproit_normalize_path(out);
}

/* ---- responses -------------------------------------------------------- */

static void respond(__u64 id, __s64 val, __s32 error, __u32 flags) {
    struct seccomp_notif_resp resp;
    memset(&resp, 0, sizeof(resp));
    resp.id = id;
    resp.val = val;
    resp.error = error;
    resp.flags = flags;
    if (ioctl(sup_notify, SECCOMP_IOCTL_NOTIF_SEND, &resp) < 0 && errno != ENOENT) {
        /* The target died mid call. Nothing to do and nothing to claim. */
    }
}

static void respond_continue(__u64 id) { respond(id, 0, 0, SECCOMP_USER_NOTIF_FLAG_CONTINUE); }

static void respond_error(__u64 id, int err) { respond(id, 0, -err, 0); }

/* Inject a descriptor into the target and answer with its number. */
static void respond_with_fd(__u64 id, int local_fd) {
    struct seccomp_notif_addfd addfd;
    memset(&addfd, 0, sizeof(addfd));
    addfd.id = id;
    addfd.srcfd = (__u32)local_fd;
    addfd.newfd = 0;
    addfd.flags = 0;
    addfd.newfd_flags = 0;
    int injected = ioctl(sup_notify, SECCOMP_IOCTL_NOTIF_ADDFD, &addfd);
    if (injected < 0) {
        respond_error(id, EIO);
        return;
    }
    respond(id, injected, 0, 0);
}

/* ---- record ----------------------------------------------------------- */

/* A directory the program opened. Its NAMES are the payload: locale and gconv
 * discovery enumerate directories, so a replay that refuses enumeration sends
 * the interpreter down a different path than the recorded run took. */
static void record_dir(const char *absolute) {
    DIR *dir = opendir(absolute);
    if (!dir) {
        return;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        record_blob(K_DIRENT, absolute, (const unsigned char *)entry->d_name,
                    strlen(entry->d_name), entry->d_type, 0);
    }
    closedir(dir);
}

static void record_file(const char *absolute) {
    struct stat info;
    int fd = open(absolute, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        record_blob(K_OPEN, absolute, NULL, 0, -errno, 0);
        return;
    }
    if (fstat(fd, &info) == 0 && S_ISDIR(info.st_mode)) {
        /* b = -1 marks a directory, which replay rebuilds rather than serving
         * as a memfd; an empty regular file records b = 0 and stays distinct. */
        record_blob(K_OPEN, absolute, NULL, 0, 0, -1);
        close(fd);
        record_dir(absolute);
        return;
    }
    if (!S_ISREG(info.st_mode)) {
        /* A device or socket: the open is recorded so replay knows the path
         * existed, with no content to serve. */
        record_blob(K_OPEN, absolute, NULL, 0, 0, 0);
        close(fd);
        return;
    }
    record_blob(K_OPEN, absolute, NULL, 0, 0, (long)info.st_size);
    unsigned char buf[CHUNK];
    size_t total = 0;
    ssize_t got;
    while (total < REPROIT_FILE_CAP) {
        size_t take = sizeof(buf);
        if (total + take > REPROIT_FILE_CAP) {
            take = REPROIT_FILE_CAP - total;
        }
        got = read(fd, buf, take);
        if (got <= 0) {
            break;
        }
        record_blob(K_READ, absolute, buf, (size_t)got, 0, 0);
        total += (size_t)got;
    }
    if ((long)total < (long)info.st_size && total >= REPROIT_FILE_CAP) {
        /* The file outgrew the per-file cap: name it now, so the replay
         * refusal carries the bound instead of an anonymous shortfall. */
        record_blob(K_TRUNC, absolute, NULL, 0, (long)REPROIT_FILE_CAP, (long)info.st_size);
    }
    close(fd);
}

static void record_stat_like(kind_t kind, const char *absolute, const void *blob, size_t len,
                             long retval) {
    record_blob(kind, absolute, (const unsigned char *)blob, len, retval, 0);
}

/* ---- replay ----------------------------------------------------------- */


/* Widen replay from "what the recording READ" to "what the recording can
 * ANSWER". A branchy startup does not take the same path twice: the recorded
 * run enumerated a directory and moved on, while replay asks about a name
 * inside it that the recording never opened. If the capsule holds that
 * directory's full listing and the name is NOT in it, the recording already
 * answers the question authoritatively: the name did not exist. Serving that
 * ENOENT is faithful, not a guess.
 *
 * Fail closed is preserved in both directions. Without a recorded listing for
 * the parent there is no evidence either way, so the caller diverges as
 * before; and a name that IS in the listing but has no entry of its own still
 * diverges, because the capsule knows it existed but not what it held. */
/* Did the recording observe this exact path as absent? A path metadata call
 * that failed is recorded with a = -errno, and ENOENT or ENOTDIR is a fact
 * about the filesystem, not about the call that asked. */
static int recorded_absent(const char *path) {
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind != K_OPEN && e->kind != K_STAT && e->kind != K_STATX &&
            e->kind != K_ACCESS) {
            continue;
        }
        if (strcmp(e->key, path) != 0) {
            continue;
        }
        if (e->a == -ENOENT || e->a == -ENOTDIR) {
            return 1;
        }
    }
    return 0;
}

static int listing_denies(const char *absolute) {
    const char *slash = strrchr(absolute, '/');
    if (!slash || slash == absolute) {
        return 0;
    }
    char parent[MAX_PATH_LEN];
    size_t parent_len = (size_t)(slash - absolute);
    if (parent_len >= sizeof(parent)) {
        return 0;
    }
    memcpy(parent, absolute, parent_len);
    parent[parent_len] = 0;
    const char *name = slash + 1;
    size_t name_len = strlen(name);
    int listed = 0;
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind != K_DIRENT || strcmp(e->key, parent) != 0 || !e->blob) {
            continue;
        }
        listed = 1;
        if (e->blob_len == name_len && memcmp(e->blob, name, name_len) == 0) {
            return 0;
        }
    }
    if (listed) {
        return 1;
    }
    /* No listing for the parent, but the recording may still answer: a
     * directory it observed as ABSENT cannot contain anything. Walk the
     * ancestors and let a recorded ENOENT or ENOTDIR settle every path
     * beneath it. Ruby's gem search asks about a specifications/default
     * directory whose parent the recording already saw fail with ENOENT. */
    char probe[MAX_PATH_LEN];
    snprintf(probe, sizeof(probe), "%s", parent);
    for (;;) {
        if (recorded_absent(probe)) {
            return 1;
        }
        char *cut = strrchr(probe, '/');
        if (!cut || cut == probe) {
            return 0;
        }
        *cut = 0;
    }
}

static void serve_open(__u64 id, const char *absolute) {
    unsigned char *content = NULL;
    /* The open comes FIRST, because it decides which reads belong to this
     * stream. This layer re-reads the whole file on every open, so gathering
     * across the log served a file opened twice with its own text twice; see
     * gather_span in the capsule header for the ruby measurement. */
    size_t at = 0;
    entry_t *opened = find_entry_at(K_OPEN, absolute, &at);
    size_t from = opened ? at + 1 : 0;
    size_t to = opened ? next_key_index(K_OPEN, absolute, at) : G.entry_count;
    size_t len = gather_span(K_READ, absolute, from, to, &content);
    if (opened && opened->b == -1) {
        free(content);
        int dirfd = reproit_serve_dir(absolute);
        if (dirfd < 0) {
            diverge("directory", absolute);
            respond_error(id, ENOENT);
            return;
        }
        G.served++;
        respond_with_fd(id, dirfd);
        close(dirfd);
        return;
    }
    if (!opened && !content) {
        if (listing_denies(absolute)) {
            G.served++;
            respond_error(id, ENOENT);
            return;
        }
        diverge("file", absolute);
        respond_error(id, ENOENT);
        return;
    }
    if (opened && opened->a < 0 && !content) {
        respond_error(id, (int)(-opened->a));
        free(content);
        return;
    }
    /* Completeness oracle, same rule the libc layer applies: the capsule saw
     * the open but never the bytes, so serving an empty file would be a
     * silent wrong replay. */
    if (opened && opened->b > 0 && len == 0) {
        diverge_short("incomplete-file", absolute, opened->b, 0);
        free(content);
        respond_error(id, EIO);
        return;
    }
    /* The capsule holds a PREFIX (the file outgrew FILE_CAP, or its reads
     * were lost to the entry bound). The injected descriptor is answered by
     * the KERNEL, so this layer never sees the target's reads and cannot
     * defer the check to the over-read the way the libc read path can.
     * Serving the short copy would replay wrong in silence (measured: a
     * capsule truncated by hand replayed a shortened cat with zero
     * divergences), so it fails at the serve, with both counts named. */
    if (opened && opened->b > 0 && len < (size_t)opened->b) {
        diverge_short("truncated-file", absolute, opened->b, (long)len);
        free(content);
        respond_error(id, EIO);
        return;
    }
    /* MORE than the recording observed is the same silent wrong replay from
     * the other side: the range was recorded twice and would serve doubled. */
    if (opened && opened->b > 0 && len > (size_t)opened->b) {
        diverge_short("overlong-file", absolute, opened->b, (long)len);
        free(content);
        respond_error(id, EIO);
        return;
    }
    int fd = reproit_materialize(absolute, content, len);
    free(content);
    if (fd < 0) {
        respond_error(id, EIO);
        return;
    }
    G.served++;
    respond_with_fd(id, fd);
    close(fd);
}

static void serve_stat(__u64 id, kind_t kind, const char *absolute, unsigned long out_ptr,
                       size_t out_len) {
    entry_t *e = find_entry(kind, absolute);
    if (!e) {
        if (listing_denies(absolute)) {
            G.served++;
            respond_error(id, ENOENT);
            return;
        }
        diverge("stat", absolute);
        respond_error(id, ENOENT);
        return;
    }
    if (e->a < 0) {
        respond_error(id, (int)(-e->a));
        return;
    }
    if (out_ptr && e->blob && e->blob_len) {
        size_t len = e->blob_len < out_len ? e->blob_len : out_len;
        if (poke(out_ptr, e->blob, len) < 0) {
            respond_error(id, EFAULT);
            return;
        }
    }
    G.served++;
    respond(id, 0, 0, 0);
}

static void serve_string(__u64 id, kind_t kind, const char *key, unsigned long out_ptr,
                         size_t out_cap) {
    entry_t *e = find_entry(kind, key);
    if (!e) {
        if (kind == K_READLINK && listing_denies(key)) {
            G.served++;
            respond_error(id, ENOENT);
            return;
        }
        diverge(kind == K_READLINK ? "readlink" : "getcwd", key);
        respond_error(id, ENOENT);
        return;
    }
    /* The recorded call FAILED (a readlink on a regular file returns EINVAL,
     * on a missing path ENOENT). Replaying that failure faithfully is the
     * whole point; treating a blobless entry as absent made an interpreter
     * diverge on its own executable. */
    if (e->a < 0 || !e->blob) {
        respond_error(id, e->a < 0 ? (int)(-e->a) : EINVAL);
        return;
    }
    size_t len = e->blob_len < out_cap ? e->blob_len : out_cap;
    if (out_ptr && len && poke(out_ptr, e->blob, len) < 0) {
        respond_error(id, EFAULT);
        return;
    }
    G.served++;
    respond(id, (long)len, 0, 0);
}

/* ---- dispatch --------------------------------------------------------- */

static void handle_open(const struct seccomp_notif *req, int dirfd_arg, int path_arg) {
    char path[MAX_PATH_LEN];
    char absolute[MAX_PATH_LEN];
    int dirfd = dirfd_arg < 0 ? AT_FDCWD : (int)req->data.args[dirfd_arg];
    if (peek_path(req->data.args[path_arg], path, sizeof(path)) < 0) {
        respond_continue(req->id);
        return;
    }
    absolutize(dirfd, path, absolute, sizeof(absolute));
    if (G.mode == 2) {
        serve_open(req->id, absolute);
    } else {
        record_file(absolute);
        respond_continue(req->id);
    }
}

static void handle_stat(const struct seccomp_notif *req, int dirfd_arg, int path_arg, int buf_arg,
                        int is_statx) {
    char path[MAX_PATH_LEN];
    char absolute[MAX_PATH_LEN];
    int dirfd = dirfd_arg < 0 ? AT_FDCWD : (int)req->data.args[dirfd_arg];
    if (peek_path(req->data.args[path_arg], path, sizeof(path)) < 0) {
        respond_continue(req->id);
        return;
    }
    /* An empty path with AT_EMPTY_PATH is a question about an already open
     * descriptor, which at replay is a real memfd the kernel answers
     * correctly. Nothing to serve and nothing to record. */
    if (!path[0]) {
        respond_continue(req->id);
        return;
    }
    absolutize(dirfd, path, absolute, sizeof(absolute));
    kind_t kind = is_statx ? K_STATX : K_STAT;
    size_t out_len = is_statx ? sizeof(struct statx) : sizeof(struct stat);
    if (G.mode == 2) {
        serve_stat(req->id, kind, absolute, req->data.args[buf_arg], out_len);
        return;
    }
    if (is_statx) {
        struct statx info;
        memset(&info, 0, sizeof(info));
        long rc = syscall(__NR_statx, AT_FDCWD, absolute, 0, STATX_BASIC_STATS, &info);
        record_stat_like(kind, absolute, &info, sizeof(info), rc == 0 ? 0 : -errno);
    } else {
        struct stat info;
        memset(&info, 0, sizeof(info));
        int rc = stat(absolute, &info);
        record_stat_like(kind, absolute, &info, sizeof(info), rc == 0 ? 0 : -errno);
    }
    respond_continue(req->id);
}

static void handle_access(const struct seccomp_notif *req, int dirfd_arg, int path_arg) {
    char path[MAX_PATH_LEN];
    char absolute[MAX_PATH_LEN];
    int dirfd = dirfd_arg < 0 ? AT_FDCWD : (int)req->data.args[dirfd_arg];
    if (peek_path(req->data.args[path_arg], path, sizeof(path)) < 0) {
        respond_continue(req->id);
        return;
    }
    absolutize(dirfd, path, absolute, sizeof(absolute));
    if (G.mode == 2) {
        entry_t *e = find_entry(K_ACCESS, absolute);
        if (!e) {
            if (listing_denies(absolute)) {
                G.served++;
                respond_error(req->id, ENOENT);
                return;
            }
            diverge("access", absolute);
            respond_error(req->id, ENOENT);
            return;
        }
        G.served++;
        if (e->a < 0) {
            respond_error(req->id, (int)(-e->a));
        } else {
            respond(req->id, 0, 0, 0);
        }
        return;
    }
    int rc = access(absolute, F_OK);
    record_blob(K_ACCESS, absolute, NULL, 0, rc == 0 ? 0 : -errno, 0);
    respond_continue(req->id);
}

static void handle_readlink(const struct seccomp_notif *req, int dirfd_arg, int path_arg,
                            int buf_arg, int size_arg) {
    char path[MAX_PATH_LEN];
    char absolute[MAX_PATH_LEN];
    int dirfd = dirfd_arg < 0 ? AT_FDCWD : (int)req->data.args[dirfd_arg];
    if (peek_path(req->data.args[path_arg], path, sizeof(path)) < 0) {
        respond_continue(req->id);
        return;
    }
    absolutize(dirfd, path, absolute, sizeof(absolute));
    if (G.mode == 2) {
        serve_string(req->id, K_READLINK, absolute, req->data.args[buf_arg],
                     (size_t)req->data.args[size_arg]);
        return;
    }
    char target[MAX_PATH_LEN];
    ssize_t n = readlink(absolute, target, sizeof(target));
    if (n < 0) {
        record_blob(K_READLINK, absolute, NULL, 0, -errno, 0);
    } else {
        record_blob(K_READLINK, absolute, (unsigned char *)target, (size_t)n, n, 0);
    }
    respond_continue(req->id);
}

static void handle_getcwd(const struct seccomp_notif *req) {
    if (G.mode == 2) {
        serve_string(req->id, K_GETCWD, "cwd", req->data.args[0], (size_t)req->data.args[1]);
        return;
    }
    char link[64];
    char cwd[MAX_PATH_LEN];
    snprintf(link, sizeof(link), "/proc/%d/cwd", (int)sup_target);
    ssize_t n = readlink(link, cwd, sizeof(cwd) - 1);
    if (n > 0) {
        cwd[n] = 0;
        /* getcwd returns the NUL, so the recorded blob carries it too. */
        record_blob(K_GETCWD, "cwd", (unsigned char *)cwd, (size_t)n + 1, n + 1, 0);
    }
    respond_continue(req->id);
}

/* Directory listing. At replay the descriptor is a REAL directory that
 * reproit_serve_dir rebuilt from the capsule's recorded names, so the kernel answers
 * the enumeration correctly and this layer stays out of the struct layout
 * business entirely. At record the directory's names were captured when it
 * was opened, so nothing is needed here either. */
static void handle_getdents(const struct seccomp_notif *req) { respond_continue(req->id); }

static void dispatch(const struct seccomp_notif *req) {
    switch ((int)req->data.nr) {
#ifdef __NR_open
    case __NR_open:
        handle_open(req, -1, 0);
        return;
#endif
#ifdef __NR_openat
    case __NR_openat:
        handle_open(req, 0, 1);
        return;
#endif
#ifdef __NR_openat2
    case __NR_openat2:
        handle_open(req, 0, 1);
        return;
#endif
#ifdef __NR_stat
    case __NR_stat:
        handle_stat(req, -1, 0, 1, 0);
        return;
#endif
#ifdef __NR_lstat
    case __NR_lstat:
        handle_stat(req, -1, 0, 1, 0);
        return;
#endif
#ifdef __NR_newfstatat
    case __NR_newfstatat:
        handle_stat(req, 0, 1, 2, 0);
        return;
#endif
#ifdef __NR_statx
    case __NR_statx:
        handle_stat(req, 0, 1, 4, 1);
        return;
#endif
#ifdef __NR_access
    case __NR_access:
        handle_access(req, -1, 0);
        return;
#endif
#ifdef __NR_faccessat
    case __NR_faccessat:
        handle_access(req, 0, 1);
        return;
#endif
#ifdef __NR_faccessat2
    case __NR_faccessat2:
        handle_access(req, 0, 1);
        return;
#endif
#ifdef __NR_readlink
    case __NR_readlink:
        handle_readlink(req, -1, 0, 1, 2);
        return;
#endif
#ifdef __NR_readlinkat
    case __NR_readlinkat:
        handle_readlink(req, 0, 1, 2, 3);
        return;
#endif
#ifdef __NR_getcwd
    case __NR_getcwd:
        handle_getcwd(req);
        return;
#endif
#ifdef __NR_getdents64
    case __NR_getdents64:
        handle_getdents(req);
        return;
#endif
    default:
        respond_continue(req->id);
        return;
    }
}

/* Notice when the target EXECs into a new program image, and record the ones
 * the libc half of the boundary cannot see inside.
 *
 * A seccomp filter survives execve, so this layer keeps supervising a
 * statically linked child while LD_PRELOAD does not reach it at all. That
 * combination produces a capsule with real file entries and NO clock, rng,
 * environment, or socket entries, which looks complete and is not: measured,
 * a `gcc -static` subject behind a `/bin/sh` wrapper captured six entries and
 * replayed as a clean "reproduced". Capture refuses on this entry instead.
 *
 * Read from /proc rather than by trapping execve, so the filter and its hot
 * path are untouched: the image only has to be re-judged when the link
 * changes, which is once per exec. */
static void note_target_image(void) {
    static char seen[MAX_PATH_LEN];
    char link[64];
    char exe[MAX_PATH_LEN];
    snprintf(link, sizeof(link), "/proc/%d/exe", (int)sup_target);
    ssize_t n = readlink(link, exe, sizeof(exe) - 1);
    if (n <= 0) {
        return;
    }
    exe[n] = 0;
    if (strcmp(exe, seen) == 0) {
        return;
    }
    snprintf(seen, sizeof(seen), "%s", exe);
    if (G.mode == 1 && reproit_elf_is_dynamic(exe) == 0) {
        record_blob(K_EXEC, exe, NULL, 0, 0, 0);
    }
}

static void supervisor_loop(void) {
    struct seccomp_notif *req = calloc(1, sizeof(*req));
    if (!req) {
        _exit(0);
    }
    for (;;) {
        memset(req, 0, sizeof(*req));
        if (ioctl(sup_notify, SECCOMP_IOCTL_NOTIF_RECV, req) < 0) {
            if (errno == EINTR) {
                continue;
            }
            break; /* the target is gone */
        }
        note_target_image();
        dispatch(req);
    }
    reproit_scratch_teardown();
    reproit_report();
    _exit(0);
}

/* ---- installation ------------------------------------------------------ */

static int send_fd(int sock, int fd) {
    struct msghdr msg;
    struct iovec io;
    char body = 'f';
    char control[CMSG_SPACE(sizeof(int))];
    memset(&msg, 0, sizeof(msg));
    memset(control, 0, sizeof(control));
    io.iov_base = &body;
    io.iov_len = 1;
    msg.msg_iov = &io;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof(control);
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    return sendmsg(sock, &msg, 0) < 0 ? -1 : 0;
}

static int recv_fd(int sock) {
    struct msghdr msg;
    struct iovec io;
    char body = 0;
    char control[CMSG_SPACE(sizeof(int))];
    memset(&msg, 0, sizeof(msg));
    io.iov_base = &body;
    io.iov_len = 1;
    msg.msg_iov = &io;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof(control);
    if (recvmsg(sock, &msg, 0) <= 0) {
        return -1;
    }
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    if (!c || c->cmsg_type != SCM_RIGHTS) {
        return -1;
    }
    int fd = -1;
    memcpy(&fd, CMSG_DATA(c), sizeof(int));
    return fd;
}

static int install_filter(void) {
    struct sock_filter filter[8 + TRAPPED_COUNT];
    int n = 0;
    filter[n++] = (struct sock_filter)BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
                                               offsetof(struct seccomp_data, arch));
    filter[n++] =
        (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, REPROIT_AUDIT_ARCH, 1, 0);
    filter[n++] = (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);
    filter[n++] =
        (struct sock_filter)BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr));
    for (int i = 0; i < TRAPPED_COUNT; i++) {
        /* Land on the USER_NOTIF at the end: skip the remaining comparisons
         * and the ALLOW that follows them. */
        __u8 jt = (__u8)(TRAPPED_COUNT - i);
        filter[n++] = (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K,
                                                   (unsigned int)TRAPPED[i], jt, 0);
    }
    filter[n++] = (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);
    filter[n++] = (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF);
    struct sock_fprog prog = {.len = (unsigned short)n, .filter = filter};
    return (int)syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER,
                        &prog);
}

int reproit_seccomp_start(void) {
    if (REPROIT_AUDIT_ARCH == 0 || G.mode == 0) {
        return 0;
    }
    const char *off = getenv("REPROIT_SECCOMP");
    if (off && off[0] == '0') {
        return 0;
    }
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        return 0;
    }
    /* Yama restricts cross process memory access to descendants; the
     * supervisor is the target's CHILD, so the target must opt in explicitly
     * before it exists. */
    prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY, 0, 0, 0);
    pid_t target = getpid();
    pid_t child = fork();
    if (child < 0) {
        close(sv[0]);
        close(sv[1]);
        return 0;
    }
    if (child == 0) {
        close(sv[0]);
        sup_target = target;
        /* The supervisor must never capture or serve its own work, and must
         * never run the program's main. A latching flag, not in_shim, which
         * LEAVE() clears on the way out of the first interposed call. */
        G.is_supervisor = 1;
        G.in_shim = 1;
        /* The fork copies whatever the target had buffered in stdio but not
         * yet flushed. The supervisor must never re-emit the program's own
         * output, so those buffers are discarded here rather than inherited:
         * measured, a python3 replay printed its line twice without it. */
        __fpurge(stdout);
        __fpurge(stderr);
        sup_notify = recv_fd(sv[1]);
        close(sv[1]);
        if (sup_notify < 0) {
            _exit(0);
        }
        supervisor_loop();
        _exit(0);
    }
    close(sv[1]);
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        close(sv[0]);
        return 0;
    }
    int listener = install_filter();
    if (listener < 0) {
        close(sv[0]);
        return 0;
    }
    if (send_fd(sv[0], listener) != 0) {
        close(sv[0]);
        close(listener);
        return 0;
    }
    close(sv[0]);
    close(listener);
    G.seccomp_files = 1;
    return 1;
}
