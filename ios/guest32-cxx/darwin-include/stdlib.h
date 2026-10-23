#ifndef _GB_STDLIB_H
#define _GB_STDLIB_H
#include <sys/types.h>

typedef struct { int quot, rem; } div_t;
typedef struct { long quot, rem; } ldiv_t;
typedef struct { long long quot, rem; } lldiv_t;

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 0x7fffffff

__BEGIN_DECLS
extern int __mb_cur_max;
#define MB_CUR_MAX ((size_t)__mb_cur_max)

void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
int posix_memalign(void **, size_t, size_t);
void *valloc(size_t);
void abort(void) __attribute__((__noreturn__));
void exit(int) __attribute__((__noreturn__));
void _Exit(int) __attribute__((__noreturn__));
int atexit(void (*)(void));
char *getenv(const char *);
int system(const char *);
int abs(int) __attribute__((__const__));
long labs(long) __attribute__((__const__));
long long llabs(long long);
div_t div(int, int);
ldiv_t ldiv(long, long);
lldiv_t lldiv(long long, long long);
int atoi(const char *);
long atol(const char *);
long long atoll(const char *);
double atof(const char *);
double strtod(const char *__restrict, char **__restrict);
float strtof(const char *__restrict, char **__restrict);
long double strtold(const char *__restrict, char **__restrict);
long strtol(const char *__restrict, char **__restrict, int);
long long strtoll(const char *__restrict, char **__restrict, int);
unsigned long strtoul(const char *__restrict, char **__restrict, int);
unsigned long long strtoull(const char *__restrict, char **__restrict, int);
int rand(void);
void srand(unsigned);
__uint32_t arc4random(void);
void arc4random_buf(void *, size_t);
__uint32_t arc4random_uniform(__uint32_t);
void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
void *bsearch(const void *, const void *, size_t, size_t, int (*)(const void *, const void *));
int mblen(const char *, size_t);
size_t mbstowcs(wchar_t *__restrict, const char *__restrict, size_t);
int mbtowc(wchar_t *__restrict, const char *__restrict, size_t);
size_t wcstombs(char *__restrict, const wchar_t *__restrict, size_t);
int wctomb(char *, wchar_t);
__END_DECLS
#endif
