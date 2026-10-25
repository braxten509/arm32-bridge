//! libSystem for 32-bit (armv7) iPhone programs: libc, libm, pthread,
//! compiler helpers, blocks, OSAtomic, a little of mach and dyld.
//!
//! Where Darwin and bionic agree on the call (opaque FILE*, strings, most of
//! stdio, math, time with 32-bit time_t, struct tm, jmp_buf fits) the
//! translator's Android handlers are reused as they are. Darwin versions
//! where they differ: `struct stat` (the 64-bit-inode layout, 108 bytes),
//! errno numbers (translated by the platform hook) and `__error()`, open
//! flags, 64-bit off_t, ctype through `_DefaultRuneLocale`, pthread objects
//! (a signature word first, static initializers), stdio's `__stderrp`,
//! sysctl/sysconf numbers, `__sincos_stret`, compiler-rt helpers.
//! 64-bit arguments follow Apple's packed rule (runtime::packed_pairs).

use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use arm32_bridge::handler;
use arm32_bridge::imports::{self, HostFile};
use arm32_bridge::ios32::{self, darwin_errno};
use arm32_bridge::runtime::{self, call_guest, mem, shared, stop_with_fault, ArgSource, Args, GuestCpu, Handler, Ret, Uc, VaList};

use super::function;

/// Android handlers whose call matches Darwin's.
const REUSED: &[&str] = &[
    // memory and strings
    "malloc", "calloc", "realloc", "free", "posix_memalign", "memcpy", "memmove", "memset", "__memcpy_chk",
    "__memmove_chk", "__memset_chk", "memcmp", "memchr", "bzero", "strlen", "strnlen", "strcmp", "strncmp",
    "strcasecmp", "strncasecmp", "strchr", "strrchr", "strstr", "strpbrk", "strspn", "strcspn", "strcpy",
    "__strcpy_chk", "strncpy", "strcat", "strncat", "strlcpy", "strdup", "strtok", "strtok_r", "strtol", "strtoul",
    "strtoll", "strtoull", "strtod", "strtof", "atoi", "atol", "atoll", "atof", "abs", "labs", "llabs", "tolower",
    "toupper", "isspace", "isupper", "isdigit", "isxdigit", "isalpha", "isalnum", "isascii", "qsort", "bsearch",
    // formatted text and stdio (FILE* is opaque to these programs)
    "sprintf", "snprintf", "vsprintf", "vsnprintf", "sscanf", "vsscanf", "printf", "vprintf", "fprintf", "vfprintf",
    "fscanf", "fopen", "fdopen", "fclose", "fread", "fwrite", "fputs", "fputc", "putc", "putchar", "puts", "feof",
    "ferror", "fileno", "clearerr", "fgets", "getc", "fgetc", "ungetc", "fseek", "ftell", "rewind", "fflush",
    "setvbuf", "setbuf",
    // files
    "read", "write", "close", "unlink", "remove", "rename", "mkdir", "rmdir", "access", "chdir", "getcwd", "isatty",
    // time
    "time", "gettimeofday", "clock", "localtime", "localtime_r", "gmtime", "gmtime_r", "mktime", "difftime",
    "strftime", "nanosleep", "usleep", "sleep", "sched_yield",
    // process
    "getpid", "getppid", "getuid", "geteuid", "getgid", "getegid", "getenv", "setenv", "_exit", "abort",
    "__stack_chk_fail", "setjmp", "_setjmp", "longjmp", "_longjmp", "sigsetjmp", "siglongjmp", "arc4random_buf",
    "gethostname", "getpagesize", "mkstemp", "syslog",
    // struct in_addr by value: one word on both
    "inet_ntoa",
    // math
    "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "exp2", "log", "log10", "log2",
    "log1p", "expm1", "cbrt", "ceil", "floor", "sqrt", "rint", "atan2", "pow", "fmod", "hypot", "round", "trunc",
    "ldexp", "frexp", "modf", "sinf", "cosf", "tanf", "asinf", "acosf", "atanf", "exp2f", "expf", "logf", "log10f",
    "log2f", "sqrtf", "floorf", "ceilf", "atan2f", "powf", "fmodf", "roundf", "truncf", "fmaxf", "fminf",
    "ldexpf", "modff", "lrint", "lrintf", "sinhf", "coshf", "tanhf", "cbrtf",
    // threads with the same objects
    "pthread_self", "pthread_equal", "pthread_join", "pthread_detach", "pthread_exit", "pthread_key_create",
    "pthread_key_delete", "pthread_getspecific", "pthread_setspecific",
];

/// (Darwin name, Android handler with the same call).
const RENAMED: &[(&str, &str)] = &[("malloc_size", "malloc_usable_size"), ("__tolower", "tolower"), ("__toupper", "toupper")];

pub fn register() {
    for &name in REUSED {
        function(name, imports::handler_for(name).unwrap_or_else(|| panic!("no Android handler for {name}")));
    }
    for &(name, android) in RENAMED {
        function(name, imports::handler_for(android).unwrap_or_else(|| panic!("no Android handler for {android}")));
    }
    register_darwin();
}

fn register_darwin() {
    files();
    text();
    numbers();
    process();
    threads();
    atomics();
    data();
}

/// A cached Android handler.
macro_rules! bionic {
    ($name:literal) => {{
        static HANDLER: OnceLock<Handler> = OnceLock::new();
        *HANDLER.get_or_init(|| imports::handler_for($name).expect($name))
    }};
}

/// A loaded guest library's export (the C++ runtime's `__cxa_atexit`, ...).
fn guest_export(name: &str) -> Option<u32> {
    shared().guest_exports.get(name).copied()
}

fn host_c(guest: u32) -> *mut libc::c_char {
    mem().host_or_null(guest) as *mut libc::c_char
}

fn host_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets the guest's errno (a Darwin number) from the host's.
fn set_errno_from_host(uc: &Uc) {
    mem().write_u32(uc.get_data().errno_slot, darwin_errno(host_errno()) as u32);
}

/// -1 and errno when `result` is negative.
fn checked(uc: &Uc, result: i64) -> Ret {
    if result < 0 {
        set_errno_from_host(uc);
    }
    Ret::U32(result as i32 as u32)
}

fn checked64(uc: &Uc, result: i64) -> Ret {
    if result < 0 {
        set_errno_from_host(uc);
    }
    Ret::U64(result as u64)
}

/// A pthread-style result code (0 or an errno number) in Darwin's numbering.
fn error_code(result: Ret) -> Ret {
    match result {
        Ret::U32(code) if code != 0 => Ret::U32(darwin_errno(code as i32) as u32),
        other => other,
    }
}

// ---- files ----

/// Size of Darwin armv7's `struct stat` (64-bit inode layout; long long is 4-aligned).
pub const STAT_SIZE: u32 = 108;

/// Writes a host `struct stat` as Darwin armv7's: dev 0, mode 4 (u16), nlink 6
/// (u16), ino 8 (u64), uid 16, gid 20, rdev 24, atime 28, mtime 36, ctime 44,
/// birthtime 52 (sec, nsec each), size 60 (u64), blocks 68 (u64), blksize 76,
/// flags 80, gen 84, lspare 88, qspare 92..108.
pub fn write_stat(guest: u32, st: &libc::stat) {
    let m = mem();
    unsafe { std::ptr::write_bytes(m.host(guest), 0, STAT_SIZE as usize) };
    m.write_u32(guest, st.st_dev as u32);
    m.write_u16(guest + 4, st.st_mode as u16);
    m.write_u16(guest + 6, st.st_nlink as u16);
    m.write_u64(guest + 8, st.st_ino as u64);
    m.write_u32(guest + 16, st.st_uid);
    m.write_u32(guest + 20, st.st_gid);
    m.write_u32(guest + 24, st.st_rdev as u32);
    let times = [
        (st.st_atime, st.st_atime_nsec),
        (st.st_mtime, st.st_mtime_nsec),
        (st.st_ctime, st.st_ctime_nsec),
        (st.st_ctime, st.st_ctime_nsec),
    ];
    for (i, (seconds, nanoseconds)) in times.iter().enumerate() {
        m.write_u32(guest + 28 + i as u32 * 8, *seconds as u32);
        m.write_u32(guest + 32 + i as u32 * 8, *nanoseconds as u32);
    }
    m.write_u64(guest + 60, st.st_size as u64);
    m.write_u64(guest + 68, st.st_blocks as u64);
    m.write_u32(guest + 76, st.st_blksize as u32);
}

