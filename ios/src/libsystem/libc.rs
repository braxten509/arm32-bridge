//! Apple C library: memory, strings, math and character tables.
//! Functions that behave the same on Apple and Linux/Android point straight
//! at the host's; the rest are adapted here.

use crate::symbols::{leak, Registry};
use crate::varargs::{self, Args};

extern "C" {
    fn acosf(x: f32) -> f32;
    fn asinf(x: f32) -> f32;
    fn atan2f(y: f32, x: f32) -> f32;
    fn atanf(x: f32) -> f32;
    fn cosf(x: f32) -> f32;
    fn exp2f(x: f32) -> f32;
    fn expf(x: f32) -> f32;
    fn fmod(x: f64, y: f64) -> f64;
    fn fmodf(x: f32, y: f32) -> f32;
    fn frexp(x: f64, e: *mut i32) -> f64;
    fn ldexp(x: f64, e: i32) -> f64;
    fn ldexpf(x: f32, e: i32) -> f32;
    fn log10f(x: f32) -> f32;
    fn logf(x: f32) -> f32;
    fn powf(x: f32, y: f32) -> f32;
    fn sinf(x: f32) -> f32;
    fn tanf(x: f32) -> f32;
    fn __cxa_atexit(f: usize, arg: usize, dso: usize) -> i32;
    fn wcscat(a: usize, b: usize) -> usize;
    fn wcschr(a: usize, c: i32) -> usize;
    fn wcscmp(a: usize, b: usize) -> i32;
    fn wcscpy(a: usize, b: usize) -> usize;
    fn wcslen(a: usize) -> usize;
    fn wcsncmp(a: usize, b: usize, n: usize) -> i32;
    fn wcsncpy(a: usize, b: usize, n: usize) -> usize;
    fn wcsrchr(a: usize, c: i32) -> usize;
    fn wcsstr(a: usize, b: usize) -> usize;
    fn wcstod(a: usize, end: usize) -> f64;
    fn wcstoul(a: usize, end: usize, base: i32) -> u64;
    fn wcstoull(a: usize, end: usize, base: i32) -> u64;
    fn wcstol(a: usize, end: usize, base: i32) -> i64;
    fn wcstoll(a: usize, end: usize, base: i32) -> i64;
    fn wcstof(a: usize, end: usize) -> f32;
    fn wcsncat(a: usize, b: usize, n: usize) -> usize;
    fn wcsspn(a: usize, b: usize) -> usize;
    fn wcscspn(a: usize, b: usize) -> usize;
    fn wcspbrk(a: usize, b: usize) -> usize;
    fn wcsnlen(a: usize, n: usize) -> usize;
    fn wmemcpy(a: usize, b: usize, n: usize) -> usize;
    fn wmemset(a: usize, c: u32, n: usize) -> usize;
    fn wmemmove(a: usize, b: usize, n: usize) -> usize;
    fn wmemcmp(a: usize, b: usize, n: usize) -> i32;
}

#[repr(C)]
struct SinCos {
    sin: f32,
    cos: f32,
}

extern "C" fn sincosf_stret(x: f32) -> SinCos {
    SinCos { sin: x.sin(), cos: x.cos() }
}

extern "C" fn memset_pattern16(dest: *mut u8, pattern: *const u8, length: usize) {
    unsafe {
        for i in 0..length {
            *dest.add(i) = *pattern.add(i % 16);
        }
    }
}

/// Whether a `_chk` call would write past the room the compiler knew about.
pub(crate) fn overflows(needed: usize, room: usize) -> bool {
    needed > room
}

/// Apple's __chk_fail_overflow: the game is stopped, as Apple aborts it.
pub(crate) fn stop_overflow(what: &str, needed: usize, room: usize) -> ! {
    crate::log!("STOP: the game overflowed a buffer ({what} needs {needed} bytes, room {room})");
    crate::report::finish(1);
}

