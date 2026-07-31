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

/* Per file inline cap. Bigger than the SDKs' 8 KiB body rule on purpose: a
 * process input is a whole file, not an HTTP body, and a locale archive is
 * 350 KiB. A file past the cap records its size but not all its bytes, which
 * the existing completeness oracle turns into a loud truncated-file at the
 * moment the program reads past what the capsule holds. */
#define FILE_CAP (4u << 20)
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
 * and fd table through /proc, never the supervisor's own. */
static void absolutize(int dirfd, const char *path, char *out, size_t cap) {
    if (!path || !path[0]) {
        snprintf(out, cap, "-");
        return;
    }
    if (path[0] == '/') {
        snprintf(out, cap, "%s", path);
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
    while (total < FILE_CAP && (got = read(fd, buf, sizeof(buf))) > 0) {
        record_blob(K_READ, absolute, buf, (size_t)got, 0, 0);
        total += (size_t)got;
    }
    close(fd);
}

static void record_stat_like(kind_t kind, const char *absolute, const void *blob, size_t len,
                             long retval) {
    record_blob(kind, absolute, (const unsigned char *)blob, len, retval, 0);
}

/* ---- replay ----------------------------------------------------------- */

/* The scratch tree replay materializes recorded content into. One per
 * supervisor, torn down when the target exits. */
static char scratch_root[64];

static const char *scratch(void) {
    if (!scratch_root[0]) {
        snprintf(scratch_root, sizeof(scratch_root), "/tmp/reproit-replay-XXXXXX");
        if (!mkdtemp(scratch_root)) {
            scratch_root[0] = 0;
            return NULL;
        }
    }
    return scratch_root;
}

static void scratch_teardown(void) {
    if (!scratch_root[0]) {
        return;
    }
    /* Bounded and non recursive by construction: the tree is exactly one
     * level of materialized files plus the rebuilt directories, which are
     * themselves one level deep. */
    DIR *root = opendir(scratch_root);
    if (root) {
        struct dirent *entry;
        while ((entry = readdir(root)) != NULL) {
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            char path[MAX_PATH_LEN];
            snprintf(path, sizeof(path), "%s/%s", scratch_root, entry->d_name);
            if (entry->d_type == DT_DIR) {
                DIR *nested = opendir(path);
                if (nested) {
                    struct dirent *inner;
                    while ((inner = readdir(nested)) != NULL) {
                        if (strcmp(inner->d_name, ".") == 0 || strcmp(inner->d_name, "..") == 0) {
                            continue;
                        }
                        char nested_path[MAX_PATH_LEN];
                        snprintf(nested_path, sizeof(nested_path), "%s/%s", path, inner->d_name);
                        remove(nested_path);
                    }
                    closedir(nested);
                }
                rmdir(path);
            } else {
                unlink(path);
            }
        }
        closedir(root);
    }
    rmdir(scratch_root);
    scratch_root[0] = 0;
}

/* A stable scratch name for one recorded path, so a file opened repeatedly is
 * materialized once. */
static void scratch_name(const char *absolute, char *out, size_t cap) {
    unsigned long hash = 1469598103934665603UL;
    for (const char *p = absolute; *p; p++) {
        hash ^= (unsigned char)*p;
        hash *= 1099511628211UL;
    }
    const char *base = strrchr(absolute, '/');
    base = base ? base + 1 : absolute;
    snprintf(out, cap, "%s/%016lx-%.64s", scratch_root, hash, base);
}

/* Materialize recorded content as a REAL file and hand back a descriptor to
 * it.
 *
 * This used to be a memfd, which was measured to break two things a copy
 * cannot fake. glibc validates a locale object structurally, and the dynamic
 * loader maps a shared object PROT_EXEC and relocates it, which a memfd is
 * refused for on kernels that default memfds to noexec. A real file on disk
 * satisfies both, and the program still never touches the host's copy: the
 * bytes come from the capsule and nothing else. */
static int materialize(const char *absolute, const unsigned char *content, size_t len) {
    if (!scratch()) {
        return -1;
    }
    char path[MAX_PATH_LEN];
    scratch_name(absolute, path, sizeof(path));
    struct stat existing;
    if (stat(path, &existing) == 0 && (size_t)existing.st_size == len) {
        return open(path, O_RDONLY | O_CLOEXEC);
    }
    /* 0755 so an executable mapping of a served shared object is permitted. */
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (fd < 0) {
        return -1;
    }
    size_t off = 0;
    while (off < len) {
        ssize_t wrote = write(fd, content + off, len - off);
        if (wrote <= 0) {
            break;
        }
        off += (size_t)wrote;
    }
    close(fd);
    return open(path, O_RDONLY | O_CLOEXEC);
}

/* Rebuild a recorded directory as a real one in a scratch tree, so the
 * program's getdents64 is answered by the KERNEL from names the capsule
 * carries. Writing dirent structs by hand would duplicate the kernel's
 * layout rules for no gain; materializing the names cannot get it wrong. */
static int serve_dir(const char *absolute) {
    if (!scratch()) {
        return -1;
    }
    char template[MAX_PATH_LEN];
    scratch_name(absolute, template, sizeof(template));
    if (mkdir(template, 0755) != 0 && errno != EEXIST) {
        return -1;
    }
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind != K_DIRENT || strcmp(e->key, absolute) != 0 || !e->blob) {
            continue;
        }
        char name[MAX_PATH_LEN];
        size_t len = e->blob_len < sizeof(name) - 1 ? e->blob_len : sizeof(name) - 1;
        memcpy(name, e->blob, len);
        name[len] = 0;
        char full[MAX_PATH_LEN];
        snprintf(full, sizeof(full), "%s/%s", template, name);
        if (e->a == DT_DIR) {
            mkdir(full, 0755);
        } else {
            int created = open(full, O_WRONLY | O_CREAT | O_EXCL, 0644);
            if (created >= 0) {
                close(created);
            }
        }
    }
    return open(template, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
}

static void serve_open(__u64 id, const char *absolute) {
    unsigned char *content = NULL;
    size_t len = gather(K_READ, absolute, &content);
    entry_t *opened = find_entry(K_OPEN, absolute);
    if (opened && opened->b == -1) {
        free(content);
        int dirfd = serve_dir(absolute);
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
        diverge("incomplete-file", absolute);
        free(content);
        respond_error(id, EIO);
        return;
    }
    int fd = materialize(absolute, content, len);
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
 * serve_dir rebuilt from the capsule's recorded names, so the kernel answers
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
        dispatch(req);
    }
    scratch_teardown();
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