/// Darwin open() flags to the host's.
fn host_open_flags(darwin: u32) -> i32 {
    const FLAGS: [(u32, i32); 9] = [
        (0x4, libc::O_NONBLOCK),
        (0x8, libc::O_APPEND),
        (0x80, libc::O_SYNC),
        (0x100, libc::O_NOFOLLOW),
        (0x200, libc::O_CREAT),
        (0x400, libc::O_TRUNC),
        (0x800, libc::O_EXCL),
        (0x10_0000, libc::O_DIRECTORY),
        (0x100_0000, libc::O_CLOEXEC),
    ];
    let mut host = (darwin & 3) as i32;
    for (apple, linux) in FLAGS {
        if darwin & apple != 0 {
            host |= linux;
        }
    }
    host
}

fn stat_into(uc: &Uc, result: i32, st: &libc::stat, out: u32) -> Ret {
    if result == 0 {
        write_stat(out, st);
    }
    checked(uc, result as i64)
}

fn files() {
    function("__error", handler!(|uc, a| Ret::U32(uc.get_data().errno_slot)).unwrap());
    function(
        "stat",
        handler!(|uc, a| {
            let (path, out) = (a.u32(), a.u32());
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::stat(host_c(path), &mut st) };
            stat_into(uc, result, &st, out)
        })
        .unwrap(),
    );
    function(
        "lstat",
        handler!(|uc, a| {
            let (path, out) = (a.u32(), a.u32());
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::lstat(host_c(path), &mut st) };
            stat_into(uc, result, &st, out)
        })
        .unwrap(),
    );
    function(
        "fstat",
        handler!(|uc, a| {
            let (fd, out) = (a.i32(), a.u32());
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::fstat(fd, &mut st) };
            stat_into(uc, result, &st, out)
        })
        .unwrap(),
    );
    function(
        "open",
        handler!(|uc, a| {
            let (path, flags, mode) = (a.u32(), a.u32(), a.u32());
            checked(uc, unsafe { libc::open(host_c(path), host_open_flags(flags), mode) } as i64)
        })
        .unwrap(),
    );
    // off_t is 64 bits on Darwin: lseek(int, off_t, int) -> off_t.
    function(
        "lseek",
        handler!(|uc, a| {
            let (fd, offset, whence) = (a.i32(), a.i64(), a.i32());
            checked64(uc, unsafe { libc::lseek(fd, offset, whence) })
        })
        .unwrap(),
    );
    function(
        "ftruncate",
        handler!(|uc, a| {
            let (fd, length) = (a.i32(), a.i64());
            checked(uc, unsafe { libc::ftruncate(fd, length) } as i64)
        })
        .unwrap(),
    );
    function(
        "pread",
        handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i64());
            checked(uc, unsafe { libc::pread(fd, mem().host(buffer).cast(), length as usize, offset) } as i64)
        })
        .unwrap(),
    );
    function(
        "pwrite",
        handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i64());
            checked(uc, unsafe { libc::pwrite(fd, mem().host(buffer).cast(), length as usize, offset) } as i64)
        })
        .unwrap(),
    );
    function(
        "fseeko",
        handler!(|uc, a| {
            let (file, offset, whence) = (a.u32(), a.i64(), a.i32());
            match shared().state.files.get(&file).copied() {
                Some(HostFile::Stream(stream)) => checked(uc, unsafe { libc::fseeko(stream as *mut libc::FILE, offset, whence) } as i64),
                _ => Ret::U32(u32::MAX),
            }
        })
        .unwrap(),
    );
    function(
        "ftello",
        handler!(|uc, a| {
            let file = a.u32();
            match shared().state.files.get(&file).copied() {
                Some(HostFile::Stream(stream)) => checked64(uc, unsafe { libc::ftello(stream as *mut libc::FILE) }),
                _ => Ret::U64(u64::MAX),
            }
        })
        .unwrap(),
    );
    // Backup flags (com.apple.MobileBackup) and other extended attributes: accepted, not kept.
    function("setxattr", handler!(|uc, a| Ret::U32(0)).unwrap());
    function("removexattr", handler!(|uc, a| Ret::U32(0)).unwrap());
    function(
        "getxattr",
        handler!(|uc, a| {
            const ENOATTR: u32 = 93;
            mem().write_u32(uc.get_data().errno_slot, ENOATTR);
            Ret::U32(u32::MAX)
        })
        .unwrap(),
    );
}

// ---- formatted text, errors ----

fn write_text(buffer: u32, size: Option<u32>, text: &[u8]) -> Ret {
    let copy = match size {
        Some(0) => return Ret::U32(text.len() as u32),
        Some(size) => text.len().min(size as usize - 1),
        None => text.len(),
    };
    mem().write_bytes(buffer, &text[..copy]);
    mem().write_u8(buffer + copy as u32, 0);
    Ret::U32(text.len() as u32)
}

/// The _chk printf calls, as Apple's Libc checks them against the object
/// size the compiler knew (-1: unknown): a max length larger than the
/// object, or text that doesn't fit it, ends the program (__chk_fail).
fn write_checked_text(uc: &mut Uc, buffer: u32, size: Option<u32>, object_size: u32, text: &[u8]) -> Ret {
    let overflows = match size {
        Some(size) => size > object_size,
        None => text.len() as u64 + 1 > u64::from(object_size),
    };
    if object_size != u32::MAX && overflows {
        stop_with_fault(uc, format!("buffer overflow detected ({}: object of {object_size} bytes)", uc.get_data().current_call));
        return Ret::Void;
    }
    write_text(buffer, size, text)
}

/// Darwin's text for a Darwin errno number.
fn darwin_strerror(darwin: i32) -> String {
    let linux = (1..134).find(|&linux| darwin_errno(linux) == darwin).unwrap_or(darwin);
    let text = std::io::Error::from_raw_os_error(linux).to_string();
    text.split(" (os error").next().unwrap_or("").to_string()
}

