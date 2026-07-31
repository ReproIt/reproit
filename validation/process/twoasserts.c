/* Two DIFFERENT assertions, both dying with SIGABRT. Selecting between them
 * with an env var makes the false-proof case testable: outcome comparison
 * alone cannot tell them apart. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
int main(void) {
    FILE *f = fopen("/tmp/reproit-subject/input.txt", "r");
    char buf[32] = {0};
    if (f) { if (fgets(buf, sizeof buf, f)) {} fclose(f); }
    printf("read:%s\n", buf);
    int n = 9;
    if (getenv("OTHER_BUG")) {
        assert(n < 5 && "fuel budget exceeded");
    } else {
        assert(n < 8 && "thrust budget exceeded");
    }
    return 0;
}
