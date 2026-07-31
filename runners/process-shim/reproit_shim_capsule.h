/*
 * Capsule plumbing for the ReproIt process shim: base64, the append-only
 * record log, the replay entry store, and the divergence marker.
 *
 * Split from reproit_shim.c so each file stays reviewable: this file never
 * interposes a libc symbol, it only reads and writes the capsule. It resolves
 * its own real_* pointers rather than sharing the interposition file's, so
 * the two halves stay independent.
 */
#ifndef REPROIT_SHIM_CAPSULE_H
#define REPROIT_SHIM_CAPSULE_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#define MAX_ENTRIES 8192
#define MAX_BLOB 8192 /* the 8 KiB inline body rule the SDKs use */
#define MAX_PATH_LEN 4096
#define MAX_FDS 4096

typedef enum {
    K_OPEN = 0,
    K_READ,
    K_CONNECT,
    K_SEND,
    K_RECV,
    K_CLOCK,
    K_TIME,
    K_RANDOM,
    K_ENV,
    /* Syscall layer kinds (reproit_seccomp.c). Path metadata is what an
     * interpreted runtime spends its startup on, and none of it crosses the
     * dynamic linking boundary. */
    K_STAT,
    K_STATX,
    K_ACCESS,
    K_READLINK,
    K_GETCWD,
    K_DIRENT,
    K_KINDS
} kind_t;

typedef struct {
    kind_t kind;
    char key[256];
    unsigned char *blob;
    size_t blob_len;
    long a;
    long b;
    int consumed;
} entry_t;

typedef struct {
    int active;
    int is_socket;
    unsigned char *buf;
    size_t len;
    size_t off;
    int incomplete;
    char key[256];
} fdstate_t;

typedef struct {
    int mode; /* 0 off, 1 record, 2 replay */
    int log_fd;
    int in_shim;
    int ready;

    entry_t entries[MAX_ENTRIES];
    size_t entry_count;
    size_t dropped;

    fdstate_t fds[MAX_FDS];
    char paths[MAX_FDS][256];

    size_t served;
    size_t diverged;
    size_t clock_overrun;
    size_t random_overrun;
    size_t env_fallthrough;

    long last_sec;
    long last_nsec;
    uint64_t rng_state;

    /* Set in the seccomp SUPERVISOR only. The supervisor does its own file
     * work (materializing recorded content, rebuilding directories) and must
     * never be captured or served by the boundary it implements. in_shim
     * cannot express that, because LEAVE() clears it on the way out of the
     * first interposed call. */
    int is_supervisor;

    /* Set when the replay runner restored the capsule's whole environment
     * block at exec, which makes the live environ authoritative and lets the
     * program's own setenv writes be seen. */
    int env_pinned;

    /* Set when the seccomp supervisor is live. The libc file interposition
     * then steps aside completely, so files and path metadata have exactly
     * one source of truth and cannot be recorded twice under two keys. */
    int seccomp_files;
} shim_state_t;

extern shim_state_t G;

void reproit_open_log(const char *path);
void record_blob(kind_t kind, const char *key, const unsigned char *blob, size_t blob_len, long a,
                 long b);
void diverge(const char *kind, const char *detail);
void load_replay(const char *path);
entry_t *next_entry(kind_t kind, const char *key);
/* Like next_entry, but a repeat lookup of an already consumed key returns
 * that entry again instead of nothing. A program legitimately stats or opens
 * the same path many times during startup, and punishing the second lookup
 * with a divergence would report drift that did not happen. */
entry_t *find_entry(kind_t kind, const char *key);
size_t gather(kind_t kind, const char *key, unsigned char **out);
void reproit_report(void);

/* Shared by both interposition units: the re-entrancy guard keeps the shim's
 * own I/O out of the capture, and serve_recv is the one socket serving path.
 */
void shim_init(void);
ssize_t serve_recv(int fd, void *buf, size_t len);

/* The syscall completeness layer. Returns 1 when a supervisor is live, 0 when
 * the platform or the environment declined it, in which case the libc
 * boundary keeps working exactly as before. */
#ifdef __linux__
int reproit_seccomp_start(void);
#else
static inline int reproit_seccomp_start(void) { return 0; }
#endif

#define ENTER()                                                                                    \
    shim_init();                                                                                   \
    if (G.mode == 0 || G.in_shim || G.is_supervisor) {                                             \
        return_real = 1;                                                                           \
    } else {                                                                                       \
        G.in_shim = 1;                                                                             \
    }
#define LEAVE() G.in_shim = 0

#endif
