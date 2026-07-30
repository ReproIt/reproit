/* Capsule plumbing: see reproit_shim_capsule.h. */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

shim_state_t G;

static const char *KIND_NAMES[K_KINDS] = {"open",  "read", "connect", "send",  "recv",
                                          "clock", "time", "random",  "env"};

/* This half resolves its own libc pointers so it never depends on the
 * interposition half's resolution order. */
static ssize_t (*cap_write)(int, const void *, size_t);
static int (*cap_open)(const char *, int, ...);
static ssize_t (*cap_read)(int, void *, size_t);
static int (*cap_close)(int);

static void cap_resolve(void) {
    if (!cap_write) {
        cap_write = dlsym(RTLD_NEXT, "write");
    }
    if (!cap_open) {
        cap_open = dlsym(RTLD_NEXT, "open");
    }
    if (!cap_read) {
        cap_read = dlsym(RTLD_NEXT, "read");
    }
    if (!cap_close) {
        cap_close = dlsym(RTLD_NEXT, "close");
    }
}

static const char B64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static size_t b64_encode(const unsigned char *in, size_t len, char *out, size_t out_cap) {
    size_t o = 0;
    for (size_t i = 0; i < len; i += 3) {
        if (o + 4 >= out_cap) {
            break;
        }
        unsigned v = in[i] << 16;
        if (i + 1 < len) {
            v |= in[i + 1] << 8;
        }
        if (i + 2 < len) {
            v |= in[i + 2];
        }
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
        out[o++] = (i + 1 < len) ? B64[(v >> 6) & 63] : '=';
        out[o++] = (i + 2 < len) ? B64[v & 63] : '=';
    }
    out[o] = 0;
    return o;
}

static int b64_value(char c) {
    if (c >= 'A' && c <= 'Z') {
        return c - 'A';
    }
    if (c >= 'a' && c <= 'z') {
        return c - 'a' + 26;
    }
    if (c >= '0' && c <= '9') {
        return c - '0' + 52;
    }
    if (c == '+') {
        return 62;
    }
    if (c == '/') {
        return 63;
    }
    return -1;
}

static size_t b64_decode(const char *in, size_t len, unsigned char *out, size_t out_cap) {
    size_t o = 0;
    unsigned acc = 0;
    int bits = 0;
    for (size_t i = 0; i < len; i++) {
        int v = b64_value(in[i]);
        if (v < 0) {
            continue;
        }
        acc = (acc << 6) | (unsigned)v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            if (o < out_cap) {
                out[o++] = (unsigned char)((acc >> bits) & 0xff);
            }
        }
    }
    return o;
}

/* Append one line to the record log. Never blocks the host on failure. */
static void emit(const char *line, size_t len) {
    if (G.log_fd < 0) {
        return;
    }
    cap_resolve();
    if (cap_write) {
        ssize_t ignored = cap_write(G.log_fd, line, len);
        (void)ignored;
    }
}

void record_blob(kind_t kind, const char *key, const unsigned char *blob, size_t blob_len,
                        long a, long b) {
    if (G.mode != 1) {
        return;
    }
    if (G.entry_count >= MAX_ENTRIES) {
        G.dropped++;
        return;
    }
    G.entry_count++;
    static char line[MAX_BLOB * 2 + 1024];
    char encoded[MAX_BLOB * 2 + 8];
    encoded[0] = 0;
    size_t truncated = 0;
    if (blob && blob_len) {
        size_t take = blob_len > MAX_BLOB ? MAX_BLOB : blob_len;
        truncated = blob_len > MAX_BLOB ? blob_len : 0;
        b64_encode(blob, take, encoded, sizeof(encoded));
    }
    int n = snprintf(line, sizeof(line), "%s\t%s\t%s\t%ld\t%ld\t%zu\n", KIND_NAMES[kind],
                     key ? key : "-", encoded[0] ? encoded : "-", a, b, truncated);
    if (n > 0) {
        emit(line, (size_t)n);
    }
}

void diverge(const char *kind, const char *detail) {
    G.diverged++;
    static char line[1024];
    int n = snprintf(line, sizeof(line),
                     "REPROIT:DIVERGENCE {\"layer\":\"process\",\"kind\":\"%s\",\"detail\":\"%s\","
                     "\"served\":%zu,\"diverged\":%zu}\n",
                     kind, detail, G.served, G.diverged);
    if (n > 0) {
        cap_resolve();
        if (cap_write) {
            ssize_t ignored = cap_write(2, line, (size_t)n);
            (void)ignored;
        }
    }
}

