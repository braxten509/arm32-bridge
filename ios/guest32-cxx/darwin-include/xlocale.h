/* Apple's per-locale functions (xlocale.h and xlocale/_*.h). */
#ifndef _GB_XLOCALE_H
#define _GB_XLOCALE_H
#include <locale.h>
#include <stdarg.h>
#include <stdio.h>
#include <wchar.h>

#define LC_COLLATE_MASK (1 << 0)
#define LC_CTYPE_MASK (1 << 1)
#define LC_MESSAGES_MASK (1 << 2)
#define LC_MONETARY_MASK (1 << 3)
#define LC_NUMERIC_MASK (1 << 4)
#define LC_TIME_MASK (1 << 5)
#define LC_ALL_MASK (LC_COLLATE_MASK | LC_CTYPE_MASK | LC_MESSAGES_MASK | LC_MONETARY_MASK | LC_NUMERIC_MASK | LC_TIME_MASK)
#define LC_GLOBAL_LOCALE ((locale_t)-1)

__BEGIN_DECLS
locale_t newlocale(int, const char *, locale_t);
locale_t duplocale(locale_t);
int freelocale(locale_t);
locale_t uselocale(locale_t);
const char *querylocale(int, locale_t);
struct lconv *localeconv_l(locale_t);
int ___mb_cur_max_l(locale_t);
#define MB_CUR_MAX_L(x) ((size_t)___mb_cur_max_l(x))

int isalnum_l(int, locale_t);
int isalpha_l(int, locale_t);
int isblank_l(int, locale_t);
int iscntrl_l(int, locale_t);
int isdigit_l(int, locale_t);
int isgraph_l(int, locale_t);
int islower_l(int, locale_t);
int isprint_l(int, locale_t);
int ispunct_l(int, locale_t);
int isspace_l(int, locale_t);
int isupper_l(int, locale_t);
int isxdigit_l(int, locale_t);
int tolower_l(int, locale_t);
int toupper_l(int, locale_t);

int iswalnum_l(wint_t, locale_t);
int iswalpha_l(wint_t, locale_t);
int iswblank_l(wint_t, locale_t);
int iswcntrl_l(wint_t, locale_t);
int iswdigit_l(wint_t, locale_t);
int iswgraph_l(wint_t, locale_t);
int iswlower_l(wint_t, locale_t);
int iswprint_l(wint_t, locale_t);
int iswpunct_l(wint_t, locale_t);
int iswspace_l(wint_t, locale_t);
int iswupper_l(wint_t, locale_t);
int iswxdigit_l(wint_t, locale_t);
int iswctype_l(wint_t, unsigned long, locale_t);
wint_t towlower_l(wint_t, locale_t);
wint_t towupper_l(wint_t, locale_t);
unsigned long wctype_l(const char *, locale_t);

wint_t btowc_l(int, locale_t);
int wctob_l(wint_t, locale_t);
size_t mbrlen_l(const char *__restrict, size_t, mbstate_t *__restrict, locale_t);
size_t mbrtowc_l(wchar_t *__restrict, const char *__restrict, size_t, mbstate_t *__restrict, locale_t);
int mbtowc_l(wchar_t *__restrict, const char *__restrict, size_t, locale_t);
size_t mbsrtowcs_l(wchar_t *__restrict, const char **__restrict, size_t, mbstate_t *__restrict, locale_t);
size_t mbsnrtowcs_l(wchar_t *__restrict, const char **__restrict, size_t, size_t, mbstate_t *__restrict, locale_t);
size_t wcrtomb_l(char *__restrict, wchar_t, mbstate_t *__restrict, locale_t);
size_t wcsnrtombs_l(char *__restrict, const wchar_t **__restrict, size_t, size_t, mbstate_t *__restrict, locale_t);
size_t wcsrtombs_l(char *__restrict, const wchar_t **__restrict, size_t, mbstate_t *__restrict, locale_t);

int snprintf_l(char *__restrict, size_t, locale_t, const char *__restrict, ...);
int asprintf_l(char **__restrict, locale_t, const char *__restrict, ...);
int sscanf_l(const char *__restrict, locale_t, const char *__restrict, ...);
int vsnprintf_l(char *__restrict, size_t, locale_t, const char *__restrict, va_list);
int vasprintf_l(char **__restrict, locale_t, const char *__restrict, va_list);

double strtod_l(const char *__restrict, char **__restrict, locale_t);
float strtof_l(const char *__restrict, char **__restrict, locale_t);
long double strtold_l(const char *__restrict, char **__restrict, locale_t);
long strtol_l(const char *__restrict, char **__restrict, int, locale_t);
long long strtoll_l(const char *__restrict, char **__restrict, int, locale_t);
unsigned long strtoul_l(const char *__restrict, char **__restrict, int, locale_t);
unsigned long long strtoull_l(const char *__restrict, char **__restrict, int, locale_t);
__END_DECLS
#endif
