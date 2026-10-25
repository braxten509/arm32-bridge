#ifndef _GB_UNISTD_H
#define _GB_UNISTD_H
#include <sys/types.h>

#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2
#define _SC_NPROCESSORS_CONF 57
#define _SC_NPROCESSORS_ONLN 58
#define _SC_PAGESIZE 29

__BEGIN_DECLS
long sysconf(int);
int usleep(useconds_t);
unsigned sleep(unsigned);
ssize_t read(int, void *, size_t);
ssize_t write(int, const void *, size_t);
off_t lseek(int, off_t, int);
int close(int);
int isatty(int);
pid_t getpid(void);
char *getcwd(char *, size_t);
int unlink(const char *);
__END_DECLS
#endif