/* Load the replay log. Called once, before any interposed call serves. */
void load_replay(const char *path) {
    cap_resolve();
    int fd = cap_open(path, O_RDONLY);
    if (fd < 0) {
        return;
    }
    static char buf[1 << 22];
    size_t total = 0;
    ssize_t got;
    while (total < sizeof(buf) - 1 && (got = cap_read(fd, buf + total, sizeof(buf) - 1 - total)) > 0) {
        total += (size_t)got;
    }
    buf[total] = 0;
    cap_close(fd);

    char *save_line = NULL;
    for (char *line = strtok_r(buf, "\n", &save_line); line; line = strtok_r(NULL, "\n", &save_line)) {
        if (G.entry_count >= MAX_ENTRIES) {
            break;
        }
        char *fields[6] = {NULL, NULL, NULL, NULL, NULL, NULL};
        int nf = 0;
        char *save_field = NULL;
        for (char *f = strtok_r(line, "\t", &save_field); f && nf < 6;
             f = strtok_r(NULL, "\t", &save_field)) {
            fields[nf++] = f;
        }
        if (nf < 5) {
            continue;
        }
        entry_t *e = &G.entries[G.entry_count];
        memset(e, 0, sizeof(*e));
        e->kind = K_KINDS;
        for (int k = 0; k < K_KINDS; k++) {
            if (strcmp(fields[0], KIND_NAMES[k]) == 0) {
                e->kind = (kind_t)k;
                break;
            }
        }
        if (e->kind == K_KINDS) {
            continue;
        }
        snprintf(e->key, sizeof(e->key), "%s", fields[1]);
        if (fields[2] && strcmp(fields[2], "-") != 0) {
            size_t enc_len = strlen(fields[2]);
            unsigned char *blob = malloc(enc_len);
            if (blob) {
                e->blob_len = b64_decode(fields[2], enc_len, blob, enc_len);
                e->blob = blob;
            }
        }
        e->a = atol(fields[3]);
        e->b = atol(fields[4]);
        G.entry_count++;
    }
}

entry_t *next_entry(kind_t kind, const char *key) {
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->consumed || e->kind != kind) {
            continue;
        }
        if (key && strcmp(e->key, key) != 0) {
            continue;
        }
        e->consumed = 1;
        return e;
    }
    return NULL;
}

/* Every recorded read of one key, concatenated: replay serves a file as one
 * buffer so a differing read granularity cannot diverge. */
size_t gather(kind_t kind, const char *key, unsigned char **out) {
    size_t total = 0;
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind == kind && strcmp(e->key, key) == 0 && e->blob) {
            total += e->blob_len;
        }
    }
    if (!total) {
        *out = NULL;
        return 0;
    }
    unsigned char *buf = malloc(total ? total : 1);
    if (!buf) {
        *out = NULL;
        return 0;
    }
    size_t off = 0;
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind == kind && strcmp(e->key, key) == 0 && e->blob) {
            memcpy(buf + off, e->blob, e->blob_len);
            off += e->blob_len;
            e->consumed = 1;
        }
    }
    *out = buf;
    return total;
}


/* Open the append-only record log. */
void reproit_open_log(const char *path) {
    cap_resolve();
    G.log_fd = cap_open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
}

/* Counters at replay exit. Best effort: a program that dies on a fatal signal
 * never runs the destructor, which is exactly the crash class this feature
 * reproduces, so the streamed divergence LINES are the authority. */
void reproit_report(void) {
    if (G.mode != 2) {
        return;
    }
    static char line[512];
    int n = snprintf(line, sizeof(line),
                     "REPROIT:PROCESS-REPLAY {\"served\":%zu,\"diverged\":%zu,"
                     "\"clockOverrun\":%zu,\"randomOverrun\":%zu,\"envFallthrough\":%zu}\n",
                     G.served, G.diverged, G.clock_overrun, G.random_overrun, G.env_fallthrough);
    if (n > 0) {
        cap_resolve();
        if (cap_write) {
            ssize_t ignored = cap_write(2, line, (size_t)n);
            (void)ignored;
        }
    }
}
