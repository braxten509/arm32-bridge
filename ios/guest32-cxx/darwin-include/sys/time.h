#ifndef _GB_SYS_TIME_H
#define _GB_SYS_TIME_H
#include <time.h>

struct timeval {
    __darwin_time_t tv_sec;
    __darwin_suseconds_t tv_usec;
};
struct timezone;

__BEGIN_DECLS
int gettimeofday(struct timeval *__restrict, void *__restrict);
__END_DECLS
#endif
