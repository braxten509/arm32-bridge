//! Apple file, memory-map and system-information calls. Apple's flag
//! numbers, error numbers and structure layouts differ from Linux/Android,
//! so these translate both ways.

use crate::symbols::{leak, Registry};
use std::cell::Cell;
use std::ffi::CStr;

pub static STDOUT_CELL: CellAddr = CellAddr::new();

/// A fixed spot holding a pointer, filled at start-up.
pub struct CellAddr(std::sync::atomic::AtomicUsize);

impl CellAddr {
    pub const fn new() -> Self {
        CellAddr(std::sync::atomic::AtomicUsize::new(0))
    }
    pub fn as_ptr(&self) -> *const usize {
        self.0.load(std::sync::atomic::Ordering::Relaxed) as *const usize
    }
}

// ----- errno -----

pub(crate) fn apple_errno(linux: i32) -> i32 {
    match linux {
        libc::EAGAIN => 35,
        0..=34 => linux,
        libc::EINPROGRESS => 36,
        libc::EALREADY => 37,
        libc::ENOTSOCK => 38,
        libc::EDESTADDRREQ => 39,
        libc::EMSGSIZE => 40,
        libc::EPROTOTYPE => 41,
        libc::ENOPROTOOPT => 42,
        libc::EPROTONOSUPPORT => 43,
        libc::EOPNOTSUPP => 45,
        libc::EAFNOSUPPORT => 47,
        libc::EADDRINUSE => 48,
        libc::EADDRNOTAVAIL => 49,
        libc::ENETDOWN => 50,
        libc::ENETUNREACH => 51,
        libc::ECONNABORTED => 53,
        libc::ECONNRESET => 54,
        libc::ENOBUFS => 55,
        libc::EISCONN => 56,
        libc::ENOTCONN => 57,
        libc::ETIMEDOUT => 60,
        libc::ECONNREFUSED => 61,
        libc::ELOOP => 62,
        libc::ENAMETOOLONG => 63,
        libc::EHOSTUNREACH => 65,
        libc::ENOTEMPTY => 66,
        libc::EDEADLK => 11,
        libc::ENOSYS => 78,
        libc::EOVERFLOW => 84,
        libc::ECANCELED => 89,
        other => other,
    }
}

thread_local! {
    static APPLE_ERRNO: Cell<i32> = const { Cell::new(0) };
}

/// Apple's errno cell. A host call that failed since the last look left a
/// non-zero host errno: translate it into the cell and clear the host value,
/// so values the game writes into the cell itself stay until the next error.
pub(crate) extern "C" fn error_location() -> *mut i32 {
    unsafe {
        let host = crate::libsystem::host_errno();
        let value = *host;
        APPLE_ERRNO.with(|cell| {
            if value != 0 {
                cell.set(apple_errno(value));
                *host = 0;
            }
            cell.as_ptr()
        })
    }
}

/// Sets Apple's errno to an Apple number that has no Linux twin (ENOATTR).
pub(crate) fn set_apple_errno(apple: i32) {
    unsafe { *crate::libsystem::host_errno() = 0 };
    APPLE_ERRNO.with(|cell| cell.set(apple));
}

/// The Linux number for an Apple errno (for the system's messages).
pub(crate) fn linux_errno(apple: i32) -> Option<i32> {
    (0..256).find(|&linux| apple_errno(linux) == apple)
}

// ----- open / fcntl -----

const APPLE_O_NONBLOCK: i32 = 0x4;
const APPLE_O_APPEND: i32 = 0x8;
const APPLE_O_CREAT: i32 = 0x200;
const APPLE_O_TRUNC: i32 = 0x400;
const APPLE_O_EXCL: i32 = 0x800;

fn open_flags(apple: i32) -> i32 {
    let mut flags = apple & 3;
    if apple & APPLE_O_NONBLOCK != 0 {
        flags |= libc::O_NONBLOCK;
    }
    if apple & APPLE_O_APPEND != 0 {
        flags |= libc::O_APPEND;
    }
    if apple & APPLE_O_CREAT != 0 {
        flags |= libc::O_CREAT;
    }
    if apple & APPLE_O_TRUNC != 0 {
        flags |= libc::O_TRUNC;
    }
    if apple & APPLE_O_EXCL != 0 {
        flags |= libc::O_EXCL;
    }
    flags
}

