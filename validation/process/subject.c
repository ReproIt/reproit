/*
 * Acceptance subject for the process capsule: a small native program that
 * reads all four external input kinds the phase 1 boundary claims to cover,
 * then fails on a specific combination.
 *
 *   FILE   config read through POSIX open/read
 *   SOCKET one HTTP request to a local server
 *   CLOCK  clock_gettime
 *   RNG    getrandom (Linux) or /dev/urandom read
 *
 * It aborts when the upstream reply carries "limit":null while the config
 * declares strict mode: the planted defect. REPROIT_FIXED=1 handles the null
 * instead, which is the fix side of the acceptance run.
 *
 * A second path (REPROIT_STDIO=1) reads the same config through fopen/fread
 * so the harness can MEASURE how much of a program glibc's internal stdio
 * hides from an LD_PRELOAD boundary.
 */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/random.h>
#endif

static int read_config(char *out, size_t cap) {
    if (getenv("REPROIT_STDIO")) {
        FILE *handle = fopen("/tmp/reproit-subject/config.json", "r");
        if (!handle) {
            return -1;
        }
        size_t got = fread(out, 1, cap - 1, handle);
        out[got] = 0;
        fclose(handle);
        return (int)got;
    }
    int fd = open("/tmp/reproit-subject/config.json", O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t got = read(fd, out, cap - 1);
    close(fd);
    if (got < 0) {
        return -1;
    }
    out[got] = 0;
    return (int)got;
}

static int fetch_quote(char *out, size_t cap) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(19981);
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    const char *request = "GET /quote HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n";
    send(fd, request, strlen(request), 0);
    ssize_t got = recv(fd, out, cap - 1, 0);
    close(fd);
    if (got <= 0) {
        return -1;
    }
    out[got] = 0;
    return (int)got;
}

int main(void) {
    char config[4096];
    if (read_config(config, sizeof(config)) < 0) {
        fprintf(stderr, "subject: config unavailable\n");
        return 3;
    }

    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);

    unsigned char entropy[8] = {0};
#ifdef __linux__
    if (getrandom(entropy, sizeof(entropy), 0) < 0) {
        fprintf(stderr, "subject: entropy unavailable\n");
        return 3;
    }
#else
    int rng = open("/dev/urandom", O_RDONLY);
    if (rng < 0) {
        fprintf(stderr, "subject: entropy unavailable\n");
        return 3;
    }
    ssize_t drawn = read(rng, entropy, sizeof(entropy));
    close(rng);
    if (drawn != (ssize_t)sizeof(entropy)) {
        return 3;
    }
#endif

    char quote[8192];
    if (fetch_quote(quote, sizeof(quote)) < 0) {
        fprintf(stderr, "subject: upstream unavailable\n");
        return 4;
    }

    int strict = strstr(config, "\"strict\": true") != NULL;
    int limit_is_null = strstr(quote, "\"limit\":null") != NULL;

    fprintf(stdout, "subject: strict=%d limitNull=%d at=%ld entropy=%02x\n", strict, limit_is_null,
            (long)now.tv_sec, entropy[0]);

    if (strict && limit_is_null) {
        if (getenv("REPROIT_FIXED")) {
            fprintf(stdout, "subject: limit absent, applying default\n");
            return 0;
        }
        /* The planted defect: a null limit under strict mode aborts. */
        fprintf(stderr, "subject: assertion failed, strict mode requires a limit\n");
        abort();
    }
    return 0;
}
