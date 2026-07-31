/*
 * A LONG RUNNING subject for checkpoint anchoring (Class C).
 *
 * Phase 1's subject fails immediately, which is the right shape for proving a
 * boundary and the wrong shape for proving an anchor: if reaching the failure
 * is cheap, skipping the head saves nothing. This one reads its config on
 * every iteration and only fails deep into the run, so "replay from zero" and
 * "restore near the failure" are measurably different amounts of work.
 *
 * Each iteration prints one line, which is what the anchoring tool watches to
 * decide it has reached the anchor point. That is deliberately an OBSERVABLE
 * position rather than an internal one: the tool must not need to reach inside
 * the shim to know how far the program has got.
 *
 * The failure is an abort() on the target iteration, so the oracle is a fatal
 * signal, the same shape phase 1 already judges.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <time.h>

/* The config path is an ARGUMENT, and the harness passes an absolute one.
 * Measured reason: the seccomp completeness layer keys a file entry by the
 * resolved absolute path while the libc-only boundary keys it by the string
 * the program passed, so a relative path produces two different keys and a
 * capsule recorded under one boundary cannot be served by the other. An
 * absolute path makes the two agree, which is what lets an anchor (libc only,
 * because a seccomp notify fd cannot be checkpointed) restore a capsule that
 * a full replay recorded. */
#define DEFAULT_CONFIG "checkpoint-config.txt"

int main(int argc, char **argv) {
    long iterations = argc > 1 ? atol(argv[1]) : 400;
    const char *config = argc > 2 ? argv[2] : DEFAULT_CONFIG;
    /* FIXED=1 turns the planted failure off, so the same capsule can be shown
     * flipping to a clean exit without editing the subject. */
    const char *fixed = getenv("FIXED");
    int is_fixed = fixed && fixed[0] == '1';
    /* Unbuffered: the anchoring tool counts lines as they appear, and a
     * buffered stream would hide the program's real position. */
    setvbuf(stdout, NULL, _IONBF, 0);

    for (long i = 1; i <= iterations; i++) {
        int fd = open(config, O_RDONLY);
        char buf[128];
        memset(buf, 0, sizeof(buf));
        if (fd >= 0) {
            ssize_t got = read(fd, buf, sizeof(buf) - 1);
            (void)got;
            close(fd);
        } else {
            /* The config is deleted before replay, so reaching the real
             * filesystem here means the boundary failed to serve it. Say so
             * loudly rather than continuing on a default. */
            fprintf(stderr, "config unreadable at iteration %ld\n", i);
            return 9;
        }
        char *nl = strchr(buf, '\n');
        if (nl) {
            *nl = 0;
        }
        printf("iter=%ld cfg=%s\n", i, buf);

        if (!is_fixed && i == iterations) {
            fprintf(stderr, "planted failure at iteration %ld\n", i);
            abort();
        }
        /* Small enough to keep the whole run quick, large enough that the head
         * of the run is measurably more work than the tail. */
        struct timespec ts = {0, 5L * 1000 * 1000};
        nanosleep(&ts, NULL);
    }
    printf("done\n");
    return 0;
}