fn apple_status_flags(linux: i32) -> i32 {
    let mut flags = linux & 3;
    if linux & libc::O_NONBLOCK != 0 {
        flags |= APPLE_O_NONBLOCK;
    }
    if linux & libc::O_APPEND != 0 {
        flags |= APPLE_O_APPEND;
    }
    flags
}

crate::libsystem::libc::variadic!(ios_open, 2, ios_open_impl);
crate::libsystem::libc::variadic!(ios_fcntl, 2, ios_fcntl_impl);

#[no_mangle]
unsafe extern "C" fn ios_open_impl(path: *const libc::c_char, flags: i32, va: *const u64) -> i32 {
    let mode = if flags & APPLE_O_CREAT != 0 { *va as u32 } else { 0 };
    let path = crate::fw::paths::host_path_c(path);
    crate::libsystem::threads::blocking(|| libc::open(path.as_ptr(), open_flags(flags), mode))
}

#[no_mangle]
unsafe extern "C" fn ios_fcntl_impl(fd: i32, command: i32, va: *const u64) -> i32 {
    let arg = *va;
    match command {
        libc::F_GETFD | libc::F_SETFD | libc::F_DUPFD => libc::fcntl(fd, command, arg as i32),
        libc::F_GETFL => {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 {
                flags
            } else {
                apple_status_flags(flags)
            }
        }
        libc::F_SETFL => libc::fcntl(fd, libc::F_SETFL, open_flags(arg as i32 & !3)),
        // F_NOCACHE (48), F_RDAHEAD (45), F_FULLFSYNC (51): hints only.
        45 | 48 | 51 => 0,
        _ => {
            crate::log!("fcntl command {command} not translated, refusing");
            *crate::libsystem::host_errno() = libc::EINVAL;
            -1
        }
    }
}

// ----- stat -----

unsafe fn apple_stat(host: &libc::stat, out: *mut u8) {
    std::ptr::write_bytes(out, 0, 144);
    *(out as *mut i32) = host.st_dev as i32;
    *(out.add(4) as *mut u16) = host.st_mode as u16;
    *(out.add(6) as *mut u16) = host.st_nlink as u16;
    *(out.add(8) as *mut u64) = host.st_ino;
    *(out.add(16) as *mut u32) = host.st_uid;
    *(out.add(20) as *mut u32) = host.st_gid;
    *(out.add(24) as *mut i32) = host.st_rdev as i32;
    let times = [
        (host.st_atime, host.st_atime_nsec),
        (host.st_mtime, host.st_mtime_nsec),
        (host.st_ctime, host.st_ctime_nsec),
        (host.st_ctime, host.st_ctime_nsec),
    ];
    for (index, (seconds, nanos)) in times.iter().enumerate() {
        *(out.add(32 + index * 16) as *mut i64) = *seconds;
        *(out.add(40 + index * 16) as *mut i64) = *nanos;
    }
    *(out.add(96) as *mut i64) = host.st_size;
    *(out.add(104) as *mut i64) = host.st_blocks;
    *(out.add(112) as *mut i32) = host.st_blksize as i32;
}

extern "C" fn ios_stat(path: *const libc::c_char, out: *mut u8) -> i32 {
    unsafe {
        let path = crate::fw::paths::host_path_c(path);
        let mut host: libc::stat = std::mem::zeroed();
        let result = libc::stat(path.as_ptr(), &mut host);
        if result == 0 {
            apple_stat(&host, out);
        }
        result
    }
}

extern "C" fn ios_lstat(path: *const libc::c_char, out: *mut u8) -> i32 {
    unsafe {
        let path = crate::fw::paths::host_path_c(path);
        let mut host: libc::stat = std::mem::zeroed();
        let result = libc::lstat(path.as_ptr(), &mut host);
        if result == 0 {
            apple_stat(&host, out);
        }
        result
    }
}

extern "C" fn ios_fstat(fd: i32, out: *mut u8) -> i32 {
    unsafe {
        let mut host: libc::stat = std::mem::zeroed();
        let result = libc::fstat(fd, &mut host);
        if result == 0 {
            apple_stat(&host, out);
        }
        result
    }
}

