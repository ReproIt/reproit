/*
 * Relative-path keying, in both directions that a bad key can break.
 *
 * A: ONE file, reached through a relative name. Record and replay must agree
 *    on its key, or the capsule diverges on a file whose bytes it holds.
 * B: TWO different files, both spelled `data.txt`, from two directories. They
 *    must key APART, or replay concatenates them and answers `OUTERINNER`
 *    with zero divergences, which is a silent wrong replay.
 *
 * The two are opened through different symbols on purpose: `openat` with
 * AT_FDCWD and plain `open`. The defect keyed those two differently.
 *
 * Exit 3 is the planted failure, so the capsule has a verdict to judge; the
 * printed lines are the byte-identity evidence.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int show(const char *label, int fd) {
    char buf[64];
    memset(buf, 0, sizeof(buf));
    ssize_t got = fd >= 0 ? read(fd, buf, sizeof(buf) - 1) : -1;
    if (fd >= 0) {
        close(fd);
    }
    if (got < 0) {
        printf("%s=<ERR>\n", label);
        return 0;
    }
    printf("%s=%s\n", label, buf);
    return 1;
}

int main(void) {
    int outer = show("A", openat(AT_FDCWD, "data.txt", O_RDONLY));
    int inner = 0;
    if (chdir("sub") == 0) {
        inner = show("B", open("data.txt", O_RDONLY));
    } else {
        printf("B=<NOCHDIR>\n");
    }
    fflush(stdout);
    /* The planted failure: both files were readable, so the run "fails" in
     * the way the capsule will be asked to reproduce. */
    return (outer && inner) ? 3 : 0;
}
