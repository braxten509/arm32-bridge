/* Apple's character classes (_ctype.h). The is* functions are plain calls
 * into the C table here; Apple's inline versions read _DefaultRuneLocale. */
#ifndef _GB_CTYPE_H
#define _GB_CTYPE_H
#include <runetype.h>
#include <xlocale.h>

#define _CTYPE_A 0x00000100L
#define _CTYPE_C 0x00000200L
#define _CTYPE_D 0x00000400L
#define _CTYPE_G 0x00000800L
#define _CTYPE_L 0x00001000L
#define _CTYPE_P 0x00002000L
#define _CTYPE_S 0x00004000L
#define _CTYPE_U 0x00008000L
#define _CTYPE_X 0x00010000L
#define _CTYPE_B 0x00020000L
#define _CTYPE_R 0x00040000L
#define _CTYPE_I 0x00080000L
#define _CTYPE_T 0x00100000L
#define _CTYPE_Q 0x00200000L
#define _CTYPE_SW0 0x20000000L
#define _CTYPE_SW1 0x40000000L
#define _CTYPE_SW2 0x80000000L
#define _CTYPE_SW3 0xc0000000L
#define _CTYPE_SWM 0xe0000000L
#define _CTYPE_SWS 30

__BEGIN_DECLS
int isalnum(int);
int isalpha(int);
int isblank(int);
int iscntrl(int);
int isdigit(int);
int isgraph(int);
int islower(int);
int isprint(int);
int ispunct(int);
int isspace(int);
int isupper(int);
int isxdigit(int);
int tolower(int);
int toupper(int);
int isascii(int);
int __maskrune(__darwin_ct_rune_t, unsigned long);
__END_DECLS

/* Apple's inline class tests, which GCC's <bits/ctype_inline.h> uses:
 * the table below 128, __maskrune above (and for negative chars). */
static __inline int __istype(__darwin_ct_rune_t _c, unsigned long _f) {
    return (_c & ~0x7F) == 0 ? !!(_DefaultRuneLocale.__runetype[_c] & _f) : !!__maskrune(_c, _f);
}
static __inline __darwin_ct_rune_t __isctype(__darwin_ct_rune_t _c, unsigned long _f) {
    return (_c < 0 || _c >= _CACHED_RUNES) ? 0 : !!(_DefaultRuneLocale.__runetype[_c] & _f);
}
#endif