// ----- directories (Apple dirent: ino 8, seekoff 8, reclen 2, namlen 2, type 1, name 1024) -----

thread_local! {
    static DIRENT: std::cell::RefCell<Box<[u8; 1048]>> = std::cell::RefCell::new(Box::new([0; 1048]));
}

extern "C" fn ios_opendir(path: *const libc::c_char) -> *mut libc::DIR {
    let path = unsafe { crate::fw::paths::host_path_c(path) };
    unsafe { libc::opendir(path.as_ptr()) }
}

extern "C" fn ios_readdir(dir: *mut libc::DIR) -> *mut u8 {
    unsafe {
        let entry = libc::readdir(dir);
        if entry.is_null() {
            return std::ptr::null_mut();
        }
        let name = CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes();
        DIRENT.with(|cell| {
            let mut buffer = cell.borrow_mut();
            buffer.fill(0);
            let out = buffer.as_mut_ptr();
            *(out as *mut u64) = (*entry).d_ino;
            *(out.add(16) as *mut u16) = 1048;
            *(out.add(18) as *mut u16) = name.len().min(1023) as u16;
            *out.add(20) = (*entry).d_type;
            std::ptr::copy_nonoverlapping(name.as_ptr(), out.add(21), name.len().min(1023));
            out
        })
    }
}

// ----- other path calls go through the app-folder mapping -----

macro_rules! path_call {
    ($name:ident, $host:ident $(, $arg:ident : $ty:ty)*) => {
        extern "C" fn $name(path: *const libc::c_char $(, $arg: $ty)*) -> i32 {
            unsafe {
                let path = crate::fw::paths::host_path_c(path);
                libc::$host(path.as_ptr() $(, $arg)*)
            }
        }
    };
}

path_call!(ios_access, access, mode: i32);
path_call!(ios_chdir, chdir);
path_call!(ios_chmod, chmod, mode: libc::mode_t);
path_call!(ios_rmdir, rmdir);
path_call!(ios_unlink, unlink);
path_call!(ios_mkdir, mkdir, mode: libc::mode_t);

extern "C" fn ios_rename(from: *const libc::c_char, to: *const libc::c_char) -> i32 {
    unsafe {
        let from = crate::fw::paths::host_path_c(from);
        let to = crate::fw::paths::host_path_c(to);
        libc::rename(from.as_ptr(), to.as_ptr())
    }
}

pub(crate) extern "C" fn ios_fopen(path: *const libc::c_char, mode: *const libc::c_char) -> *mut libc::FILE {
    unsafe {
        let path = crate::fw::paths::host_path_c(path);
        libc::fopen(path.as_ptr(), mode)
    }
}

extern "C" fn ios_utimes(path: *const libc::c_char, times: *const libc::timeval) -> i32 {
    unsafe {
        let path = crate::fw::paths::host_path_c(path);
        libc::utimes(path.as_ptr(), times)
    }
}

extern "C" fn ios_mkstemp(template: *mut libc::c_char) -> i32 {
    unsafe { libc::mkstemp(template) }
}

// ----- mmap -----

pub(crate) extern "C" fn ios_mmap(address: usize, length: usize, protection: i32, apple_flags: i32, fd: i32, offset: i64) -> usize {
    let mut flags = apple_flags & (libc::MAP_SHARED | libc::MAP_PRIVATE | libc::MAP_FIXED);
    if apple_flags & 0x1000 != 0 {
        flags |= libc::MAP_ANONYMOUS;
    }
    let protection = protection & (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC);
    // Plain memory goes in the near stretch, with the game's malloc blocks.
    if flags & (libc::MAP_ANONYMOUS | libc::MAP_FIXED) == libc::MAP_ANONYMOUS && flags & libc::MAP_SHARED == 0 {
        if let Some(address) = super::near::map(length, protection, 0) {
            return address;
        }
    }
    unsafe { libc::mmap(address as *mut _, length, protection, flags, if flags & libc::MAP_ANONYMOUS != 0 { -1 } else { fd }, offset) as usize }
}

