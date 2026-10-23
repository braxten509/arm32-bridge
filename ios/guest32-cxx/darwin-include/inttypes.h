/* armv7 Darwin: int32_t is int, intptr_t long, int64_t long long. */
#ifndef _GB_INTTYPES_H
#define _GB_INTTYPES_H
#include <stdint.h>
#include <_darwin.h>

#define PRId8 "hhd"
#define PRId16 "hd"
#define PRId32 "d"
#define PRId64 "lld"
#define PRIi32 "i"
#define PRIi64 "lli"
#define PRIu8 "hhu"
#define PRIu16 "hu"
#define PRIu32 "u"
#define PRIu64 "llu"
#define PRIx32 "x"
#define PRIx64 "llx"
#define PRIX32 "X"
#define PRIX64 "llX"
#define PRIdPTR "ld"
#define PRIuPTR "lu"
#define PRIxPTR "lx"
#define PRIXPTR "lX"
#define PRIdMAX "jd"
#define PRIuMAX "ju"
#define SCNd32 "d"
#define SCNd64 "lld"
#define SCNu32 "u"
#define SCNu64 "llu"

typedef struct { intmax_t quot, rem; } imaxdiv_t;
__BEGIN_DECLS
intmax_t imaxabs(intmax_t);
imaxdiv_t imaxdiv(intmax_t, intmax_t);
intmax_t strtoimax(const char *__restrict, char **__restrict, int);
uintmax_t strtoumax(const char *__restrict, char **__restrict, int);
__END_DECLS
#endif
