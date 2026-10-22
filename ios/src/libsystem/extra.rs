//! More of Apple's C library: double-precision math, stdio odds and ends,
//! string helpers, the environment, locales, uname, extended attributes,
//! Apple's string hash tables (strhash.c) and dyld's image queries.

use crate::symbols::{leak, leak_cstr, Registry};
use crate::varargs::{self, Args};
use std::collections::HashMap;
use std::sync::Mutex;

extern "C" {
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn acos(x: f64) -> f64;
    fn asin(x: f64) -> f64;
    fn atan(x: f64) -> f64;
    fn atan2(y: f64, x: f64) -> f64;
    fn exp(x: f64) -> f64;
    fn exp2(x: f64) -> f64;
    fn log(x: f64) -> f64;
    fn log10(x: f64) -> f64;
    fn pow(x: f64, y: f64) -> f64;
    fn modf(x: f64, whole: *mut f64) -> f64;
    fn modff(x: f32, whole: *mut f32) -> f32;
    fn lrintf(x: f32) -> i64;
    fn log2f(x: f32) -> f32;
    fn clearerr(file: usize);
    fn feof(file: usize) -> i32;
    fn ferror(file: usize) -> i32;
    fn fileno(file: usize) -> i32;
    fn fputc(c: i32, file: usize) -> i32;
    fn getc(file: usize) -> i32;
    fn strncat(dest: usize, src: usize, n: usize) -> usize;
    fn bsearch(key: usize, base: usize, count: usize, size: usize, compare: usize) -> usize;
    fn clock() -> i64;
    fn localtime(time: usize) -> usize;
    fn strftime(out: usize, size: usize, format: usize, tm: usize) -> usize;
    fn getcwd(buffer: usize, size: usize) -> usize;
    fn __cxa_atexit(f: usize, arg: usize, dso: usize) -> i32;
    fn ios_printf_impl(fmt: *const libc::c_char, va: *const u64) -> i32;
    fn ios_snprintf_impl(buf: *mut libc::c_char, size: usize, fmt: *const libc::c_char, va: *const u64) -> i32;
}

fn set_errno(linux: i32) {
    unsafe { *crate::libsystem::host_errno() = linux };
}

#[repr(C)]
struct SinCos {
    sin: f64,
    cos: f64,
}

extern "C" fn sincos_stret(x: f64) -> SinCos {
    SinCos { sin: x.sin(), cos: x.cos() }
}

// ----- stdio -----

/// perror: "text: message" for Apple's current errno, on standard error.
extern "C" fn ios_perror(text: *const libc::c_char) {
    let apple = unsafe { *crate::libsystem::io::error_location() };
    let message = crate::libsystem::io::linux_errno(apple)
        .map(|linux| unsafe { String::from_utf8_lossy(varargs::c_bytes(libc::strerror(linux))).into_owned() })
        .unwrap_or_else(|| format!("Unknown error: {apple}"));
    let prefix = if text.is_null() { Vec::new() } else { unsafe { varargs::c_bytes(text).to_vec() } };
    let mut line = prefix;
    if !line.is_empty() {
        line.extend_from_slice(b": ");
    }
    line.extend_from_slice(message.as_bytes());
    line.push(b'\n');
    unsafe { libc::write(2, line.as_ptr() as *const _, line.len()) };
}

unsafe extern "C" fn ios_vprintf(fmt: *const libc::c_char, va: *const u64) -> i32 {
    ios_printf_impl(fmt, va)
}

crate::libsystem::libc::variadic!(ios_asprintf, 2, ios_asprintf_impl);

/// asprintf: the formatted text in fresh malloc'd memory.
#[no_mangle]
unsafe extern "C" fn ios_asprintf_impl(out: *mut *mut u8, fmt: *const libc::c_char, va: *const u64) -> i32 {
    let text = varargs::format(varargs::c_bytes(fmt), &mut Args::new(va), false);
    let memory = libc::malloc(text.len() + 1) as *mut u8;
    if memory.is_null() {
        *out = std::ptr::null_mut();
        set_errno(libc::ENOMEM);
        return -1;
    }
    std::ptr::copy_nonoverlapping(text.as_ptr(), memory, text.len());
    *memory.add(text.len()) = 0;
    *out = memory;
    text.len() as i32
}