fn text() {
    // __sprintf_chk(buffer, flags, buffer size, format, ...)
    function(
        "__sprintf_chk",
        handler!(|uc, a| {
            let (buffer, _flags, object_size, format) = (a.u32(), a.u32(), a.u32(), a.u32());
            let text = arm32_bridge::imports::printf::format(mem(), format, a);
            write_checked_text(uc, buffer, None, object_size, &text)
        })
        .unwrap(),
    );
    // __snprintf_chk(buffer, max length, flags, buffer size, format, ...)
    function(
        "__snprintf_chk",
        handler!(|uc, a| {
            let (buffer, size, _flags, object_size, format) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let text = arm32_bridge::imports::printf::format(mem(), format, a);
            write_checked_text(uc, buffer, Some(size), object_size, &text)
        })
        .unwrap(),
    );
    function(
        "__vsprintf_chk",
        handler!(|uc, a| {
            let (buffer, _flags, object_size, format, list) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let text = arm32_bridge::imports::printf::format(mem(), format, &mut VaList { pointer: list });
            write_checked_text(uc, buffer, None, object_size, &text)
        })
        .unwrap(),
    );
    function(
        "__vsnprintf_chk",
        handler!(|uc, a| {
            let (buffer, size, _flags, object_size, format, list) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let text = arm32_bridge::imports::printf::format(mem(), format, &mut VaList { pointer: list });
            write_checked_text(uc, buffer, Some(size), object_size, &text)
        })
        .unwrap(),
    );
    function(
        "__strcat_chk",
        handler!(|uc, a| {
            let (destination, source, _size) = (a.u32(), a.u32(), a.u32());
            unsafe { libc::strcat(host_c(destination), host_c(source)) };
            Ret::U32(destination)
        })
        .unwrap(),
    );
    function(
        "__strncpy_chk",
        handler!(|uc, a| {
            let (destination, source, length, _size) = (a.u32(), a.u32(), a.u32(), a.u32());
            unsafe { libc::strncpy(host_c(destination), host_c(source), length as usize) };
            Ret::U32(destination)
        })
        .unwrap(),
    );
    function(
        "__strncat_chk",
        handler!(|uc, a| {
            let (destination, source, length, _size) = (a.u32(), a.u32(), a.u32(), a.u32());
            unsafe { libc::strncat(host_c(destination), host_c(source), length as usize) };
            Ret::U32(destination)
        })
        .unwrap(),
    );
    for (name, width) in [("memset_pattern4", 4u32), ("memset_pattern8", 8), ("memset_pattern16", 16)] {
        let handler: Handler = match width {
            4 => |_uc, a| memset_pattern(a, 4),
            8 => |_uc, a| memset_pattern(a, 8),
            _ => |_uc, a| memset_pattern(a, 16),
        };
        function(name, handler);
    }
    function(
        "strerror",
        handler!(|uc, a| {
            let number = a.i32();
            Ret::U32(shared().cached_cstr(format!("darwin-strerror:{number}"), &darwin_strerror(number)))
        })
        .unwrap(),
    );
    // POSIX strerror_r(Darwin number, buffer, length): 0, or ERANGE (34) if cut short.
    function(
        "strerror_r",
        handler!(|uc, a| {
            let (number, buffer, length) = (a.i32(), a.u32(), a.u32());
            let text = darwin_strerror(number);
            if length == 0 {
                return Ret::U32(34);
            }
            let copy = text.len().min(length as usize - 1);
            mem().write_bytes(buffer, &text.as_bytes()[..copy]);
            mem().write_u8(buffer + copy as u32, 0);
            Ret::U32(if copy < text.len() { 34 } else { 0 })
        })
        .unwrap(),
    );
    // __assert_rtn(function, file, line, expression)
    function(
        "__assert_rtn",
        handler!(|uc, a| {
            let (function, file, line, expression) = (a.u32(), a.u32(), a.u32(), a.u32());
            let m = mem();
            let text = format!(
                "game assertion failed: {} in {} at {}:{}",
                m.string(expression),
                m.string(function),
                m.string(file),
                line
            );
            stop_with_fault(uc, text);
            Ret::Void
        })
        .unwrap(),
    );
}

#[allow(unused_variables)]
fn memset_pattern(a: &mut Args, width: u32) -> Ret {
    let (destination, pattern, length) = (a.u32(), a.u32(), a.u32());
    let m = mem();
    let pattern = m.bytes(pattern, width as usize).to_vec();
    let mut at = 0;
    while at < length {
        let count = (length - at).min(width);
        m.write_bytes(destination + at, &pattern[..count as usize]);
        at += count;
    }
    Ret::Void
}

// ---- numbers ----

/// Darwin's rand(): the Park-Miller "minimal standard" generator (Libc rand.c), seed 1.
static RAND_STATE: Mutex<u32> = Mutex::new(1);

fn darwin_rand(state: &mut u32) -> u32 {
    let mut x = i64::from(*state);
    if x == 0 {
        x = 123_459_876;
    }
    let (hi, lo) = (x / 127_773, x % 127_773);
    let mut next = 16_807 * lo - 2_836 * hi;
    if next < 0 {
        next += 0x7fff_ffff;
    }
    *state = next as u32;
    *state % 0x8000_0000
}

fn numbers() {
    // struct { double sin, cos } __sincos_stret(double): returned through r0.
    function(
        "__sincos_stret",
        handler!(|uc, a| {
            let (out, x) = (a.u32(), a.f64());
            mem().write_u64(out, x.sin().to_bits());
            mem().write_u64(out + 8, x.cos().to_bits());
            Ret::Void
        })
        .unwrap(),
    );
    function(
        "__sincosf_stret",
        handler!(|uc, a| {
            let (out, x) = (a.u32(), a.f32());
            mem().write_u32(out, x.sin().to_bits());
            mem().write_u32(out + 4, x.cos().to_bits());
            Ret::Void
        })
        .unwrap(),
    );
    function("__tanpi", handler!(|uc, a| Ret::F64((a.f64() * std::f64::consts::PI).tan())).unwrap());
    // Compiler helpers (compiler-rt names; division by zero gives 0).
    function("__divsi3", handler!(|uc, a| { let (n, d) = (a.i32(), a.i32()); Ret::U32(n.checked_div(d).unwrap_or(if d == 0 { 0 } else { n.wrapping_div(d) }) as u32) }).unwrap());
    function("__udivsi3", handler!(|uc, a| { let (n, d) = (a.u32(), a.u32()); Ret::U32(n.checked_div(d).unwrap_or(0)) }).unwrap());
    function("__modsi3", handler!(|uc, a| { let (n, d) = (a.i32(), a.i32()); Ret::U32(if d == 0 { 0 } else { n.wrapping_rem(d) } as u32) }).unwrap());
    function("__umodsi3", handler!(|uc, a| { let (n, d) = (a.u32(), a.u32()); Ret::U32(n.checked_rem(d).unwrap_or(0)) }).unwrap());
    function(
        "__divmodsi4",
        handler!(|uc, a| {
            let (n, d, remainder) = (a.i32(), a.i32(), a.u32());
            let (q, r) = if d == 0 { (0, 0) } else { (n.wrapping_div(d), n.wrapping_rem(d)) };
            mem().write_u32(remainder, r as u32);
            Ret::U32(q as u32)
        })
        .unwrap(),
    );
    function(
        "__udivmodsi4",
        handler!(|uc, a| {
            let (n, d, remainder) = (a.u32(), a.u32(), a.u32());
            let (q, r) = if d == 0 { (0, 0) } else { (n / d, n % d) };
            mem().write_u32(remainder, r);
            Ret::U32(q)
        })
        .unwrap(),
    );
    function("__divdi3", handler!(|uc, a| { let (n, d) = (a.i64(), a.i64()); Ret::U64(if d == 0 { 0 } else { n.wrapping_div(d) } as u64) }).unwrap());
    function("__udivdi3", handler!(|uc, a| { let (n, d) = (a.u64(), a.u64()); Ret::U64(n.checked_div(d).unwrap_or(0)) }).unwrap());
    function("__moddi3", handler!(|uc, a| { let (n, d) = (a.i64(), a.i64()); Ret::U64(if d == 0 { 0 } else { n.wrapping_rem(d) } as u64) }).unwrap());
    function("__umoddi3", handler!(|uc, a| { let (n, d) = (a.u64(), a.u64()); Ret::U64(n.checked_rem(d).unwrap_or(0)) }).unwrap());
    function("__fixdfdi", handler!(|uc, a| Ret::U64(a.f64() as i64 as u64)).unwrap());
    function("__fixsfdi", handler!(|uc, a| Ret::U64(a.f32() as i64 as u64)).unwrap());
    function("__fixunsdfdi", handler!(|uc, a| Ret::U64(a.f64() as u64)).unwrap());
    function("__fixunssfdi", handler!(|uc, a| Ret::U64(a.f32() as u64)).unwrap());
    function("__floatdidf", handler!(|uc, a| Ret::F64(a.i64() as f64)).unwrap());
    function("__floatdisf", handler!(|uc, a| Ret::F32(a.i64() as f32)).unwrap());
    function("__floatundidf", handler!(|uc, a| Ret::F64(a.u64() as f64)).unwrap());
    function("__floatundisf", handler!(|uc, a| Ret::F32(a.u64() as f32)).unwrap());
    // Random numbers.
    function("rand", handler!(|uc, a| Ret::U32(darwin_rand(&mut RAND_STATE.lock().unwrap_or_else(|e| e.into_inner())))).unwrap());
    function(
        "srand",
        handler!(|uc, a| {
            *RAND_STATE.lock().unwrap_or_else(|e| e.into_inner()) = a.u32();
            Ret::Void
        })
        .unwrap(),
    );
    extern "C" {
        fn random() -> libc::c_long;
        fn srandom(seed: libc::c_uint);
    }
    function("random", handler!(|uc, a| Ret::U32(unsafe { random() } as u32)).unwrap());
    function(
        "srandom",
        handler!(|uc, a| {
            unsafe { srandom(a.u32()) };
            Ret::Void
        })
        .unwrap(),
    );
    function(
        "arc4random",
        handler!(|uc, a| {
            let mut value = [0u8; 4];
            unsafe { libc::getrandom(value.as_mut_ptr().cast(), 4, 0) };
            Ret::U32(u32::from_le_bytes(value))
        })
        .unwrap(),
    );
    function(
        "arc4random_uniform",
        handler!(|uc, a| {
            let bound = a.u32();
            if bound < 2 {
                return Ret::U32(0);
            }
            // Rejection sampling, as libc does, so every value is equally likely.
            let floor = bound.wrapping_neg() % bound;
            loop {
                let mut value = [0u8; 4];
                unsafe { libc::getrandom(value.as_mut_ptr().cast(), 4, 0) };
                let value = u32::from_le_bytes(value);
                if value >= floor {
                    return Ret::U32(value % bound);
                }
            }
        })
        .unwrap(),
    );
    // CommonCrypto one-shot hashes: CC_MD5(data, length, out) -> out.
    function(
        "CC_MD5",
        handler!(|uc, a| {
            let (data, length, out) = (a.u32(), a.u32(), a.u32());
            mem().write_bytes(out, &crate::fw::crypto::md5(mem().bytes(data, length as usize)));
            Ret::U32(out)
        })
        .unwrap(),
    );
    function(
        "CC_SHA1",
        handler!(|uc, a| {
            let (data, length, out) = (a.u32(), a.u32(), a.u32());
            mem().write_bytes(out, &crate::fw::crypto::sha1(mem().bytes(data, length as usize)));
            Ret::U32(out)
        })
        .unwrap(),
    );
    // Character classes outside the cached 0-255 range of _DefaultRuneLocale.
    function(
        "__maskrune",
        handler!(|uc, a| {
            let (c, mask) = (a.i32(), a.u32());
            Ret::U32(rune_type(c) & mask)
        })
        .unwrap(),
    );
}