extern "C" fn strcat_chk(dest: *mut libc::c_char, src: *const libc::c_char, room: usize) -> *mut libc::c_char {
    unsafe {
        let needed = libc::strlen(dest) + libc::strlen(src) + 1;
        if overflows(needed, room) {
            stop_overflow("strcat", needed, room);
        }
        libc::strcat(dest, src)
    }
}

extern "C" fn stack_chk_fail() -> ! {
    crate::log!("STOP: the game found its stack damaged (stack check failed)");
    crate::report::finish(1);
}

extern "C" fn assert_rtn(function: *const libc::c_char, file: *const libc::c_char, line: i32, expr: *const libc::c_char) -> ! {
    unsafe {
        crate::log!(
            "STOP: game assertion failed: {} in {} ({}:{line})",
            String::from_utf8_lossy(varargs::c_bytes(expr)),
            String::from_utf8_lossy(varargs::c_bytes(function)),
            String::from_utf8_lossy(varargs::c_bytes(file))
        );
    }
    crate::report::finish(1);
}

extern "C" fn cxa_pure_virtual() -> ! {
    crate::log!("STOP: the game called an unfinished C++ method (pure virtual)");
    crate::report::finish(1);
}

extern "C" fn unwind_resume() -> ! {
    crate::log!("STOP: a C++ exception is passing through (not supported yet)");
    crate::report::finish(1);
}

extern "C" fn cxa_atexit(f: usize, arg: usize, dso: usize) -> i32 {
    unsafe { __cxa_atexit(f, arg, dso) }
}

// C++ one-time static set-up guards (64-bit guard word, first byte = done).
static GUARD_HELD: parking::Reentrant = parking::Reentrant::new();

pub mod parking {
    use std::sync::{Condvar, Mutex};
    /// A lock the same thread may take again (C++ guards can nest).
    pub struct Reentrant {
        state: Mutex<(u64, u32)>,
        wake: Condvar,
    }
    impl Reentrant {
        pub const fn new() -> Self {
            Reentrant { state: Mutex::new((0, 0)), wake: Condvar::new() }
        }
        pub fn enter(&self) {
            let me = unsafe { libc::pthread_self() } as u64;
            let mut s = self.state.lock().unwrap();
            while s.1 > 0 && s.0 != me {
                s = self.wake.wait(s).unwrap();
            }
            s.0 = me;
            s.1 += 1;
        }
        pub fn exit(&self) {
            let mut s = self.state.lock().unwrap();
            s.1 -= 1;
            if s.1 == 0 {
                s.0 = 0;
                self.wake.notify_all();
            }
        }
    }
}

fn guard_byte(guard: *mut u8) -> &'static std::sync::atomic::AtomicU8 {
    unsafe { &*(guard as *const std::sync::atomic::AtomicU8) }
}

extern "C" fn cxa_guard_acquire(guard: *mut u8) -> i32 {
    use std::sync::atomic::Ordering;
    if guard_byte(guard).load(Ordering::Acquire) != 0 {
        return 0;
    }
    GUARD_HELD.enter();
    if guard_byte(guard).load(Ordering::Acquire) != 0 {
        GUARD_HELD.exit();
        return 0;
    }
    1
}

extern "C" fn cxa_guard_release(guard: *mut u8) {
    guard_byte(guard).store(1, std::sync::atomic::Ordering::Release);
    GUARD_HELD.exit();
}

extern "C" fn cxa_guard_abort(_guard: *mut u8) {
    GUARD_HELD.exit();
}

// ----- printf family (Apple puts extra arguments in stack slots) -----

macro_rules! variadic {
    ($tramp:ident, $fixed:literal, $target:ident) => {
        std::arch::global_asm!(
            concat!(".globl ", stringify!($tramp)),
            ".p2align 2",
            concat!(stringify!($tramp), ":"),
            concat!("mov x", $fixed, ", sp"),
            concat!("b ", stringify!($target)),
        );
        extern "C" {
            fn $tramp();
        }
    };
}
pub(crate) use variadic;

