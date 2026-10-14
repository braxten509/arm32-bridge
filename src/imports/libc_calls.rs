//! libc, libdl and liblog for 32-bit callers. Pointers are guest addresses
//! (turned into host pointers with `mem.host`), `long`/`size_t`/`time_t`/
//! `off_t` are 32 bits, and structs with longs or pointers in them are
//! converted field by field.

use crate::handler;
use crate::imports::{printf, scanf, HostFile};
use crate::mem::GuestMem;
use crate::runtime::{call_guest, mem, shared, stop_with_fault, stub_for, ArgSource, Args, GuestCpu, Handler, Ret, Uc, VaList, without_gil, LR, SP};


fn host_c(guest: u32) -> *mut libc::c_char {
    mem().host_or_null(guest) as *mut libc::c_char
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Copies the host errno into the guest's errno slot.
fn sync_errno(uc: &Uc) {
    let slot = uc.get_data().errno_slot;
    mem().write_u32(slot, last_errno() as u32);
}

fn vsnprintf(buffer: u32, size: u32, format: u32, list: u32) -> Ret {
    let mut list = VaList { pointer: list };
    let text = printf::format(mem(), format, &mut list);
    if size > 0 {
        let copy = text.len().min(size as usize - 1);
        mem().write_bytes(buffer, &text[..copy]);
        mem().write_u8(buffer + copy as u32, 0);
    }
    Ret::U32(text.len() as u32)
}

fn set_errno(uc: &Uc, value: i32) {
    let slot = uc.get_data().errno_slot;
    mem().write_u32(slot, value as u32);
}

/// Result of a host call that returns -1 and sets errno on failure.
fn checked(uc: &Uc, result: i64) -> Ret {
    if result < 0 {
        sync_errno(uc);
    }
    Ret::U32(result as i32 as u32)
}

/// Guest pointer for a host pointer that points into guest memory.
fn back(host: *const libc::c_char) -> Ret {
    Ret::U32(mem().guest(host as *const u8))
}

/// Writes a host directory entry as a bionic ARM32 dirent: u64 d_ino, i64 d_off, u16 d_reclen,
/// u8 d_type, char d_name[256].
fn write_dirent(buffer: u32, entry: &libc::dirent) {
    let m = mem();
    m.write_u64(buffer, entry.d_ino as u64);
    m.write_u64(buffer + 8, entry.d_off as u64);
    m.write_u16(buffer + 16, 280);
    m.write_u8(buffer + 18, entry.d_type);
    let name = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
    let length = name.len().min(255);
    m.write_bytes(buffer + 19, &name[..length]);
    m.write_u8(buffer + 19 + length as u32, 0);
}

/// The classes wctype() knows, numbered from 1 in this order.
const WIDE_CLASSES: [&str; 12] =
    ["alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space", "upper", "xdigit"];

/// A guest wchar_t string (4-byte characters), without its end mark. Characters that aren't valid become U+FFFD.
fn guest_wide_text(guest: u32) -> Vec<char> {
    let mut text = Vec::new();
    loop {
        let c = mem().read_u32(guest + text.len() as u32 * 4);
        if c == 0 {
            return text;
        }
        text.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
    }
}

/// Writes `text` and an end mark as a guest wchar_t string.
fn write_wide_text(guest: u32, text: &[char]) {
    for (i, &c) in text.iter().enumerate() {
        mem().write_u32(guest + i as u32 * 4, c as u32);
    }
    mem().write_u32(guest + text.len() as u32 * 4, 0);
}

fn guest_string_in_heap(text: &[u8]) -> u32 {
    crate::runtime::heap().strdup(text)
}

/// A shared 1 KiB scratch copy for host results like strerror().
fn static_text(text: &[u8]) -> u32 {
    if shared().state.static_text == 0 {
        let area = shared().misc_alloc(1024, 8);
        shared().state.static_text = area;
    }
    let area = shared().state.static_text;
    let length = text.len().min(1023);
    mem().write_bytes(area, &text[..length]);
    mem().write_u8(area + length as u32, 0);
    area
}

/// ARM32 open() flag bits that differ from the host's numbering.
fn host_open_flags(flags: u32) -> i32 {
    const ARM_DIRECTORY: u32 = 0o40000;
    const ARM_NOFOLLOW: u32 = 0o100000;
    const ARM_DIRECT: u32 = 0o200000;
    const ARM_LARGEFILE: u32 = 0o400000;
    let mut host = (flags & !(ARM_DIRECTORY | ARM_NOFOLLOW | ARM_DIRECT | ARM_LARGEFILE)) as i32;
    if flags & ARM_DIRECTORY != 0 {
        host |= libc::O_DIRECTORY;
    }
    if flags & ARM_NOFOLLOW != 0 {
        host |= libc::O_NOFOLLOW;
    }
    if flags & ARM_DIRECT != 0 {
        host |= libc::O_DIRECT;
    }
    host
}

fn emulated_cpu_file(path: u32) -> Option<i32> {
    if path == 0 {
        return None;
    }
    let contents = match mem().string(path).as_str() {
        "/proc/cpuinfo" => cpuinfo_text().into_bytes(),
        "/proc/self/auxv" => vec![0; 8],
        _ => return None,
    };
    let fd = unsafe { libc::memfd_create(c"cpu".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let written = unsafe { libc::write(fd, contents.as_ptr().cast(), contents.len()) };
    if written != contents.len() as isize || unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } != 0 {
        unsafe { libc::close(fd) };
        return None;
    }
    Some(fd)
}

fn cpuinfo_text() -> String {
    let processors = std::thread::available_parallelism().map_or(1, |count| count.get());
    let mut text = String::new();
    for processor in 0..processors {
        text.push_str(&format!(
            "processor\t: {processor}\nmodel name\t: ARMv7 Processor rev 0 (v7l)\nBogoMIPS\t: 38.40\n\
             Features\t: half thumb fastmult vfp edsp neon vfpv3 tls vfpv4 idiva idivt vfpd32 lpae\n\
             CPU implementer\t: 0x41\nCPU architecture: 7\nCPU variant\t: 0x0\nCPU part\t: 0xc0f\n\
             CPU revision\t: 0\n\n"
        ));
    }
    text.push_str("Hardware\t: arm32-bridge\nRevision\t: 0000\nSerial\t\t: 0000000000000000\n");
    text
}

fn open_guest_path(path: u32, flags: u32, mode: u32) -> i32 {
    if let Some(fd) = emulated_cpu_file(path) {
        return fd;
    }
    unsafe { libc::open(host_c(path), host_open_flags(flags), mode) }
}

/// Writes a host `struct stat` as bionic ARM32's 104-byte `struct stat`.
fn write_stat32(memory: GuestMem, guest: u32, st: &libc::stat) {
    unsafe { std::ptr::write_bytes(memory.host(guest), 0, 104) };
    memory.write_u64(guest, st.st_dev as u64);
    memory.write_u32(guest + 12, st.st_ino as u32);
    memory.write_u32(guest + 16, st.st_mode as u32);
    memory.write_u32(guest + 20, st.st_nlink as u32);
    memory.write_u32(guest + 24, st.st_uid);
    memory.write_u32(guest + 28, st.st_gid);
    memory.write_u64(guest + 32, st.st_rdev as u64);
    memory.write_u64(guest + 48, st.st_size as u64);
    memory.write_u32(guest + 56, st.st_blksize as u32);
    memory.write_u64(guest + 64, st.st_blocks as u64);
    memory.write_u32(guest + 72, st.st_atime as u32);
    memory.write_u32(guest + 76, st.st_atime_nsec as u32);
    memory.write_u32(guest + 80, st.st_mtime as u32);
    memory.write_u32(guest + 84, st.st_mtime_nsec as u32);
    memory.write_u32(guest + 88, st.st_ctime as u32);
    memory.write_u32(guest + 92, st.st_ctime_nsec as u32);
    memory.write_u64(guest + 96, st.st_ino as u64);
}

/// ARM32 `struct tm`: nine ints, `long tm_gmtoff`, `const char* tm_zone` (44 bytes).
fn read_tm32(memory: GuestMem, guest: u32) -> libc::tm {
    let field = |i: u32| memory.read_u32(guest + i * 4) as i32;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_sec = field(0);
    tm.tm_min = field(1);
    tm.tm_hour = field(2);
    tm.tm_mday = field(3);
    tm.tm_mon = field(4);
    tm.tm_year = field(5);
    tm.tm_wday = field(6);
    tm.tm_yday = field(7);
    tm.tm_isdst = field(8);
    tm.tm_gmtoff = field(9) as libc::c_long;
    tm
}

fn write_tm32(guest: u32, tm: &libc::tm) {
    let memory = mem();
    let fields = [tm.tm_sec, tm.tm_min, tm.tm_hour, tm.tm_mday, tm.tm_mon, tm.tm_year, tm.tm_wday, tm.tm_yday, tm.tm_isdst];
    for (i, value) in fields.iter().enumerate() {
        memory.write_u32(guest + i as u32 * 4, *value as u32);
    }
    memory.write_u32(guest + 36, tm.tm_gmtoff as u32);
    let zone = if tm.tm_zone.is_null() {
        0
    } else {
        let text = unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }.to_bytes().to_vec();
        let key = format!("tz:{}", String::from_utf8_lossy(&text));
        shared().cached_cstr(key, &String::from_utf8_lossy(&text))
    };
    mem().write_u32(guest + 40, zone);
}

fn broken_down_time(a: &mut Args, local: bool) -> Ret {
    let clock = a.u32();
    let seconds = mem().read_u32(clock) as i32 as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        if local {
            libc::localtime_r(&seconds, &mut tm);
        } else {
            libc::gmtime_r(&seconds, &mut tm);
        }
    }
    let out = static_tm();
    write_tm32(out, &tm);
    Ret::U32(out)
}

fn static_tm() -> u32 {
    if shared().state.static_tm == 0 {
        let area = shared().misc_alloc(44, 4);
        shared().state.static_tm = area;
    }
    shared().state.static_tm
}

fn host_file(guest: u32) -> Option<HostFile> {
    shared().state.files.get(&guest).copied()
}

fn write_to_file(guest_file: u32, bytes: &[u8]) -> usize {
    match host_file(guest_file) {
        Some(HostFile::Descriptor(fd)) => unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) }.max(0) as usize,
        Some(HostFile::Stream(stream)) => unsafe {
            libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), stream as *mut libc::FILE)
        },
        None => 0,
    }
}

fn register_file(uc: &mut Uc, stream: *mut libc::FILE) -> u32 {
    if stream.is_null() {
        sync_errno(uc);
        return 0;
    }
    let handle = crate::runtime::heap().calloc(1, 84);
    shared().state.files.insert(handle, HostFile::Stream(stream as usize));
    handle
}

fn strtol_32(uc: &Uc, a: &mut Args, signed: bool) -> Ret {
    let text = a.u32();
    let end_out = a.u32();
    let base = a.i32();
    let mut end: *mut libc::c_char = std::ptr::null_mut();
    clear_errno();
    let (value, out_of_range) = if signed {
        let value = unsafe { libc::strtoll(host_c(text), &mut end, base) };
        let clamped = value.clamp(i32::MIN as i64, i32::MAX as i64);
        (clamped as u32, clamped != value)
    } else {
        let value = unsafe { libc::strtoull(host_c(text), &mut end, base) };
        // strtoul accepts "-1" and wraps; keep that in 32 bits.
        let as_signed = value as i64;
        if as_signed < 0 && as_signed >= i32::MIN as i64 {
            (as_signed as i32 as u32, false)
        } else if value > u32::MAX as u64 {
            (u32::MAX, true)
        } else {
            (value as u32, false)
        }
    };
    if end_out != 0 {
        mem().write_u32(end_out, mem().guest(end as *const u8));
    }
    if out_of_range {
        set_errno(uc, libc::ERANGE);
    } else {
        sync_errno(uc);
    }
    Ret::U32(value)
}

