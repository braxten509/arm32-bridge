#ifndef _GB_SCHED_H
#define _GB_SCHED_H
#include <_darwin.h>

struct sched_param {
    int sched_priority;
    char __opaque[4];
};

__BEGIN_DECLS
int sched_yield(void);
__END_DECLS
#endif