variadic!(ios_printf, 1, ios_printf_impl);
variadic!(ios_fprintf, 2, ios_fprintf_impl);
variadic!(ios_snprintf, 3, ios_snprintf_impl);
variadic!(ios_sprintf, 2, ios_sprintf_impl);
variadic!(ios_snprintf_chk, 5, ios_snprintf_chk_impl);
variadic!(ios_sprintf_chk, 4, ios_sprintf_chk_impl);
variadic!(ios_wprintf, 1, ios_wprintf_impl);
variadic!(ios_sscanf, 2, ios_sscanf_impl);
variadic!(ios_swscanf, 2, ios_swscanf_impl);

// ----- sscanf / swscanf: every extra argument is a pointer -----

const SCAN_SLOTS: usize = 16;

/// How many pointers a scanf format takes (`%%` and `%*...` take none).
fn scan_targets(format: impl Iterator<Item = u32>) -> usize {
    let mut count = 0;
    let mut chars = format.peekable();
    while let Some(c) = chars.next() {
        if c != '%' as u32 {
            continue;
        }
        match chars.peek() {
            Some(&next) if next == '%' as u32 => {
                chars.next();
            }
            Some(&next) if next == '*' as u32 => {}
            Some(_) => count += 1,
            None => {}
        }
    }
    count
}

unsafe fn scan_pointers(slots: *const u64, needed: usize, what: &str) -> [usize; SCAN_SLOTS] {
    if needed > SCAN_SLOTS {
        crate::log!("STOP: {what} with {needed} targets (up to {SCAN_SLOTS} handled)");
        crate::report::finish(1);
    }
    let mut out = [0usize; SCAN_SLOTS];
    for (i, slot) in out.iter_mut().enumerate().take(needed) {
        *slot = slots.add(i).read_unaligned() as usize;
    }
    out
}

extern "C" {
    #[link_name = "sscanf"]
    fn host_sscanf(text: *const libc::c_char, format: *const libc::c_char, ...) -> i32;
    #[link_name = "swscanf"]
    fn host_swscanf(text: *const i32, format: *const i32, ...) -> i32;
}

#[no_mangle]
unsafe extern "C" fn ios_sscanf_impl(text: *const libc::c_char, format: *const libc::c_char, slots: *const u64) -> i32 {
    let bytes = std::ffi::CStr::from_ptr(format).to_bytes();
    let p = scan_pointers(slots, scan_targets(bytes.iter().map(|&b| b as u32)), "sscanf");
    host_sscanf(text, format, p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15])
}

#[no_mangle]
unsafe extern "C" fn ios_swscanf_impl(text: *const i32, format: *const i32, slots: *const u64) -> i32 {
    let mut length = 0;
    while *format.add(length) != 0 {
        length += 1;
    }
    let wide = std::slice::from_raw_parts(format as *const u32, length);
    let p = scan_pointers(slots, scan_targets(wide.iter().copied()), "swscanf");
    host_swscanf(text, format, p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15])
}

fn host_write(file: *mut libc::FILE, text: &[u8]) {
    unsafe { libc::fwrite(text.as_ptr() as *const _, 1, text.len(), file) };
}

pub fn stdout_file() -> *mut libc::FILE {
    unsafe { *(crate::libsystem::io::STDOUT_CELL.as_ptr()) as *mut libc::FILE }
}

#[no_mangle]
unsafe extern "C" fn ios_printf_impl(fmt: *const libc::c_char, va: *const u64) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    host_write(stdout_file(), &text);
    text.len() as i32
}

/// puts: the text and a newline to standard output, like printf.
unsafe extern "C" fn ios_puts(text: *const libc::c_char) -> i32 {
    let mut bytes = varargs::c_bytes(text).to_vec();
    bytes.push(b'\n');
    host_write(stdout_file(), &bytes);
    bytes.len() as i32
}

#[no_mangle]
unsafe extern "C" fn ios_fprintf_impl(file: *mut libc::FILE, fmt: *const libc::c_char, va: *const u64) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    host_write(file, &text);
    text.len() as i32
}

