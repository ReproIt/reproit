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
/* Entry key width. A path is up to MAX_PATH_LEN, so keys are FOLDED into this
 * width rather than truncated; see reproit_path_key. */
#define MAX_KEY_LEN 256

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
    /* Phase 2: the timed input stream. A session-shaped program's trigger is
     * input arriving over time, not a single request, so an input read is
     * stamped with the TICK it arrived on and replay holds it back until the
     * program reaches that tick again. */
    K_INPUT,
    /* A program image the target EXECed into. Recorded only when the image is
     * statically linked, because that is the case the boundary cannot fully
     * observe: the libc half needs a dynamic loader to be interposed at all,
     * so a static image is covered by the syscall layer ALONE and its clock,
     * randomness, environment, and socket traffic go unseen. A capture that
     * shipped anyway would look complete and would not be. */
    K_EXEC,
    K_KINDS
} kind_t;

typedef struct {
    kind_t kind;
    char key[MAX_KEY_LEN];
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
    char key[MAX_KEY_LEN];
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
    char paths[MAX_FDS][MAX_KEY_LEN];

    size_t served;
    size_t diverged;
    size_t clock_overrun;
    size_t random_overrun;
    size_t env_fallthrough;

    long last_sec;
    long last_nsec;
    /* Where the REAL clock stood when the capsule's clock ran out. A replay
     * that outlives its recording (a fixed program that no longer crashes)
     * must keep time MOVING, or a loop waiting for wall clock progress spins
     * forever. Past the recording the run is no longer reproducing it, which
     * clock_overrun already reports. */
    int overrun_anchored;
    long overrun_real_sec;
    long overrun_real_nsec;
    uint64_t rng_state;

    /* The logical clock Phase 2 schedules input against: the ordinal of the
     * clock reads the program has made. Replay serves clock reads from the
     * capsule IN ORDER, so the Nth clock read at replay is the Nth clock read
     * of the recording, which makes this ordinal aligned between the two runs
     * without the program needing to expose a frame counter. A fixed timestep
     * loop reads the clock once per frame, so this counts frames. */
    size_t tick;
    size_t input_served;
    /* Bounds the hold back so a program that never advances its clock cannot
     * spin forever waiting for input that is scheduled in its future. */
    size_t input_holds;
    size_t input_hold_tick;
    /* Input served before its recorded tick because the fd was blocking and
     * holding it back would have hung the program. Counted, never hidden. */
    size_t input_early;

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

/* The capsule key for a filesystem path, shared by both layers so a file
 * keys the same way whoever observed it.
 *
 * Two properties, and the boundary had NEITHER. Measured with the libc layer
 * alone (REPROIT_SECCOMP=0) on a subject that opens one relative name from
 * two directories:
 *
 *   - ONE file, TWO keys. Record stored an `openat` key verbatim while replay
 *     resolved it against the cwd, so a file recorded as `data.txt` was looked
 *     up as `/w/data.txt` and DIVERGED with its own bytes sitting unread in
 *     the capsule.
 *   - TWO files, ONE key. Two different `data.txt` files recorded under that
 *     single relative key, and replay concatenated them into `OUTERINNER`,
 *     with zero divergences. That is the silent wrong replay this project
 *     exists to refuse, so it is the direction that made this urgent.
 *
 * `absolute` must already be absolute. It is normalized lexically (not
 * through realpath, which reads the filesystem replay must not touch), then
 * folded to MAX_KEY_LEN. Folding hashes the WHOLE path and keeps the tail,
 * because plain truncation is itself a way for two files to share one key.
 *
 * Bound: a symlink and its target still key apart, because telling them apart
 * needs the filesystem. Under replay both spellings are then unrecorded and
 * DIVERGE, which is the safe direction.
 */
void reproit_path_key(const char *absolute, char *out, size_t cap);

/* Collapse `//`, `/./`, and a trailing `/` in place. These three rewrites
 * never change WHICH file a path names, so the result is still usable as a
 * real path and both layers can normalize before keying.
 *
 * `a/b/..` is deliberately NOT folded. The kernel resolves `..` after
 * following any symlink in the way, so folding it lexically would make
 * `/a/link/../b` key as `/a/b`, a DIFFERENT file. That turns a normalization
 * meant to merge two keys for one file into two files sharing one key, which
 * is the silent wrong replay this whole key exists to prevent. */
void reproit_normalize_path(char *path);

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
/* The same two lookups, reporting WHERE in the log the entry sits. A read
 * belongs to the open it followed, and only the position says which. */
entry_t *next_entry_at(kind_t kind, const char *key, size_t *index);
entry_t *find_entry_at(kind_t kind, const char *key, size_t *index);
/* Index of the next entry with this kind and key strictly after `after`, or
 * G.entry_count when there is none. */
size_t next_key_index(kind_t kind, const char *key, size_t after);
size_t gather(kind_t kind, const char *key, unsigned char **out);
/* Every recorded read of one key BETWEEN two log positions, concatenated.
 *
 * Gathering by key across the whole log was a silent wrong replay, and ruby
 * is the program that showed it. A recording of `ruby -e` opens
 * `/usr/lib/ruby/vendor_ruby/rubygems.rb` TWICE, and the capsule holds the
 * file's 37,245 bytes once per open. Replay then served 74,490 bytes: one
 * evaluation of a file containing its own text twice. Rubygems duly warned
 * `already initialized constant Gem::MARSHAL_SPEC_DIR` at line 2644 with the
 * previous definition at 1295, exactly 1,349 lines apart, and Debian's
 * `alias upstream_default_path default_path` ran a second time and aliased
 * itself into `stack level too deep`.
 *
 * A file opened twice is two independent streams, so replay serves the reads
 * that followed THIS open and stops at the next one. */
size_t gather_span(kind_t kind, const char *key, size_t from, size_t to, unsigned char **out);
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

/* The scratch tree the completeness layer replays files out of
 * (reproit_seccomp_scratch.c). Real files, not memfds: glibc validates a
 * locale object structurally and the loader maps a shared object PROT_EXEC. */
const char *reproit_scratch(void);
void reproit_scratch_teardown(void);
void reproit_scratch_name(const char *absolute, char *out, size_t cap);
int reproit_materialize(const char *absolute, const unsigned char *content, size_t len);
int reproit_serve_dir(const char *absolute);

/* Is this ELF dynamically linked (reproit_elf.c)? 1 yes, 0 no, -1 when the
 * file is not an ELF this parser can judge, which is never evidence either
 * way. A statically linked image resolves no dynamic symbols, so the libc
 * half of the boundary is blind inside it. */
int reproit_elf_is_dynamic(const char *path);
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