unsafe extern "C" fn vsnprintf_chk(buf: *mut libc::c_char, size: usize, _flag: i32, _room: usize, fmt: *const libc::c_char, va: *const u64) -> i32 {
    ios_snprintf_impl(buf, size, fmt, va)
}

/// __memcpy_chk: memcpy that stops the game when the copy is bigger than
/// the room the compiler knew about (Apple aborts there too).
unsafe extern "C" fn memcpy_chk(dest: *mut u8, src: *const u8, length: usize, room: usize) -> *mut u8 {
    if length > room {
        crate::log!("STOP: the game overflowed a buffer (memcpy of {length} bytes into {room})");
        crate::report::finish(1);
    }
    std::ptr::copy(src, dest, length);
    dest
}

// ----- strings -----

/// strlcpy: copies what fits (always ending the text), returns the source length.
unsafe extern "C" fn strlcpy(dest: *mut u8, src: *const libc::c_char, size: usize) -> usize {
    let source = varargs::c_bytes(src);
    if size > 0 {
        let count = source.len().min(size - 1);
        std::ptr::copy(source.as_ptr(), dest, count);
        *dest.add(count) = 0;
    }
    source.len()
}

unsafe extern "C" fn bcopy(src: *const u8, dest: *mut u8, length: usize) {
    std::ptr::copy(src, dest, length);
}

// ----- environment (an iPhone app's, not this computer's) -----

static ENVIRONMENT: Mutex<Option<HashMap<Vec<u8>, usize>>> = Mutex::new(None);

fn with_environment<T>(work: impl FnOnce(&mut HashMap<Vec<u8>, usize>) -> T) -> T {
    let mut table = ENVIRONMENT.lock().unwrap_or_else(|e| e.into_inner());
    let table = table.get_or_insert_with(|| {
        let home = crate::fw::foundation::home();
        [
            ("HOME", home.clone()),
            ("CFFIXED_USER_HOME", home.clone()),
            ("TMPDIR", format!("{home}/tmp/")),
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("SHELL", "/bin/sh".into()),
            ("USER", "mobile".into()),
            ("LOGNAME", "mobile".into()),
            ("__CF_USER_TEXT_ENCODING", "0x1F5:0:0".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.as_bytes().to_vec(), leak_cstr(&value)))
        .collect()
    });
    work(table)
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.contains(&b'=')
}

extern "C" fn ios_getenv(name: *const libc::c_char) -> usize {
    if name.is_null() {
        return 0;
    }
    let name = unsafe { varargs::c_bytes(name) };
    with_environment(|table| table.get(name).copied().unwrap_or(0))
}

/// Values stay readable after a change (getenv hands out their address).
extern "C" fn ios_setenv(name: *const libc::c_char, value: *const libc::c_char, overwrite: i32) -> i32 {
    let name = if name.is_null() { &[][..] } else { unsafe { varargs::c_bytes(name) } };
    if !valid_name(name) {
        set_errno(libc::EINVAL);
        return -1;
    }
    let value = if value.is_null() { &[][..] } else { unsafe { varargs::c_bytes(value) } };
    with_environment(|table| {
        if overwrite != 0 || !table.contains_key(name) {
            table.insert(name.to_vec(), leak_cstr(&String::from_utf8_lossy(value)));
        }
    });
    0
}

extern "C" fn ios_unsetenv(name: *const libc::c_char) -> i32 {
    let name = if name.is_null() { &[][..] } else { unsafe { varargs::c_bytes(name) } };
    if !valid_name(name) {
        set_errno(libc::EINVAL);
        return -1;
    }
    with_environment(|table| table.remove(name));
    0
}

// ----- locales: only the C locale (plus UTF-8 characters), like an app with no language set -----

const APPLE_LC_ALL: i32 = 0;
const APPLE_LC_CTYPE: i32 = 2;
const APPLE_LC_LAST: i32 = 6;

static LOCALE: Mutex<[&'static str; 6]> = Mutex::new(["C"; 6]);

fn locale_name(category: i32, asked: &str) -> Option<&'static str> {
    match asked {
        "" | "C" | "POSIX" => Some("C"),
        "UTF-8" | "C.UTF-8" | "en_US.UTF-8" if category == APPLE_LC_CTYPE => Some("UTF-8"),
        _ => None,
    }
}