#[no_mangle]
unsafe extern "C" fn ios_snprintf_impl(buf: *mut libc::c_char, size: usize, fmt: *const libc::c_char, va: *const u64) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    varargs::write_bounded(buf, size, &text)
}

#[no_mangle]
unsafe extern "C" fn ios_sprintf_impl(buf: *mut libc::c_char, fmt: *const libc::c_char, va: *const u64) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    varargs::write_bounded(buf, usize::MAX, &text)
}

#[no_mangle]
unsafe extern "C" fn ios_snprintf_chk_impl(
    buf: *mut libc::c_char,
    size: usize,
    _flag: i32,
    room: usize,
    fmt: *const libc::c_char,
    va: *const u64,
) -> i32 {
    if overflows(size, room) {
        stop_overflow("snprintf", size, room);
    }
    ios_snprintf_impl(buf, size, fmt, va)
}

#[no_mangle]
unsafe extern "C" fn ios_sprintf_chk_impl(
    buf: *mut libc::c_char,
    _flag: i32,
    slen: usize,
    fmt: *const libc::c_char,
    va: *const u64,
) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    if slen <= i32::MAX as usize && overflows(text.len() + 1, slen) {
        stop_overflow("sprintf", text.len() + 1, slen);
    }
    varargs::write_bounded(buf, usize::MAX, &text)
}

#[no_mangle]
unsafe extern "C" fn ios_wprintf_impl(fmt: *const i32, va: *const u64) -> i32 {
    let fmt = varargs::wide_to_utf8(fmt);
    let text = varargs::format(&fmt, &mut Args::new(va), false);
    host_write(stdout_file(), &text);
    text.chars_count()
}

trait CharsCount {
    fn chars_count(&self) -> i32;
}

impl CharsCount for Vec<u8> {
    fn chars_count(&self) -> i32 {
        String::from_utf8_lossy(self).chars().count() as i32
    }
}

unsafe extern "C" fn vsnprintf(buf: *mut libc::c_char, size: usize, fmt: *const libc::c_char, va: *const u64) -> i32 {
    ios_snprintf_impl(buf, size, fmt, va)
}

unsafe extern "C" fn vfprintf(file: *mut libc::FILE, fmt: *const libc::c_char, va: *const u64) -> i32 {
    ios_fprintf_impl(file, fmt, va)
}

// ----- Apple character tables -----

const CT_A: u32 = 0x100;
const CT_C: u32 = 0x200;
const CT_D: u32 = 0x400;
const CT_G: u32 = 0x800;
const CT_L: u32 = 0x1000;
const CT_P: u32 = 0x2000;
const CT_S: u32 = 0x4000;
const CT_U: u32 = 0x8000;
const CT_X: u32 = 0x10000;
const CT_B: u32 = 0x20000;
const CT_R: u32 = 0x40000;
const CT_SW1: u32 = 0x4000_0000;

fn rune_flags(c: u32) -> u32 {
    if c > 127 {
        return 0;
    }
    let ch = c as u8;
    let mut flags = 0;
    if ch.is_ascii_alphabetic() {
        flags |= CT_A;
    }
    if ch.is_ascii_control() {
        flags |= CT_C;
    }
    if ch.is_ascii_digit() {
        flags |= CT_D | (c - u32::from(b'0'));
    }
    if ch.is_ascii_graphic() {
        flags |= CT_G;
    }
    if ch.is_ascii_lowercase() {
        flags |= CT_L;
    }
    if ch.is_ascii_punctuation() {
        flags |= CT_P;
    }
    if matches!(ch, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        flags |= CT_S;
    }
    if ch.is_ascii_uppercase() {
        flags |= CT_U;
    }
    if ch.is_ascii_hexdigit() {
        flags |= CT_X;
        if !ch.is_ascii_digit() {
            flags |= u32::from(ch.to_ascii_lowercase() - b'a' + 10);
        }
    }
    if ch == b' ' || ch == b'\t' {
        flags |= CT_B;
    }
    if (0x20..0x7F).contains(&ch) {
        flags |= CT_R | CT_SW1;
    }
    flags
}

