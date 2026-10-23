/* Apple's math.h, armv7: long double is double; the classification macros
 * use the compiler's builtins so no __fpclassify* calls are needed. */
#ifndef _GB_MATH_H
#define _GB_MATH_H
#include <_darwin.h>

typedef float float_t;
typedef double double_t;

#define HUGE_VAL __builtin_huge_val()
#define HUGE_VALF __builtin_huge_valf()
#define HUGE_VALL __builtin_huge_vall()
#define INFINITY __builtin_inff()
#define NAN __builtin_nanf("")
#define FP_NAN 1
#define FP_INFINITE 2
#define FP_ZERO 3
#define FP_NORMAL 4
#define FP_SUBNORMAL 5
#define FP_ILOGB0 (-2147483647 - 1)
#define FP_ILOGBNAN (-2147483647 - 1)
#define MATH_ERRNO 1
#define MATH_ERREXCEPT 2
#define math_errhandling 0

#define fpclassify(x) __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, FP_ZERO, x)
#define isfinite(x) __builtin_isfinite(x)
#define isinf(x) __builtin_isinf(x)
#define isnan(x) __builtin_isnan(x)
#define isnormal(x) __builtin_isnormal(x)
#define signbit(x) __builtin_signbit(x)
#define isgreater(x, y) __builtin_isgreater(x, y)
#define isgreaterequal(x, y) __builtin_isgreaterequal(x, y)
#define isless(x, y) __builtin_isless(x, y)
#define islessequal(x, y) __builtin_islessequal(x, y)
#define islessgreater(x, y) __builtin_islessgreater(x, y)
#define isunordered(x, y) __builtin_isunordered(x, y)

__BEGIN_DECLS
#define _GB_MATH3(name) float name##f(float); double name(double); long double name##l(long double);
#define _GB_MATH3_2(name) float name##f(float, float); double name(double, double); long double name##l(long double, long double);
_GB_MATH3(acos) _GB_MATH3(asin) _GB_MATH3(atan) _GB_MATH3_2(atan2) _GB_MATH3(cos) _GB_MATH3(sin) _GB_MATH3(tan)
_GB_MATH3(acosh) _GB_MATH3(asinh) _GB_MATH3(atanh) _GB_MATH3(cosh) _GB_MATH3(sinh) _GB_MATH3(tanh)
_GB_MATH3(exp) _GB_MATH3(exp2) _GB_MATH3(expm1) _GB_MATH3(log) _GB_MATH3(log10) _GB_MATH3(log2) _GB_MATH3(log1p)
_GB_MATH3(logb) _GB_MATH3(cbrt) _GB_MATH3(sqrt) _GB_MATH3(erf) _GB_MATH3(erfc) _GB_MATH3(lgamma) _GB_MATH3(tgamma)
_GB_MATH3(ceil) _GB_MATH3(floor) _GB_MATH3(nearbyint) _GB_MATH3(rint) _GB_MATH3(round) _GB_MATH3(trunc) _GB_MATH3(fabs)
_GB_MATH3_2(fmod) _GB_MATH3_2(remainder) _GB_MATH3_2(copysign) _GB_MATH3_2(nextafter) _GB_MATH3_2(fdim)
_GB_MATH3_2(fmax) _GB_MATH3_2(fmin) _GB_MATH3_2(pow) _GB_MATH3_2(hypot)
float frexpf(float, int *); double frexp(double, int *); long double frexpl(long double, int *);
float ldexpf(float, int); double ldexp(double, int); long double ldexpl(long double, int);
float modff(float, float *); double modf(double, double *); long double modfl(long double, long double *);
float scalbnf(float, int); double scalbn(double, int); long double scalbnl(long double, int);
float scalblnf(float, long); double scalbln(double, long); long double scalblnl(long double, long);
int ilogbf(float); int ilogb(double); int ilogbl(long double);
long lrintf(float); long lrint(double); long lrintl(long double);
long long llrintf(float); long long llrint(double); long long llrintl(long double);
long lroundf(float); long lround(double); long lroundl(long double);
long long llroundf(float); long long llround(double); long long llroundl(long double);
float remquof(float, float, int *); double remquo(double, double, int *); long double remquol(long double, long double, int *);
float fmaf(float, float, float); double fma(double, double, double); long double fmal(long double, long double, long double);
float nanf(const char *); double nan(const char *); long double nanl(const char *);
float nexttowardf(float, long double); double nexttoward(double, long double); long double nexttowardl(long double, long double);
#undef _GB_MATH3
#undef _GB_MATH3_2
__END_DECLS
#endif