fn locale_answer(names: &[&'static str; 6], category: i32) -> usize {
    static ANSWERS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    let text = if category == APPLE_LC_ALL {
        if names.iter().all(|n| *n == names[0]) { names[0].to_string() } else { names.join("/") }
    } else {
        names[category as usize - 1].to_string()
    };
    *ANSWERS.lock().unwrap().get_or_insert_with(Default::default).entry(text.clone()).or_insert_with(|| leak_cstr(&text))
}

/// setlocale with Apple's category numbers. The system's own locale is left
/// alone (this layer formats numbers for the game in the C locale).
extern "C" fn ios_setlocale(category: i32, asked: *const libc::c_char) -> usize {
    if !(APPLE_LC_ALL..=APPLE_LC_LAST).contains(&category) {
        set_errno(libc::EINVAL);
        return 0;
    }
    let mut names = LOCALE.lock().unwrap();
    if !asked.is_null() {
        let asked = unsafe { String::from_utf8_lossy(varargs::c_bytes(asked)).into_owned() };
        if category == APPLE_LC_ALL {
            match locale_name(APPLE_LC_ALL, &asked) {
                Some(name) => names.fill(name),
                None => {
                    crate::log!("setlocale: locale '{asked}' isn't here (answered none)");
                    return 0;
                }
            }
        } else {
            match locale_name(category, &asked) {
                Some(name) => names[category as usize - 1] = name,
                None => {
                    crate::log!("setlocale {category}: locale '{asked}' isn't here (answered none)");
                    return 0;
                }
            }
        }
    }
    locale_answer(&names, category)
}

// ----- system names -----

/// uname: the same iPhone the rest of the layer describes (sysctl).
extern "C" fn ios_uname(out: *mut [u8; 256]) -> i32 {
    let fields = [
        "Darwin".to_string(),
        "iPhone".to_string(),
        "14.0.0".to_string(),
        "Darwin Kernel Version 14.0.0: Sun Mar 29 19:47:37 PDT 2015; root:xnu-2784.20.34~2/RELEASE_ARM64_S5L8960X".to_string(),
        crate::libsystem::io::device_model(),
    ];
    for (index, text) in fields.iter().enumerate() {
        unsafe {
            let field = &mut *out.add(index);
            field.fill(0);
            let count = text.len().min(255);
            field[..count].copy_from_slice(&text.as_bytes()[..count]);
        }
    }
    0
}

extern "C" fn getpagesize() -> i32 {
    16384
}

extern "C" fn malloc_size(pointer: *mut libc::c_void) -> usize {
    if pointer.is_null() {
        return 0;
    }
    unsafe { libc::malloc_usable_size(pointer) }
}

extern "C" fn call_exit_function(function: usize) {
    let f: extern "C" fn() = unsafe { std::mem::transmute(function) };
    f();
}

/// atexit through __cxa_atexit (Android's atexit isn't a library function).
extern "C" fn ios_atexit(function: usize) -> i32 {
    unsafe { __cxa_atexit(call_exit_function as usize, function, 0) }
}

// ----- extended attributes -----
// Kept for the run in memory: on the phone, app folders don't take them.

const APPLE_ENOATTR: i32 = 93;
const XATTR_CREATE: i32 = 2;
const XATTR_REPLACE: i32 = 4;

static ATTRIBUTES: Mutex<Option<HashMap<(Vec<u8>, Vec<u8>), Vec<u8>>>> = Mutex::new(None);

/// The file's own identity (device, inode) so different spellings of a path agree.
fn attribute_key(path: *const libc::c_char, name: *const libc::c_char) -> Option<(Vec<u8>, Vec<u8>)> {
    if path.is_null() || name.is_null() {
        set_errno(libc::EINVAL);
        return None;
    }
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    let host = unsafe { crate::fw::paths::host_path_c(path) };
    if unsafe { libc::stat(host.as_ptr(), &mut status) } != 0 {
        return None;
    }
    let file = format!("{}:{}", status.st_dev, status.st_ino).into_bytes();
    Some((file, unsafe { varargs::c_bytes(name).to_vec() }))
}

extern "C" fn ios_setxattr(path: *const libc::c_char, name: *const libc::c_char, value: *const u8, size: usize, _position: u32, options: i32) -> i32 {
    let Some(key) = attribute_key(path, name) else { return -1 };
    let mut table = ATTRIBUTES.lock().unwrap();
    let table = table.get_or_insert_with(Default::default);
    let exists = table.contains_key(&key);
    if options & XATTR_CREATE != 0 && exists {
        set_errno(libc::EEXIST);
        return -1;
    }
    if options & XATTR_REPLACE != 0 && !exists {
        crate::libsystem::io::set_apple_errno(APPLE_ENOATTR);
        return -1;
    }
    let bytes = if value.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(value, size).to_vec() } };
    table.insert(key, bytes);
    0
}

