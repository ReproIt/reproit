/*
 * Is an ELF image dynamically linked?
 *
 * The process boundary has two halves and only one of them survives a static
 * image. The libc half is LD_PRELOAD, which needs a dynamic loader to exist
 * at all; the seccomp half filters the KERNEL boundary and keeps working. So
 * a statically linked program is observed for files and path metadata and for
 * NOTHING else: no clock, no randomness, no environment, no sockets.
 *
 * Measured, not assumed. A `gcc -static` subject launched behind a `/bin/sh`
 * wrapper produced a capsule of six entries that replayed and reported
 * "reproduced" with its input file deleted, because that subject happened to
 * only read a file. The same shape with one socket dial would have replayed
 * against the LIVE network with zero divergences. Naming the image is what
 * lets capture refuse instead.
 *
 * The judgement is PT_INTERP: an image with no interpreter program header has
 * no dynamic loader. Mirrors the Rust side in workflows/process_capsule, which
 * judges the LAUNCHED command before it runs; this one judges every image the
 * process EXECs into afterwards, which is the case the Rust check cannot see.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <fcntl.h>
#include <string.h>
#include <unistd.h>

#define PT_INTERP_TYPE 3u
/* Enough for an ELF header plus a generous program header table. A file this
 * small cannot be judged and says so rather than guessing. */
#define ELF_PROBE 8192

int reproit_elf_is_dynamic(const char *path) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        return -1;
    }
    unsigned char head[ELF_PROBE];
    ssize_t got = read(fd, head, sizeof(head));
    close(fd);
    if (got < 64 || memcmp(head, "\177ELF", 4) != 0) {
        return -1;
    }
    if (head[4] != 2 || head[5] != 1) {
        /* 64 bit little endian only, which is every target this tool ships
         * for. Saying nothing beats guessing on the rest. */
        return -1;
    }
    unsigned long long table = 0;
    for (int i = 7; i >= 0; i--) {
        table = (table << 8) | head[0x20 + i];
    }
    unsigned entry_size = (unsigned)head[0x36] | ((unsigned)head[0x37] << 8);
    unsigned entries = (unsigned)head[0x38] | ((unsigned)head[0x39] << 8);
    if (entry_size == 0) {
        return -1;
    }
    for (unsigned i = 0; i < entries; i++) {
        unsigned long long offset = table + (unsigned long long)i * entry_size;
        if (offset + 4 > (unsigned long long)got) {
            /* The table runs past what was read, so absence of PT_INTERP is
             * not established. Unjudgeable, not static. */
            return -1;
        }
        unsigned kind = (unsigned)head[offset] | ((unsigned)head[offset + 1] << 8) |
                        ((unsigned)head[offset + 2] << 16) | ((unsigned)head[offset + 3] << 24);
        if (kind == PT_INTERP_TYPE) {
            return 1;
        }
    }
    return 0;
}
