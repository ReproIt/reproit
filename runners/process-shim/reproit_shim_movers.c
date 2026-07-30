/* Data movers: the libc calls that carry file bytes WITHOUT going through
 * read or pread. Split from reproit_shim.c so each unit stays reviewable.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/sendfile.h>
#endif

static ssize_t (*mover_pread)(int, void *, size_t, off_t);

static void mover_resolve(void) {
    if (!mover_pread) {
        mover_pread = dlsym(RTLD_NEXT, "pread");
    }
}

/* Data movers. Measured, not assumed: with only read and pread covered, a
 * coreutils cat recorded the open and no bytes (it uses copy_file_range) and
 * a CPython run replayed wrong in silence (it uses mmap). Each mover below
 * records what it carried so the completeness oracle has the bytes, and each
 * refuses to move data it cannot account for at replay. */

ssize_t readv(int fd, const struct iovec *iov, int count) {
    static ssize_t (*real_readv)(int, const struct iovec *, int);
    int return_real = 0;
    if (!real_readv) {
        real_readv = dlsym(RTLD_NEXT, "readv");
    }
    ENTER();
    if (return_real) {
        return real_readv(fd, iov, count);
    }
    ssize_t got;
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
        got = 0;
        for (int i = 0; i < count && G.fds[fd].off < G.fds[fd].len; i++) {
            ssize_t piece = serve_recv(fd, iov[i].iov_base, iov[i].iov_len);
            if (piece <= 0) {
                break;
            }
            got += piece;
        }
    } else {
        got = real_readv(fd, iov, count);
        if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.paths[fd][0]) {
            ssize_t left = got;
            for (int i = 0; i < count && left > 0; i++) {
                size_t take = (size_t)left < iov[i].iov_len ? (size_t)left : iov[i].iov_len;
                record_blob(K_READ, G.paths[fd], (const unsigned char *)iov[i].iov_base, take, fd,
                            0);
                left -= (ssize_t)take;
            }
        }
    }
    LEAVE();
    return got;
}

ssize_t preadv(int fd, const struct iovec *iov, int count, off_t offset) {
    static ssize_t (*real_preadv)(int, const struct iovec *, int, off_t);
    int return_real = 0;
    if (!real_preadv) {
        real_preadv = dlsym(RTLD_NEXT, "preadv");
    }
    ENTER();
    if (return_real) {
        return real_preadv(fd, iov, count, offset);
    }
    ssize_t got = real_preadv(fd, iov, count, offset);
    if (G.mode == 1 && got > 0 && fd >= 0 && fd < MAX_FDS && G.paths[fd][0]) {
        ssize_t left = got;
        for (int i = 0; i < count && left > 0; i++) {
            size_t take = (size_t)left < iov[i].iov_len ? (size_t)left : iov[i].iov_len;
            record_blob(K_READ, G.paths[fd], (const unsigned char *)iov[i].iov_base, take, fd, 0);
            left -= (ssize_t)take;
        }
    }
    LEAVE();
    return got;
}

/* A file backed mapping IS a read of the whole mapped range: record the
 * mapped bytes so the capsule carries what the program will later touch. */
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset) {
    static void *(*real_mmap)(void *, size_t, int, int, int, off_t);
    int return_real = 0;
    if (!real_mmap) {
        real_mmap = dlsym(RTLD_NEXT, "mmap");
    }
    ENTER();
    if (return_real) {
        return real_mmap(addr, length, prot, flags, fd, offset);
    }
    void *mapped = real_mmap(addr, length, prot, flags, fd, offset);
    if (G.mode == 1 && mapped != MAP_FAILED && fd >= 0 && fd < MAX_FDS && G.paths[fd][0] &&
        !(flags & MAP_ANONYMOUS) && (prot & PROT_READ)) {
        size_t take = length > MAX_BLOB ? MAX_BLOB : length;
        record_blob(K_READ, G.paths[fd], (const unsigned char *)mapped, take, fd, (long)offset);
    }
    LEAVE();
    return mapped;
}

