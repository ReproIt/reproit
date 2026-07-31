/*
 * The scratch tree the seccomp completeness layer replays files out of.
 *
 * Split from reproit_seccomp.c so the supervisor unit stays reviewable: this
 * file owns WHERE recorded content lands on disk and nothing about the
 * syscall protocol. Recorded bytes are materialized as REAL files rather than
 * memfds because glibc validates a locale object structurally and the loader
 * maps a shared object PROT_EXEC, and a memfd copy satisfies neither.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* The scratch tree replay materializes recorded content into. One per
 * supervisor, torn down when the target exits. */
static char scratch_root[64];

const char *reproit_scratch(void) {
    if (!scratch_root[0]) {
        snprintf(scratch_root, sizeof(scratch_root), "/tmp/reproit-replay-XXXXXX");
        if (!mkdtemp(scratch_root)) {
            scratch_root[0] = 0;
            return NULL;
        }
    }
    return scratch_root;
}

void reproit_scratch_teardown(void) {
    if (!scratch_root[0]) {
        return;
    }
    /* Bounded and non recursive by construction: the tree is exactly one
     * level of materialized files plus the rebuilt directories, which are
     * themselves one level deep. */
    DIR *root = opendir(scratch_root);
    if (root) {
        struct dirent *entry;
        while ((entry = readdir(root)) != NULL) {
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            char path[MAX_PATH_LEN];
            snprintf(path, sizeof(path), "%s/%s", scratch_root, entry->d_name);
            if (entry->d_type == DT_DIR) {
                DIR *nested = opendir(path);
                if (nested) {
                    struct dirent *inner;
                    while ((inner = readdir(nested)) != NULL) {
                        if (strcmp(inner->d_name, ".") == 0 || strcmp(inner->d_name, "..") == 0) {
                            continue;
                        }
                        char nested_path[MAX_PATH_LEN];
                        snprintf(nested_path, sizeof(nested_path), "%s/%s", path, inner->d_name);
                        remove(nested_path);
                    }
                    closedir(nested);
                }
                rmdir(path);
            } else {
                unlink(path);
            }
        }
        closedir(root);
    }
    rmdir(scratch_root);
    scratch_root[0] = 0;
}

/* A stable scratch name for one recorded path, so a file opened repeatedly is
 * materialized once. */
void reproit_scratch_name(const char *absolute, char *out, size_t cap) {
    unsigned long hash = 1469598103934665603UL;
    for (const char *p = absolute; *p; p++) {
        hash ^= (unsigned char)*p;
        hash *= 1099511628211UL;
    }
    const char *base = strrchr(absolute, '/');
    base = base ? base + 1 : absolute;
    snprintf(out, cap, "%s/%016lx-%.64s", scratch_root, hash, base);
}

/* Materialize recorded content as a REAL file and hand back a descriptor to
 * it.
 *
 * This used to be a memfd, which was measured to break two things a copy
 * cannot fake. glibc validates a locale object structurally, and the dynamic
 * loader maps a shared object PROT_EXEC and relocates it, which a memfd is
 * refused for on kernels that default memfds to noexec. A real file on disk
 * satisfies both, and the program still never touches the host's copy: the
 * bytes come from the capsule and nothing else. */
int reproit_materialize(const char *absolute, const unsigned char *content, size_t len) {
    if (!reproit_scratch()) {
        return -1;
    }
    char path[MAX_PATH_LEN];
    reproit_scratch_name(absolute, path, sizeof(path));
    struct stat existing;
    if (stat(path, &existing) == 0 && (size_t)existing.st_size == len) {
        return open(path, O_RDONLY | O_CLOEXEC);
    }
    /* 0755 so an executable mapping of a served shared object is permitted. */
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (fd < 0) {
        return -1;
    }
    size_t off = 0;
    while (off < len) {
        ssize_t wrote = write(fd, content + off, len - off);
        if (wrote <= 0) {
            break;
        }
        off += (size_t)wrote;
    }
    close(fd);
    return open(path, O_RDONLY | O_CLOEXEC);
}

/* Rebuild a recorded directory as a real one in a scratch tree, so the
 * program's getdents64 is answered by the KERNEL from names the capsule
 * carries. Writing dirent structs by hand would duplicate the kernel's
 * layout rules for no gain; materializing the names cannot get it wrong. */
int reproit_serve_dir(const char *absolute) {
    if (!reproit_scratch()) {
        return -1;
    }
    char template[MAX_PATH_LEN];
    reproit_scratch_name(absolute, template, sizeof(template));
    if (mkdir(template, 0755) != 0 && errno != EEXIST) {
        return -1;
    }
    for (size_t i = 0; i < G.entry_count; i++) {
        entry_t *e = &G.entries[i];
        if (e->kind != K_DIRENT || strcmp(e->key, absolute) != 0 || !e->blob) {
            continue;
        }
        char name[MAX_PATH_LEN];
        size_t len = e->blob_len < sizeof(name) - 1 ? e->blob_len : sizeof(name) - 1;
        memcpy(name, e->blob, len);
        name[len] = 0;
        char full[MAX_PATH_LEN];
        snprintf(full, sizeof(full), "%s/%s", template, name);
        if (e->a == DT_DIR) {
            mkdir(full, 0755);
        } else {
            int created = open(full, O_WRONLY | O_CREAT | O_EXCL, 0644);
            if (created >= 0) {
                close(created);
            }
        }
    }
    return open(template, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
}