// ---- ctype: _DefaultRuneLocale ----

pub const RUNE_ALPHA: u32 = 0x100;
pub const RUNE_CONTROL: u32 = 0x200;
pub const RUNE_DIGIT: u32 = 0x400;
pub const RUNE_GRAPH: u32 = 0x800;
pub const RUNE_LOWER: u32 = 0x1000;
pub const RUNE_PUNCT: u32 = 0x2000;
pub const RUNE_SPACE: u32 = 0x4000;
pub const RUNE_UPPER: u32 = 0x8000;
pub const RUNE_XDIGIT: u32 = 0x10000;
pub const RUNE_BLANK: u32 = 0x20000;
pub const RUNE_PRINT: u32 = 0x40000;
/// Printing width 1.
pub const RUNE_WIDTH1: u32 = 0x4000_0000;

/// Offsets in Darwin armv7's `_RuneLocale`: magic[8], encoding[32], two
/// function pointers, invalid rune, then the 256-entry tables.
pub const RUNE_TYPES: u32 = 52;
pub const RUNE_LOWER_MAP: u32 = RUNE_TYPES + 1024;
pub const RUNE_UPPER_MAP: u32 = RUNE_LOWER_MAP + 1024;
pub const RUNE_LOCALE_SIZE: u32 = RUNE_UPPER_MAP + 1024 + 40;

/// The C locale's class bits for `c` (the low byte holds a digit's value).
pub fn rune_type(c: i32) -> u32 {
    let Ok(byte) = u8::try_from(c) else { return 0 };
    if byte > 127 {
        return 0;
    }
    let mut bits = 0;
    let set = |condition: bool, flag: u32, bits: &mut u32| {
        if condition {
            *bits |= flag;
        }
    };
    set(byte.is_ascii_alphabetic(), RUNE_ALPHA, &mut bits);
    set(byte.is_ascii_control(), RUNE_CONTROL, &mut bits);
    set(byte.is_ascii_digit(), RUNE_DIGIT, &mut bits);
    set(byte.is_ascii_graphic(), RUNE_GRAPH, &mut bits);
    set(byte.is_ascii_lowercase(), RUNE_LOWER, &mut bits);
    set(byte.is_ascii_punctuation(), RUNE_PUNCT, &mut bits);
    set(byte == b' ' || (9..=13).contains(&byte), RUNE_SPACE, &mut bits);
    set(byte.is_ascii_uppercase(), RUNE_UPPER, &mut bits);
    set(byte.is_ascii_hexdigit(), RUNE_XDIGIT, &mut bits);
    set(byte == b' ' || byte == b'\t', RUNE_BLANK, &mut bits);
    set(byte.is_ascii_graphic() || byte == b' ', RUNE_PRINT | RUNE_WIDTH1, &mut bits);
    if byte.is_ascii_hexdigit() {
        bits |= (byte as char).to_digit(16).unwrap();
    }
    bits
}

fn make_rune_locale() -> u32 {
    let address = super::alloc_permanent(RUNE_LOCALE_SIZE, 4);
    let m = mem();
    m.write_bytes(address, b"RuneMagA");
    m.write_bytes(address + 8, b"NONE");
    m.write_u32(address + 48, 0xFFFD); // invalid rune
    for c in 0..256u32 {
        let byte = c as u8;
        m.write_u32(address + RUNE_TYPES + c * 4, rune_type(c as i32));
        m.write_u32(address + RUNE_LOWER_MAP + c * 4, byte.to_ascii_lowercase() as u32);
        m.write_u32(address + RUNE_UPPER_MAP + c * 4, byte.to_ascii_uppercase() as u32);
    }
    address
}

// ---- process, system, dyld ----

fn sysctl_answer(uc: &Uc, value: &[u8], old: u32, old_length: u32) -> Ret {
    let m = mem();
    if old_length == 0 {
        return Ret::U32(0);
    }
    if old == 0 {
        m.write_u32(old_length, value.len() as u32);
        return Ret::U32(0);
    }
    if (m.read_u32(old_length) as usize) < value.len() {
        m.write_u32(uc.get_data().errno_slot, 12); // ENOMEM
        return Ret::U32(u32::MAX);
    }
    m.write_bytes(old, value);
    m.write_u32(old_length, value.len() as u32);
    Ret::U32(0)
}

/// The 64-bit layer's answers, with armv7's sizes and CPU.
fn sysctl_value32(name: &str) -> Option<Vec<u8>> {
    Some(match name {
        "hw.pagesize" => 4096i32.to_le_bytes().to_vec(),
        "hw.cputype" => 12i32.to_le_bytes().to_vec(),   // CPU_TYPE_ARM
        "hw.cpusubtype" => 9i32.to_le_bytes().to_vec(), // CPU_SUBTYPE_ARM_V7
        _ => crate::libsystem::io::sysctl_value(name)?,
    })
}

fn sysctl_unknown(uc: &Uc, what: String) -> Ret {
    crate::log!("32-bit sysctl {what}: not known, answered 'no such entry'");
    mem().write_u32(uc.get_data().errno_slot, 2); // ENOENT
    Ret::U32(u32::MAX)
}

