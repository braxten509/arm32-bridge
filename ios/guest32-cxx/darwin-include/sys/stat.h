/* Darwin armv7's struct stat (64-bit inode layout, 108 bytes; long long is
 * 4-aligned), as the C table's stat/fstat write it. */
#ifndef _GB_SYS_STAT_H
#define _GB_SYS_STAT_H
#include <sys/types.h>
#include <time.h>

struct stat {
    dev_t st_dev;
    mode_t st_mode;
    unsigned short st_nlink;
    ino_t st_ino;
    uid_t st_uid;
    gid_t st_gid;
    dev_t st_rdev;
    struct timespec st_atimespec;
    struct timespec st_mtimespec;
    struct timespec st_ctimespec;
    struct timespec st_birthtimespec;
    off_t st_size;
    long long st_blocks;
    int st_blksize;
    unsigned int st_flags;
    unsigned int st_gen;
    int st_lspare;
    long long st_qspare[2];
};

#define S_IFMT 0170000
#define S_IFIFO 0010000
#define S_IFCHR 0020000
#define S_IFDIR 0040000
#define S_IFREG 0100000
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)

__BEGIN_DECLS
int stat(const char *, struct stat *);
int fstat(int, struct stat *);
__END_DECLS
#endif
