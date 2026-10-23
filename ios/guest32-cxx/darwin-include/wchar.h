#ifndef _GB_WCHAR_H
#define _GB_WCHAR_H
#include <sys/types.h>
#include <stdarg.h>
#include <stdio.h>
#include <time.h>

typedef __darwin_mbstate_t mbstate_t;
typedef __darwin_wint_t wint_t;
#define WEOF ((wint_t)-1)
#ifndef WCHAR_MIN
#define WCHAR_MIN __WCHAR_MIN__
#define WCHAR_MAX __WCHAR_MAX__
#endif

__BEGIN_DECLS
wint_t btowc(int);
int wctob(wint_t);
wint_t fgetwc(FILE *);
wint_t fputwc(wchar_t, FILE *);
wint_t getwc(FILE *);
wint_t putwc(wchar_t, FILE *);
wint_t ungetwc(wint_t, FILE *);
wint_t getwchar(void);
wint_t putwchar(wchar_t);
wchar_t *fgetws(wchar_t *__restrict, int, FILE *__restrict);
int fputws(const wchar_t *__restrict, FILE *__restrict);
int fwide(FILE *, int);
int mbsinit(const mbstate_t *);
size_t mbrlen(const char *__restrict, size_t, mbstate_t *__restrict);
size_t mbrtowc(wchar_t *__restrict, const char *__restrict, size_t, mbstate_t *__restrict);
size_t mbsrtowcs(wchar_t *__restrict, const char **__restrict, size_t, mbstate_t *__restrict);
size_t wcrtomb(char *__restrict, wchar_t, mbstate_t *__restrict);
size_t wcsrtombs(char *__restrict, const wchar_t **__restrict, size_t, mbstate_t *__restrict);
size_t mbsnrtowcs(wchar_t *__restrict, const char **__restrict, size_t, size_t, mbstate_t *__restrict);
size_t wcsnrtombs(char *__restrict, const wchar_t **__restrict, size_t, size_t, mbstate_t *__restrict);
wchar_t *wcscat(wchar_t *__restrict, const wchar_t *__restrict);
wchar_t *wcschr(const wchar_t *, wchar_t);
int wcscmp(const wchar_t *, const wchar_t *);
int wcscoll(const wchar_t *, const wchar_t *);
wchar_t *wcscpy(wchar_t *__restrict, const wchar_t *__restrict);
size_t wcscspn(const wchar_t *, const wchar_t *);
size_t wcslen(const wchar_t *);
wchar_t *wcsncat(wchar_t *__restrict, const wchar_t *__restrict, size_t);
int wcsncmp(const wchar_t *, const wchar_t *, size_t);
wchar_t *wcsncpy(wchar_t *__restrict, const wchar_t *__restrict, size_t);
wchar_t *wcspbrk(const wchar_t *, const wchar_t *);
wchar_t *wcsrchr(const wchar_t *, wchar_t);
size_t wcsspn(const wchar_t *, const wchar_t *);
wchar_t *wcsstr(const wchar_t *__restrict, const wchar_t *__restrict);
wchar_t *wcstok(wchar_t *__restrict, const wchar_t *__restrict, wchar_t **__restrict);
size_t wcsxfrm(wchar_t *__restrict, const wchar_t *__restrict, size_t);
size_t wcsftime(wchar_t *__restrict, size_t, const wchar_t *__restrict, const struct tm *__restrict);
double wcstod(const wchar_t *__restrict, wchar_t **__restrict);
float wcstof(const wchar_t *__restrict, wchar_t **__restrict);
long double wcstold(const wchar_t *__restrict, wchar_t **__restrict);
long wcstol(const wchar_t *__restrict, wchar_t **__restrict, int);
long long wcstoll(const wchar_t *__restrict, wchar_t **__restrict, int);
unsigned long wcstoul(const wchar_t *__restrict, wchar_t **__restrict, int);
unsigned long long wcstoull(const wchar_t *__restrict, wchar_t **__restrict, int);
wchar_t *wmemchr(const wchar_t *, wchar_t, size_t);
int wmemcmp(const wchar_t *, const wchar_t *, size_t);
wchar_t *wmemcpy(wchar_t *__restrict, const wchar_t *__restrict, size_t);
wchar_t *wmemmove(wchar_t *, const wchar_t *, size_t);
wchar_t *wmemset(wchar_t *, wchar_t, size_t);
int swprintf(wchar_t *__restrict, size_t, const wchar_t *__restrict, ...);
int vswprintf(wchar_t *__restrict, size_t, const wchar_t *__restrict, va_list);
int fwprintf(FILE *__restrict, const wchar_t *__restrict, ...);
int wprintf(const wchar_t *__restrict, ...);
int vfwprintf(FILE *__restrict, const wchar_t *__restrict, va_list);
int vwprintf(const wchar_t *__restrict, va_list);
int swscanf(const wchar_t *__restrict, const wchar_t *__restrict, ...);
int fwscanf(FILE *__restrict, const wchar_t *__restrict, ...);
int wscanf(const wchar_t *__restrict, ...);
int vfwscanf(FILE *__restrict, const wchar_t *__restrict, va_list);
int vswscanf(const wchar_t *__restrict, const wchar_t *__restrict, va_list);
int vwscanf(const wchar_t *__restrict, va_list);
int wcscoll_l(const wchar_t *, const wchar_t *, locale_t);
size_t wcsxfrm_l(wchar_t *, const wchar_t *, size_t, locale_t);
__END_DECLS
#endif