fn process() {
    function(
        "sysctlbyname",
        handler!(|uc, a| {
            let (name, old, old_length) = (a.u32(), a.u32(), a.u32());
            let name = mem().string(name);
            match sysctl_value32(&name) {
                Some(value) => sysctl_answer(uc, &value, old, old_length),
                None => sysctl_unknown(uc, name),
            }
        })
        .unwrap(),
    );
    function(
        "sysctl",
        handler!(|uc, a| {
            let (mib, count, old, old_length) = (a.u32(), a.u32(), a.u32(), a.u32());
            let parts: Vec<i32> = (0..count.min(8)).map(|i| mem().read_u32(mib + i * 4) as i32).collect();
            if let Some(value) = crate::libsystem::io::sysctl_name(&parts).and_then(sysctl_value32) {
                return sysctl_answer(uc, &value, old, old_length);
            }
            if let Some(value) = crate::libsystem::io::sysctl_numeric(&parts) {
                return sysctl_answer(uc, &value, old, old_length);
            }
            if let [1, 14, ..] = parts.as_slice() {
                // Process information (a debugger check): all zero = not traced.
                return sysctl_answer(uc, &[0u8; 492], old, old_length);
            }
            sysctl_unknown(uc, format!("{parts:?}"))
        })
        .unwrap(),
    );
    // Darwin's _SC_ numbers.
    function(
        "sysconf",
        handler!(|uc, a| {
            let value = match a.i32() {
                3 => unsafe { libc::sysconf(libc::_SC_CLK_TCK) },
                5 => unsafe { libc::sysconf(libc::_SC_OPEN_MAX) },
                29 => 4096,
                57 => unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) },
                58 => unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) },
                200 => (1 << 30) / 4096,
                other => {
                    crate::log!("32-bit sysconf {other}: not known");
                    -1
                }
            };
            Ret::U32(value as u32)
        })
        .unwrap(),
    );
    // As Apple's Libc: atexit is __cxa_atexit(routine, NULL, NULL) (the C++
    // runtime's), and exit first runs __cxa_finalize(NULL): every routine
    // and static destructor, newest first. _exit runs none.
    function(
        "atexit",
        handler!(|uc, a| {
            let routine = a.u32();
            let Some(register) = guest_export("__cxa_atexit") else {
                shared().state.at_exit.push((routine, 0));
                return Ret::U32(0);
            };
            match call_guest(uc, register, &[routine, 0, 0]) {
                Ok(result) => Ret::U32(result as u32),
                Err(error) => {
                    stop_with_fault(uc, error);
                    Ret::Void
                }
            }
        })
        .unwrap(),
    );
    function(
        "exit",
        handler!(|uc, a| {
            static FINALIZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            let code = a.u32();
            if let Some(finalize) = guest_export("__cxa_finalize") {
                if !FINALIZING.swap(true, Ordering::SeqCst) {
                    if let Err(error) = call_guest(uc, finalize, &[0]) {
                        stop_with_fault(uc, error);
                        return Ret::Void;
                    }
                }
            }
            forward(uc, bionic!("exit"), [code, 0, 0, 0])
        })
        .unwrap(),
    );
    // Apple's clock ids.
    function(
        "clock_gettime",
        handler!(|uc, a| {
            let (clock, out) = (a.u32(), a.u32());
            let host = match clock {
                0 => libc::CLOCK_REALTIME,
                4 | 8 => libc::CLOCK_MONOTONIC_RAW, // MONOTONIC_RAW, UPTIME_RAW
                5 | 9 => libc::CLOCK_MONOTONIC,     // their _APPROX forms
                6 => libc::CLOCK_BOOTTIME,          // MONOTONIC: counts sleep too
                12 => libc::CLOCK_PROCESS_CPUTIME_ID,
                16 => libc::CLOCK_THREAD_CPUTIME_ID,
                _ => {
                    mem().write_u32(uc.get_data().errno_slot, 22); // EINVAL
                    return Ret::U32(u32::MAX);
                }
            };
            let mut now: libc::timespec = unsafe { std::mem::zeroed() };
            if unsafe { libc::clock_gettime(host, &mut now) } != 0 {
                return checked(uc, -1);
            }
            mem().write_u32(out, now.tv_sec as u32);
            mem().write_u32(out + 4, now.tv_nsec as u32);
            Ret::U32(0)
        })
        .unwrap(),
    );
    // Libraries a 32-bit game could open are already loaded: RTLD_NOLOAD
    // (0x10) asks whether one is (libc++ asks about GCC's libstdc++), and
    // isn't answered yes; other opens behave as on Android.
    function(
        "dlopen",
        handler!(|uc, a| {
            let (name, flags) = (a.u32(), a.u32());
            if name != 0 && flags & 0x10 != 0 {
                return Ret::U32(0);
            }
            forward(uc, bionic!("dlopen"), [name, flags, 0, 0])
        })
        .unwrap(),
    );
    // dlsym(handle, C name): guest libraries' exports, then this layer's table.
    function(
        "dlsym",
        handler!(|uc, a| {
            let (_handle, name) = (a.u32(), a.u32());
            let name = mem().string(name);
            let address = guest_export(&name)
                .or_else(|| ios32::data_address(&name))
                .or_else(|| ios32::provides_function(&name).then(|| runtime::stub_for(&name)))
                .unwrap_or(0);
            Ret::U32(address)
        })
        .unwrap(),
    );
    // Nanosecond ticks: mach_timebase_info says 1/1.
    function(
        "mach_absolute_time",
        handler!(|uc, a| {
            let mut now: libc::timespec = unsafe { std::mem::zeroed() };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
            Ret::U64(now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64)
        })
        .unwrap(),
    );
    function(
        "mach_timebase_info",
        handler!(|uc, a| {
            let info = a.u32();
            mem().write_u32(info, 1);
            mem().write_u32(info + 4, 1);
            Ret::U32(0)
        })
        .unwrap(),
    );
    // dyld's view of the loaded images: just the program.
    function(
        "_dyld_register_func_for_add_image",
        handler!(|uc, a| {
            let callback = a.u32();
            if let Some(&(header, slide, _)) = super::main_image() {
                if let Err(error) = call_guest(uc, callback, &[header, slide]) {
                    stop_with_fault(uc, error);
                }
            }
            Ret::Void
        })
        .unwrap(),
    );
    function("_dyld_register_func_for_remove_image", handler!(|uc, a| Ret::Void).unwrap());
    function("_dyld_image_count", handler!(|uc, a| Ret::U32(super::main_image().is_some() as u32)).unwrap());
    function(
        "_dyld_get_image_header",
        handler!(|uc, a| Ret::U32(if a.u32() == 0 { super::main_image().map_or(0, |image| image.0) } else { 0 })).unwrap(),
    );
    function(
        "_dyld_get_image_vmaddr_slide",
        handler!(|uc, a| Ret::U32(if a.u32() == 0 { super::main_image().map_or(0, |image| image.1) } else { 0 })).unwrap(),
    );
    function(
        "_dyld_get_image_name",
        handler!(|uc, a| {
            let index = a.u32();
            match super::main_image() {
                Some((_, _, path)) if index == 0 => Ret::U32(super::cstring_permanent(path)),
                _ => Ret::U32(0),
            }
        })
        .unwrap(),
    );
    // Lazy pointers are all bound at load time, so dyld's lazy binder is never reached.
    function(
        "dyld_stub_binder",
        handler!(|uc, a| {
            stop_with_fault(uc, "dyld_stub_binder was called: a lazy pointer was not bound at load time".into());
            Ret::Void
        })
        .unwrap(),
    );
    function("getifaddrs", handler!(|uc, a| Ret::U32(interface_list(a.u32()))).unwrap());
    function(
        "freeifaddrs",
        handler!(|uc, a| {
            let list = a.u32();
            if list != 0 {
                super::free(list);
            }
            Ret::Void
        })
        .unwrap(),
    );
    function(
        "if_nametoindex",
        handler!(|uc, a| Ret::U32(match mem().string(a.u32()).as_str() {
            "lo0" => 1,
            "en0" => 2,
            _ => 0,
        }))
        .unwrap(),
    );
    // The program file's name, as argv[0] gives it.
    function(
        "getprogname",
        handler!(|uc, a| {
            let path = super::main_image().map_or("", |image| image.2.as_str());
            Ret::U32(super::cstring_permanent(path.rsplit('/').next().unwrap_or(path)))
        })
        .unwrap(),
    );
    // _NSGetExecutablePath(buffer, uint32_t *size): 0, or -1 with the size needed.
    function(
        "_NSGetExecutablePath",
        handler!(|uc, a| {
            let (buffer, size) = (a.u32(), a.u32());
            let path = super::main_image().map_or("", |image| image.2.as_str()).as_bytes();
            let needed = path.len() as u32 + 1;
            if mem().read_u32(size) < needed {
                mem().write_u32(size, needed);
                return Ret::U32(u32::MAX);
            }
            mem().write_bytes(buffer, path);
            mem().write_u8(buffer + path.len() as u32, 0);
            Ret::U32(0)
        })
        .unwrap(),
    );
    // Halfway cases away from zero, as Rust's round.
    function("lround", handler!(|uc, a| Ret::U32(a.f64().round() as i32 as u32)).unwrap());
    function("lroundf", handler!(|uc, a| Ret::U32(a.f32().round() as i32 as u32)).unwrap());
    // reallocf: realloc that frees the old block when it fails.
    function(
        "reallocf",
        handler!(|uc, a| {
            let (block, size) = (a.u32(), a.u32());
            let moved = runtime::heap().realloc(block, size);
            if moved == 0 && block != 0 && size != 0 {
                super::free(block);
            }
            Ret::U32(moved)
        })
        .unwrap(),
    );
    // perror(prefix): "prefix: text" for the guest's (Darwin) errno, on stderr.
    function(
        "perror",
        handler!(|uc, a| {
            let prefix = a.u32();
            let text = darwin_strerror(mem().read_u32(uc.get_data().errno_slot) as i32);
            let prefix = if prefix == 0 { String::new() } else { mem().string(prefix) };
            let line = if prefix.is_empty() { format!("{text}\n") } else { format!("{prefix}: {text}\n") };
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
            Ret::Void
        })
        .unwrap(),
    );
    function("backtrace", handler!(|uc, a| { let (buffer, size) = (a.u32(), a.u32()); Ret::U32(backtrace(uc, buffer, size)) }).unwrap());
    function(
        "backtrace_symbols",
        handler!(|uc, a| {
            let (addresses, count) = (a.u32(), a.u32());
            Ret::U32(backtrace_symbols(addresses, count as i32))
        })
        .unwrap(),
    );
    // dladdr(address, Dl_info *{ fname, fbase, sname, saddr }): the program
    // only (its symbols aren't kept, so no symbol name: NULL, as Apple gives
    // when none is found).
    function(
        "dladdr",
        handler!(|uc, a| {
            let (address, info) = (a.u32(), a.u32());
            let Some((header, path)) = image_of(address) else { return Ret::U32(0) };
            for (i, word) in [super::cstring_permanent(&path), header, 0, 0].iter().enumerate() {
                mem().write_u32(info + i as u32 * 4, *word);
            }
            Ret::U32(1)
        })
        .unwrap(),
    );
}