pub(crate) extern "C" fn ios_munmap(address: usize, length: usize) -> i32 {
    if super::near::unmap(address, length) {
        return 0;
    }
    unsafe { libc::munmap(address as *mut _, length) }
}

// ----- system information -----

pub fn device_model() -> String {
    std::env::var("IOS_DEVICE").unwrap_or_else(|_| "iPhone6,1".into())
}

fn sysctl_value(name: &str) -> Option<Vec<u8>> {
    let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.clamp(1, 2) as i32;
    Some(match name {
        "hw.machine" => {
            let mut bytes = device_model().into_bytes();
            bytes.push(0);
            bytes
        }
        "hw.model" => b"N51AP\0".to_vec(),
        "kern.osversion" => b"12D508\0".to_vec(),
        "kern.osrelease" => b"14.0.0\0".to_vec(),
        "kern.ostype" => b"Darwin\0".to_vec(),
        "hw.ncpu" | "hw.activecpu" | "hw.physicalcpu" | "hw.logicalcpu" => cpus.to_le_bytes().to_vec(),
        "hw.memsize" => (1u64 << 30).to_le_bytes().to_vec(),
        "hw.physmem" | "hw.usermem" => (1u32 << 30).to_le_bytes().to_vec(),
        "hw.pagesize" => 16384i32.to_le_bytes().to_vec(),
        "hw.cpufrequency" | "hw.cpufrequency_max" => 1_300_000_000u64.to_le_bytes().to_vec(),
        "hw.busfrequency" => 100_000_000u64.to_le_bytes().to_vec(),
        "hw.cachelinesize" => 64i32.to_le_bytes().to_vec(),
        "hw.l1dcachesize" | "hw.l1icachesize" => 65536i32.to_le_bytes().to_vec(),
        "hw.l2cachesize" => (1i32 << 20).to_le_bytes().to_vec(),
        "hw.cputype" => 0x0100_000Ci32.to_le_bytes().to_vec(),
        "hw.cpusubtype" => 0i32.to_le_bytes().to_vec(),
        _ => return None,
    })
}

unsafe fn answer(value: &[u8], old: *mut u8, old_length: *mut usize) -> i32 {
    if old_length.is_null() {
        return 0;
    }
    if old.is_null() {
        *old_length = value.len();
        return 0;
    }
    if *old_length < value.len() {
        *crate::libsystem::host_errno() = libc::ENOMEM;
        return -1;
    }
    std::ptr::copy_nonoverlapping(value.as_ptr(), old, value.len());
    *old_length = value.len();
    0
}

extern "C" fn ios_sysctlbyname(name: *const libc::c_char, old: *mut u8, old_length: *mut usize, _new: usize, _new_length: usize) -> i32 {
    let name = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
    match sysctl_value(&name) {
        Some(value) => unsafe { answer(&value, old, old_length) },
        None => {
            crate::log!("sysctl {name}: not known, answered 'no such entry'");
            unsafe { *crate::libsystem::host_errno() = libc::ENOENT };
            -1
        }
    }
}

extern "C" fn ios_sysctl(mib: *const i32, count: u32, old: *mut u8, old_length: *mut usize, _new: usize, _new_length: usize) -> i32 {
    let parts: Vec<i32> = (0..count as usize).map(|i| unsafe { *mib.add(i) }).collect();
    let name = match parts.as_slice() {
        [6, 1, ..] => Some("hw.machine"),
        [6, 2, ..] => Some("hw.model"),
        [6, 3, ..] => Some("hw.ncpu"),
        [6, 5, ..] => Some("hw.physmem"),
        [6, 7, ..] => Some("hw.pagesize"),
        [6, 24, ..] => Some("hw.memsize"),
        [6, 15, ..] => Some("hw.cpufrequency"),
        [1, 65, ..] => Some("kern.osversion"),
        _ => None,
    };
    if let Some(name) = name {
        return unsafe { answer(&sysctl_value(name).unwrap(), old, old_length) };
    }
    if let [1, 14, ..] = parts.as_slice() {
        // Process information (used to check for a debugger): all zero = not traced.
        return unsafe { answer(&[0u8; 648], old, old_length) };
    }
    crate::log!("sysctl {parts:?}: not known, answered 'no such entry'");
    unsafe { *crate::libsystem::host_errno() = libc::ENOENT };
    -1
}

