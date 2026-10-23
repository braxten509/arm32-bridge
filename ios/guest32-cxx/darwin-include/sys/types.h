#ifndef _GB_SYS_TYPES_H
#define _GB_SYS_TYPES_H
#include <_darwin.h>
#include <stddef.h>
#include <stdint.h>

typedef __darwin_ssize_t ssize_t;
typedef __darwin_off_t off_t;
typedef __darwin_pid_t pid_t;
typedef __darwin_uid_t uid_t;
typedef __darwin_gid_t gid_t;
typedef __darwin_mode_t mode_t;
typedef __darwin_time_t time_t;
typedef __darwin_clock_t clock_t;
typedef __darwin_suseconds_t suseconds_t;
typedef __darwin_useconds_t useconds_t;
typedef __darwin_ino64_t ino_t;
typedef __darwin_dev_t dev_t;
typedef unsigned int u_int;
typedef unsigned char u_char;
typedef unsigned short u_short;
typedef unsigned long u_long;

typedef __darwin_pthread_t pthread_t;
typedef __darwin_pthread_attr_t pthread_attr_t;
typedef __darwin_pthread_mutex_t pthread_mutex_t;
typedef __darwin_pthread_mutexattr_t pthread_mutexattr_t;
typedef __darwin_pthread_cond_t pthread_cond_t;
typedef __darwin_pthread_condattr_t pthread_condattr_t;
typedef __darwin_pthread_once_t pthread_once_t;
typedef __darwin_pthread_rwlock_t pthread_rwlock_t;
typedef __darwin_pthread_rwlockattr_t pthread_rwlockattr_t;
typedef __darwin_pthread_key_t pthread_key_t;
#endif
