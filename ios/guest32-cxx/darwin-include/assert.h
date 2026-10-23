/* Not include-guarded, like the C standard's assert.h. */
#include <_darwin.h>
#undef assert
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
__BEGIN_DECLS
void __assert_rtn(const char *, const char *, int, const char *) __attribute__((__noreturn__));
__END_DECLS
#define assert(e) (__builtin_expect(!(e), 0) ? __assert_rtn(__func__, __FILE__, __LINE__, #e) : (void)0)
#endif
#ifndef __cplusplus
#define static_assert _Static_assert
#endif