/// jmp_buf layout (ours): r4-r11, sp, lr, then d8-d15.
const SAVED_CORE: [usize; 10] = [4, 5, 6, 7, 8, 9, 10, 11, SP, LR];

fn save_jump_buffer(cpu: &mut Uc, buffer: u32) {
    let memory = mem();
    for (i, &register) in SAVED_CORE.iter().enumerate() {
        memory.write_u32(buffer + i as u32 * 4, cpu.reg(register));
    }
    for i in 0..8 {
        memory.write_u64(buffer + 40 + i as u32 * 8, cpu.dreg(8 + i));
    }
}

fn restore_jump_buffer(cpu: &mut Uc, buffer: u32, value: u32) -> Ret {
    let memory = mem();
    for (i, &register) in SAVED_CORE.iter().enumerate() {
        cpu.set_reg(register, memory.read_u32(buffer + i as u32 * 4));
    }
    for i in 0..8 {
        cpu.set_dreg(8 + i, memory.read_u64(buffer + 40 + i as u32 * 8));
    }
    cpu.set_reg(0, if value == 0 { 1 } else { value });
    Ret::Jumped
}

/// The guest's one locale handle (a small zeroed block; bionic never looks inside).
fn locale_handle() -> u32 {
    static HANDLE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *HANDLE.get_or_init(|| shared().misc_alloc(16, 4))
}

fn page_round(length: u32) -> u32 {
    length.saturating_add(crate::mem::PAGE - 1) & !(crate::mem::PAGE - 1)
}

/// Whether [address, address + length) lies inside one block mmap handed out.
fn inside_mapping(shared: &crate::runtime::Shared, address: u32, length: u32) -> bool {
    shared
        .state
        .mappings
        .range(..=address)
        .next_back()
        .is_some_and(|(&start, &size)| address as u64 + length as u64 <= start as u64 + size as u64)
}

/// mmap for the guest: a page-aligned heap block, filled from `fd` for file mappings.
fn map_memory(uc: &mut Uc, address: u32, length: u32, _protection: u32, flags: u32, fd: u32, offset: u64) -> Ret {
    const MAP_FAILED: u32 = u32::MAX;
    let size = page_round(length);
    if length == 0 || size == 0 {
        set_errno(uc, libc::EINVAL);
        return Ret::U32(MAP_FAILED);
    }
    let anonymous = flags & libc::MAP_ANONYMOUS as u32 != 0;
    let start = if flags & libc::MAP_FIXED as u32 != 0 {
        // Only inside a block already handed out (allocators trim and reuse those).
        // Checked and cleared in one go, so the block can't be unmapped in between.
        let shared = shared();
        if !inside_mapping(&shared, address, size) {
            drop(shared);
            crate::log::line(&format!("[bridge] mmap at fixed address {address:#x} ({length} bytes) outside its blocks"));
            set_errno(uc, libc::ENOMEM);
            return Ret::U32(MAP_FAILED);
        }
        unsafe { std::ptr::write_bytes(mem().host(address), 0, size as usize) };
        address
    } else {
        let block = crate::runtime::heap().memalign(crate::mem::PAGE, size);
        if block == 0 {
            set_errno(uc, libc::ENOMEM);
            return Ret::U32(MAP_FAILED);
        }
        unsafe { std::ptr::write_bytes(mem().host(block), 0, size as usize) };
        shared().state.mappings.insert(block, size);
        block
    };
    if !anonymous {
        let read = unsafe { libc::pread(fd as i32, mem().host(start).cast(), length as usize, offset as libc::off_t) };
        if read < 0 {
            sync_errno(uc);
            if start != address {
                let mut shared = shared();
                shared.state.mappings.remove(&start);
                crate::runtime::heap().free(start);
            }
            return Ret::U32(MAP_FAILED);
        }
    }
    Ret::U32(start)
}

/// dlopen handle for phone libraries (and dlopen(NULL)).
const PHONE_HANDLE: u32 = 1;
/// dlopen handle of the shared loader's first game library.
const FIRST_LIBRARY_HANDLE: u32 = 0x100;

/// Address for a symbol looked up at run time (dlsym, eglGetProcAddress):
/// a loaded game library's export, or a stand-in or data object if the bridge has one.
fn lookup_symbol(name: &str) -> u32 {
    if let Some(&address) = shared().guest_exports.get(name) {
        return address;
    }
    if crate::imports::handler_for(name).is_some() {
        return stub_for(name);
    }
    // Data the bridge provides at load time (OpenSL ES interface IDs...) too.
    if crate::imports::known_data_import(name) {
        return crate::imports::data_import(name);
    }
    crate::log::line(&format!("[bridge] run-time lookup of {name}: not available"));
    0
}

fn log_line(priority: u32, tag: &str, message: &[u8]) {
    let letter = ["?", "?", "V", "D", "I", "W", "E", "F"].get(priority as usize).copied().unwrap_or("?");
    eprintln!("[game log {letter}/{tag}] {}", String::from_utf8_lossy(message).trim_end());
    #[cfg(target_os = "android")]
    {
        let tag = std::ffi::CString::new(format!("arm32:{tag}")).unwrap_or_default();
        let text = std::ffi::CString::new(message.to_vec()).unwrap_or_default();
        unsafe { android_log::__android_log_write(priority as i32, tag.as_ptr(), text.as_ptr()) };
    }
}

#[cfg(target_os = "android")]
mod android_log {
    #[link(name = "log")]
    extern "C" {
        pub fn __android_log_write(priority: i32, tag: *const libc::c_char, text: *const libc::c_char) -> i32;
    }
}

fn clear_errno() {
    #[cfg(target_os = "android")]
    unsafe {
        *libc::__errno() = 0
    };
    #[cfg(not(target_os = "android"))]
    unsafe {
        *libc::__errno_location() = 0
    };
}

extern "C" {
    fn lrand48() -> libc::c_long;
    fn srand48(seed: libc::c_long);
    fn strerror_r(errnum: i32, buffer: *mut libc::c_char, length: usize) -> *mut libc::c_char;
}