/// The image (header address, path) holding guest address `address`.
fn image_of(address: u32) -> Option<(u32, String)> {
    let (start, end) = super::main_image_span()?;
    let (header, _, path) = super::main_image()?;
    (start..end).contains(&address).then(|| (*header, path.clone()))
}

/// backtrace(buffer, size): return addresses along the frame chain. Apple
/// armv7 code keeps the frame pointer in r7, pointing at { saved r7, lr };
/// callers' frames sit higher on the stack. The first entry is the caller's
/// return address (lr).
fn backtrace(uc: &Uc, buffer: u32, size: u32) -> u32 {
    let m = mem();
    let readable = |at: u32| {
        at % 4 == 0 && arm32_bridge::mem::layout().regions.iter().any(|&(from, to, _)| from <= at && at.checked_add(8).is_some_and(|end| end <= to))
    };
    let mut count = 0;
    let add = |address: u32, count: &mut u32| {
        m.write_u32(buffer + *count * 4, address);
        *count += 1;
    };
    if size == 0 {
        return 0;
    }
    add(uc.reg(14), &mut count);
    let mut frame = uc.reg(7);
    while count < size && frame != 0 && readable(frame) {
        let (next, address) = (m.read_u32(frame), m.read_u32(frame + 4));
        if address == 0 {
            break;
        }
        add(address, &mut count);
        if next <= frame {
            break;
        }
        frame = next;
    }
    count
}

/// backtrace_symbols(addresses, count): one heap block (the caller frees
/// it) with `count` text pointers, in Apple's
/// "index image address symbol + offset" form.
fn backtrace_symbols(addresses: u32, count: i32) -> u32 {
    if count <= 0 {
        return 0;
    }
    let m = mem();
    let lines: Vec<String> = (0..count as u32)
        .map(|i| {
            let address = m.read_u32(addresses + i * 4);
            let (image, base) = match image_of(address) {
                Some((header, path)) => (path.rsplit('/').next().unwrap_or("").to_string(), header),
                None => ("???".to_string(), 0),
            };
            format!("{i:<4}{image:<35} 0x{address:08x} 0x{base:x} + {}", address.wrapping_sub(base))
        })
        .collect();
    let table = count as u32 * 4;
    let total = table + lines.iter().map(|line| line.len() as u32 + 1).sum::<u32>();
    let block = super::alloc(total);
    let mut text = block + table;
    for (i, line) in lines.iter().enumerate() {
        m.write_u32(block + i as u32 * 4, text);
        m.write_bytes(text, line.as_bytes());
        m.write_u8(text + line.len() as u32, 0);
        text += line.len() as u32 + 1;
    }
    block
}

/// getifaddrs(): lo0 (127.0.0.1) and en0 (an address and the MAC address
/// iOS 7+ shows apps, 02:00:00:00:00:00), in one heap block. Darwin's
/// struct ifaddrs: next, name, flags, addr, netmask, dstaddr, data (7 words);
/// BSD sockaddrs start with a length byte.
fn interface_list(out: u32) -> u32 {
    const NODE: u32 = 28;
    const IFF_UP: u32 = 0x1;
    const IFF_BROADCAST: u32 = 0x2;
    const IFF_LOOPBACK: u32 = 0x8;
    const IFF_RUNNING: u32 = 0x40;
    const IFF_MULTICAST: u32 = 0x8000;
    let block = super::alloc(512);
    let m = mem();
    unsafe { std::ptr::write_bytes(m.host(block), 0, 512) };
    let names = block + 3 * NODE;
    m.write_bytes(names, b"lo0\0en0\0");
    let sockaddrs = names + 8;
    let inet = |at: u32, address: [u8; 4]| {
        m.write_bytes(at, &[16, 2, 0, 0]);
        m.write_bytes(at + 4, &address);
    };
    inet(sockaddrs, [127, 0, 0, 1]);
    inet(sockaddrs + 16, [255, 0, 0, 0]);
    inet(sockaddrs + 32, [10, 0, 2, 15]);
    inet(sockaddrs + 48, [255, 255, 255, 0]);
    // sockaddr_dl: len, AF_LINK 18, index 2, IFT_ETHER 6, name length 3, address length 6, then "en0" and the MAC.
    let link = sockaddrs + 64;
    m.write_bytes(link, &[20, 18, 2, 0, 6, 3, 6, 0]);
    m.write_bytes(link + 8, &[b'e', b'n', b'0', 0x02, 0, 0, 0, 0, 0]);
    let nodes = [
        (names, IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_MULTICAST, sockaddrs, sockaddrs + 16),
        (names + 4, IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST, link, 0),
        (names + 4, IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST, sockaddrs + 32, sockaddrs + 48),
    ];
    for (i, (name, flags, address, netmask)) in nodes.iter().enumerate() {
        let node = block + i as u32 * NODE;
        let next = if i + 1 < nodes.len() { node + NODE } else { 0 };
        for (j, word) in [next, *name, *flags, *address, *netmask, 0, 0].iter().enumerate() {
            m.write_u32(node + j as u32 * 4, *word);
        }
    }
    m.write_u32(out, block);
    0
}

