/*
 * Schedule fuzzing prototype: an LD_PRELOAD that perturbs thread timing at the
 * libc entry points it can see.
 *
 * The honest claim this exists to measure is NOT "we reproduce the race". It is
 * "we reproduce the input conditions and can fuzz the schedule". A preload can
 * only perturb a program where the program crosses the boundary, so this
 * deliberately hooks a small, realistic set and the measurement then shows how
 * much that buys on a race whose window contains such a call, versus one whose
 * window is pure memory traffic.
 *
 * Controls:
 *   REPROIT_SCHED_FUZZ=1        enable
 *   REPROIT_SCHED_SEED=<n>      seed the decision stream (default 1)
 *   REPROIT_SCHED_DELAY_NS=<n>  delay injected when a site fires (default 50000)
 *   REPROIT_SCHED_RATE=<0..100> percent of hooked calls that get a delay
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <time.h>

static int fuzz_enabled;
static long delay_ns = 50000;
static int fire_rate = 50;
static atomic_ullong stream;
static atomic_ullong hooked_calls;
static atomic_ullong injected_delays;

static int (*real_clock_gettime)(clockid_t, struct timespec *);
static int (*real_pthread_create)(pthread_t *, const pthread_attr_t *,
                                  void *(*)(void *), void *);

static long env_long(const char *name, long fallback)
{
    const char *raw = getenv(name);
    if (raw == NULL || *raw == '\0') {
        return fallback;
    }
    char *end = NULL;
    long value = strtol(raw, &end, 10);
    if (end == raw) {
        return fallback;
    }
    return value;
}

__attribute__((constructor)) static void setup(void)
{
    const char *on = getenv("REPROIT_SCHED_FUZZ");
    fuzz_enabled = on != NULL && *on == '1';
    delay_ns = env_long("REPROIT_SCHED_DELAY_NS", 50000);
    fire_rate = (int)env_long("REPROIT_SCHED_RATE", 50);
    atomic_store(&stream, (unsigned long long)env_long("REPROIT_SCHED_SEED", 1));
}

/* xorshift64star: a deterministic decision stream from the seed. */
static unsigned long long next_draw(void)
{
    unsigned long long x = atomic_fetch_add(&stream, 0x9E3779B97F4A7C15ull) +
                           0x9E3779B97F4A7C15ull;
    x ^= x >> 30;
    x *= 0xBF58476D1CE4E5B9ull;
    x ^= x >> 27;
    x *= 0x94D049BB133111EBull;
    x ^= x >> 31;
    return x;
}

static void maybe_perturb(void)
{
    if (!fuzz_enabled) {
        return;
    }
    atomic_fetch_add(&hooked_calls, 1);
    if ((int)(next_draw() % 100ull) >= fire_rate) {
        return;
    }
    atomic_fetch_add(&injected_delays, 1);
    sched_yield();
    if (delay_ns > 0) {
        struct timespec req = {.tv_sec = 0, .tv_nsec = delay_ns};
        nanosleep(&req, NULL);
    }
}

int clock_gettime(clockid_t clk, struct timespec *ts)
{
    if (real_clock_gettime == NULL) {
        real_clock_gettime = dlsym(RTLD_NEXT, "clock_gettime");
    }
    maybe_perturb();
    return real_clock_gettime(clk, ts);
}

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start)(void *), void *arg)
{
    if (real_pthread_create == NULL) {
        real_pthread_create = dlsym(RTLD_NEXT, "pthread_create");
    }
    maybe_perturb();
    return real_pthread_create(thread, attr, start, arg);
}

__attribute__((destructor)) static void report(void)
{
    if (!fuzz_enabled) {
        return;
    }
    const char *path = getenv("REPROIT_SCHED_STATS");
    if (path == NULL) {
        return;
    }
    FILE *out = fopen(path, "a");
    if (out == NULL) {
        return;
    }
    fprintf(out, "hooked=%llu injected=%llu\n",
            (unsigned long long)atomic_load(&hooked_calls),
            (unsigned long long)atomic_load(&injected_delays));
    fclose(out);
}
