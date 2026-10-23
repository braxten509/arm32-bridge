/* libpthread (32-bit): Apple's layouts and initializer signatures. */
#ifndef _GB_PTHREAD_H
#define _GB_PTHREAD_H
#include <sys/types.h>
#include <time.h>
#include <sched.h>

#define _PTHREAD_MUTEX_SIG_init 0x32AAABA7
#define _PTHREAD_ERRORCHECK_MUTEX_SIG_init 0x32AAABA1
#define _PTHREAD_RECURSIVE_MUTEX_SIG_init 0x32AAABA2
#define _PTHREAD_COND_SIG_init 0x3CB0B1BB
#define _PTHREAD_ONCE_SIG_init 0x30B1BCBA
#define _PTHREAD_RWLOCK_SIG_init 0x2DA8B3B4
#define PTHREAD_MUTEX_INITIALIZER {_PTHREAD_MUTEX_SIG_init, {0}}
#define PTHREAD_RECURSIVE_MUTEX_INITIALIZER {_PTHREAD_RECURSIVE_MUTEX_SIG_init, {0}}
#define PTHREAD_COND_INITIALIZER {_PTHREAD_COND_SIG_init, {0}}
#define PTHREAD_ONCE_INIT {_PTHREAD_ONCE_SIG_init, {0}}
#define PTHREAD_RWLOCK_INITIALIZER {_PTHREAD_RWLOCK_SIG_init, {0}}

#define PTHREAD_MUTEX_NORMAL 0
#define PTHREAD_MUTEX_ERRORCHECK 1
#define PTHREAD_MUTEX_RECURSIVE 2
#define PTHREAD_MUTEX_DEFAULT PTHREAD_MUTEX_NORMAL
#define PTHREAD_CREATE_JOINABLE 1
#define PTHREAD_CREATE_DETACHED 2
#define PTHREAD_DESTRUCTOR_ITERATIONS 4
#define PTHREAD_KEYS_MAX 512

__BEGIN_DECLS
int pthread_create(pthread_t *__restrict, const pthread_attr_t *__restrict, void *(*)(void *), void *__restrict);
int pthread_join(pthread_t, void **);
int pthread_detach(pthread_t);
pthread_t pthread_self(void);
int pthread_equal(pthread_t, pthread_t);
int pthread_once(pthread_once_t *, void (*)(void));
int pthread_attr_init(pthread_attr_t *);
int pthread_attr_destroy(pthread_attr_t *);
int pthread_mutex_init(pthread_mutex_t *__restrict, const pthread_mutexattr_t *__restrict);
int pthread_mutex_destroy(pthread_mutex_t *);
int pthread_mutex_lock(pthread_mutex_t *);
int pthread_mutex_trylock(pthread_mutex_t *);
int pthread_mutex_unlock(pthread_mutex_t *);
int pthread_mutexattr_init(pthread_mutexattr_t *);
int pthread_mutexattr_destroy(pthread_mutexattr_t *);
int pthread_mutexattr_settype(pthread_mutexattr_t *, int);
int pthread_cond_init(pthread_cond_t *__restrict, const pthread_condattr_t *__restrict);
int pthread_cond_destroy(pthread_cond_t *);
int pthread_cond_wait(pthread_cond_t *__restrict, pthread_mutex_t *__restrict);
int pthread_cond_timedwait(pthread_cond_t *__restrict, pthread_mutex_t *__restrict, const struct timespec *__restrict);
int pthread_cond_signal(pthread_cond_t *);
int pthread_cond_broadcast(pthread_cond_t *);
int pthread_rwlock_init(pthread_rwlock_t *__restrict, const pthread_rwlockattr_t *__restrict);
int pthread_rwlock_destroy(pthread_rwlock_t *);
int pthread_rwlock_rdlock(pthread_rwlock_t *);
int pthread_rwlock_tryrdlock(pthread_rwlock_t *);
int pthread_rwlock_wrlock(pthread_rwlock_t *);
int pthread_rwlock_trywrlock(pthread_rwlock_t *);
int pthread_rwlock_unlock(pthread_rwlock_t *);
int pthread_key_create(pthread_key_t *, void (*)(void *));
int pthread_key_delete(pthread_key_t);
void *pthread_getspecific(pthread_key_t);
int pthread_setspecific(pthread_key_t, const void *);
typedef unsigned int mach_port_t;
mach_port_t pthread_mach_thread_np(pthread_t);
int pthread_threadid_np(pthread_t, __uint64_t *);
int pthread_main_np(void);
__END_DECLS
#endif