extern "C" fn ios_getxattr(path: *const libc::c_char, name: *const libc::c_char, value: *mut u8, size: usize, _position: u32, _options: i32) -> isize {
    let Some(key) = attribute_key(path, name) else { return -1 };
    let table = ATTRIBUTES.lock().unwrap();
    let Some(bytes) = table.as_ref().and_then(|t| t.get(&key)) else {
        crate::libsystem::io::set_apple_errno(APPLE_ENOATTR);
        return -1;
    };
    if value.is_null() {
        return bytes.len() as isize;
    }
    if size < bytes.len() {
        set_errno(libc::ERANGE);
        return -1;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), value, bytes.len()) };
    bytes.len() as isize
}

extern "C" fn ios_removexattr(path: *const libc::c_char, name: *const libc::c_char, _options: i32) -> i32 {
    let Some(key) = attribute_key(path, name) else { return -1 };
    if ATTRIBUTES.lock().unwrap().get_or_insert_with(Default::default).remove(&key).is_none() {
        crate::libsystem::io::set_apple_errno(APPLE_ENOATTR);
        return -1;
    }
    0
}

// ----- Apple's string hash tables (Libc strhash.c: hash_create / hash_search) -----

/// Apple's hash_node: key, data, next. Nodes live as long as their table.
#[repr(C)]
struct HashNode {
    key: usize,
    data: usize,
    next: usize,
}

struct HashTable {
    nodes: Mutex<HashMap<Vec<u8>, Box<HashNode>>>,
}

/// hash_create: an empty table (the size is only a hint for buckets).
extern "C" fn hash_create(size: i32) -> usize {
    if size < 0 {
        return 0;
    }
    leak(HashTable { nodes: Mutex::new(HashMap::new()) })
}

/// hash_search: with no data, finds the key's data (or null). With data, a
/// new key is stored (the key copied) and its node is returned; a known key
/// keeps its data and that is returned, unless a replace function is given,
/// which is called with the old data before the new data goes in (null back).
extern "C" fn hash_search(table: usize, key: *const libc::c_char, data: usize, replace: usize) -> usize {
    if table == 0 || key.is_null() {
        return 0;
    }
    let table = unsafe { &*(table as *const HashTable) };
    let key = unsafe { varargs::c_bytes(key) };
    let mut nodes = table.nodes.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(node) = nodes.get_mut(key) {
        if replace == 0 {
            return node.data;
        }
        let old = node.data;
        node.data = data;
        drop(nodes);
        let replace: extern "C" fn(usize) = unsafe { std::mem::transmute(replace) };
        replace(old);
        return 0;
    }
    if data == 0 {
        return 0;
    }
    let mut copy = key.to_vec();
    copy.push(0);
    let node = Box::new(HashNode { key: Box::leak(copy.into_boxed_slice()).as_ptr() as usize, data, next: 0 });
    let address = &*node as *const HashNode as usize;
    nodes.insert(key.to_vec(), node);
    address
}

// ----- dyld: the game is the one image we have -----

extern "C" fn dyld_image_count() -> u32 {
    crate::loader::try_loaded().is_some() as u32
}

extern "C" fn dyld_get_image_name(index: u32) -> usize {
    static NAME: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if index != 0 || crate::loader::try_loaded().is_none() {
        return 0;
    }
    *NAME.get_or_init(|| leak_cstr(&crate::fw::paths::apple_executable_path()))
}

extern "C" fn dyld_get_image_vmaddr_slide(index: u32) -> isize {
    match crate::loader::try_loaded() {
        Some(loaded) if index == 0 => loaded.slide as isize,
        _ => 0,
    }
}