extern "C" fn ios_sysconf(name: i32) -> i64 {
    let host = match name {
        3 => libc::_SC_CLK_TCK,
        5 => libc::_SC_OPEN_MAX,
        29 => return 16384,
        57 => libc::_SC_NPROCESSORS_CONF,
        58 => libc::_SC_NPROCESSORS_ONLN,
        200 => return (1i64 << 30) / 16384,
        _ => {
            crate::log!("sysconf {name}: not known");
            return -1;
        }
    };
    unsafe { libc::sysconf(host) }
}

extern "C" fn ios_getprogname() -> usize {
    static NAME: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *NAME.get_or_init(|| {
        let name = crate::loader::try_loaded()
            .map(|l| l.path.rsplit('/').next().unwrap_or("game").to_string())
            .unwrap_or_else(|| "game".into());
        crate::symbols::leak_cstr(&name)
    })
}

extern "C" fn ios_ns_get_executable_path(buffer: *mut libc::c_char, size: *mut u32) -> i32 {
    let path = crate::fw::paths::apple_executable_path();
    let bytes = path.as_bytes();
    unsafe {
        if (*size as usize) < bytes.len() + 1 {
            *size = bytes.len() as u32 + 1;
            return -1;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    0
}

extern "C" fn ios_gethostname(name: *mut libc::c_char, length: usize) -> i32 {
    unsafe { crate::varargs::write_bounded(name, length, b"iPhone") };
    0
}

pub fn register(r: &mut Registry) {
    let stdin = unsafe { libc::fdopen(0, c"r".as_ptr()) } as usize;
    let stdout = crate::output::game_file(1) as usize;
    let stderr = crate::output::game_file(2) as usize;
    let stdout_cell = leak(stdout);
    STDOUT_CELL.0.store(stdout_cell, std::sync::atomic::Ordering::Relaxed);
    r.data("__stdinp", leak(stdin));
    r.data("__stdoutp", stdout_cell);
    r.data("__stderrp", leak(stderr));
    r.function("__error", error_location as usize);
    r.function("open", ios_open as usize);
    r.function("fcntl", ios_fcntl as usize);
    r.function("stat", ios_stat as usize);
    r.function("lstat", ios_lstat as usize);
    r.function("fstat", ios_fstat as usize);
    r.function("opendir", ios_opendir as usize);
    r.function("readdir", ios_readdir as usize);
    r.function("closedir", libc::closedir as usize);
    r.function("access", ios_access as usize);
    r.function("chdir", ios_chdir as usize);
    r.function("chmod", ios_chmod as usize);
    r.function("rmdir", ios_rmdir as usize);
    r.function("unlink", ios_unlink as usize);
    r.function("mkdir", ios_mkdir as usize);
    r.function("rename", ios_rename as usize);
    r.function("fopen", ios_fopen as usize);
    r.function("utimes", ios_utimes as usize);
    r.function("mkstemp", ios_mkstemp as usize);
    r.function("mmap", ios_mmap as usize);
    r.function("munmap", ios_munmap as usize);
    r.function("mprotect", libc::mprotect as usize);
    r.function("close", libc::close as usize);
    r.function("lseek", libc::lseek as usize);
    r.function("fflush", libc::fflush as usize);
    r.function("fwrite", libc::fwrite as usize);
    r.function("fread", libc::fread as usize);
    r.function("fclose", libc::fclose as usize);
    r.function("fgets", libc::fgets as usize);
    r.function("fseek", libc::fseek as usize);
    r.function("ftell", libc::ftell as usize);
    r.function("fputs", libc::fputs as usize);
    r.function("gettimeofday", libc::gettimeofday as usize);
    r.function("sysctlbyname", ios_sysctlbyname as usize);
    r.function("sysctl", ios_sysctl as usize);
    r.function("sysconf", ios_sysconf as usize);
    r.function("getprogname", ios_getprogname as usize);
    r.function("_NSGetExecutablePath", ios_ns_get_executable_path as usize);
    r.function("gethostname", ios_gethostname as usize);
}
