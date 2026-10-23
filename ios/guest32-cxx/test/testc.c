/* A tiny Darwin-ABI C library for the libguest32cxx.so test, standing in
 * for the runner's C table (single thread; only what the test reaches).
 * Built like the library, loaded before it, so its exports win. */
#include <ctype.h>
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xlocale.h>

#define EXPORT __attribute__((visibility("default")))

/* ---- memory: a bump heap; each block keeps its size before it ---- */
static _Alignas(16) unsigned char heap[4 << 20];
static size_t heap_used;

EXPORT void *malloc(size_t size) {
    size_t start = (heap_used + 16 + 15) & ~(size_t)15;
    if (start + size > sizeof heap)
        return NULL;
    ((size_t *)(heap + start))[-1] = size;
    heap_used = start + size;
    return heap + start;
}
EXPORT void free(void *p) { (void)p; }
EXPORT void *calloc(size_t count, size_t size) {
    void *p = malloc(count * size);
    if (p)
        memset(p, 0, count * size);
    return p;
}
EXPORT void *realloc(void *old, size_t size) {
    void *p = malloc(size);
    if (p && old) {
        size_t old_size = ((size_t *)old)[-1];
        memcpy(p, old, old_size < size ? old_size : size);
    }
    return p;
}
EXPORT int posix_memalign(void **out, size_t alignment, size_t size) {
    unsigned char *p = malloc(size + alignment);
    if (!p)
        return ENOMEM;
    *out = (void *)(((unsigned long)p + alignment - 1) & ~(unsigned long)(alignment - 1));
    return 0;
}

EXPORT void *memcpy(void *__restrict d, const void *__restrict s, size_t n) {
    unsigned char *a = d;
    const unsigned char *b = s;
    while (n--)
        *a++ = *b++;
    return d;
}
EXPORT void *memmove(void *d, const void *s, size_t n) {
    unsigned char *a = d;
    const unsigned char *b = s;
    if (a < b)
        while (n--)
            *a++ = *b++;
    else
        while (n--)
            a[n] = b[n];
    return d;
}
EXPORT void *memset(void *d, int c, size_t n) {
    unsigned char *a = d;
    while (n--)
        *a++ = (unsigned char)c;
    return d;
}
EXPORT int memcmp(const void *x, const void *y, size_t n) {
    const unsigned char *a = x, *b = y;
    for (; n; n--, a++, b++)
        if (*a != *b)
            return *a - *b;
    return 0;
}
EXPORT void *memchr(const void *s, int c, size_t n) {
    const unsigned char *a = s;
    for (; n; n--, a++)
        if (*a == (unsigned char)c)
            return (void *)a;
    return NULL;
}
EXPORT size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n])
        n++;
    return n;
}
EXPORT int strcmp(const char *a, const char *b) {
    while (*a && *a == *b)
        a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}

EXPORT void abort(void) { __builtin_trap(); }

/* iostream's start-up keeps these; the test never reads or writes them. */
EXPORT FILE *__stdinp, *__stdoutp, *__stderrp;

static int error_number;
EXPORT int *__error(void) { return &error_number; }

/* ---- threads: one thread; Apple's once and key semantics ---- */
static struct _opaque_pthread_t self;
EXPORT pthread_t pthread_self(void) { return &self; }
EXPORT mach_port_t pthread_mach_thread_np(pthread_t thread) { return thread == &self ? 0x103 : 0; }
EXPORT int pthread_once(pthread_once_t *once, void (*init)(void)) {
    if (once->__sig != _PTHREAD_ONCE_SIG_init)
        return EINVAL;
    if (!once->__opaque[0]) {
        once->__opaque[0] = 1;
        init();
    }
    return 0;
}
static const void *key_values[64];
static unsigned key_count;
EXPORT int pthread_key_create(pthread_key_t *key, void (*destructor)(void *)) {
    (void)destructor;
    if (key_count + 1 >= 64)
        return EAGAIN;
    *key = ++key_count;
    return 0;
}
EXPORT void *pthread_getspecific(pthread_key_t key) { return key < 64 ? (void *)key_values[key] : NULL; }
EXPORT int pthread_setspecific(pthread_key_t key, const void *value) {
    if (key >= 64)
        return EINVAL;
    key_values[key] = value;
    return 0;
}
/* A mutex or condition must carry Apple's signature, as the library's static ones do. */
static int checked(long sig) { return sig == _PTHREAD_MUTEX_SIG_init || sig == _PTHREAD_RECURSIVE_MUTEX_SIG_init ? 0 : EINVAL; }
EXPORT int pthread_mutex_lock(pthread_mutex_t *m) { return checked(m->__sig); }
EXPORT int pthread_mutex_unlock(pthread_mutex_t *m) { return checked(m->__sig); }
EXPORT int pthread_mutex_destroy(pthread_mutex_t *m) { (void)m; return 0; }
EXPORT int pthread_cond_broadcast(pthread_cond_t *c) { return c->__sig == _PTHREAD_COND_SIG_init ? 0 : EINVAL; }
EXPORT int pthread_cond_signal(pthread_cond_t *c) { return c->__sig == _PTHREAD_COND_SIG_init ? 0 : EINVAL; }
EXPORT int pthread_cond_destroy(pthread_cond_t *c) { (void)c; return 0; }
EXPORT int pthread_cond_wait(pthread_cond_t *c, pthread_mutex_t *m) { (void)c; (void)m; return EINVAL; }

