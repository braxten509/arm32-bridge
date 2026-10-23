/* What Unwind-sjlj.c takes from Apple's System/pthread_machdep.h: the
 * thread's list of SjLj function contexts lives in Apple's reserved
 * thread-specific slot 18 (__PTK_LIBC_DYLD_Unwind_SjLj_Key), read through
 * TPIDRURO with its low 2 bits masked, as on a real iPhone. Pure guest code:
 * every game function with a cleanup registers and unregisters a context,
 * so this must not cost a call into the runner. The runner gives each
 * thread a zeroed block there (see README.md, "Interface"). */
#ifndef _GB_PTHREAD_MACHDEP_H
#define _GB_PTHREAD_MACHDEP_H

#define __PTK_LIBC_DYLD_Unwind_SjLj_Key 18

static inline void **_gb_tsd_base(void) {
    unsigned long base;
    __asm__("mrc p15, 0, %0, c13, c0, 3" : "=r"(base));
    return (void **)(base & ~3ul);
}

#define _pthread_getspecific_direct(key) ((struct _Unwind_FunctionContext *)_gb_tsd_base()[key])
#define _pthread_setspecific_direct(key, value) (_gb_tsd_base()[key] = (void *)(value))

#endif