// ---- pthread ----
//
// Darwin's pthread objects start with a signature word, and static
// initializers only set that word. Bionic's handlers run on the word right
// after it (Darwin's objects are big enough: mutex 44 bytes vs bionic's 4,
// cond 28 vs 4, attr 40 vs 24, rwlock 200 vs 40, once 8 vs 4).

const MUTEX_SIGNATURE: u32 = 0x4D55_5458; // _PTHREAD_MUTEX_SIG
const MUTEX_CONVERTING: u32 = 0x4D55_5400;
const COND_SIGNATURE: u32 = 0x434F_4E44; // _PTHREAD_COND_SIG
const ATTR_SIGNATURE: u32 = 0x5448_5244; // _PTHREAD_ATTR_SIG
const MUTEXATTR_SIGNATURE: u32 = 0x4D54_5841; // _PTHREAD_MUTEX_ATTR_SIG
/// Bionic's mutex kinds, by Darwin's (NORMAL 0, ERRORCHECK 1, RECURSIVE 2).
const BIONIC_KIND: [u32; 3] = [0, 2, 1];

/// The bionic mutex word inside Darwin mutex `mutex`. A statically
/// initialized mutex (PTHREAD_MUTEX_INITIALIZER, PTHREAD_RECURSIVE_MUTEX_INITIALIZER,
/// ...) gets its word made on first use, once, even when threads race.
pub fn mutex_word(mutex: u32) -> u32 {
    let signature = mem().atomic_u32(mutex);
    loop {
        let current = signature.load(Ordering::Acquire);
        if current == MUTEX_SIGNATURE {
            return mutex + 4;
        }
        if current == MUTEX_CONVERTING {
            std::hint::spin_loop();
            continue;
        }
        let kind = match current {
            0x32AA_ABA1 => BIONIC_KIND[1], // _PTHREAD_ERRORCHECK_MUTEX_SIG_init
            0x32AA_ABA2 => BIONIC_KIND[2], // _PTHREAD_RECURSIVE_MUTEX_SIG_init
            _ => 0,                        // normal, first-fit, or zeroed memory
        };
        if signature.compare_exchange(current, MUTEX_CONVERTING, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            mem().write_u32(mutex + 4, kind << 14);
            signature.store(MUTEX_SIGNATURE, Ordering::Release);
            return mutex + 4;
        }
    }
}

/// Runs bionic's handler on adjusted arguments.
fn forward(uc: &mut Uc, handler: Handler, registers: [u32; 4]) -> Ret {
    handler(uc, &mut Args::new(registers, uc.reg(runtime::SP)))
}

fn inner(object: u32) -> u32 {
    if object == 0 {
        0
    } else {
        object + 4
    }
}

fn threads() {
    function(
        "pthread_mutex_init",
        handler!(|uc, a| {
            let (mutex, attributes) = (a.u32(), a.u32());
            let kind = if attributes == 0 { 0 } else { mem().read_u32(attributes + 4) };
            mem().write_u32(mutex + 4, kind << 14);
            mem().atomic_u32(mutex).store(MUTEX_SIGNATURE, Ordering::Release);
            Ret::U32(0)
        })
        .unwrap(),
    );
    function("pthread_mutex_destroy", handler!(|uc, a| Ret::U32(0)).unwrap());
    function("pthread_mutex_lock", handler!(|uc, a| { let m = mutex_word(a.u32()); error_code(forward(uc, bionic!("pthread_mutex_lock"), [m, 0, 0, 0])) }).unwrap());
    function("pthread_mutex_trylock", handler!(|uc, a| { let m = mutex_word(a.u32()); error_code(forward(uc, bionic!("pthread_mutex_trylock"), [m, 0, 0, 0])) }).unwrap());
    function("pthread_mutex_unlock", handler!(|uc, a| { let m = mutex_word(a.u32()); error_code(forward(uc, bionic!("pthread_mutex_unlock"), [m, 0, 0, 0])) }).unwrap());
    function(
        "pthread_mutexattr_init",
        handler!(|uc, a| {
            let attributes = a.u32();
            mem().write_u32(attributes, MUTEXATTR_SIGNATURE);
            mem().write_u32(attributes + 4, 0);
            Ret::U32(0)
        })
        .unwrap(),
    );
    function("pthread_mutexattr_destroy", handler!(|uc, a| Ret::U32(0)).unwrap());
    function(
        "pthread_mutexattr_settype",
        handler!(|uc, a| {
            let (attributes, kind) = (a.u32(), a.u32());
            match BIONIC_KIND.get(kind as usize) {
                Some(&bionic) => {
                    mem().write_u32(attributes + 4, bionic);
                    Ret::U32(0)
                }
                None => Ret::U32(22), // EINVAL
            }
        })
        .unwrap(),
    );
    function(
        "pthread_cond_init",
        handler!(|uc, a| {
            let condition = a.u32();
            mem().write_u32(condition, COND_SIGNATURE);
            error_code(forward(uc, bionic!("pthread_cond_init"), [condition + 4, 0, 0, 0]))
        })
        .unwrap(),
    );
    function("pthread_cond_destroy", handler!(|uc, a| Ret::U32(0)).unwrap());
    function("pthread_cond_signal", handler!(|uc, a| { let c = a.u32(); error_code(forward(uc, bionic!("pthread_cond_signal"), [c + 4, 0, 0, 0])) }).unwrap());
    function("pthread_cond_broadcast", handler!(|uc, a| { let c = a.u32(); error_code(forward(uc, bionic!("pthread_cond_broadcast"), [c + 4, 0, 0, 0])) }).unwrap());
    function(
        "pthread_cond_wait",
        handler!(|uc, a| {
            let (c, m) = (a.u32(), a.u32());
            let m = mutex_word(m);
            error_code(forward(uc, bionic!("pthread_cond_wait"), [c + 4, m, 0, 0]))
        })
        .unwrap(),
    );
    function(
        "pthread_cond_timedwait",
        handler!(|uc, a| {
            let (c, m, deadline) = (a.u32(), a.u32(), a.u32());
            let m = mutex_word(m);
            error_code(forward(uc, bionic!("pthread_cond_timedwait"), [c + 4, m, deadline, 0]))
        })
        .unwrap(),
    );
    function(
        "pthread_cond_timedwait_relative_np",
        handler!(|uc, a| {
            let (c, m, relative) = (a.u32(), a.u32(), a.u32());
            let m = mutex_word(m);
            error_code(forward(uc, bionic!("pthread_cond_timedwait_relative_np"), [c + 4, m, relative, 0]))
        })
        .unwrap(),
    );
    function(
        "pthread_attr_init",
        handler!(|uc, a| {
            let attributes = a.u32();
            mem().write_u32(attributes, ATTR_SIGNATURE);
            forward(uc, bionic!("pthread_attr_init"), [attributes + 4, 0, 0, 0])
        })
        .unwrap(),
    );
    function("pthread_attr_destroy", handler!(|uc, a| Ret::U32(0)).unwrap());
    // Darwin: JOINABLE 1, DETACHED 2; bionic: 0 and 1.
    function(
        "pthread_attr_setdetachstate",
        handler!(|uc, a| {
            let (attributes, state) = (a.u32(), a.u32());
            if !(1..=2).contains(&state) {
                return Ret::U32(22);
            }
            error_code(forward(uc, bionic!("pthread_attr_setdetachstate"), [attributes + 4, state - 1, 0, 0]))
        })
        .unwrap(),
    );
    function(
        "pthread_attr_getdetachstate",
        handler!(|uc, a| {
            let (attributes, out) = (a.u32(), a.u32());
            let result = forward(uc, bionic!("pthread_attr_getdetachstate"), [attributes + 4, out, 0, 0]);
            mem().write_u32(out, mem().read_u32(out) + 1);
            result
        })
        .unwrap(),
    );
    // Apple: whole pages of at least PTHREAD_STACK_MIN (16 KiB), else EINVAL.
    function(
        "pthread_attr_setstacksize",
        handler!(|uc, a| {
            let (attributes, size) = (a.u32(), a.u32());
            if size < 16384 || size % 4096 != 0 {
                return Ret::U32(22);
            }
            error_code(forward(uc, bionic!("pthread_attr_setstacksize"), [attributes + 4, size, 0, 0]))
        })
        .unwrap(),
    );
    function(
        "pthread_attr_getstacksize",
        handler!(|uc, a| {
            let (attributes, out) = (a.u32(), a.u32());
            forward(uc, bionic!("pthread_attr_getstacksize"), [attributes + 4, out, 0, 0])
        })
        .unwrap(),
    );
    function(
        "pthread_create",
        handler!(|uc, a| {
            let (thread, attributes, routine, argument) = (a.u32(), a.u32(), a.u32(), a.u32());
            error_code(forward(uc, bionic!("pthread_create"), [thread, inner(attributes), routine, argument]))
        })
        .unwrap(),
    );
    function(
        "pthread_once",
        handler!(|uc, a| {
            let (once, routine) = (a.u32(), a.u32());
            forward(uc, bionic!("pthread_once"), [once + 4, routine, 0, 0])
        })
        .unwrap(),
    );
    for name in ["pthread_rwlock_rdlock", "pthread_rwlock_wrlock", "pthread_rwlock_tryrdlock", "pthread_rwlock_trywrlock", "pthread_rwlock_unlock", "pthread_rwlock_destroy"] {
        let handler: Handler = |uc, a| {
            let lock = a.u32();
            let bionic = imports::handler_for(uc.get_data().current_call).expect("rwlock handler");
            error_code(forward(uc, bionic, [lock + 4, 0, 0, 0]))
        };
        function(name, handler);
    }
    function("pthread_rwlock_init", handler!(|uc, a| { let lock = a.u32(); error_code(forward(uc, bionic!("pthread_rwlock_init"), [lock + 4, 0, 0, 0])) }).unwrap());
}

