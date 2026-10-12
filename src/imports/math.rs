//! libm. ARM32 Android uses the softfp calling convention: a double arrives
//! in a register pair (or 8-aligned stack slot) and returns in r0:r1; a float
//! arrives in one register and returns in r0.

use crate::handler;
use crate::runtime::{ArgSource, Handler, Ret};

#[allow(non_upper_case_globals)]
mod c {
    extern "C" {
        pub fn sin(x: f64) -> f64;
        pub fn cos(x: f64) -> f64;
        pub fn tan(x: f64) -> f64;
        pub fn asin(x: f64) -> f64;
        pub fn acos(x: f64) -> f64;
        pub fn atan(x: f64) -> f64;
        pub fn sinh(x: f64) -> f64;
        pub fn cosh(x: f64) -> f64;
        pub fn tanh(x: f64) -> f64;
        pub fn asinh(x: f64) -> f64;
        pub fn acosh(x: f64) -> f64;
        pub fn atanh(x: f64) -> f64;
        pub fn exp(x: f64) -> f64;
        pub fn exp2(x: f64) -> f64;
        pub fn expm1(x: f64) -> f64;
        pub fn log(x: f64) -> f64;
        pub fn log10(x: f64) -> f64;
        pub fn log1p(x: f64) -> f64;
        pub fn logb(x: f64) -> f64;
        pub fn cbrt(x: f64) -> f64;
        pub fn ceil(x: f64) -> f64;
        pub fn floor(x: f64) -> f64;
        pub fn sqrt(x: f64) -> f64;
        pub fn rint(x: f64) -> f64;
        pub fn nearbyint(x: f64) -> f64;
        pub fn erf(x: f64) -> f64;
        pub fn erfc(x: f64) -> f64;
        pub fn lgamma(x: f64) -> f64;
        pub fn tgamma(x: f64) -> f64;
        pub fn atan2(y: f64, x: f64) -> f64;
        pub fn pow(x: f64, y: f64) -> f64;
        pub fn fmod(x: f64, y: f64) -> f64;
        pub fn hypot(x: f64, y: f64) -> f64;
        pub fn remainder(x: f64, y: f64) -> f64;
        pub fn nextafter(x: f64, y: f64) -> f64;
        pub fn remquo(x: f64, y: f64, quotient: *mut i32) -> f64;
        pub fn modf(x: f64, integer: *mut f64) -> f64;
        pub fn scalbn(x: f64, exponent: i32) -> f64;
        pub fn lrint(x: f64) -> i64;
        pub fn llrint(x: f64) -> i64;
        pub fn sinf(x: f32) -> f32;
        pub fn cosf(x: f32) -> f32;
        pub fn tanf(x: f32) -> f32;
        pub fn asinf(x: f32) -> f32;
        pub fn acosf(x: f32) -> f32;
        pub fn atanf(x: f32) -> f32;
        pub fn exp2f(x: f32) -> f32;
        pub fn expf(x: f32) -> f32;
        pub fn logf(x: f32) -> f32;
        pub fn sqrtf(x: f32) -> f32;
        pub fn floorf(x: f32) -> f32;
        pub fn ceilf(x: f32) -> f32;
        pub fn atan2f(y: f32, x: f32) -> f32;
        pub fn powf(x: f32, y: f32) -> f32;
        pub fn fmodf(x: f32, y: f32) -> f32;
        pub fn nextafterf(x: f32, y: f32) -> f32;
    }
}

