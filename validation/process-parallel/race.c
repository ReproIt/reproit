/*
 * A real publish-before-initialize data race, in two variants.
 *
 * The bug is the same in both: the producer publishes a pointer and sets the
 * ready flag BEFORE filling the payload, so a consumer that observes the flag
 * can read the payload while it is still uninitialized. This is an ordinary
 * logic defect, not a memory-model subtlety, which is what makes it a fair
 * subject: it is the kind of race a person actually ships.
 *
 * The two variants differ ONLY in what sits inside the race window:
 *
 *   pure  the window is stores and arithmetic, nothing else. Nothing outside
 *         the process is called, so an LD_PRELOAD boundary has no entry point
 *         to perturb.
 *   libc  the window contains clock_gettime, because the payload is
 *         timestamped. That is a realistic thing for a producer to do and it
 *         gives a preload something to hook.
 *
 * Both threads meet at a barrier before the publish, so the measurement is of
 * the race window itself rather than of thread startup skew. REPROIT_RACE_WINDOW
 * scales the work inside the window, which is what makes the natural failure
 * rate tunable and lets the fuzzing effect be measured across regimes.
 *
 * Exit codes: 0 clean, 42 the race was observed, 1 usage error.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define MAGIC 0x5A5AC0DEu
#define SPIN_BOUND 200000000L

struct payload {
    unsigned magic;
    long stamp;
};

static volatile int ready;
static struct payload *volatile shared;
static int use_libc_in_window;
static volatile int race_observed;
static long window_work = 64;
static pthread_barrier_t start_line;

static long timestamp_ns(void)
{
    struct timespec ts;
    /* The hookable call inside the window for the libc variant. */
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000000000L + ts.tv_nsec;
}

static void *producer(void *arg)
{
    (void)arg;
    struct payload *p = malloc(sizeof *p);
    if (p == NULL) {
        return NULL;
    }
    p->magic = 0;
    p->stamp = 0;

    /* Both threads are live before the publish, so what follows measures the
       window and not the cost of starting a thread. */
    pthread_barrier_wait(&start_line);

    /* The defect: publish, then initialize. */
    shared = p;
    ready = 1;

    if (use_libc_in_window) {
        p->stamp = timestamp_ns();
    } else {
        long spin = 0;
        for (long i = 0; i < window_work; i++) {
            spin += i * i;
        }
        p->stamp = spin;
    }
    p->magic = MAGIC;
    return NULL;
}

static void *consumer(void *arg)
{
    (void)arg;
    long spins = 0;
    pthread_barrier_wait(&start_line);
    while (ready == 0) {
        if (++spins > SPIN_BOUND) {
            /* Bounded: never hang a measurement run. */
            return NULL;
        }
    }
    struct payload *p = shared;
    if (p == NULL) {
        race_observed = 1;
        return NULL;
    }
    if (p->magic != MAGIC) {
        race_observed = 1;
    }
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc != 2 || (strcmp(argv[1], "pure") != 0 && strcmp(argv[1], "libc") != 0)) {
        fprintf(stderr, "usage: race <pure|libc>\n");
        return 1;
    }
    use_libc_in_window = strcmp(argv[1], "libc") == 0;
    const char *window = getenv("REPROIT_RACE_WINDOW");
    if (window != NULL && *window != '\0') {
        window_work = strtol(window, NULL, 10);
    }
    if (pthread_barrier_init(&start_line, NULL, 2) != 0) {
        return 1;
    }

    pthread_t tp, tc;
    /* Start the consumer first so it is already spinning on the flag. */
    if (pthread_create(&tc, NULL, consumer, NULL) != 0) {
        return 1;
    }
    if (pthread_create(&tp, NULL, producer, NULL) != 0) {
        return 1;
    }
    pthread_join(tp, NULL);
    pthread_join(tc, NULL);
    pthread_barrier_destroy(&start_line);

    if (race_observed) {
        fprintf(stderr, "RACE OBSERVED: consumer read an uninitialized payload\n");
        return 42;
    }
    return 0;
}