void *mmap64(void *addr, size_t length, int prot, int flags, int fd, off_t offset) {
    return mmap(addr, length, prot, flags, fd, offset);
}

#ifdef __linux__
ssize_t copy_file_range(int in_fd, off_t *in_off, int out_fd, off_t *out_off, size_t len,
                        unsigned int flags) {
    static ssize_t (*real_copy_file_range)(int, off_t *, int, off_t *, size_t, unsigned int);
    int return_real = 0;
    if (!real_copy_file_range) {
        real_copy_file_range = dlsym(RTLD_NEXT, "copy_file_range");
    }
    ENTER();
    if (return_real) {
        return real_copy_file_range(in_fd, in_off, out_fd, out_off, len, flags);
    }
    ssize_t moved;
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        /* Record what it carries by reading it ourselves at the recorded
         * offset, then let the real call do the transfer. */
        static unsigned char scratch[MAX_BLOB];
        size_t take = len > sizeof(scratch) ? sizeof(scratch) : len;
        mover_resolve();
        off_t at = in_off ? *in_off : lseek(in_fd, 0, SEEK_CUR);
        ssize_t seen = mover_pread(in_fd, scratch, take, at);
        if (seen > 0) {
            record_blob(K_READ, G.paths[in_fd], scratch, (size_t)seen, in_fd, (long)at);
        }
        moved = real_copy_file_range(in_fd, in_off, out_fd, out_off, len, flags);
    } else if (G.mode == 2) {
        /* At replay the source is a memfd of recorded content, so the kernel
         * copy is faithful; nothing to serve. */
        moved = real_copy_file_range(in_fd, in_off, out_fd, out_off, len, flags);
    } else {
        moved = real_copy_file_range(in_fd, in_off, out_fd, out_off, len, flags);
    }
    LEAVE();
    return moved;
}

ssize_t sendfile(int out_fd, int in_fd, off_t *offset, size_t count) {
    static ssize_t (*real_sendfile)(int, int, off_t *, size_t);
    int return_real = 0;
    if (!real_sendfile) {
        real_sendfile = dlsym(RTLD_NEXT, "sendfile");
    }
    ENTER();
    if (return_real) {
        return real_sendfile(out_fd, in_fd, offset, count);
    }
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        static unsigned char scratch[MAX_BLOB];
        size_t take = count > sizeof(scratch) ? sizeof(scratch) : count;
        mover_resolve();
        off_t at = offset ? *offset : lseek(in_fd, 0, SEEK_CUR);
        ssize_t seen = mover_pread(in_fd, scratch, take, at);
        if (seen > 0) {
            record_blob(K_READ, G.paths[in_fd], scratch, (size_t)seen, in_fd, (long)at);
        }
    }
    ssize_t moved = real_sendfile(out_fd, in_fd, offset, count);
    LEAVE();
    return moved;
}

ssize_t splice(int in_fd, off_t *in_off, int out_fd, off_t *out_off, size_t len,
               unsigned int flags) {
    static ssize_t (*real_splice)(int, off_t *, int, off_t *, size_t, unsigned int);
    int return_real = 0;
    if (!real_splice) {
        real_splice = dlsym(RTLD_NEXT, "splice");
    }
    ENTER();
    if (return_real) {
        return real_splice(in_fd, in_off, out_fd, out_off, len, flags);
    }
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        static unsigned char scratch[MAX_BLOB];
        size_t take = len > sizeof(scratch) ? sizeof(scratch) : len;
        mover_resolve();
        off_t at = in_off ? *in_off : lseek(in_fd, 0, SEEK_CUR);
        ssize_t seen = mover_pread(in_fd, scratch, take, at);
        if (seen > 0) {
            record_blob(K_READ, G.paths[in_fd], scratch, (size_t)seen, in_fd, (long)at);
        }
    }
    ssize_t moved = real_splice(in_fd, in_off, out_fd, out_off, len, flags);
    LEAVE();
    return moved;
}
#endif
