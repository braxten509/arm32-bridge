#ifndef _GB_POLL_H
#define _GB_POLL_H
#include <sys/types.h>

struct pollfd {
    int fd;
    short events;
    short revents;
};
typedef unsigned int nfds_t;
#define POLLIN 0x0001
#define POLLPRI 0x0002
#define POLLOUT 0x0004
#define POLLERR 0x0008
#define POLLHUP 0x0010
#define POLLNVAL 0x0020

__BEGIN_DECLS
int poll(struct pollfd *, nfds_t, int);
__END_DECLS
#endif
