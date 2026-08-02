/*
 * A hand-rolled SGD training loop with application-level checkpoints, the
 * Class C fixture for validation/process-checkpoint/gate-anchor.sh.
 *
 * One scalar weight fits y = w * x by SGD over a data file of "x y" lines,
 * one line per step, consumed in order. Every CKPT_EVERY steps it writes its
 * own checkpoint (step, weight, and the in-process RNG state) and it resumes
 * from that file with `resume` as the fourth argument, which is exactly the
 * argv-level restore an application anchor records.
 *
 * The planted defect: a poisoned sample (an absurd y) blows the gradient up,
 * the weight leaves its invariant bound, and the trainer dies on a declared
 * assertion at that step. TRAINER_FIXED=1 is the fix: it skips samples whose
 * label is outside the plausible range, so the run completes cleanly.
 *
 * Pure CPU, libc only, deterministic: no clock reads, no sockets, no
 * threads. The only nondeterminism is the LCG, whose state lives in the
 * checkpoint, which is precisely the "in-process RNG state carried inside
 * the checkpoint" line of the capsule's uncontrolled-sources statement.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_STEPS 100000
#define CKPT_EVERY 50
#define LEARNING_RATE 0.01
#define WEIGHT_BOUND 1.0e6
#define LABEL_BOUND 1.0e3 /* the fix: no real sample has |y| past this */
#define PROGRESS_EVERY 50

static unsigned long long lcg_next(unsigned long long state) {
    return state * 6364136223846793005ULL + 1442695040888963407ULL;
}

static void write_checkpoint(const char *path, int step, double weight,
                             unsigned long long rng) {
    FILE *out = fopen(path, "w");
    if (!out) {
        fprintf(stderr, "trainer: cannot write checkpoint %s\n", path);
        exit(2);
    }
    fprintf(out, "%d %.17g %llu\n", step, weight, rng);
    fclose(out);
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: trainer <data-file> <steps> <ckpt-file> [resume]\n");
        return 2;
    }
    const char *data_path = argv[1];
    int steps = atoi(argv[2]);
    const char *ckpt_path = argv[3];
    int resume = (argc > 4 && strcmp(argv[4], "resume") == 0);
    if (steps <= 0 || steps > MAX_STEPS) {
        fprintf(stderr, "trainer: steps must be in 1..%d\n", MAX_STEPS);
        return 2;
    }
    int fixed = getenv("TRAINER_FIXED") != NULL;

    /* One sample per step, read up front so the boundary sees the whole
     * data file once. Bounded by steps, which is bounded above. */
    double *xs = calloc((size_t)steps, sizeof(double));
    double *ys = calloc((size_t)steps, sizeof(double));
    if (!xs || !ys) {
        fprintf(stderr, "trainer: out of memory\n");
        return 2;
    }
    FILE *data = fopen(data_path, "r");
    if (!data) {
        fprintf(stderr, "trainer: cannot open data file %s\n", data_path);
        return 2;
    }
    int samples = 0;
    while (samples < steps && fscanf(data, "%lf %lf", &xs[samples], &ys[samples]) == 2) {
        samples++;
    }
    fclose(data);
    if (samples < steps) {
        fprintf(stderr, "trainer: data file holds %d samples, need %d\n", samples, steps);
        return 2;
    }

    int start = 0;
    double weight = 0.0;
    unsigned long long rng = 0x5eed5eed5eed5eedULL;
    if (resume) {
        FILE *ckpt = fopen(ckpt_path, "r");
        if (!ckpt) {
            fprintf(stderr, "trainer: cannot open checkpoint %s to resume\n", ckpt_path);
            return 2;
        }
        if (fscanf(ckpt, "%d %lf %llu", &start, &weight, &rng) != 3) {
            fprintf(stderr, "trainer: checkpoint %s is malformed\n", ckpt_path);
            fclose(ckpt);
            return 2;
        }
        fclose(ckpt);
        if (start < 0 || start >= steps) {
            fprintf(stderr, "trainer: checkpoint step %d is outside this run\n", start);
            return 2;
        }
        fprintf(stderr, "trainer: resumed from checkpoint at step %d\n", start);
    }

    for (int step = start + 1; step <= steps; step++) {
        double x = xs[step - 1];
        double y = ys[step - 1];
        if (fixed && fabs(y) > LABEL_BOUND) {
            fprintf(stderr, "trainer: skipped implausible sample at step %d\n", step);
            rng = lcg_next(rng);
            continue;
        }
        /* Deterministic jitter from the checkpointed LCG: consumed every
         * step so the RNG stream position is part of the training state. */
        rng = lcg_next(rng);
        double jitter = (double)((rng >> 33) % 1000ULL) * 1e-9;
        double gradient = 2.0 * (weight * (x + jitter) - y) * (x + jitter);
        weight -= LEARNING_RATE * gradient;
        if (!(fabs(weight) < WEIGHT_BOUND)) {
            fprintf(stderr, "trainer: assertion failed: weight left its bound at step %d\n",
                    step);
            free(xs);
            free(ys);
            return 7;
        }
        if (step % PROGRESS_EVERY == 0) {
            fprintf(stderr, "trainer: step %d w=%.6f\n", step, weight);
        }
        if (step % CKPT_EVERY == 0 && step < steps) {
            write_checkpoint(ckpt_path, step, weight, rng);
        }
    }
    printf("trainer: done after %d steps, w=%.6f\n", steps, weight);
    free(xs);
    free(ys);
    return 0;
}