macro_rules! unary_double {
    ($function:path) => {
        handler!(|uc, a| Ret::F64(unsafe { $function(a.f64()) }))
    };
}
macro_rules! binary_double {
    ($function:path) => {
        handler!(|uc, a| {
            let x = a.f64();
            let y = a.f64();
            Ret::F64(unsafe { $function(x, y) })
        })
    };
}
macro_rules! unary_float {
    ($function:path) => {
        handler!(|uc, a| Ret::F32(unsafe { $function(a.f32()) }))
    };
}
macro_rules! binary_float {
    ($function:path) => {
        handler!(|uc, a| {
            let x = a.f32();
            let y = a.f32();
            Ret::F32(unsafe { $function(x, y) })
        })
    };
}

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        "sin" => unary_double!(c::sin),
        "cos" => unary_double!(c::cos),
        "tan" => unary_double!(c::tan),
        "asin" => unary_double!(c::asin),
        "acos" => unary_double!(c::acos),
        "atan" => unary_double!(c::atan),
        "sinh" => unary_double!(c::sinh),
        "cosh" => unary_double!(c::cosh),
        "tanh" => unary_double!(c::tanh),
        "asinh" => unary_double!(c::asinh),
        "acosh" => unary_double!(c::acosh),
        "atanh" => unary_double!(c::atanh),
        "exp" => unary_double!(c::exp),
        "exp2" => unary_double!(c::exp2),
        "expm1" => unary_double!(c::expm1),
        "log" => unary_double!(c::log),
        "log10" => unary_double!(c::log10),
        "log1p" => unary_double!(c::log1p),
        "logb" => unary_double!(c::logb),
        "cbrt" => unary_double!(c::cbrt),
        "ceil" => unary_double!(c::ceil),
        "floor" => unary_double!(c::floor),
        "sqrt" => unary_double!(c::sqrt),
        "rint" => unary_double!(c::rint),
        "nearbyint" => unary_double!(c::nearbyint),
        "erf" => unary_double!(c::erf),
        "erfc" => unary_double!(c::erfc),
        "lgamma" => unary_double!(c::lgamma),
        "tgamma" => unary_double!(c::tgamma),
        "atan2" => binary_double!(c::atan2),
        "pow" => binary_double!(c::pow),
        "fmod" => binary_double!(c::fmod),
        "hypot" => binary_double!(c::hypot),
        "remainder" => binary_double!(c::remainder),
        "nextafter" => binary_double!(c::nextafter),
        "sinf" => unary_float!(c::sinf),
        "cosf" => unary_float!(c::cosf),
        "tanf" => unary_float!(c::tanf),
        "asinf" => unary_float!(c::asinf),
        "acosf" => unary_float!(c::acosf),
        "atanf" => unary_float!(c::atanf),
        "exp2f" => unary_float!(c::exp2f),
        "expf" => unary_float!(c::expf),
        "logf" => unary_float!(c::logf),
        "sqrtf" => unary_float!(c::sqrtf),
        "floorf" => unary_float!(c::floorf),
        "ceilf" => unary_float!(c::ceilf),
        "atan2f" => binary_float!(c::atan2f),
        "powf" => binary_float!(c::powf),
        "fmodf" => binary_float!(c::fmodf),
        "nextafterf" => binary_float!(c::nextafterf),
        "ldexp" | "scalbn" | "scalbnl" => handler!(|uc, a| {
            let x = a.f64();
            let exponent = a.i32();
            Ret::F64(unsafe { c::scalbn(x, exponent) })
        }),
        "modf" => handler!(|uc, a| {
            let x = a.f64();
            let integer_out = a.u32();
            let mut integer = 0.0;
            let fraction = unsafe { c::modf(x, &mut integer) };
            uc.get_data().mem.write_u64(integer_out, integer.to_bits());
            Ret::F64(fraction)
        }),
        "remquo" => handler!(|uc, a| {
            let x = a.f64();
            let y = a.f64();
            let quotient_out = a.u32();
            let mut quotient = 0;
            let result = unsafe { c::remquo(x, y, &mut quotient) };
            uc.get_data().mem.write_u32(quotient_out, quotient as u32);
            Ret::F64(result)
        }),
        // long is 32 bits on ARM32.
        "lrint" => handler!(|uc, a| Ret::U32(unsafe { c::lrint(a.f64()) }.clamp(i32::MIN as i64, i32::MAX as i64) as u32)),
        "llrint" => handler!(|uc, a| Ret::U64(unsafe { c::llrint(a.f64()) } as u64)),
        "isnan" | "__isnan" => handler!(|uc, a| Ret::U32(a.f64().is_nan() as u32)),
        "isnanf" | "__isnanf" => handler!(|uc, a| Ret::U32(a.f32().is_nan() as u32)),
        "__isfinite" | "isfinite" => handler!(|uc, a| Ret::U32(a.f64().is_finite() as u32)),
        "__signbit" => handler!(|uc, a| Ret::U32(a.f64().is_sign_negative() as u32)),
        // bionic: FP_INFINITE 1, FP_NAN 2, FP_NORMAL 4, FP_SUBNORMAL 8, FP_ZERO 16.
        "__fpclassifyd" | "__fpclassify" => handler!(|uc, a| {
            use std::num::FpCategory::*;
            Ret::U32(match a.f64().classify() {
                Infinite => 1,
                Nan => 2,
                Normal => 4,
                Subnormal => 8,
                Zero => 16,
            })
        }),
        _ => None,
    }
}