/* ---- C locale ---- */
EXPORT _RuneLocale _DefaultRuneLocale;
EXPORT int __mb_cur_max = 1;
static struct _xlocale *c_locale = (struct _xlocale *)&_DefaultRuneLocale;
__attribute__((constructor)) static void make_rune_table(void) {
    for (int c = 0; c < 256; c++) {
        unsigned long t = 0;
        if (c < 128) {
            if (c >= '0' && c <= '9')
                t |= _CTYPE_D | _CTYPE_X | _CTYPE_G | _CTYPE_R;
            if ((c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'))
                t |= _CTYPE_X;
            if (c >= 'a' && c <= 'z')
                t |= _CTYPE_L | _CTYPE_A | _CTYPE_G | _CTYPE_R;
            if (c >= 'A' && c <= 'Z')
                t |= _CTYPE_U | _CTYPE_A | _CTYPE_G | _CTYPE_R;
            if (c == ' ' || (c >= '\t' && c <= '\r'))
                t |= _CTYPE_S;
            if (c == ' ' || c == '\t')
                t |= _CTYPE_B;
            if (c == ' ')
                t |= _CTYPE_R;
            if (c < 32 || c == 127)
                t |= _CTYPE_C;
            if (c > 32 && c < 127 && !(t & (_CTYPE_A | _CTYPE_D)))
                t |= _CTYPE_P | _CTYPE_G | _CTYPE_R;
        }
        _DefaultRuneLocale.__runetype[c] = (__uint32_t)t;
        _DefaultRuneLocale.__maplower[c] = c >= 'A' && c <= 'Z' ? c + 32 : c;
        _DefaultRuneLocale.__mapupper[c] = c >= 'a' && c <= 'z' ? c - 32 : c;
    }
}
EXPORT locale_t newlocale(int mask, const char *name, locale_t base) {
    (void)mask;
    (void)base;
    return name && (strcmp(name, "C") == 0 || strcmp(name, "POSIX") == 0 || !name[0]) ? c_locale : NULL;
}
EXPORT int freelocale(locale_t l) { (void)l; return 0; }
EXPORT locale_t uselocale(locale_t l) { (void)l; return c_locale; }
EXPORT int ___mb_cur_max_l(locale_t l) { (void)l; return 1; }
EXPORT int mbtowc_l(wchar_t *out, const char *s, size_t n, locale_t l) {
    (void)l;
    if (!s)
        return 0;
    if (!n)
        return -1;
    if (out)
        *out = (unsigned char)*s;
    return *s != 0;
}
static char dot[] = ".", empty[] = "";
static struct lconv c_conventions = {dot, empty, empty, empty, empty, empty, empty, empty, empty, empty};
EXPORT struct lconv *localeconv_l(locale_t l) { (void)l; return &c_conventions; }
EXPORT struct lconv *localeconv(void) { return &c_conventions; }

/* Integers only: what num_put/num_get ask for in the test. */
EXPORT int vsnprintf_l(char *out, size_t size, locale_t l, const char *format, va_list args) {
    (void)l;
    char text[64];
    size_t n = 0;
    for (const char *f = format; *f; f++) {
        if (*f != '%') {
            if (n + 1 < size)
                out[n] = *f;
            n++;
            continue;
        }
        int plus = 0, longs = 0;
        f++;
        while (*f == '+' || *f == '#')
            plus |= *f++ == '+';
        while (*f == 'l' || *f == 'h' || *f == 'j' || *f == 'z' || *f == 't')
            longs += *f++ == 'l';
        unsigned long long magnitude;
        int negative = 0;
        if (*f == 'd' || *f == 'i') {
            long long v = longs == 2 ? va_arg(args, long long) : longs ? va_arg(args, long) : va_arg(args, int);
            negative = v < 0;
            magnitude = negative ? 0ull - (unsigned long long)v : (unsigned long long)v;
        } else {
            magnitude = longs == 2 ? va_arg(args, unsigned long long) : longs ? va_arg(args, unsigned long) : va_arg(args, unsigned);
        }
        unsigned base = *f == 'x' || *f == 'X' ? 16 : *f == 'o' ? 8 : 10;
        int length = 0;
        do {
            text[length++] = "0123456789abcdef"[magnitude % base];
            magnitude /= base;
        } while (magnitude);
        if (negative || plus)
            text[length++] = negative ? '-' : '+';
        while (length--) {
            if (n + 1 < size)
                out[n] = text[length];
            n++;
        }
    }
    if (size)
        out[n < size ? n : size - 1] = 0;
    return (int)n;
}
EXPORT int snprintf_l(char *out, size_t size, locale_t l, const char *format, ...) {
    va_list args;
    va_start(args, format);
    int n = vsnprintf_l(out, size, l, format, args);
    va_end(args);
    return n;
}
EXPORT long long strtoll_l(const char *s, char **end, int base, locale_t l) {
    (void)l;
    int negative = *s == '-';
    if (*s == '-' || *s == '+')
        s++;
    long long v = 0;
    if (base == 0)
        base = 10;
    while (*s >= '0' && *s <= '9')
        v = v * base + (*s++ - '0');
    if (end)
        *end = (char *)s;
    return negative ? -v : v;
}
EXPORT unsigned long long strtoull_l(const char *s, char **end, int base, locale_t l) {
    return (unsigned long long)strtoll_l(s, end, base, l);
}
EXPORT long strtol_l(const char *s, char **end, int base, locale_t l) { return (long)strtoll_l(s, end, base, l); }

/* No libstdc++ in this process (refstring.h asks). */
EXPORT void *dlopen(const char *path, int mode) { (void)path; (void)mode; return NULL; }
EXPORT void *dlsym(void *handle, const char *name) { (void)handle; (void)name; return NULL; }
