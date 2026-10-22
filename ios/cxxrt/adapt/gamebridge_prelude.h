// Game Bridge: included before every file of the C++ runtime port.
//
// The port is Apple's libstdc++ (GCC 4.2.1 based) built for Linux/Android,
// so the objects it shares with a game must have Apple's arm64 layout, not
// the host's. This file loads the host C headers once, with the host's
// multibyte state type renamed out of the way, and then defines the Apple
// types and ctype names the library headers expect.

#ifndef GAMEBRIDGE_PRELUDE_H
#define GAMEBRIDGE_PRELUDE_H

#define mbstate_t __gamebridge_host_mbstate_t
#define __mbstate_t __gamebridge_host___mbstate_t
#define wint_t __gamebridge_host_wint_t
#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>
#include <limits.h>
#include <float.h>
#include <errno.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/ioctl.h>
#include <sys/uio.h>
#include <fcntl.h>
#include <poll.h>
#include <unistd.h>
#include <pthread.h>
#include <sched.h>
#include <time.h>
#include <locale.h>
#include <ctype.h>
#include <wctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <wchar.h>
#include <langinfo.h>
#include <unwind.h>
#undef mbstate_t
#undef __mbstate_t
#undef wint_t

// Apple's wint_t is int (glibc's is unsigned int); it is part of the
// wide-stream functions' names (char_traits<wchar_t>::int_type).
typedef int wint_t;

// glibc declares the const-correct C++ overloads of strchr and friends
// itself; the 2007 <cstring>/<cwchar> must not declare them again.
#ifdef __CORRECT_ISO_CPP_STRING_H_PROTO
#define _GAMEBRIDGE_HOST_STRING_OVERLOADS 1
#endif
#ifdef __CORRECT_ISO_CPP_WCHAR_H_PROTO
#define _GAMEBRIDGE_HOST_WCHAR_OVERLOADS 1
#endif

// Apple's multibyte conversion state: 128 bytes. It sits three times in
// every basic_filebuf and once in every stream position (fpos), so it has
// to be Apple's size. The host functions keep their (smaller) state at the
// start of it.
typedef union {
  char __mbstate8[128];
  long long _mbstateL;
} __mbstate_t;
typedef __mbstate_t mbstate_t;

#ifdef __cplusplus
#define GAMEBRIDGE_HOST_STATE(ps) reinterpret_cast<__gamebridge_host_mbstate_t*>(ps)
#define GAMEBRIDGE_HOST_CSTATE(ps) reinterpret_cast<const __gamebridge_host_mbstate_t*>(ps)
inline int mbsinit(const mbstate_t* ps)
{ return ::mbsinit(GAMEBRIDGE_HOST_CSTATE(ps)); }
inline size_t mbrlen(const char* s, size_t n, mbstate_t* ps)
{ return ::mbrlen(s, n, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t mbrtowc(wchar_t* pwc, const char* s, size_t n, mbstate_t* ps)
{ return ::mbrtowc(pwc, s, n, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t wcrtomb(char* s, wchar_t wc, mbstate_t* ps)
{ return ::wcrtomb(s, wc, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t mbsrtowcs(wchar_t* dst, const char** src, size_t len, mbstate_t* ps)
{ return ::mbsrtowcs(dst, src, len, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t wcsrtombs(char* dst, const wchar_t** src, size_t len, mbstate_t* ps)
{ return ::wcsrtombs(dst, src, len, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t mbsnrtowcs(wchar_t* dst, const char** src, size_t nms, size_t len, mbstate_t* ps)
{ return ::mbsnrtowcs(dst, src, nms, len, GAMEBRIDGE_HOST_STATE(ps)); }
inline size_t wcsnrtombs(char* dst, const wchar_t** src, size_t nwc, size_t len, mbstate_t* ps)
{ return ::wcsnrtombs(dst, src, nwc, len, GAMEBRIDGE_HOST_STATE(ps)); }
#endif

// Apple's character classes (<_ctype.h>), which ctype_base uses as its mask
// values. The answers come from the same table the game's own ctype calls
// use (libsystem's rune flags), so the C++ and C sides always agree.
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

#ifdef __cplusplus
extern "C" {
#endif
unsigned long gamebridge_cxx_maskrune(int c, unsigned long mask);
// fopen with the game's path turned into the host's (as libsystem's fopen).
FILE* gamebridge_cxx_fopen(const char* path, const char* mode);
#ifdef __cplusplus
}
#endif
static inline unsigned long __maskrune(int c, unsigned long mask)
{ return gamebridge_cxx_maskrune(c, mask); }
static inline int __istype(int c, unsigned long mask)
{ return gamebridge_cxx_maskrune(c, mask) != 0; }

#endif