/// Apple's NXArchInfo: name, cputype, cpusubtype, byte order, description.
#[repr(C)]
struct ArchInfo {
    name: usize,
    cpu_type: i32,
    cpu_subtype: i32,
    byte_order: i32,
    description: usize,
}

extern "C" fn nx_get_local_arch_info() -> usize {
    static INFO: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *INFO.get_or_init(|| {
        leak(ArchInfo { name: leak_cstr("arm64"), cpu_type: 0x0100_000C, cpu_subtype: 0, byte_order: 1, description: leak_cstr("arm64 v8") })
    })
}

pub fn register(r: &mut Registry) {
    macro_rules! host {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), $name as usize); )* };
    }
    host!(
        sin, cos, tan, acos, asin, atan, atan2, exp, exp2, log, log10, pow, modf, modff, lrintf, log2f,
        clearerr, feof, ferror, fileno, fputc, getc, strncat, bsearch, clock, localtime, strftime, getcwd,
        strlcpy, bcopy, getpagesize, malloc_size, hash_create, hash_search
    );
    r.function("flock", libc::flock as usize);
    r.function("ftruncate", libc::ftruncate as usize);
    r.function("__sincos_stret", sincos_stret as usize);
    r.function("perror", ios_perror as usize);
    r.function("vprintf", ios_vprintf as usize);
    r.function("asprintf", ios_asprintf as usize);
    r.function("__vsnprintf_chk", vsnprintf_chk as usize);
    r.function("__memcpy_chk", memcpy_chk as usize);
    r.function("getenv", ios_getenv as usize);
    r.function("setenv", ios_setenv as usize);
    r.function("unsetenv", ios_unsetenv as usize);
    r.function("setlocale", ios_setlocale as usize);
    r.function("uname", ios_uname as usize);
    r.function("atexit", ios_atexit as usize);
    r.function("setxattr", ios_setxattr as usize);
    r.function("getxattr", ios_getxattr as usize);
    r.function("removexattr", ios_removexattr as usize);
    r.function("_dyld_image_count", dyld_image_count as usize);
    r.function("_dyld_get_image_name", dyld_get_image_name as usize);
    r.function("_dyld_get_image_vmaddr_slide", dyld_get_image_vmaddr_slide as usize);
    r.function("NXGetLocalArchInfo", nx_get_local_arch_info as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn hash_tables_keep_first_data_unless_replaced() {
        let table = hash_create(0);
        let (key, other) = (c"Swift.Int", c"Swift.String");
        assert_eq!(hash_search(table, key.as_ptr(), 0, 0), 0, "empty table found something");
        assert_ne!(hash_search(table, key.as_ptr(), 11, 0), 0, "insert gave no node");
        assert_eq!(hash_search(table, key.as_ptr(), 0, 0), 11);
        assert_eq!(hash_search(table, key.as_ptr(), 22, 0), 11, "a known key took new data without a replace function");
        assert_eq!(hash_search(table, other.as_ptr(), 0, 0), 0);
        static OLD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        extern "C" fn replaced(old: usize) {
            OLD.store(old, std::sync::atomic::Ordering::SeqCst);
        }
        assert_eq!(hash_search(table, key.as_ptr(), 33, replaced as usize), 0);
        assert_eq!(OLD.load(std::sync::atomic::Ordering::SeqCst), 11);
        assert_eq!(hash_search(table, key.as_ptr(), 0, 0), 33);
    }

    #[test]
    fn setlocale_uses_apple_categories() {
        let name = |p: usize| unsafe { CStr::from_ptr(p as *const libc::c_char).to_str().unwrap().to_string() };
        assert_eq!(name(ios_setlocale(APPLE_LC_ALL, c"".as_ptr())), "C");
        assert_eq!(name(ios_setlocale(APPLE_LC_CTYPE, c"UTF-8".as_ptr())), "UTF-8");
        assert_eq!(name(ios_setlocale(APPLE_LC_ALL, std::ptr::null())), "C/UTF-8/C/C/C/C");
        assert_eq!(ios_setlocale(4, c"de_DE".as_ptr()), 0, "a missing locale was accepted");
        assert_eq!(name(ios_setlocale(4, std::ptr::null())), "C");
        assert_eq!(name(ios_setlocale(APPLE_LC_ALL, c"C".as_ptr())), "C");
    }
}