/// Apple's _RuneLocale: runetype table at 60, maplower at 1084, mapupper at 2108.
fn default_rune_locale() -> usize {
    let table = leak([0u8; 4096]) as *mut u8;
    unsafe {
        std::ptr::copy_nonoverlapping(b"RuneMagA".as_ptr(), table, 8);
        std::ptr::copy_nonoverlapping(b"NONE".as_ptr(), table.add(8), 4);
        *(table.add(56) as *mut i32) = 0xFFFD;
        for c in 0..256u32 {
            *(table.add(60 + c as usize * 4) as *mut u32) = rune_flags(c);
            let lower = if c < 128 { (c as u8).to_ascii_lowercase() as u32 } else { c };
            let upper = if c < 128 { (c as u8).to_ascii_uppercase() as u32 } else { c };
            *(table.add(1084 + c as usize * 4) as *mut u32) = lower;
            *(table.add(2108 + c as usize * 4) as *mut u32) = upper;
        }
    }
    table as usize
}

extern "C" fn maskrune(c: i32, mask: u64) -> u64 {
    if (0..128).contains(&c) {
        u64::from(rune_flags(c as u32)) & mask
    } else {
        0
    }
}

extern "C" fn apple_toupper(c: i32) -> i32 {
    if (0..128).contains(&c) {
        (c as u8).to_ascii_uppercase() as i32
    } else {
        c
    }
}

extern "C" fn apple_tolower(c: i32) -> i32 {
    if (0..128).contains(&c) {
        (c as u8).to_ascii_lowercase() as i32
    } else {
        c
    }
}

extern "C" fn arc4random() -> u32 {
    let mut value = 0u32;
    unsafe { libc::getrandom(&mut value as *mut u32 as *mut _, 4, 0) };
    value
}