/// A free or realloc of something that isn't a live heap block would corrupt
/// the heap; log who did it (first few times) and leave the heap alone.
fn refuse_bad_block(uc: &mut Uc, call: &str, pointer: u32) -> bool {
    static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let Some(reason) = crate::runtime::heap().not_owned(pointer) else { return false };
    if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 30 {
        crate::log::line(&format!(
            "[heap] refused {call}({pointer:#x}): {reason}; called from lr {:#x}",
            uc.reg(14)
        ));
    }
    true
}

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        // ---- memory ----
        "malloc" => handler!(|uc, a| {
            let size = a.u32();
            Ret::U32(crate::runtime::heap().malloc(size))
        }),
        "calloc" => handler!(|uc, a| {
            let count = a.u32();
            let size = a.u32();
            Ret::U32(crate::runtime::heap().calloc(count, size))
        }),
        "realloc" => handler!(|uc, a| {
            let pointer = a.u32();
            let size = a.u32();
            if pointer != 0 && refuse_bad_block(uc, "realloc", pointer) {
                return Ret::U32(0);
            }
            Ret::U32(crate::runtime::heap().realloc(pointer, size))
        }),
        "free" | "_ZdlPv" => handler!(|uc, a| {
            let pointer = a.u32();
            if pointer != 0 && refuse_bad_block(uc, "free", pointer) {
                return Ret::Void;
            }
            crate::runtime::heap().free(pointer);
            Ret::Void
        }),
        "memalign" => handler!(|uc, a| {
            let align = a.u32();
            let size = a.u32();
            Ret::U32(crate::runtime::heap().memalign(align, size))
        }),
        "posix_memalign" => handler!(|uc, a| {
            let out = a.u32();
            let align = a.u32();
            let size = a.u32();
            let pointer = crate::runtime::heap().memalign(align, size);
            mem().write_u32(out, pointer);
            Ret::U32(if pointer == 0 { libc::ENOMEM as u32 } else { 0 })
        }),
        "memcpy" | "__aeabi_memcpy" | "__aeabi_memcpy4" | "__aeabi_memcpy8" | "memmove" | "__aeabi_memmove"
        | "__aeabi_memmove4" | "__aeabi_memmove8" | "__memcpy_chk" | "__memmove_chk" => handler!(|uc, a| {
            let (destination, source, length) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            unsafe { std::ptr::copy(m.host(source), m.host(destination), length as usize) };
            Ret::U32(destination)
        }),
        "memset" | "__memset_chk" => handler!(|uc, a| {
            let (destination, value, length) = (a.u32(), a.u32(), a.u32());
            unsafe { std::ptr::write_bytes(mem().host(destination), value as u8, length as usize) };
            Ret::U32(destination)
        }),
        // Note the different argument order: (dest, n, c).
        "__aeabi_memset" | "__aeabi_memset4" | "__aeabi_memset8" => handler!(|uc, a| {
            let (destination, length, value) = (a.u32(), a.u32(), a.u32());
            unsafe { std::ptr::write_bytes(mem().host(destination), value as u8, length as usize) };
            Ret::U32(destination)
        }),
        "bzero" | "__aeabi_memclr" | "__aeabi_memclr4" | "__aeabi_memclr8" => handler!(|uc, a| {
            let (destination, length) = (a.u32(), a.u32());
            unsafe { std::ptr::write_bytes(mem().host(destination), 0, length as usize) };
            Ret::Void
        }),
        "memcmp" => handler!(|uc, a| {
            let (x, y, length) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            Ret::U32(unsafe { libc::memcmp(m.host(x).cast(), m.host(y).cast(), length as usize) } as u32)
        }),
        "memchr" => handler!(|uc, a| {
            let (start, value, length) = (a.u32(), a.i32(), a.u32());
            let found = unsafe { libc::memchr(mem().host(start).cast(), value, length as usize) };
            back(found as *const libc::c_char)
        }),
        "memrchr" => handler!(|uc, a| {
            let (start, value, length) = (a.u32(), a.u32(), a.u32());
            let memory = mem();
            let bytes = memory.bytes(start, length as usize);
            Ret::U32(bytes.iter().rposition(|b| *b == value as u8).map(|i| start + i as u32).unwrap_or(0))
        }),
        "memmem" => handler!(|uc, a| {
            let (haystack, haystack_length, needle, needle_length) = (a.u32(), a.u32(), a.u32(), a.u32());
            let m = mem();
            let found = unsafe {
                libc::memmem(m.host(haystack).cast(), haystack_length as usize, m.host(needle).cast(), needle_length as usize)
            };
            back(found as *const libc::c_char)
        }),

        // ---- strings ----
        "strlen" | "__strlen_chk" => handler!(|uc, a| Ret::U32(unsafe { libc::strlen(host_c(a.u32())) } as u32)),
        // Only the C locale exists, so collating is plain comparing.
        "strcmp" | "strcoll" => handler!(|uc, a| {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32(unsafe { libc::strcmp(host_c(x), host_c(y)) } as u32)
        }),
        "strncmp" => handler!(|uc, a| {
            let (x, y, n) = (a.u32(), a.u32(), a.u32());
            Ret::U32(unsafe { libc::strncmp(host_c(x), host_c(y), n as usize) } as u32)
        }),
        "strcasecmp" => handler!(|uc, a| {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32(unsafe { libc::strcasecmp(host_c(x), host_c(y)) } as u32)
        }),
        "strncasecmp" => handler!(|uc, a| {
            let (x, y, n) = (a.u32(), a.u32(), a.u32());
            Ret::U32(unsafe { libc::strncasecmp(host_c(x), host_c(y), n as usize) } as u32)
        }),
        "strchr" | "__strchr_chk" => handler!(|uc, a| {
            let (text, c) = (a.u32(), a.i32());
            back(unsafe { libc::strchr(host_c(text), c) })
        }),
        "strrchr" | "__strrchr_chk" => handler!(|uc, a| {
            let (text, c) = (a.u32(), a.i32());
            back(unsafe { libc::strrchr(host_c(text), c) })
        }),
        "strstr" => handler!(|uc, a| {
            let (haystack, needle) = (a.u32(), a.u32());
            back(unsafe { libc::strstr(host_c(haystack), host_c(needle)) })
        }),
        "strpbrk" => handler!(|uc, a| {
            let (text, accept) = (a.u32(), a.u32());
            back(unsafe { libc::strpbrk(host_c(text), host_c(accept)) })
        }),
        "strspn" => handler!(|uc, a| {
            let (text, accept) = (a.u32(), a.u32());
            Ret::U32(unsafe { libc::strspn(host_c(text), host_c(accept)) } as u32)
        }),
        "strcspn" => handler!(|uc, a| {
            let (text, reject) = (a.u32(), a.u32());
            Ret::U32(unsafe { libc::strcspn(host_c(text), host_c(reject)) } as u32)
        }),
        "strcpy" | "__strcpy_chk" => handler!(|uc, a| {
            let (destination, source) = (a.u32(), a.u32());
            unsafe { libc::strcpy(host_c(destination), host_c(source)) };
            Ret::U32(destination)
        }),
        "strncpy" => handler!(|uc, a| {
            let (destination, source, n) = (a.u32(), a.u32(), a.u32());
            unsafe { libc::strncpy(host_c(destination), host_c(source), n as usize) };
            Ret::U32(destination)
        }),
        "strcat" => handler!(|uc, a| {
            let (destination, source) = (a.u32(), a.u32());
            unsafe { libc::strcat(host_c(destination), host_c(source)) };
            Ret::U32(destination)
        }),
        "strncat" => handler!(|uc, a| {
            let (destination, source, n) = (a.u32(), a.u32(), a.u32());
            unsafe { libc::strncat(host_c(destination), host_c(source), n as usize) };
            Ret::U32(destination)
        }),
        // C locale: the transformed text is the text itself.
        "strxfrm" => handler!(|uc, a| {
            let (destination, source, size) = (a.u32(), a.u32(), a.u32());
            let text = mem().c_str(source).to_bytes().to_vec();
            if (text.len() as u32) < size {
                mem().write_bytes(destination, &text);
                mem().write_u8(destination + text.len() as u32, 0);
            }
            Ret::U32(text.len() as u32)
        }),
        "strlcpy" | "__strlcpy_chk" => handler!(|uc, a| {
            let (destination, source, size) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            let text = m.c_str(source).to_bytes().to_vec();
            if size > 0 {
                let copy = text.len().min(size as usize - 1);
                m.write_bytes(destination, &text[..copy]);
                m.write_u8(destination + copy as u32, 0);
            }
            Ret::U32(text.len() as u32)
        }),
        "strdup" => handler!(|uc, a| {
            let source = a.u32();
            let text = mem().c_str(source).to_bytes().to_vec();
            Ret::U32(guest_string_in_heap(&text))
        }),
        "strtok" => handler!(|uc, a| {
            let (text, delimiters) = (a.u32(), a.u32());
            let mut next = if text == 0 { shared().state.strtok_next as *mut libc::c_char } else { host_c(text) };
            let token = unsafe { libc::strtok_r(if text == 0 { std::ptr::null_mut() } else { next }, host_c(delimiters), &mut next) };
            shared().state.strtok_next = next as usize;
            back(token)
        }),
        "strtok_r" => handler!(|uc, a| {
            let (text, delimiters, save) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            let mut next = m.host_or_null(m.read_u32(save)) as *mut libc::c_char;
            let token = unsafe { libc::strtok_r(host_c(text), host_c(delimiters), &mut next) };
            m.write_u32(save, m.guest(next as *const u8));
            back(token)
        }),
        "strtol" => handler!(|uc, a| strtol_32(uc, a, true)),
        "strtoul" => handler!(|uc, a| strtol_32(uc, a, false)),
        "strtoll" | "strtoull" | "strtoll_l" | "strtoull_l" => handler!(|uc, a| {
            let (text, end_out, base) = (a.u32(), a.u32(), a.i32());
            let mut end: *mut libc::c_char = std::ptr::null_mut();
            let value = unsafe { libc::strtoull(host_c(text), &mut end, base) };
            if end_out != 0 {
                mem().write_u32(end_out, mem().guest(end as *const u8));
            }
            sync_errno(uc);
            Ret::U64(value)
        }),
        "strtod" | "strtold" | "strtold_l" => handler!(|uc, a| {
            let (text, end_out) = (a.u32(), a.u32());
            let mut end: *mut libc::c_char = std::ptr::null_mut();
            let value = unsafe { libc::strtod(host_c(text), &mut end) };
            if end_out != 0 {
                mem().write_u32(end_out, mem().guest(end as *const u8));
            }
            Ret::F64(value)
        }),
        "strtof" => handler!(|uc, a| {
            let (text, end_out) = (a.u32(), a.u32());
            let mut end: *mut libc::c_char = std::ptr::null_mut();
            let value = unsafe { libc::strtof(host_c(text), &mut end) };
            if end_out != 0 {
                mem().write_u32(end_out, mem().guest(end as *const u8));
            }
            Ret::F32(value)
        }),
        "atoi" | "atol" => handler!(|uc, a| Ret::U32(unsafe { libc::atoll(host_c(a.u32())) } as i32 as u32)),
        "atof" => handler!(|uc, a| Ret::F64(unsafe { libc::atof(host_c(a.u32())) })),
        "abs" | "labs" => handler!(|uc, a| Ret::U32(a.i32().wrapping_abs() as u32)),
        "llabs" | "imaxabs" => handler!(|uc, a| Ret::U64(a.i64().wrapping_abs() as u64)),
        "tolower" => handler!(|uc, a| Ret::U32(unsafe { libc::tolower(a.i32()) } as u32)),
        "toupper" => handler!(|uc, a| Ret::U32(unsafe { libc::toupper(a.i32()) } as u32)),
        "isspace" => handler!(|uc, a| Ret::U32((unsafe { libc::isspace(a.i32()) } != 0) as u32)),
        "isupper" => handler!(|uc, a| Ret::U32((unsafe { libc::isupper(a.i32()) } != 0) as u32)),
        "isblank" => handler!(|uc, a| Ret::U32((unsafe { libc::isblank(a.i32()) } != 0) as u32)),
        "isdigit" => handler!(|uc, a| Ret::U32((unsafe { libc::isdigit(a.i32()) } != 0) as u32)),
        "isxdigit" => handler!(|uc, a| Ret::U32((unsafe { libc::isxdigit(a.i32()) } != 0) as u32)),
        "isalpha" => handler!(|uc, a| Ret::U32((unsafe { libc::isalpha(a.i32()) } != 0) as u32)),
        "isalnum" => handler!(|uc, a| Ret::U32((unsafe { libc::isalnum(a.i32()) } != 0) as u32)),
        "isascii" => handler!(|uc, a| Ret::U32((a.u32() < 0x80) as u32)),
        "iswlower" => handler!(|uc, a| Ret::U32(char::from_u32(a.u32()).is_some_and(|c| c.is_lowercase()) as u32)),
        // wchar_t is 32 bits for the guest too; the C library is always UTF-8.
        "wcslen" => handler!(|uc, a| {
            let text = a.u32();
            let mut length = 0;
            while mem().read_u32(text + length * 4) != 0 {
                length += 1;
            }
            Ret::U32(length)
        }),
        "wmemcpy" | "wmemmove" => handler!(|uc, a| {
            let (destination, source, count) = (a.u32(), a.u32(), a.u32());
            unsafe { std::ptr::copy(mem().host(source), mem().host(destination), count as usize * 4) };
            Ret::U32(destination)
        }),
        "wmemset" => handler!(|uc, a| {
            let (destination, value, count) = (a.u32(), a.u32(), a.u32());
            for i in 0..count {
                mem().write_u32(destination + i * 4, value);
            }
            Ret::U32(destination)
        }),
        "wcrtomb" => handler!(|uc, a| {
            let (out, character) = (a.u32(), a.u32());
            if out == 0 {
                return Ret::U32(1);
            }
            let Some(character) = char::from_u32(character) else {
                set_errno(uc, libc::EILSEQ);
                return Ret::U32(u32::MAX);
            };
            let mut bytes = [0u8; 4];
            let encoded = character.encode_utf8(&mut bytes);
            mem().write_bytes(out, encoded.as_bytes());
            Ret::U32(encoded.len() as u32)
        }),
        // mbrtowc(out, text, length, state) / mbtowc(out, text, length) / mbrlen(text, length, state).
        "mbrtowc" | "mbtowc" | "mbrlen" => handler!(|uc, a| {
            let name = uc.get_data().current_call;
            let out = if name == "mbrlen" { 0 } else { a.u32() };
            let (text, length) = (a.u32(), a.u32());
            if text == 0 {
                return Ret::U32(0);
            }
            let available = (length.min(4)) as usize;
            let bytes = mem().bytes(text, available).to_vec();
            let needed = match bytes.first() {
                None => return Ret::U32(u32::MAX - 1),
                Some(0) => {
                    if out != 0 {
                        mem().write_u32(out, 0);
                    }
                    return Ret::U32(0);
                }
                Some(&b) if b < 0x80 => 1,
                Some(&b) if b >> 5 == 0b110 => 2,
                Some(&b) if b >> 4 == 0b1110 => 3,
                Some(&b) if b >> 3 == 0b11110 => 4,
                Some(_) => {
                    set_errno(uc, libc::EILSEQ);
                    return Ret::U32(u32::MAX);
                }
            };
            if needed > bytes.len() {
                // Incomplete: -2 (mbtowc reports it as invalid).
                return Ret::U32(if name == "mbtowc" { u32::MAX } else { u32::MAX - 1 });
            }
            match std::str::from_utf8(&bytes[..needed]).ok().and_then(|text| text.chars().next()) {
                Some(character) => {
                    if out != 0 {
                        mem().write_u32(out, character as u32);
                    }
                    Ret::U32(needed as u32)
                }
                None => {
                    set_errno(uc, libc::EILSEQ);
                    Ret::U32(u32::MAX)
                }
            }
        }),
        // Only the C locale exists, so collating is plain comparing.
        "wcscmp" | "wcscoll" => handler!(|uc, a| {
            // wchar_t is 4 bytes on both sides.
            let (x, y) = (a.u32(), a.u32());
            let m = mem();
            let mut i = 0;
            loop {
                let (cx, cy) = (m.read_u32(x + i * 4) as i32, m.read_u32(y + i * 4) as i32);
                if cx != cy || cx == 0 {
                    return Ret::U32(cx.cmp(&cy) as i32 as u32);
                }
                i += 1;
            }
        }),
        "wmemchr" => handler!(|uc, a| {
            let (text, character, count) = (a.u32(), a.u32(), a.u32());
            Ret::U32((0..count).map(|i| text + i * 4).find(|&at| mem().read_u32(at) == character).unwrap_or(0))
        }),
        "wmemcmp" => handler!(|uc, a| {
            let (x, y, count) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            for i in 0..count {
                let (cx, cy) = (m.read_u32(x + i * 4), m.read_u32(y + i * 4));
                if cx != cy {
                    return Ret::U32(if cx < cy { u32::MAX } else { 1 });
                }
            }
            Ret::U32(0)
        }),
        // C locale: the transformed text is the text itself.
        "wcsxfrm" => handler!(|uc, a| {
            let (destination, source, size) = (a.u32(), a.u32(), a.u32());
            let text = guest_wide_text(source);
            if (text.len() as u32) < size {
                write_wide_text(destination, &text);
            }
            Ret::U32(text.len() as u32)
        }),
        "towlower" | "towupper" => handler!(|uc, a| {
            let character = a.u32();
            let Some(c) = char::from_u32(character) else { return Ret::U32(character) };
            // Only one-to-one mappings: wide functions can't grow a letter into two.
            let mapped: Vec<char> = if uc.get_data().current_call == "towlower" {
                c.to_lowercase().collect()
            } else {
                c.to_uppercase().collect()
            };
            Ret::U32(if mapped.len() == 1 { mapped[0] as u32 } else { character })
        }),
        // wctype(name) -> class number (0 = unknown); iswctype(character, class).
        "wctype" => handler!(|uc, a| {
            let name = mem().string(a.u32());
            Ret::U32(WIDE_CLASSES.iter().position(|&class| class == name).map_or(0, |i| i as u32 + 1))
        }),
        "iswctype" => handler!(|uc, a| {
            let (character, class) = (a.u32(), a.u32());
            let Some(c) = char::from_u32(character) else { return Ret::U32(0) };
            let Some(&name) = (class as usize).checked_sub(1).and_then(|i| WIDE_CLASSES.get(i)) else { return Ret::U32(0) };
            Ret::U32(match name {
                "alnum" => c.is_alphanumeric(),
                "alpha" => c.is_alphabetic(),
                "blank" => c == ' ' || c == '\t',
                "cntrl" => c.is_control(),
                "digit" => c.is_ascii_digit(),
                "graph" => !c.is_control() && !c.is_whitespace(),
                "lower" => c.is_lowercase(),
                "print" => !c.is_control(),
                "punct" => c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control()),
                "space" => c.is_whitespace(),
                "upper" => c.is_uppercase(),
                _ => c.is_ascii_hexdigit(),
            } as u32)
        }),
        // UTF-8: only the 7-bit characters are single bytes. EOF and WEOF are -1.
        "btowc" => handler!(|uc, a| {
            let c = a.i32();
            Ret::U32(if (0..0x80).contains(&c) { c as u32 } else { u32::MAX })
        }),
        "wctob" => handler!(|uc, a| {
            let c = a.u32();
            Ret::U32(if c < 0x80 { c } else { u32::MAX })
        }),
        "wcscpy" => handler!(|uc, a| {
            let (destination, source) = (a.u32(), a.u32());
            let m = mem();
            let mut i = 0;
            loop {
                let c = m.read_u32(source + i * 4);
                m.write_u32(destination + i * 4, c);
                if c == 0 {
                    return Ret::U32(destination);
                }
                i += 1;
            }
        }),
        "strerror" => handler!(|uc, a| {
            let number = a.i32();
            let text = std::io::Error::from_raw_os_error(number).to_string();
            let text = text.split(" (os error").next().unwrap_or("").as_bytes().to_vec();
            Ret::U32(static_text(&text))
        }),
        "strerror_r" => handler!(|uc, a| {
            // bionic's strerror_r is the POSIX one: returns 0 and fills the buffer.
            let (number, buffer, length) = (a.i32(), a.u32(), a.u32());
            let mut scratch = [0 as libc::c_char; 256];
            let text = unsafe { strerror_r(number, scratch.as_mut_ptr(), 256) };
            let text = if text.is_null() { scratch.as_ptr() } else { text };
            let bytes = unsafe { std::ffi::CStr::from_ptr(text) }.to_bytes();
            if length > 0 {
                let copy = bytes.len().min(length as usize - 1);
                mem().write_bytes(buffer, &bytes[..copy]);
                mem().write_u8(buffer + copy as u32, 0);
            }
            Ret::U32(0)
        }),
        "basename" => handler!(|uc, a| {
            let path = mem().string(a.u32());
            let trimmed = path.trim_end_matches('/');
            let base = if trimmed.is_empty() { "/" } else { trimmed.rsplit('/').next().unwrap_or("") };
            Ret::U32(static_text(base.as_bytes()))
        }),

        // ---- formatted text ----
        "sprintf" => handler!(|uc, a| {
            let (buffer, format) = (a.u32(), a.u32());
            let text = printf::format(mem(), format, a);
            mem().write_bytes(buffer, &text);
            mem().write_u8(buffer + text.len() as u32, 0);
            Ret::U32(text.len() as u32)
        }),
        "snprintf" => handler!(|uc, a| {
            let (buffer, size, format) = (a.u32(), a.u32(), a.u32());
            let text = printf::format(mem(), format, a);
            if size > 0 {
                let copy = text.len().min(size as usize - 1);
                mem().write_bytes(buffer, &text[..copy]);
                mem().write_u8(buffer + copy as u32, 0);
            }
            Ret::U32(text.len() as u32)
        }),
        "sscanf" => handler!(|uc, a| {
            let (input, format) = (a.u32(), a.u32());
            Ret::U32(scanf::scan(mem(), input, format, a) as u32)
        }),
        "vsscanf" => handler!(|uc, a| {
            let (input, format, list) = (a.u32(), a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            Ret::U32(scanf::scan(mem(), input, format, &mut list) as u32)
        }),
        "vsnprintf" => handler!(|uc, a| {
            let (buffer, size, format, list) = (a.u32(), a.u32(), a.u32(), a.u32());
            vsnprintf(buffer, size, format, list)
        }),
        "__vsnprintf_chk" => handler!(|uc, a| {
            let (buffer, size, _flags, _buffer_size, format, list) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            vsnprintf(buffer, size, format, list)
        }),
        // CPU feature bits: none, so libraries take their plain code paths.
        "getauxval" => handler!(|uc, a| Ret::U32(0)),
        "vsprintf" => handler!(|uc, a| {
            let (buffer, format, list) = (a.u32(), a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            mem().write_bytes(buffer, &text);
            mem().write_u8(buffer + text.len() as u32, 0);
            Ret::U32(text.len() as u32)
        }),
        "vasprintf" => handler!(|uc, a| {
            let (out, format, list) = (a.u32(), a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            let copy = guest_string_in_heap(&text);
            mem().write_u32(out, copy);
            Ret::U32(text.len() as u32)
        }),
        "fprintf" => handler!(|uc, a| {
            let (file, format) = (a.u32(), a.u32());
            let text = printf::format(mem(), format, a);
            write_to_file(file, &text);
            Ret::U32(text.len() as u32)
        }),
        "vfprintf" => handler!(|uc, a| {
            let (file, format, list) = (a.u32(), a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            write_to_file(file, &text);
            Ret::U32(text.len() as u32)
        }),
        "printf" => handler!(|uc, a| {
            let format = a.u32();
            let text = printf::format(mem(), format, a);
            unsafe { libc::write(1, text.as_ptr().cast(), text.len()) };
            Ret::U32(text.len() as u32)
        }),
        "vprintf" => handler!(|uc, a| {
            let (format, list) = (a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            unsafe { libc::write(1, text.as_ptr().cast(), text.len()) };
            Ret::U32(text.len() as u32)
        }),
        "syslog" => handler!(|uc, a| {
            let (priority, format) = (a.u32(), a.u32());
            let text = printf::format(mem(), format, a);
            log_line(priority & 7, "syslog", &text);
            Ret::Void
        }),
        "__android_log_print" => handler!(|uc, a| {
            let (priority, tag, format) = (a.u32(), a.u32(), a.u32());
            let tag = mem().string(tag);
            let text = printf::format(mem(), format, a);
            log_line(priority, &tag, &text);
            Ret::U32(1)
        }),
        "__android_log_vprint" => handler!(|uc, a| {
            let (priority, tag, format, list) = (a.u32(), a.u32(), a.u32(), a.u32());
            let tag = mem().string(tag);
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            log_line(priority, &tag, &text);
            Ret::U32(1)
        }),
        "__android_log_write" => handler!(|uc, a| {
            let (priority, tag, text) = (a.u32(), a.u32(), a.u32());
            let tag = mem().string(tag);
            let text = mem().c_str(text).to_bytes().to_vec();
            log_line(priority, &tag, &text);
            Ret::U32(1)
        }),
        "__android_log_assert" => handler!(|uc, a| {
            let (_condition, tag, format) = (a.u32(), a.u32(), a.u32());
            let tag = mem().string(tag);
            let text = if format == 0 { Vec::new() } else { printf::format(mem(), format, a) };
            log_line(7, &tag, &text);
            stop_with_fault(uc, format!("game assertion: {}", String::from_utf8_lossy(&text)));
            Ret::Void
        }),

        // ---- stdio ----
        "fopen" => handler!(|uc, a| {
            let (path, mode) = (a.u32(), a.u32());
            let stream = match emulated_cpu_file(path) {
                Some(fd) => unsafe { libc::fdopen(fd, host_c(mode)) },
                None => unsafe { libc::fopen(host_c(path), host_c(mode)) },
            };
            Ret::U32(register_file(uc, stream))
        }),
        "fdopen" => handler!(|uc, a| {
            let (fd, mode) = (a.i32(), a.u32());
            let stream = unsafe { libc::fdopen(fd, host_c(mode)) };
            Ret::U32(register_file(uc, stream))
        }),
        "fclose" => handler!(|uc, a| {
            let file = a.u32();
            let removed = shared().state.files.remove(&file);
            match removed {
                Some(HostFile::Stream(stream)) => {
                    let result = unsafe { libc::fclose(stream as *mut libc::FILE) };
                    crate::runtime::heap().free(file);
                    Ret::U32(result as u32)
                }
                Some(other) => {
                    shared().state.files.insert(file, other);
                    Ret::U32(0)
                }
                None => Ret::U32(u32::MAX),
            }
        }),
        "fread" => handler!(|uc, a| {
            let (buffer, size, count, file) = (a.u32(), a.u32(), a.u32(), a.u32());
            let host = mem().host(buffer);
            Ret::U32(match host_file(file) {
                Some(HostFile::Stream(stream)) => unsafe {
                    libc::fread(host.cast(), size as usize, count as usize, stream as *mut libc::FILE) as u32
                },
                Some(HostFile::Descriptor(fd)) if size > 0 => {
                    (unsafe { libc::read(fd, host.cast(), (size * count) as usize) }.max(0) as u32) / size
                }
                _ => 0,
            })
        }),
        "fwrite" => handler!(|uc, a| {
            let (buffer, size, count, file) = (a.u32(), a.u32(), a.u32(), a.u32());
            if size == 0 {
                return Ret::U32(0);
            }
            let bytes = mem().bytes(buffer, (size * count) as usize).to_vec();
            Ret::U32(write_to_file(file, &bytes) as u32 / size)
        }),
        "fputs" => handler!(|uc, a| {
            let (text, file) = (a.u32(), a.u32());
            let bytes = mem().c_str(text).to_bytes().to_vec();
            write_to_file(file, &bytes);
            Ret::U32(1)
        }),
        "fputc" | "putc" => handler!(|uc, a| {
            let (c, file) = (a.u32(), a.u32());
            write_to_file(file, &[c as u8]);
            Ret::U32(c & 0xFF)
        }),
        "fputwc" | "putwc" => handler!(|uc, a| {
            let (c, file) = (a.u32(), a.u32());
            let mut buffer = [0u8; 4];
            let text = char::from_u32(c).unwrap_or('?').encode_utf8(&mut buffer);
            write_to_file(file, text.as_bytes());
            Ret::U32(c)
        }),
        "putchar" => handler!(|uc, a| {
            let c = a.u32();
            unsafe { libc::write(1, [c as u8].as_ptr().cast(), 1) };
            Ret::U32(c & 0xFF)
        }),
        "puts" => handler!(|uc, a| {
            let mut bytes = mem().c_str(a.u32()).to_bytes().to_vec();
            bytes.push(b'\n');
            unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) };
            Ret::U32(1)
        }),
        "perror" => handler!(|uc, a| {
            let text = a.u32();
            let error = std::io::Error::from_raw_os_error(mem().read_u32(uc.get_data().errno_slot) as i32);
            if text != 0 {
                eprintln!("{}: {error}", mem().string(text));
            } else {
                eprintln!("{error}");
            }
            Ret::Void
        }),
        "feof" | "ferror" | "fileno" | "clearerr" => handler!(|uc, a| {
            let file = a.u32();
            let Some(HostFile::Stream(stream)) = host_file(file) else { return Ret::U32(0) };
            let stream = stream as *mut libc::FILE;
            Ret::U32(match uc.get_data().current_call {
                "feof" => (unsafe { libc::feof(stream) }) as u32,
                "ferror" => (unsafe { libc::ferror(stream) }) as u32,
                "fileno" => (unsafe { libc::fileno(stream) }) as u32,
                _ => {
                    unsafe { libc::clearerr(stream) };
                    0
                }
            })
        }),
        "fgets" => handler!(|uc, a| {
            let (buffer, size, file) = (a.u32(), a.i32(), a.u32());
            match host_file(file) {
                Some(HostFile::Stream(stream)) => {
                    let result = unsafe { libc::fgets(host_c(buffer), size, stream as *mut libc::FILE) };
                    Ret::U32(if result.is_null() { 0 } else { buffer })
                }
                _ => Ret::U32(0),
            }
        }),
        "getc" | "fgetc" => handler!(|uc, a| {
            let file = a.u32();
            match host_file(file) {
                Some(HostFile::Stream(stream)) => Ret::U32(unsafe { libc::fgetc(stream as *mut libc::FILE) } as u32),
                _ => Ret::U32(u32::MAX),
            }
        }),
        "ungetc" => handler!(|uc, a| {
            let (c, file) = (a.i32(), a.u32());
            match host_file(file) {
                Some(HostFile::Stream(stream)) => Ret::U32(unsafe { libc::ungetc(c, stream as *mut libc::FILE) } as u32),
                _ => Ret::U32(u32::MAX),
            }
        }),
        // Wide characters in files are UTF-8, like everything else in the C library.
        "getwc" => handler!(|uc, a| {
            let Some(HostFile::Stream(stream)) = host_file(a.u32()) else { return Ret::U32(u32::MAX) };
            let stream = stream as *mut libc::FILE;
            let first = unsafe { libc::fgetc(stream) };
            if first < 0 {
                return Ret::U32(u32::MAX);
            }
            let length = match first {
                0..0x80 => 1,
                _ if first >> 5 == 0b110 => 2,
                _ if first >> 4 == 0b1110 => 3,
                _ if first >> 3 == 0b11110 => 4,
                _ => 0,
            };
            let mut bytes = vec![first as u8];
            while bytes.len() < length {
                let next = unsafe { libc::fgetc(stream) };
                if next < 0 {
                    break;
                }
                bytes.push(next as u8);
            }
            match std::str::from_utf8(&bytes).ok().and_then(|text| text.chars().next()) {
                Some(c) => Ret::U32(c as u32),
                None => {
                    set_errno(uc, libc::EILSEQ);
                    Ret::U32(u32::MAX)
                }
            }
        }),
        "ungetwc" => handler!(|uc, a| {
            let (character, file) = (a.u32(), a.u32());
            let (Some(c), Some(HostFile::Stream(stream))) = (char::from_u32(character), host_file(file)) else {
                return Ret::U32(u32::MAX);
            };
            let mut buffer = [0u8; 4];
            // Pushed back last byte first, so they are read again in order.
            for &byte in c.encode_utf8(&mut buffer).as_bytes().iter().rev() {
                if unsafe { libc::ungetc(byte as i32, stream as *mut libc::FILE) } < 0 {
                    return Ret::U32(u32::MAX);
                }
            }
            Ret::U32(character)
        }),
        "fseek" | "fseeko" => handler!(|uc, a| {
            let (file, offset, whence) = (a.u32(), a.i32(), a.i32());
            match host_file(file) {
                Some(HostFile::Stream(stream)) => {
                    let result = unsafe { libc::fseeko(stream as *mut libc::FILE, offset as libc::off_t, whence) };
                    checked(uc, result as i64)
                }
                _ => Ret::U32(u32::MAX),
            }
        }),
        "ftell" | "ftello" => handler!(|uc, a| {
            let file = a.u32();
            match host_file(file) {
                Some(HostFile::Stream(stream)) => checked(uc, unsafe { libc::ftello(stream as *mut libc::FILE) } as i64),
                _ => Ret::U32(u32::MAX),
            }
        }),
        "rewind" => handler!(|uc, a| {
            if let Some(HostFile::Stream(stream)) = host_file(a.u32()) {
                unsafe { libc::rewind(stream as *mut libc::FILE) };
            }
            Ret::Void
        }),
        "fflush" => handler!(|uc, a| {
            let file = a.u32();
            if let Some(HostFile::Stream(stream)) = host_file(file) {
                unsafe { libc::fflush(stream as *mut libc::FILE) };
            }
            Ret::U32(0)
        }),
        "setvbuf" => handler!(|uc, a| Ret::U32(0)),
        "setbuf" => handler!(|uc, a| Ret::Void),
        // A file with no name, deleted when closed.
        "tmpfile" => handler!(|uc, a| {
            let fd = unsafe { libc::memfd_create(c"tmpfile".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                sync_errno(uc);
                return Ret::U32(0);
            }
            let stream = unsafe { libc::fdopen(fd, c"w+".as_ptr()) };
            if stream.is_null() {
                unsafe { libc::close(fd) };
            }
            Ret::U32(register_file(uc, stream))
        }),
        "tmpnam" => handler!(|uc, a| {
            let out = a.u32();
            static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("{}/tmp.{}.{count}", std::env::temp_dir().display(), unsafe { libc::getpid() });
            if out == 0 {
                return Ret::U32(static_text(name.as_bytes()));
            }
            // L_tmpnam is 4096 on bionic.
            let length = name.len().min(4095);
            mem().write_bytes(out, &name.as_bytes()[..length]);
            mem().write_u8(out + length as u32, 0);
            Ret::U32(out)
        }),
        "popen" => handler!(|uc, a| {
            let (command, mode) = (a.u32(), a.u32());
            let stream = unsafe { libc::popen(host_c(command), host_c(mode)) };
            Ret::U32(register_file(uc, stream))
        }),
        "pclose" => handler!(|uc, a| {
            let file = a.u32();
            let Some(HostFile::Stream(stream)) = host_file(file) else { return Ret::U32(u32::MAX) };
            shared().state.files.remove(&file);
            crate::runtime::heap().free(file);
            // Waits for the command to finish.
            let status = without_gil(move || unsafe { libc::pclose(stream as *mut libc::FILE) });
            checked(uc, status as i64)
        }),
        "freopen" => handler!(|uc, a| {
            let (path, mode, file) = (a.u32(), a.u32(), a.u32());
            if let Some(HostFile::Stream(stream)) = host_file(file) {
                let reopened = unsafe { libc::freopen(host_c(path), host_c(mode), stream as *mut libc::FILE) };
                if reopened.is_null() {
                    sync_errno(uc);
                    return Ret::U32(0);
                }
                shared().state.files.insert(file, HostFile::Stream(reopened as usize));
                return Ret::U32(file);
            }
            Ret::U32(0)
        }),

        // ---- files and directories ----
        // The _chk forms of the string and memory calls above only add a buffer size at the end.
        "__vsprintf_chk" => handler!(|uc, a| {
            let (buffer, _flags, _size, format, list) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let mut list = VaList { pointer: list };
            let text = printf::format(mem(), format, &mut list);
            mem().write_bytes(buffer, &text);
            mem().write_u8(buffer + text.len() as u32, 0);
            Ret::U32(text.len() as u32)
        }),
        // fd_set is an array of 32-bit words for the guest.
        "__FD_SET_chk" => handler!(|uc, a| {
            let (fd, set) = (a.u32(), a.u32());
            let word = set + fd / 32 * 4;
            mem().write_u32(word, mem().read_u32(word) | 1 << (fd % 32));
            Ret::Void
        }),
        "__FD_ISSET_chk" => handler!(|uc, a| {
            let (fd, set) = (a.u32(), a.u32());
            Ret::U32(mem().read_u32(set + fd / 32 * 4) >> (fd % 32) & 1)
        }),
        "__open_2" => handler!(|uc, a| {
            let (path, flags) = (a.u32(), a.u32());
            checked(uc, open_guest_path(path, flags, 0) as i64)
        }),
        "getpriority" => handler!(|uc, a| {
            let (which, who) = (a.u32(), a.u32());
            clear_errno();
            Ret::U32(unsafe { libc::getpriority(which as _, who as _) } as u32)
        }),
        "setpriority" => handler!(|uc, a| {
            let (which, who, priority) = (a.u32(), a.u32(), a.i32());
            checked(uc, unsafe { libc::setpriority(which as _, who as _, priority) } as i64)
        }),
        // struct rusage for the guest: two 32-bit timevals, then 14 longs (72 bytes).
        "getrusage" => handler!(|uc, a| {
            let (who, out) = (a.i32(), a.u32());
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            if unsafe { libc::getrusage(who, &mut usage) } != 0 {
                sync_errno(uc);
                return Ret::U32(u32::MAX);
            }
            let fields = [
                usage.ru_utime.tv_sec as i64,
                usage.ru_utime.tv_usec as i64,
                usage.ru_stime.tv_sec as i64,
                usage.ru_stime.tv_usec as i64,
                usage.ru_maxrss as i64,
                usage.ru_ixrss as i64,
                usage.ru_idrss as i64,
                usage.ru_isrss as i64,
                usage.ru_minflt as i64,
                usage.ru_majflt as i64,
                usage.ru_nswap as i64,
                usage.ru_inblock as i64,
                usage.ru_oublock as i64,
                usage.ru_msgsnd as i64,
                usage.ru_msgrcv as i64,
                usage.ru_nsignals as i64,
                usage.ru_nvcsw as i64,
                usage.ru_nivcsw as i64,
            ];
            for (i, value) in fields.iter().enumerate() {
                mem().write_u32(out + i as u32 * 4, (*value).clamp(i32::MIN as i64, i32::MAX as i64) as i32 as u32);
            }
            Ret::U32(0)
        }),
        "sigaltstack" => handler!(|uc, a| Ret::U32(0)),
        "open" => handler!(|uc, a| {
            let (path, flags, mode) = (a.u32(), a.u32(), a.u32());
            checked(uc, open_guest_path(path, flags, mode) as i64)
        }),
        // The _chk forms add a buffer size after the same arguments.
        "read" | "__read_chk" => handler!(|uc, a| {
            let (fd, buffer, length) = (a.i32(), a.u32(), a.u32());
            checked(uc, unsafe { libc::read(fd, mem().host(buffer).cast(), length as usize) } as i64)
        }),
        "write" | "__write_chk" => handler!(|uc, a| {
            let (fd, buffer, length) = (a.i32(), a.u32(), a.u32());
            checked(uc, unsafe { libc::write(fd, mem().host(buffer).cast(), length as usize) } as i64)
        }),
        "writev" => handler!(|uc, a| {
            // struct iovec is { pointer, length }: 8 bytes here, 16 on the phone.
            let (fd, vector, count) = (a.i32(), a.u32(), a.i32());
            let parts: Vec<libc::iovec> = (0..count.max(0) as u32)
                .map(|i| libc::iovec {
                    iov_base: mem().host_or_null(mem().read_u32(vector + i * 8)).cast(),
                    iov_len: mem().read_u32(vector + i * 8 + 4) as usize,
                })
                .collect();
            checked(uc, unsafe { libc::writev(fd, parts.as_ptr(), parts.len() as i32) } as i64)
        }),
        "close" => handler!(|uc, a| checked(uc, unsafe { libc::close(a.i32()) } as i64)),
        "lseek" => handler!(|uc, a| {
            let (fd, offset, whence) = (a.i32(), a.i32(), a.i32());
            checked(uc, unsafe { libc::lseek(fd, offset as libc::off_t, whence) } as i64)
        }),
        "lseek64" => handler!(|uc, a| {
            let (fd, offset, whence) = (a.i32(), a.i64(), a.i32());
            let result = unsafe { libc::lseek(fd, offset as libc::off_t, whence) } as i64;
            if result < 0 {
                sync_errno(uc);
            }
            Ret::U64(result as u64)
        }),
        "pread" => handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i32());
            checked(uc, unsafe { libc::pread(fd, mem().host(buffer).cast(), length as usize, offset as libc::off_t) } as i64)
        }),
        "pread64" => handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i64());
            checked(uc, unsafe { libc::pread(fd, mem().host(buffer).cast(), length as usize, offset as libc::off_t) } as i64)
        }),
        "pwrite" => handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i32());
            checked(uc, unsafe { libc::pwrite(fd, mem().host(buffer).cast(), length as usize, offset as libc::off_t) } as i64)
        }),
        "pwrite64" => handler!(|uc, a| {
            let (fd, buffer, length, offset) = (a.i32(), a.u32(), a.u32(), a.i64());
            checked(uc, unsafe { libc::pwrite(fd, mem().host(buffer).cast(), length as usize, offset as libc::off_t) } as i64)
        }),
        "ftruncate" => handler!(|uc, a| {
            let (fd, length) = (a.i32(), a.i32());
            checked(uc, unsafe { libc::ftruncate(fd, length as libc::off_t) } as i64)
        }),
        "ftruncate64" => handler!(|uc, a| {
            let (fd, length) = (a.i32(), a.i64());
            checked(uc, unsafe { libc::ftruncate(fd, length as libc::off_t) } as i64)
        }),
        "truncate" => handler!(|uc, a| {
            let (path, length) = (a.u32(), a.i32());
            checked(uc, unsafe { libc::truncate(host_c(path), length as libc::off_t) } as i64)
        }),
        "fsync" => handler!(|uc, a| checked(uc, unsafe { libc::fsync(a.i32()) } as i64)),
        "rmdir" => handler!(|uc, a| checked(uc, unsafe { libc::rmdir(host_c(a.u32())) } as i64)),
        "strnlen" => handler!(|uc, a| {
            let (text, limit) = (a.u32(), a.u32());
            Ret::U32(unsafe { libc::strnlen(host_c(text), limit as usize) } as u32)
        }),
        "access" => handler!(|uc, a| {
            let (path, mode) = (a.u32(), a.i32());
            checked(uc, unsafe { libc::access(host_c(path), mode) } as i64)
        }),
        "mkdir" => handler!(|uc, a| {
            let (path, mode) = (a.u32(), a.u32());
            checked(uc, unsafe { libc::mkdir(host_c(path), mode as libc::mode_t) } as i64)
        }),
        "chdir" => handler!(|uc, a| checked(uc, unsafe { libc::chdir(host_c(a.u32())) } as i64)),
        "chmod" => handler!(|uc, a| {
            let (path, mode) = (a.u32(), a.u32());
            checked(uc, unsafe { libc::chmod(host_c(path), mode as libc::mode_t) } as i64)
        }),
        "unlink" => handler!(|uc, a| checked(uc, unsafe { libc::unlink(host_c(a.u32())) } as i64)),
        "remove" => handler!(|uc, a| checked(uc, unsafe { libc::remove(host_c(a.u32())) } as i64)),
        "rename" => handler!(|uc, a| {
            let (from, to) = (a.u32(), a.u32());
            checked(uc, unsafe { libc::rename(host_c(from), host_c(to)) } as i64)
        }),
        "umask" => handler!(|uc, a| Ret::U32(unsafe { libc::umask(a.u32() as libc::mode_t) } as u32)),
        "mkstemp" => handler!(|uc, a| checked(uc, unsafe { libc::mkstemp(host_c(a.u32())) } as i64)),
        "statfs" => handler!(|uc, a| {
            let (path, out) = (a.u32(), a.u32());
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::statfs(host_c(path), &mut st) };
            if result == 0 {
                // bionic ARM32 struct statfs: u32 type and block size, five u64 counts, then
                // fsid (2 x i32), name length, fragment size, flags and 4 spare words (84 bytes). Flags
                // stay 0: not every host libc reports them in statfs.
                let m = mem();
                m.write_bytes(out, &[0; 84]);
                m.write_u32(out, st.f_type as u32);
                m.write_u32(out + 4, st.f_bsize as u32);
                for (i, count) in [st.f_blocks, st.f_bfree, st.f_bavail, st.f_files, st.f_ffree].into_iter().enumerate() {
                    m.write_u64(out + 8 + i as u32 * 8, count as u64);
                }
                let fsid: [i32; 2] = unsafe { std::mem::transmute(st.f_fsid) };
                m.write_u32(out + 48, fsid[0] as u32);
                m.write_u32(out + 52, fsid[1] as u32);
                m.write_u32(out + 56, st.f_namelen as u32);
                m.write_u32(out + 60, st.f_frsize as u32);
            }
            checked(uc, result as i64)
        }),
        // utime(path, times): times is { time_t actime, modtime } with 32-bit time_t, or null for now.
        "utime" => handler!(|uc, a| {
            let (path, times) = (a.u32(), a.u32());
            let host_times = (times != 0).then(|| libc::utimbuf {
                actime: mem().read_u32(times) as i32 as libc::time_t,
                modtime: mem().read_u32(times + 4) as i32 as libc::time_t,
            });
            let pointer = host_times.as_ref().map_or(std::ptr::null(), |t| t as *const libc::utimbuf);
            checked(uc, unsafe { libc::utime(host_c(path), pointer) } as i64)
        }),
        "pathconf" => handler!(|uc, a| {
            let (path, name) = (a.u32(), a.u32());
            // Games use bionic's _PC_ numbers (0-19), the same as the phone's own; glibc (the PC
            // tool) numbers them differently.
            #[cfg(target_os = "android")]
            let host = (name < 20).then_some(name as i32);
            #[cfg(not(target_os = "android"))]
            let host = [13, 0, 1, 2, 3, 4, 5, 20, 18, 14, 15, 16, 17, 19, 6, 7, 8, 10, 11, 9].get(name as usize).copied();
            let Some(host) = host else {
                set_errno(uc, libc::EINVAL);
                return Ret::U32(u32::MAX);
            };
            // long is 32 bits here.
            let value = unsafe { libc::pathconf(host_c(path), host) };
            checked(uc, value.clamp(i32::MIN as libc::c_long, i32::MAX as libc::c_long) as i64)
        }),
        "fnmatch" => handler!(|uc, a| {
            let (pattern, text, flags) = (a.u32(), a.u32(), a.u32());
            // bionic flag bits: NOESCAPE 1, PATHNAME 2, PERIOD 4, LEADING_DIR 8, CASEFOLD 16.
            let host_flags = [
                (1, libc::FNM_NOESCAPE),
                (2, libc::FNM_PATHNAME),
                (4, libc::FNM_PERIOD),
                (16, libc::FNM_CASEFOLD),
            ]
            .into_iter()
            .filter(|&(bit, _)| flags & bit != 0)
            .fold(0, |all, (_, host)| all | host);
            let result = unsafe { libc::fnmatch(host_c(pattern), host_c(text), host_flags) };
            // FNM_NOMATCH is 1 on bionic.
            Ret::U32(if result == 0 { 0 } else { 1 })
        }),
        "stat" | "lstat" => handler!(|uc, a| {
            let (path, out) = (a.u32(), a.u32());
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::stat(host_c(path), &mut st) };
            if result == 0 {
                write_stat32(mem(), out, &st);
            }
            checked(uc, result as i64)
        }),
        "fstat" => handler!(|uc, a| {
            let (fd, out) = (a.i32(), a.u32());
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::fstat(fd, &mut st) };
            if result == 0 {
                write_stat32(mem(), out, &st);
            }
            checked(uc, result as i64)
        }),
        "fcntl" => handler!(|uc, a| {
            let (fd, command, argument) = (a.i32(), a.i32(), a.u32());
            if (0..=4).contains(&command) {
                checked(uc, unsafe { libc::fcntl(fd, command, argument as libc::c_int) } as i64)
            } else {
                set_errno(uc, libc::EINVAL);
                Ret::U32(u32::MAX)
            }
        }),
        "dup" => handler!(|uc, a| checked(uc, unsafe { libc::dup(a.i32()) } as i64)),
        "dup2" => handler!(|uc, a| {
            let (from, to) = (a.i32(), a.i32());
            checked(uc, unsafe { libc::dup2(from, to) } as i64)
        }),
        "pipe" => handler!(|uc, a| {
            let out = a.u32();
            let mut fds = [0i32; 2];
            let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
            mem().write_u32(out, fds[0] as u32);
            mem().write_u32(out + 4, fds[1] as u32);
            checked(uc, result as i64)
        }),
        "poll" => handler!(|uc, a| {
            // struct pollfd is the same 8 bytes on both sides.
            let (fds, count, timeout) = (a.u32(), a.u32(), a.i32());
            let fds = mem().host(fds) as usize;
            let (result, error) = crate::runtime::without_gil(|| {
                let result = unsafe { libc::poll(fds as *mut libc::pollfd, count as libc::nfds_t, timeout) };
                (result, last_errno())
            });
            if result < 0 {
                set_errno(uc, error);
            }
            Ret::U32(result as u32)
        }),
        "opendir" => handler!(|uc, a| {
            let path = a.u32();
            let directory = unsafe { libc::opendir(host_c(path)) };
            if directory.is_null() {
                sync_errno(uc);
                return Ret::U32(0);
            }
            // The guest handle doubles as the buffer readdir() fills.
            let handle = crate::runtime::heap().calloc(1, 280);
            shared().state.directories.insert(handle, (directory as usize, handle));
            Ret::U32(handle)
        }),
        "readdir" => handler!(|uc, a| {
            let handle = a.u32();
            let Some(&(directory, buffer)) = shared().state.directories.get(&handle) else { return Ret::U32(0) };
            let entry = unsafe { libc::readdir(directory as *mut libc::DIR) };
            if entry.is_null() {
                return Ret::U32(0);
            }
            write_dirent(buffer, unsafe { &*entry });
            Ret::U32(buffer)
        }),
        // readdir_r(directory, entry, result): fills the caller's entry; *result is it, or null at the end.
        "readdir_r" => handler!(|uc, a| {
            let (handle, out, result) = (a.u32(), a.u32(), a.u32());
            let Some(&(directory, _)) = shared().state.directories.get(&handle) else { return Ret::U32(libc::EBADF as u32) };
            let entry = unsafe { libc::readdir(directory as *mut libc::DIR) };
            if entry.is_null() {
                mem().write_u32(result, 0);
            } else {
                write_dirent(out, unsafe { &*entry });
                mem().write_u32(result, out);
            }
            Ret::U32(0)
        }),
        "closedir" => handler!(|uc, a| {
            let handle = a.u32();
            let removed = shared().state.directories.remove(&handle);
            match removed {
                Some((directory, _)) => {
                    unsafe { libc::closedir(directory as *mut libc::DIR) };
                    crate::runtime::heap().free(handle);
                    Ret::U32(0)
                }
                None => Ret::U32(u32::MAX),
            }
        }),

        // ---- time ----
        "time" => handler!(|uc, a| {
            let out = a.u32();
            let now = unsafe { libc::time(std::ptr::null_mut()) } as u32;
            if out != 0 {
                mem().write_u32(out, now);
            }
            Ret::U32(now)
        }),
        "gettimeofday" => handler!(|uc, a| {
            let out = a.u32();
            let mut now: libc::timeval = unsafe { std::mem::zeroed() };
            unsafe { libc::gettimeofday(&mut now, std::ptr::null_mut()) };
            if out != 0 {
                mem().write_u32(out, now.tv_sec as u32);
                mem().write_u32(out + 4, now.tv_usec as u32);
            }
            Ret::U32(0)
        }),
        // clock_t is 32 bits; CLOCKS_PER_SEC is a million.
        "clock" => handler!(|uc, a| {
            let mut used: libc::timespec = unsafe { std::mem::zeroed() };
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut used) };
            Ret::U32((used.tv_sec as u64 * 1_000_000 + used.tv_nsec as u64 / 1000) as u32)
        }),
        "clock_getres" => handler!(|uc, a| {
            let (clock, out) = (a.i32(), a.u32());
            let mut resolution: libc::timespec = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::clock_getres(clock, &mut resolution) };
            if result == 0 && out != 0 {
                mem().write_u32(out, resolution.tv_sec as u32);
                mem().write_u32(out + 4, resolution.tv_nsec as u32);
            }
            checked(uc, result as i64)
        }),
        "readlink" => handler!(|uc, a| {
            let (path, buffer, size) = (a.u32(), a.u32(), a.u32());
            checked(uc, unsafe { libc::readlink(host_c(path), host_c(buffer), size as usize) } as i64)
        }),
        "isatty" => handler!(|uc, a| Ret::U32((unsafe { libc::isatty(a.i32()) } == 1) as u32)),
        // bionic's __pthread_cleanup_t: { prev, routine, arg }.
        "__pthread_cleanup_push" => handler!(|uc, a| {
            let (record, routine, argument) = (a.u32(), a.u32(), a.u32());
            mem().write_u32(record + 4, routine);
            mem().write_u32(record + 8, argument);
            Ret::Void
        }),
        "__pthread_cleanup_pop" => handler!(|uc, a| {
            let (record, execute) = (a.u32(), a.u32());
            if execute != 0 {
                let (routine, argument) = (mem().read_u32(record + 4), mem().read_u32(record + 8));
                if let Err(error) = call_guest(uc, routine, &[argument]) {
                    stop_with_fault(uc, error);
                }
            }
            Ret::Void
        }),
        "clock_gettime" => handler!(|uc, a| {
            let (clock, out) = (a.i32(), a.u32());
            let mut now: libc::timespec = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::clock_gettime(clock, &mut now) };
            if result == 0 {
                mem().write_u32(out, now.tv_sec as u32);
                mem().write_u32(out + 4, now.tv_nsec as u32);
            }
            checked(uc, result as i64)
        }),
        "localtime" => handler!(|uc, a| broken_down_time(a, true)),
        "gmtime" => handler!(|uc, a| broken_down_time(a, false)),
        "gmtime_r" => handler!(|uc, a| {
            let (clock, out) = (a.u32(), a.u32());
            let seconds = mem().read_u32(clock) as i32 as libc::time_t;
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::gmtime_r(&seconds, &mut tm) };
            write_tm32(out, &tm);
            Ret::U32(out)
        }),
        "localtime_r" => handler!(|uc, a| {
            let (clock, out) = (a.u32(), a.u32());
            let seconds = mem().read_u32(clock) as i32 as libc::time_t;
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&seconds, &mut tm) };
            write_tm32(out, &tm);
            Ret::U32(out)
        }),
        "mktime" => handler!(|uc, a| {
            let guest_tm = a.u32();
            let mut tm = read_tm32(mem(), guest_tm);
            let result = unsafe { libc::mktime(&mut tm) };
            write_tm32(guest_tm, &tm);
            Ret::U32(result as u32)
        }),
        "difftime" => handler!(|uc, a| {
            let (end, start) = (a.i32(), a.i32());
            Ret::F64(end as f64 - start as f64)
        }),
        "strftime" => handler!(|uc, a| {
            let (buffer, size, format, guest_tm) = (a.u32(), a.u32(), a.u32(), a.u32());
            let tm = read_tm32(mem(), guest_tm);
            Ret::U32(unsafe { libc::strftime(host_c(buffer), size as usize, host_c(format), &tm) } as u32)
        }),

        "wcsftime" => handler!(|uc, a| {
            let (buffer, size, format, guest_tm) = (a.u32(), a.u32(), a.u32(), a.u32());
            let tm = read_tm32(mem(), guest_tm);
            let mut format: Vec<u8> = guest_wide_text(format).into_iter().collect::<String>().into_bytes();
            format.push(0);
            let mut text = vec![0u8; size as usize * 4 + 1];
            let length = unsafe { libc::strftime(text.as_mut_ptr().cast(), text.len(), format.as_ptr().cast(), &tm) };
            let wide: Vec<char> = String::from_utf8_lossy(&text[..length]).chars().collect();
            if length == 0 || wide.len() >= size as usize {
                return Ret::U32(0);
            }
            write_wide_text(buffer, &wide);
            Ret::U32(wide.len() as u32)
        }),

        // ---- process and environment ----
        "getpid" => handler!(|uc, a| Ret::U32(unsafe { libc::getpid() } as u32)),
        "getppid" => handler!(|uc, a| Ret::U32(unsafe { libc::getppid() } as u32)),
        "getuid" => handler!(|uc, a| Ret::U32(unsafe { libc::getuid() })),
        "geteuid" => handler!(|uc, a| Ret::U32(unsafe { libc::geteuid() })),
        "getgid" => handler!(|uc, a| Ret::U32(unsafe { libc::getgid() })),
        "getegid" => handler!(|uc, a| Ret::U32(unsafe { libc::getegid() })),
        "getdtablesize" => handler!(|uc, a| Ret::U32(1024)),
        "gethostname" => handler!(|uc, a| {
            let (buffer, length) = (a.u32(), a.u32());
            checked(uc, unsafe { libc::gethostname(host_c(buffer), length as usize) } as i64)
        }),
        "getenv" => handler!(|uc, a| {
            let name = a.u32();
            let value = unsafe { libc::getenv(host_c(name)) };
            if value.is_null() {
                return Ret::U32(0);
            }
            let text = unsafe { std::ffi::CStr::from_ptr(value) }.to_bytes().to_vec();
            Ret::U32(guest_string_in_heap(&text))
        }),
        "setlocale" => handler!(|uc, a| {
            Ret::U32(shared().cached_cstr("locale:C".to_string(), "C"))
        }),
        "sysconf" => handler!(|uc, a| {
            // bionic uses the same _SC_ numbers on every architecture.
            Ret::U32(unsafe { libc::sysconf(a.i32()) } as u32)
        }),
        "__system_property_get" => handler!(|uc, a| {
            let (name, value) = (a.u32(), a.u32());
            #[cfg(target_os = "android")]
            {
                extern "C" {
                    fn __system_property_get(name: *const libc::c_char, value: *mut libc::c_char) -> i32;
                }
                return Ret::U32(unsafe { __system_property_get(host_c(name), host_c(value)) } as u32);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = name;
                mem().write_u8(value, 0);
                Ret::U32(0)
            }
        }),
        "arc4random_buf" => handler!(|uc, a| {
            let (buffer, length) = (a.u32(), a.u32());
            unsafe { libc::getrandom(mem().host(buffer).cast(), length as usize, 0) };
            Ret::Void
        }),
        "lrand48" => handler!(|uc, a| Ret::U32(unsafe { lrand48() } as u32)),
        "srand48" => handler!(|uc, a| {
            unsafe { srand48(a.i32() as libc::c_long) };
            Ret::Void
        }),
        "getpwuid" => handler!(|uc, a| Ret::U32(0)),
        // For the self-test: tests that need the translator (time slicing inside game code).
        "arm32_bridge_uses_translator" => handler!(|uc, a| Ret::U32(crate::dynarec::enabled() as u32)),
        // The checker runs every block twice (and one thread at a time): long tests shrink.
        "arm32_bridge_is_checking" => handler!(|uc, a| Ret::U32(crate::dynarec::check::enabled() as u32)),
        "arm32_bridge_runs_in_parallel" => handler!(|uc, a| Ret::U32(crate::runtime::parallel() as u32)),
        "arm32_bridge_forces_fallbacks" => handler!(|uc, a| Ret::U32(crate::dynarec::thumb::force_sync_fallback() as u32)),
        "fork" | "execl" | "system" | "setsid" => handler!(|uc, a| {
            set_errno(uc, libc::ENOSYS);
            Ret::U32(u32::MAX)
        }),
        "abort" => handler!(|uc, a| {
            stop_with_fault(uc, "game called abort()".into());
            Ret::Void
        }),
        "exit" | "_exit" => handler!(|uc, a| {
            uc.get_data_mut().exit_code = Some(a.i32());
            uc.stop();
            Ret::Void
        }),
        "__errno" => handler!(|uc, a| Ret::U32(uc.get_data().errno_slot)),
        "__stack_chk_fail" => handler!(|uc, a| {
            stop_with_fault(uc, "stack protector tripped (__stack_chk_fail)".into());
            Ret::Void
        }),
        "__assert2" => handler!(|uc, a| {
            let (file, line, function, message) = (a.u32(), a.u32(), a.u32(), a.u32());
            let m = mem();
            let text = format!("game assertion failed: {} at {}:{} in {}", m.string(message), m.string(file), line, m.string(function));
            stop_with_fault(uc, text);
            Ret::Void
        }),
        "__cxa_atexit" => handler!(|uc, a| {
            let (function, argument) = (a.u32(), a.u32());
            shared().state.at_exit.push((function, argument));
            Ret::U32(0)
        }),
        // The ARM form of __cxa_atexit, with the first two arguments swapped.
        "__aeabi_atexit" => handler!(|uc, a| {
            let (argument, function) = (a.u32(), a.u32());
            shared().state.at_exit.push((function, argument));
            Ret::U32(0)
        }),
        "__cxa_finalize" => handler!(|uc, a| Ret::Void),
        "__cxa_pure_virtual" => handler!(|uc, a| {
            stop_with_fault(uc, "game called a pure virtual function (__cxa_pure_virtual)".into());
            Ret::Void
        }),

        // Signals between game threads are in signals.rs; no timer signals.
        "alarm" => handler!(|uc, a| Ret::U32(0)),

        // ---- setjmp / longjmp ----
        "setjmp" | "_setjmp" | "sigsetjmp" => handler!(|uc, a| {
            let buffer = a.u32();
            save_jump_buffer(uc, buffer);
            Ret::U32(0)
        }),
        "longjmp" | "_longjmp" | "siglongjmp" => handler!(|uc, a| {
            let (buffer, value) = (a.u32(), a.u32());
            restore_jump_buffer(uc, buffer, value)
        }),

        // ---- sorting with guest callbacks ----
        "qsort" => handler!(|uc, a| {
            let (base, count, size, compare) = (a.u32(), a.u32(), a.u32(), a.u32());
            if count < 2 || size == 0 {
                return Ret::Void;
            }
            let m = mem();
            let items: Vec<Vec<u8>> = (0..count).map(|i| m.bytes(base + i * size, size as usize).to_vec()).collect();
            let left = crate::runtime::heap().malloc(size);
            let right = crate::runtime::heap().malloc(size);
            let mut order: Vec<usize> = (0..items.len()).collect();
            order.sort_by(|&x, &y| {
                m.write_bytes(left, &items[x]);
                m.write_bytes(right, &items[y]);
                match call_guest(uc, compare, &[left, right]) {
                    Ok(result) => (result as u32 as i32).cmp(&0),
                    Err(_) => std::cmp::Ordering::Equal,
                }
            });
            for (slot, &item) in order.iter().enumerate() {
                m.write_bytes(base + slot as u32 * size, &items[item]);
            }
            crate::runtime::heap().free(left);
            crate::runtime::heap().free(right);
            Ret::Void
        }),
        "bsearch" => handler!(|uc, a| {
            let (key, base, count) = (a.u32(), a.u32(), a.u32());
            let (size, compare) = (a.u32(), a.u32());
            let (mut low, mut high) = (0u32, count);
            while low < high {
                let middle = low + (high - low) / 2;
                let element = base + middle * size;
                let Ok(result) = call_guest(uc, compare, &[key, element]) else { return Ret::U32(0) };
                match (result as u32 as i32).cmp(&0) {
                    std::cmp::Ordering::Equal => return Ret::U32(element),
                    std::cmp::Ordering::Less => high = middle,
                    std::cmp::Ordering::Greater => low = middle + 1,
                }
            }
            Ret::U32(0)
        }),

        // ---- dynamic linking ----
        // A game library is really loaded (its lib<name>-arm32.so); its
        // handle is its loader index + FIRST_LIBRARY_HANDLE. Phone libraries
        // get PHONE_HANDLE: the bridge supplies their functions itself.
        "dlopen" => handler!(|uc, a| {
            let (name, _flags) = (a.u32(), a.u32());
            if name == 0 {
                return Ret::U32(PHONE_HANDLE);
            }
            let name = mem().string(name);
            match crate::elf::open_by_name(uc, &name) {
                Some(Ok(index)) => {
                    crate::log::line(&format!("[bridge] game code opened {name}"));
                    Ret::U32(FIRST_LIBRARY_HANDLE + index as u32)
                }
                Some(Err(error)) => {
                    crate::log::line(&format!("[bridge] game code could not open {name}: {error}"));
                    Ret::U32(0)
                }
                None => {
                    eprintln!("[bridge] dlopen({name})");
                    Ret::U32(PHONE_HANDLE)
                }
            }
        }),
        "dlsym" => handler!(|uc, a| {
            let (handle, name) = (a.u32(), a.u32());
            let name = mem().string(name);
            // Waits while another thread is loading a library (or running its start-up code).
            let _dl = crate::elf::lock_dl();
            // A game library's own symbol first (each part has its own JNI_OnLoad).
            let own = handle
                .checked_sub(FIRST_LIBRARY_HANDLE)
                .and_then(|index| {
                    crate::elf::with_loader(|loader| loader.libraries.get(index as usize).and_then(|l| l.export(uc, &name)))
                });
            Ret::U32(own.unwrap_or_else(|| lookup_symbol(&name)))
        }),
        // dladdr(address, info): which game library holds an address, and the nearest symbol at or
        // below it. Dl_info is { file name, base, symbol name, symbol address }.
        "dladdr" => handler!(|uc, a| {
            let (address, info) = (a.u32(), a.u32());
            let found = crate::elf::with_loader(|loader| {
                let library = loader.libraries.iter().find(|l| (l.base..l.base + l.size).contains(&address))?;
                let symbol = library.exports(uc).into_iter().filter(|(_, at)| *at <= address).max_by_key(|(_, at)| *at);
                Some((library.name.clone(), library.base, symbol))
            });
            let Some((name, base, symbol)) = found else { return Ret::U32(0) };
            let m = mem();
            m.write_u32(info, shared().cached_cstr(format!("dladdr:{name}"), &name));
            m.write_u32(info + 4, base);
            match symbol {
                Some((symbol_name, at)) => {
                    m.write_u32(info + 8, shared().cached_cstr(format!("dladdr:{symbol_name}"), &symbol_name));
                    m.write_u32(info + 12, at);
                }
                None => {
                    m.write_u32(info + 8, 0);
                    m.write_u32(info + 12, 0);
                }
            }
            Ret::U32(1)
        }),
        "eglGetProcAddress" => handler!(|uc, a| {
            let name = mem().string(a.u32());
            Ret::U32(lookup_symbol(&name))
        }),
        "getpagesize" => handler!(|uc, a| Ret::U32(crate::mem::PAGE)),
        // A 32-bit sigset_t is one word: bit n-1 for signal n.
        "sigemptyset" => handler!(|uc, a| {
            mem().write_u32(a.u32(), 0);
            Ret::U32(0)
        }),
        "sigfillset" => handler!(|uc, a| {
            mem().write_u32(a.u32(), u32::MAX);
            Ret::U32(0)
        }),
        "sigaddset" | "sigdelset" | "sigismember" => handler!(|uc, a| {
            let (set, signal) = (a.u32(), a.u32());
            if !(1..=32).contains(&signal) {
                set_errno(uc, libc::EINVAL);
                return Ret::U32(u32::MAX);
            }
            let bit = 1u32 << (signal - 1);
            let value = mem().read_u32(set);
            match uc.get_data().current_call {
                "sigaddset" => mem().write_u32(set, value | bit),
                "sigdelset" => mem().write_u32(set, value & !bit),
                _ => return Ret::U32((value & bit != 0) as u32),
            }
            Ret::U32(0)
        }),
        // Thread names and similar process settings don't matter to the bridge.
        "prctl" => handler!(|uc, a| {
            let option = a.u32();
            if option != libc::PR_SET_NAME as u32 {
                crate::log::line(&format!("[bridge] prctl option {option} ignored"));
            }
            Ret::U32(0)
        }),
        // Bionic has only the C/UTF-8 locale; one shared handle stands for it.
        "newlocale" | "duplocale" => handler!(|uc, a| Ret::U32(locale_handle())),
        "uselocale" => handler!(|uc, a| Ret::U32(locale_handle())),
        "freelocale" => handler!(|uc, a| Ret::Void),
        // Reads one line and scans it: enough for the /proc and /sys values games read.
        "fscanf" => handler!(|uc, a| {
            let (file, format) = (a.u32(), a.u32());
            let Some(HostFile::Stream(stream)) = host_file(file) else { return Ret::U32(u32::MAX) };
            let mut line = [0u8; 1024];
            let got = unsafe { libc::fgets(line.as_mut_ptr().cast(), line.len() as i32, stream as *mut libc::FILE) };
            if got.is_null() {
                return Ret::U32(u32::MAX);
            }
            let length = line.iter().position(|&b| b == 0).unwrap_or(line.len() - 1);
            let input = crate::runtime::heap().malloc(length as u32 + 1);
            mem().write_bytes(input, &line[..=length.min(line.len() - 1)]);
            mem().write_u8(input + length as u32, 0);
            let result = scanf::scan(mem(), input, format, a);
            crate::runtime::heap().free(input);
            Ret::U32(result as u32)
        }),
        // Memory blocks come from the guest heap, page aligned. File mappings are
        // private copies (writes don't reach the file).
        "mmap" | "mmap64" => handler!(|uc, a| {
            let (address, length, protection, flags, fd, offset) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            map_memory(uc, address, length, protection, flags, fd, offset as u64)
        }),
        "__mmap2" => handler!(|uc, a| {
            let (address, length, protection, flags, fd, pages) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            map_memory(uc, address, length, protection, flags, fd, pages as u64 * crate::mem::PAGE as u64)
        }),
        "munmap" => handler!(|uc, a| {
            let (address, length) = (a.u32(), a.u32());
            // Checked and removed in one go: of two threads unmapping the same block, only one frees it.
            let mut shared = shared();
            if shared.state.mappings.get(&address).copied() == Some(page_round(length)) {
                shared.state.mappings.remove(&address);
                crate::runtime::heap().free(address);
            }
            // Part of a block: it stays in use until the whole block goes.
            Ret::U32(0)
        }),
        "mprotect" => handler!(|uc, a| {
            let (address, length, protection) = (a.u32(), a.u32(), a.u32());
            if protection & libc::PROT_EXEC as u32 != 0 && !crate::mem::fixed(address) {
                crate::log::line(&format!(
                    "[bridge] game made memory executable at {address:#x} ({length} bytes): code made while running"
                ));
            }
            // Library code or constants made writable: they may change now.
            // One at a time, so a second call for the same page returns only
            // after the first has dropped the old translations.
            static UNFIXING: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one_at_a_time = UNFIXING.lock().unwrap_or_else(|e| e.into_inner());
            let end = address.saturating_add(page_round(length));
            if protection & libc::PROT_WRITE as u32 != 0 && crate::mem::unfix(address, end) {
                crate::log::line(&format!(
                    "[bridge] game made read-only library memory writable at {address:#x} ({length} bytes): watched as made code from now on"
                ));
                crate::runtime::note_code_change(address, end);
                crate::dynarec::invalidate_library(uc, address, end);
            }
            Ret::U32(0)
        }),
        // MADV_DONTNEED empties anonymous memory; other advice changes nothing.
        "madvise" => handler!(|uc, a| {
            let (address, length, advice) = (a.u32(), a.u32(), a.u32());
            let shared = shared();
            if advice == libc::MADV_DONTNEED as u32 && inside_mapping(&shared, address, length) {
                unsafe { std::ptr::write_bytes(mem().host(address), 0, length as usize) };
            }
            Ret::U32(0)
        }),
        "gettid" => handler!(|uc, a| Ret::U32(unsafe { libc::gettid() } as u32)),
        // struct utsname is six 65-byte strings on both sides. A 32-bit program
        // on a 64-bit ARM kernel sees "armv8l" as the machine.
        "uname" => handler!(|uc, a| {
            let out = a.u32();
            let mut names: libc::utsname = unsafe { std::mem::zeroed() };
            if unsafe { libc::uname(&mut names) } != 0 {
                sync_errno(uc);
                return Ret::U32(u32::MAX);
            }
            let fields = [names.sysname, names.nodename, names.release, names.version, names.machine, names.domainname];
            for (i, field) in fields.iter().enumerate() {
                let bytes: Vec<u8> = field.iter().map(|&c| c as u8).collect();
                mem().write_bytes(out + i as u32 * 65, &bytes[..65]);
            }
            mem().write_bytes(out + 4 * 65, b"armv8l\0");
            Ret::U32(0)
        }),
        // rlim_t is 32 bits for the guest; unlimited stays unlimited.
        "getrlimit" => handler!(|uc, a| {
            let (resource, out) = (a.u32(), a.u32());
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if unsafe { libc::getrlimit(resource as _, &mut limit) } != 0 {
                sync_errno(uc);
                return Ret::U32(u32::MAX);
            }
            let narrow = |value: libc::rlim_t| if value == libc::RLIM_INFINITY { u32::MAX } else { value.min(u32::MAX as libc::rlim_t - 1) as u32 };
            mem().write_u32(out, narrow(limit.rlim_cur));
            mem().write_u32(out + 4, narrow(limit.rlim_max));
            Ret::U32(0)
        }),
        "dlclose" => handler!(|uc, a| Ret::U32(0)),
        "dlerror" => handler!(|uc, a| Ret::U32(0)),
        "getcwd" => handler!(|uc, a| {
            let (buffer, size) = (a.u32(), a.u32());
            match std::env::current_dir() {
                Ok(path) => {
                    let text = path.to_string_lossy().into_owned();
                    let needed = text.len() as u32 + 1;
                    if (buffer != 0 || size != 0) && needed > size {
                        set_errno(uc, libc::ERANGE);
                        return Ret::U32(0);
                    }
                    // getcwd(NULL, size) allocates the buffer (freed by the game with free).
                    let buffer = if buffer == 0 { crate::runtime::heap().malloc(size.max(needed)) } else { buffer };
                    mem().write_bytes(buffer, text.as_bytes());
                    mem().write_u8(buffer + text.len() as u32, 0);
                    Ret::U32(buffer)
                }
                Err(_) => {
                    sync_errno(uc);
                    Ret::U32(0)
                }
            }
        }),
        // Older ARM toolchains' name for dl_unwind_find_exidx.
        "dl_unwind_find_exidx" | "__gnu_Unwind_Find_exidx" => handler!(|uc, a| {
            let (pc, count_out) = (a.u32(), a.u32());
            for &(start, end, table, count) in &shared().state.unwind_tables {
                if pc >= start && pc < end {
                    mem().write_u32(count_out, count);
                    return Ret::U32(table);
                }
            }
            mem().write_u32(count_out, 0);
            Ret::U32(0)
        }),

        _ => None,
    }
}
