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
} shim_state_t;

extern shim_state_t G;

void reproit_open_log(const char *path);
void record_blob(kind_t kind, const char *key, const unsigned char *blob, size_t blob_len, long a,
                 long b);
void diverge(const char *kind, const char *detail);
void load_replay(const char *path);
entry_t *next_entry(kind_t kind, const char *key);
size_t gather(kind_t kind, const char *key, unsigned char **out);
void reproit_report(void);

/* Shared by both interposition units: the re-entrancy guard keeps the shim's
 * own I/O out of the capture, and serve_recv is the one socket serving path.
 */
void shim_init(void);
ssize_t serve_recv(int fd, void *buf, size_t len);

#define ENTER()                                                                                    \
    shim_init();                                                                                   \
    if (G.mode == 0 || G.in_shim) {                                                                \
        return_real = 1;                                                                           \
    } else {                                                                                       \
        G.in_shim = 1;                                                                             \
    }
#define LEAVE() G.in_shim = 0

#endif