// ---- OSAtomic, OSSpinLock ----

fn atomics() {
    fn add(a: &mut Args, amount: Option<u32>) -> Ret {
        let amount = amount.unwrap_or_else(|| a.u32());
        let pointer = a.u32();
        Ret::U32(mem().atomic_u32(pointer).fetch_add(amount, Ordering::SeqCst).wrapping_add(amount))
    }
    for name in ["OSAtomicAdd32", "OSAtomicAdd32Barrier"] {
        function(name, |_uc, a| add(a, None));
    }
    for name in ["OSAtomicIncrement32", "OSAtomicIncrement32Barrier"] {
        function(name, |_uc, a| add(a, Some(1)));
    }
    for name in ["OSAtomicDecrement32", "OSAtomicDecrement32Barrier"] {
        function(name, |_uc, a| add(a, Some(u32::MAX)));
    }
    for name in [
        "OSAtomicCompareAndSwap32",
        "OSAtomicCompareAndSwap32Barrier",
        "OSAtomicCompareAndSwapInt",
        "OSAtomicCompareAndSwapIntBarrier",
        "OSAtomicCompareAndSwapLong",
        "OSAtomicCompareAndSwapLongBarrier",
        "OSAtomicCompareAndSwapPtr",
        "OSAtomicCompareAndSwapPtrBarrier",
    ] {
        function(name, |_uc, a| {
            let (old, new, pointer) = (a.u32(), a.u32(), a.u32());
            Ret::U32(mem().atomic_u32(pointer).compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst).is_ok() as u32)
        });
    }
    for (name, operation) in [("OSAtomicOr32", 0), ("OSAtomicOr32Barrier", 0), ("OSAtomicAnd32", 1), ("OSAtomicAnd32Barrier", 1), ("OSAtomicXor32", 2), ("OSAtomicXor32Barrier", 2)] {
        let handler: Handler = match operation {
            0 => |_uc, a| { let (mask, pointer) = (a.u32(), a.u32()); Ret::U32(mem().atomic_u32(pointer).fetch_or(mask, Ordering::SeqCst) | mask) },
            1 => |_uc, a| { let (mask, pointer) = (a.u32(), a.u32()); Ret::U32(mem().atomic_u32(pointer).fetch_and(mask, Ordering::SeqCst) & mask) },
            _ => |_uc, a| { let (mask, pointer) = (a.u32(), a.u32()); Ret::U32(mem().atomic_u32(pointer).fetch_xor(mask, Ordering::SeqCst) ^ mask) },
        };
        function(name, handler);
    }
    function("OSMemoryBarrier", |_uc, _a| {
        std::sync::atomic::fence(Ordering::SeqCst);
        Ret::Void
    });
    function("OSSpinLockTry", |_uc, a| {
        let lock = a.u32();
        Ret::U32(mem().atomic_u32(lock).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() as u32)
    });
    function("OSSpinLockLock", |_uc, a| {
        let lock = mem().atomic_u32(a.u32());
        while lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            // The holder may need the guest lock to get going again.
            runtime::without_gil(std::thread::yield_now);
        }
        Ret::Void
    });
    function("OSSpinLockUnlock", |_uc, a| {
        mem().atomic_u32(a.u32()).store(0, Ordering::Release);
        Ret::Void
    });
}

// ---- data ----

/// Darwin armv7's FILE is 88 bytes; these programs never look inside.
const FILE_SIZE: u32 = 88;

/// A guest FILE standing for host file descriptor `fd`.
fn standard_file(fd: i32) -> u32 {
    let key = format!("guest32-file:{fd}");
    if let Some(&file) = shared().state.data_imports.get(&key) {
        return file;
    }
    let file = super::alloc_permanent(FILE_SIZE, 4);
    let mut shared = shared();
    shared.state.files.insert(file, HostFile::Descriptor(fd));
    *shared.state.data_imports.entry(key).or_insert(file)
}

fn data() {
    super::data("__stack_chk_guard", || {
        let mut guard = [0u8; 4];
        unsafe { libc::getrandom(guard.as_mut_ptr().cast(), 4, 0) };
        let address = super::alloc_permanent(4, 4);
        mem().write_bytes(address, &guard);
        address
    });
    super::data("_DefaultRuneLocale", make_rune_locale);
    for (name, fd) in [("__stdinp", 0), ("__stdoutp", 1), ("__stderrp", 2)] {
        super::data(name, move || {
            let pointer = super::alloc_permanent(4, 4);
            mem().write_u32(pointer, standard_file(fd));
            pointer
        });
    }
    // Older programs: FILE __sF[3].
    super::data("__sF", || {
        let base = super::alloc_permanent(FILE_SIZE * 3, 4);
        let mut shared = shared();
        for fd in 0..3 {
            shared.state.files.insert(base + fd * FILE_SIZE, HostFile::Descriptor(fd as i32));
        }
        base
    });
    // Block classes: placeholders until the Objective-C helper registers real ones.
    for name in [
        "_NSConcreteStackBlock",
        "_NSConcreteGlobalBlock",
        "_NSConcreteMallocBlock",
        "_NSConcreteAutoBlock",
        "_NSConcreteFinalizingBlock",
        "_NSConcreteWeakBlockVariable",
    ] {
        super::data_bytes(name, vec![0; 32], 4);
    }
    super::data_words("mach_task_self_", vec![0x103]);
    // vm_page_size and NDR_record: guest32/mach.rs.
    // The offset of NSObject's one ivar, isa.
    super::data_words("OBJC_IVAR_$_NSObject.isa", vec![0]);
}
