/* Game Bridge: the Darwin (iPhone OS, armv7) C library types libc++ needs.
 * Not Apple's headers: just the declarations, with Apple's sizes, layouts
 * and constants for 32-bit ARM (xnu bsd/arm/_types.h, bsd/sys/_types.h,
 * libpthread's sys/_pthread/_pthread_types.h). The functions are the
 * runner's C table (Apple's libSystem names). */
#ifndef _GB_DARWIN_H
#define _GB_DARWIN_H

#ifdef __cplusplus
#define __BEGIN_DECLS extern "C" {
#define __END_DECLS }
#else
#define __BEGIN_DECLS
#define __END_DECLS
#endif

#ifndef __restrict
#define __restrict __restrict__
#endif

typedef signed char __int8_t;
typedef unsigned char __uint8_t;
typedef short __int16_t;
typedef unsigned short __uint16_t;
typedef int __int32_t;
typedef unsigned int __uint32_t;
typedef long long __int64_t;
typedef unsigned long long __uint64_t;
typedef long __darwin_intptr_t;
typedef unsigned long __darwin_size_t;
typedef long __darwin_ssize_t;
typedef long __darwin_time_t;
typedef unsigned long __darwin_clock_t;
typedef int __darwin_wint_t;
typedef int __darwin_rune_t;
typedef int __darwin_ct_rune_t;
typedef __int64_t __darwin_off_t;
typedef __int32_t __darwin_pid_t;
typedef __uint32_t __darwin_uid_t;
typedef __uint32_t __darwin_gid_t;
typedef __uint16_t __darwin_mode_t;
typedef __int32_t __darwin_suseconds_t;
typedef __uint32_t __darwin_useconds_t;
typedef __uint64_t __darwin_ino64_t;
typedef __int32_t __darwin_dev_t;
typedef __uint32_t __darwin_socklen_t;

/* 128 bytes: Apple's mbstate_t. */
typedef union {
    char __mbstate8[128];
    long long _mbstateL;
} __mbstate_t;
typedef __mbstate_t __darwin_mbstate_t;

/* struct _xlocale is libSystem's; programs only hold pointers. */
typedef struct _xlocale *locale_t;

/* libpthread, 32-bit: { long __sig; char __opaque[N]; } */
#define __PTHREAD_SIZE__ 4088
#define __PTHREAD_ATTR_SIZE__ 36
#define __PTHREAD_MUTEXATTR_SIZE__ 8
#define __PTHREAD_MUTEX_SIZE__ 40
#define __PTHREAD_CONDATTR_SIZE__ 4
#define __PTHREAD_COND_SIZE__ 24
#define __PTHREAD_ONCE_SIZE__ 4
#define __PTHREAD_RWLOCK_SIZE__ 124
#define __PTHREAD_RWLOCKATTR_SIZE__ 12

struct __darwin_pthread_handler_rec;
struct _opaque_pthread_t {
    long __sig;
    struct __darwin_pthread_handler_rec *__cleanup_stack;
    char __opaque[__PTHREAD_SIZE__];
};
typedef struct _opaque_pthread_t *__darwin_pthread_t;
typedef struct { long __sig; char __opaque[__PTHREAD_ATTR_SIZE__]; } __darwin_pthread_attr_t;
typedef struct { long __sig; char __opaque[__PTHREAD_MUTEX_SIZE__]; } __darwin_pthread_mutex_t;
typedef struct { long __sig; char __opaque[__PTHREAD_MUTEXATTR_SIZE__]; } __darwin_pthread_mutexattr_t;
typedef struct { long __sig; char __opaque[__PTHREAD_COND_SIZE__]; } __darwin_pthread_cond_t;
typedef struct { long __sig; char __opaque[__PTHREAD_CONDATTR_SIZE__]; } __darwin_pthread_condattr_t;
typedef struct { long __sig; char __opaque[__PTHREAD_ONCE_SIZE__]; } __darwin_pthread_once_t;
typedef struct { long __sig; char __opaque[__PTHREAD_RWLOCK_SIZE__]; } __darwin_pthread_rwlock_t;
typedef struct { long __sig; char __opaque[__PTHREAD_RWLOCKATTR_SIZE__]; } __darwin_pthread_rwlockattr_t;
typedef unsigned long __darwin_pthread_key_t;

#endif
