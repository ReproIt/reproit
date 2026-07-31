/*
 * The determinism envelope: clock, wall time, randomness, and environment.
 *
 * Split from reproit_shim.c so each interposition unit stays reviewable. This
 * file owns the classes that are POLICY SERVED rather than failed closed: a
 * replayed run legitimately makes a different NUMBER of clock and random
 * draws while depending on the same external inputs, so running past the
 * capsule here is counted (clockOverrun, randomOverrun) and never reported as
 * a divergence. Files and sockets, where running past the capsule IS drift,
 * live in reproit_shim.c.
 *
 * It resolves its own real_* pointers, as the capsule half does, so the units
 * do not depend on each other's resolution order.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <dlfcn.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/random.h>
#include <sys/syscall.h>
#endif

static int (*real_clock_gettime)(clockid_t, struct timespec *);
static int (*real_gettimeofday)(struct timeval *, void *);
static time_t (*real_time)(time_t *);
static char *(*real_getenv)(const char *);

#define RESOLVE(fn)                                                                                \
    do {                                                                                           \
        if (!real_##fn) {                                                                          \
            real_##fn = dlsym(RTLD_NEXT, #fn);                                                      \
        }                                                                                          \
    } while (0)

int clock_gettime(clockid_t id, struct timespec *ts) {
    int return_real = 0;
    RESOLVE(clock_gettime);
    ENTER();
    if (return_real) {
        return real_clock_gettime(id, ts);
    }
    int result = 0;
    G.tick++;
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
             * of clock reads while depending on the same external inputs.
             *
             * Past the end of the recording, time must keep MOVING at a real
             * rate. Incrementing by a nanosecond per call hung a fixed program
             * whose frame loop waits five milliseconds of wall clock: it
             * outlived its recording, so the clock never advanced far enough
             * and the loop spun forever. The served clock therefore continues
             * from the last recorded instant at the real elapsed rate. */
            struct timespec now;
            if (real_clock_gettime(CLOCK_MONOTONIC, &now) == 0) {
                if (!G.overrun_anchored) {
                    G.overrun_anchored = 1;
                    G.overrun_real_sec = now.tv_sec;
                    G.overrun_real_nsec = now.tv_nsec;
                }
                long delta_sec = now.tv_sec - G.overrun_real_sec;
                long delta_nsec = now.tv_nsec - G.overrun_real_nsec;
                if (delta_nsec < 0) {
                    delta_sec--;
                    delta_nsec += 1000000000L;
                }
                ts->tv_sec = G.last_sec + delta_sec;
                ts->tv_nsec = G.last_nsec + delta_nsec;
                if (ts->tv_nsec >= 1000000000L) {
                    ts->tv_sec++;
                    ts->tv_nsec -= 1000000000L;
                }
            } else {
                G.last_nsec++;
                ts->tv_sec = G.last_sec;
                ts->tv_nsec = G.last_nsec;
            }
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
    G.tick++;
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
    G.tick++;
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
        /* When the whole environment block is pinned at exec (the CLI clears
         * and restores it), the live environ IS the recorded one, and serving
         * a capsule snapshot on top of it is worse than useless: it replays a
         * value from before the PROGRAM's own setenv. Measured: CPython
         * coerces LC_CTYPE at startup, the stale snapshot hid that write, and
         * glibc then looked for a locale named "UTF-8" that the recorded run
         * never opened. Falling through honours the program's own writes. */
        if (G.env_pinned) {
            G.env_fallthrough++;
            value = real_getenv(name);
            LEAVE();
            return value;
        }
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