pub fn register(r: &mut Registry) {
    macro_rules! same {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), libc::$name as usize); )* };
    }
    same!(
        malloc, calloc, realloc, free, memcpy, memmove, memset, memcmp, strlen, strcmp, strncmp,
        strcpy, strncpy, strcat, strchr, strrchr, strdup, strcasecmp, strtod, strtol, strtoull, atoi,
        qsort, abort, exit, getpid, sched_yield, time, difftime, mktime,
        gmtime, gmtime_r, localtime_r, putchar, strerror, strstr, strncasecmp, strtoul, strtoll,
        atof, atol, memchr, posix_memalign, malloc_usable_size, strcspn, strspn, strpbrk
    );
    r.function("rand", crate::libsystem::host::rand as usize);
    r.function("sscanf", ios_sscanf as usize);
    r.function("swscanf", ios_swscanf as usize);
    r.function("srand", crate::libsystem::host::srand as usize);
    r.function("bzero", libc_bzero as usize);
    r.function("acosf", acosf as usize);
    r.function("asinf", asinf as usize);
    r.function("atan2f", atan2f as usize);
    r.function("atanf", atanf as usize);
    r.function("cosf", cosf as usize);
    r.function("exp2f", exp2f as usize);
    r.function("expf", expf as usize);
    r.function("fmod", fmod as usize);
    r.function("fmodf", fmodf as usize);
    r.function("frexp", frexp as usize);
    r.function("ldexp", ldexp as usize);
    r.function("ldexpf", ldexpf as usize);
    r.function("log10f", log10f as usize);
    r.function("logf", logf as usize);
    r.function("powf", powf as usize);
    r.function("sinf", sinf as usize);
    r.function("tanf", tanf as usize);
    r.function("wcscat", wcscat as usize);
    r.function("wcschr", wcschr as usize);
    r.function("wcscmp", wcscmp as usize);
    r.function("wcscpy", wcscpy as usize);
    r.function("wcslen", wcslen as usize);
    r.function("wcsncmp", wcsncmp as usize);
    r.function("wcsncpy", wcsncpy as usize);
    r.function("wcsrchr", wcsrchr as usize);
    r.function("wcsstr", wcsstr as usize);
    r.function("wcstod", wcstod as usize);
    r.function("wcstoul", wcstoul as usize);
    macro_rules! wide {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), $name as usize); )* };
    }
    wide!(wcstoull, wcstol, wcstoll, wcstof, wcsncat, wcsspn, wcscspn, wcspbrk, wcsnlen, wmemcpy, wmemset, wmemmove, wmemcmp);
    // These depend on the character set, so they run in the game's LC_CTYPE.
    macro_rules! in_guest_ctype {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty),* $(,)?) => {
            $(
                extern "C" fn $name($($arg: $ty),*) -> $ret {
                    crate::libsystem::extra::with_guest_ctype(|| unsafe { host::$name($($arg),*) })
                }
                r.function(stringify!($name), $name as usize);
            )*
        };
    }
    mod host {
        extern "C" {
            pub fn towlower(c: u32) -> u32;
            pub fn towupper(c: u32) -> u32;
            pub fn iswspace(c: u32) -> i32;
            pub fn iswalpha(c: u32) -> i32;
            pub fn iswdigit(c: u32) -> i32;
            pub fn iswalnum(c: u32) -> i32;
            pub fn iswupper(c: u32) -> i32;
            pub fn iswlower(c: u32) -> i32;
            pub fn iswpunct(c: u32) -> i32;
            pub fn wcscasecmp(a: usize, b: usize) -> i32;
            pub fn wcsncasecmp(a: usize, b: usize, n: usize) -> i32;
        }
    }
    in_guest_ctype!(
        towlower(c: u32) -> u32, towupper(c: u32) -> u32, iswspace(c: u32) -> i32, iswalpha(c: u32) -> i32,
        iswdigit(c: u32) -> i32, iswalnum(c: u32) -> i32, iswupper(c: u32) -> i32, iswlower(c: u32) -> i32,
        iswpunct(c: u32) -> i32, wcscasecmp(a: usize, b: usize) -> i32, wcsncasecmp(a: usize, b: usize, n: usize) -> i32,
    );
    r.function("__sincosf_stret", sincosf_stret as usize);
    r.function("memset_pattern16", memset_pattern16 as usize);
    r.function("__strcat_chk", strcat_chk as usize);
    r.function("__stack_chk_fail", stack_chk_fail as usize);
    r.function("__assert_rtn", assert_rtn as usize);
    r.function("__cxa_pure_virtual", cxa_pure_virtual as usize);
    r.function("_Unwind_Resume", unwind_resume as usize);
    r.function("__gxx_personality_v0", unwind_resume as usize);
    r.function("__cxa_atexit", cxa_atexit as usize);
    r.function("__cxa_guard_acquire", cxa_guard_acquire as usize);
    r.function("__cxa_guard_release", cxa_guard_release as usize);
    r.function("__cxa_guard_abort", cxa_guard_abort as usize);
    r.function("printf", ios_printf as usize);
    r.function("puts", ios_puts as usize);
    r.function("fprintf", ios_fprintf as usize);
    r.function("snprintf", ios_snprintf as usize);
    r.function("sprintf", ios_sprintf as usize);
    r.function("__snprintf_chk", ios_snprintf_chk as usize);
    r.function("__sprintf_chk", ios_sprintf_chk as usize);
    r.function("wprintf", ios_wprintf as usize);
    r.function("vsnprintf", vsnprintf as usize);
    r.function("vfprintf", vfprintf as usize);
    r.function("__maskrune", maskrune as usize);
    r.function("__toupper", apple_toupper as usize);
    r.function("__tolower", apple_tolower as usize);
    r.function("arc4random", arc4random as usize);
    r.data("_DefaultRuneLocale", default_rune_locale());
    let mut guard = 0u64;
    unsafe { libc::getrandom(&mut guard as *mut u64 as *mut _, 8, 0) };
    r.data("__stack_chk_guard", leak(guard));
}

extern "C" fn libc_bzero(p: *mut u8, n: usize) {
    unsafe { std::ptr::write_bytes(p, 0, n) };
}
