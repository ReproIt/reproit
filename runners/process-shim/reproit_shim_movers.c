/* Data movers: the libc calls that carry file bytes WITHOUT going through
 * read or pread. Split from reproit_shim.c so each unit stays reviewable.
 */
#define _GNU_SOURCE
#include "reproit_shim_capsule.h"

#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
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
 * refuses to move data it cannot account for at replay.
 *
 * Recording is FULL, bounded the honest way. The first cut recorded at most
 * one 8 KiB chunk per call, so any file past 8 KiB replayed as a loud
 * truncated-file; correct capture is what turns that loud failure into a
 * correct replay. mover_record reads the moved range back through pread in
 * MAX_BLOB chunks, up to REPROIT_FILE_CAP per file, and a range past the cap
 * records a `trunc` marker so the replay divergence NAMES the cap instead of
 * reporting an anonymous shortfall. G.mover_end is the high-water offset a
 * mover has covered for this fd: a re-mapped or re-copied range below it is
 * not recorded twice, because doubled content serves doubled, which is the
 * overlong-file silent wrong replay. */
static void mover_record(int fd, off_t at, size_t want) {
    if (G.mode != 1 || fd < 0 || fd >= MAX_FDS || !G.paths[fd][0] || want == 0 || at < 0) {
        return;
    }
    mover_resolve();
    if (!mover_pread) {
        return;
    }
    off_t end = at + (off_t)want;
    off_t from = (off_t)G.mover_end[fd];
    if (at > from) {
        from = at; /* a gap stays a gap: replay refuses short, loudly */
    }
    static unsigned char scratch[MAX_BLOB];
    while (from < end) {
        if ((size_t)from >= REPROIT_FILE_CAP) {
            if (!G.mover_capped[fd]) {
                G.mover_capped[fd] = 1;
                struct stat info;
                long size = fstat(fd, &info) == 0 ? (long)info.st_size : 0;
                record_blob(K_TRUNC, G.paths[fd], NULL, 0, (long)REPROIT_FILE_CAP, size);
            }
            return;
        }
        size_t take = (size_t)(end - from);
        if (take > sizeof(scratch)) {
            take = sizeof(scratch);
        }
        if ((size_t)from + take > REPROIT_FILE_CAP) {
            take = REPROIT_FILE_CAP - (size_t)from;
        }
        ssize_t seen = mover_pread(fd, scratch, take, from);
        if (seen <= 0) {
            return; /* end of file: the range outran the content, nothing lost */
        }
        record_blob(K_READ, G.paths[fd], scratch, (size_t)seen, fd, (long)from);
        from += seen;
        if ((size_t)from > G.mover_end[fd]) {
            G.mover_end[fd] = (size_t)from;
        }
    }
}

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
    if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active && !G.fds[fd].is_socket) {
        /* A replayed FILE is a real descriptor the kernel answers; asking the
         * socket stream for it returned a false EOF (measured before this
         * branch existed). The same partial-capture backstop read() applies. */
        got = real_readv(fd, iov, count);
        size_t asked = 0;
        for (int i = 0; i < count; i++) {
            asked += iov[i].iov_len;
        }
        if (got == 0 && asked > 0 && G.fds[fd].incomplete) {
            diverge_short("truncated-file", G.fds[fd].key, G.fds[fd].recorded, G.fds[fd].held);
            errno = EIO;
            got = -1;
        } else if (got > 0) {
            G.served++;
        }
    } else if (G.mode == 2 && fd >= 0 && fd < MAX_FDS && G.fds[fd].active) {
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
                record_content(K_READ, G.paths[fd], (const unsigned char *)iov[i].iov_base, take,
                               fd, 0);
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
            record_content(K_READ, G.paths[fd], (const unsigned char *)iov[i].iov_base, take, fd,
                           (long)offset);
            offset += (off_t)take;
            left -= (ssize_t)take;
        }
    }
    LEAVE();
    return got;
}

ssize_t preadv64(int fd, const struct iovec *iov, int count, off_t offset) {
    return preadv(fd, iov, count, offset);
}

/* A file backed mapping IS a read of the whole mapped range: record the
 * mapped bytes so the capsule carries what the program will later touch.
 * Read back through pread rather than through the mapping, because touching
 * pages past EOF is SIGBUS while pread simply stops at the end. */
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
        mover_record(fd, offset, length);
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
    /* The source offset BEFORE the kernel advances it, then record exactly
     * what MOVED. Recording the requested length instead re-recorded the
     * overlap when the kernel moved less than asked, and doubled content
     * serves doubled. */
    off_t at = -1;
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        at = in_off ? *in_off : lseek(in_fd, 0, SEEK_CUR);
    }
    ssize_t moved = real_copy_file_range(in_fd, in_off, out_fd, out_off, len, flags);
    if (moved > 0 && at >= 0) {
        mover_record(in_fd, at, (size_t)moved);
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
    off_t at = -1;
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        at = offset ? *offset : lseek(in_fd, 0, SEEK_CUR);
    }
    ssize_t moved = real_sendfile(out_fd, in_fd, offset, count);
    if (moved > 0 && at >= 0) {
        mover_record(in_fd, at, (size_t)moved);
    }
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
    off_t at = -1;
    if (G.mode == 1 && in_fd >= 0 && in_fd < MAX_FDS && G.paths[in_fd][0]) {
        at = in_off ? *in_off : lseek(in_fd, 0, SEEK_CUR);
    }
    ssize_t moved = real_splice(in_fd, in_off, out_fd, out_off, len, flags);
    if (moved > 0 && at >= 0) {
        mover_record(in_fd, at, (size_t)moved);
    }
    LEAVE();
    return moved;
}
#endif
