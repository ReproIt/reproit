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

static const char *KIND_NAMES[K_KINDS] = {"open",  "read",   "connect",  "send",   "recv",
                                          "clock", "time",   "random",   "env",    "stat",
                                          "statx", "access", "readlink", "getcwd", "dirent",
                                          "input", "exec"};

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

void reproit_normalize_path(char *path) {
    char *write = path;
    const char *read = path;
    while (*read) {
        if (read[0] == '/' && read[1] == '/') {
            read++;
            continue;
        }
        if (read[0] == '/' && read[1] == '.' && (read[2] == '/' || read[2] == 0)) {
            read += 2;
            continue;
        }
        *write++ = *read++;
    }
    /* A trailing separator names the same directory, so it must not make a
     * second key for it. The root itself keeps its one slash. */
    while (write > path + 1 && *(write - 1) == '/') {
        write--;
    }
    if (write == path) {
        *write++ = '/';
    }
    *write = 0;
}

/* FNV-1a. Only ever compared against itself, so the choice is about spread
 * and size, not about cryptography. */
static uint64_t key_hash(const char *s) {
    uint64_t h = 1469598103934665603ULL;
    for (; *s; s++) {
        h ^= (unsigned char)*s;
        h *= 1099511628211ULL;
    }
    return h;
}

void reproit_path_key(const char *absolute, char *out, size_t cap) {
    if (!absolute || !absolute[0] || cap < 32) {
        snprintf(out, cap, "-");
        return;
    }
    char folded[MAX_PATH_LEN];
    snprintf(folded, sizeof(folded), "%s", absolute);
    reproit_normalize_path(folded);
    size_t len = strlen(folded);
    if (len < cap) {
        memcpy(out, folded, len + 1);
        return;
    }
    /* 16 hex digits, one separator, one NUL. */
    size_t tail = cap - 18;
    snprintf(out, cap, "%016llx:%s", (unsigned long long)key_hash(folded), folded + (len - tail));
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

entry_t *next_entry_at(kind_t kind, const char *key, size_t *index) {
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->consumed || e->kind != kind) {
            continue;
        }
        if (key && strcmp(e->key, key) != 0) {
            continue;
        }
        e->consumed = 1;
        if (index) {
            *index = i;
        }
        return e;
    }
    return NULL;
}

entry_t *next_entry(kind_t kind, const char *key) { return next_entry_at(kind, key, NULL); }

entry_t *find_entry_at(kind_t kind, const char *key, size_t *index) {
    entry_t *fallback = NULL;
    size_t fallback_index = 0;
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind != kind) {
            continue;
        }
        if (key && strcmp(e->key, key) != 0) {
            continue;
        }
        if (!e->consumed) {
            e->consumed = 1;
            if (index) {
                *index = i;
            }
            return e;
        }
        fallback = e;
        fallback_index = i;
    }
    if (fallback && index) {
        *index = fallback_index;
    }
    return fallback;
}

entry_t *find_entry(kind_t kind, const char *key) { return find_entry_at(kind, key, NULL); }

size_t next_key_index(kind_t kind, const char *key, size_t after) {
    for (size_t i = after + 1; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind == kind && (!key || strcmp(e->key, key) == 0)) {
            return i;
        }
    }
    return G.entry_count;
}

size_t gather_span(kind_t kind, const char *key, size_t from, size_t to, unsigned char **out) {
    if (to > G.entry_count) {
        to = G.entry_count;
    }
    size_t total = 0;
    for (size_t i = from; i < to; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind == kind && strcmp(e->key, key) == 0 && e->blob) {
            total += e->blob_len;
        }
    }
    if (!total) {
        *out = NULL;
        return 0;
    }
    unsigned char *buf = malloc(total);
    if (!buf) {
        *out = NULL;
        return 0;
    }
    size_t off = 0;
    for (size_t i = from; i < to; i++) {
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

/* Every recorded read of one key, concatenated: replay serves a file as one
 * buffer so a differing read granularity cannot diverge. Callers that know
 * WHICH open they are serving use gather_span instead; this whole-log form is
 * the answer only when the capsule has reads for a key and no open to tie
 * them to. */
size_t gather(kind_t kind, const char *key, unsigned char **out) {
    return gather_span(kind, key, 0, G.entry_count, out);
}


/* Open the append-only record log. */
void reproit_open_log(const char *path) {
    cap_resolve();
    /* O_TRUNC because a capsule describes ONE session. Appending to a stale
     * log silently merged two runs into a capsule that never happened, which
     * replay would then be unable to satisfy. The CLI always passes a fresh
     * temp path, so this only bites a hand run, which is exactly when a
     * confusing capsule is hardest to spot. */
    G.log_fd = cap_open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
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
                     "\"clockOverrun\":%zu,\"randomOverrun\":%zu,\"envFallthrough\":%zu,"
                     "\"inputServed\":%zu,\"inputEarly\":%zu,\"ticks\":%zu}\n",
                     G.served, G.diverged, G.clock_overrun, G.random_overrun, G.env_fallthrough,
                     G.input_served, G.input_early, G.tick);
    if (n > 0) {
        cap_resolve();
        if (cap_write) {
            ssize_t ignored = cap_write(2, line, (size_t)n);
            (void)ignored;
        }
    }
}
