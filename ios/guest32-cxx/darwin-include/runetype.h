/* Apple's rune table (runetype.h): the C table provides _DefaultRuneLocale. */
#ifndef _GB_RUNETYPE_H
#define _GB_RUNETYPE_H
#include <_darwin.h>
#include <stddef.h>

#define _CACHED_RUNES (1 << 8)

typedef struct {
    __darwin_rune_t __min, __max, __map;
    __uint32_t *__types;
} _RuneEntry;
typedef struct {
    int __nranges;
    _RuneEntry *__ranges;
} _RuneRange;
typedef struct {
    char __name[14];
    __uint32_t __mask;
} _RuneCharClass;

/* Offsets on 32-bit: __runetype 52, __maplower 1076, __mapupper 2100. */
typedef struct {
    char __magic[8];
    char __encoding[32];
    __darwin_rune_t (*__sgetrune)(const char *, __darwin_size_t, char const **);
    int (*__sputrune)(__darwin_rune_t, char *, __darwin_size_t, char **);
    __darwin_rune_t __invalid_rune;
    __uint32_t __runetype[_CACHED_RUNES];
    __darwin_rune_t __maplower[_CACHED_RUNES];
    __darwin_rune_t __mapupper[_CACHED_RUNES];
    _RuneRange __runetype_ext;
    _RuneRange __maplower_ext;
    _RuneRange __mapupper_ext;
    void *__variable;
    int __variable_len;
    int __ncharclasses;
    _RuneCharClass *__charclasses;
} _RuneLocale;

#ifdef __cplusplus
extern "C" {
#endif
extern _RuneLocale _DefaultRuneLocale;
#ifdef __cplusplus
}
#endif
#endif
