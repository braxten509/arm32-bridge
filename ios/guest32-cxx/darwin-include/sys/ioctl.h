#ifndef _GB_SYS_IOCTL_H
#define _GB_SYS_IOCTL_H
#include <sys/filio.h>

__BEGIN_DECLS
int ioctl(int, unsigned long, ...);
__END_DECLS
#endif
