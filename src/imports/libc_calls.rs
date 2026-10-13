//! libc, libdl and liblog for 32-bit callers. Pointers are guest addresses
//! (turned into host pointers with `mem.host`), `long`/`size_t`/`time_t`/
//! `off_t` are 32 bits, and structs with longs or pointers in them are
//! converted field by field.

use crate::handler;
use crate::imports::{printf, scanf, HostFile};
use crate::mem::GuestMem;
use crate::runtime::{call_guest, mem, shared, stop_with_fault, stub_for, ArgSource, Args, GuestCpu, Handler, Ret, Uc, VaList, LR, SP};


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

fn guest_string_in_heap(text: &[u8]) -> u32 {
    shared().heap.strdup(text)
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
        let cached = shared().state.data_imports.get(&key).copied();
        match cached {
            Some(address) => address,
            None => {
                let address = shared().misc_cstr(&String::from_utf8_lossy(&text));
                shared().state.data_imports.insert(key, address);
                address
            }
        }
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
    let handle = shared().heap.calloc(1, 84);
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

/// dlopen handle for phone libraries (and dlopen(NULL)).
const PHONE_HANDLE: u32 = 1;
/// dlopen handle of the shared loader's first game library.
const FIRST_LIBRARY_HANDLE: u32 = 0x100;

/// Address for a symbol looked up at run time (dlsym, eglGetProcAddress):
/// a loaded game library's export, or a stand-in if the bridge has one.
fn lookup_symbol(name: &str) -> u32 {
    if let Some(&address) = shared().guest_exports.get(name) {
        return address;
    }
    if crate::imports::handler_for(name).is_some() {
        return stub_for(name);
    }
    eprintln!("[bridge] run-time lookup of {name}: not available");
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

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        // ---- memory ----
        "malloc" => handler!(|uc, a| {
            let size = a.u32();
            Ret::U32(shared().heap.malloc(size))
        }),
        "calloc" => handler!(|uc, a| {
            let count = a.u32();
            let size = a.u32();
            Ret::U32(shared().heap.calloc(count, size))
        }),
        "realloc" => handler!(|uc, a| {
            let pointer = a.u32();
            let size = a.u32();
            Ret::U32(shared().heap.realloc(pointer, size))
        }),
        "free" => handler!(|uc, a| {
            let pointer = a.u32();
            shared().heap.free(pointer);
            Ret::Void
        }),
        "memalign" => handler!(|uc, a| {
            let align = a.u32();
            let size = a.u32();
            Ret::U32(shared().heap.memalign(align, size))
        }),
        "posix_memalign" => handler!(|uc, a| {
            let out = a.u32();
            let align = a.u32();
            let size = a.u32();
            let pointer = shared().heap.memalign(align, size);
            mem().write_u32(out, pointer);
            Ret::U32(if pointer == 0 { libc::ENOMEM as u32 } else { 0 })
        }),
        "memcpy" | "__aeabi_memcpy" | "__aeabi_memcpy4" | "__aeabi_memcpy8" | "memmove" | "__aeabi_memmove"
        | "__aeabi_memmove4" | "__aeabi_memmove8" => handler!(|uc, a| {
            let (destination, source, length) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            unsafe { std::ptr::copy(m.host(source), m.host(destination), length as usize) };
            Ret::U32(destination)
        }),
        "memset" => handler!(|uc, a| {
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
        "strlen" => handler!(|uc, a| Ret::U32(unsafe { libc::strlen(host_c(a.u32())) } as u32)),
        "strcmp" => handler!(|uc, a| {
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
        "strchr" => handler!(|uc, a| {
            let (text, c) = (a.u32(), a.i32());
            back(unsafe { libc::strchr(host_c(text), c) })
        }),
        "strrchr" => handler!(|uc, a| {
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
        "strcpy" => handler!(|uc, a| {
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
        "strlcpy" => handler!(|uc, a| {
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
        "strtoll" | "strtoull" => handler!(|uc, a| {
            let (text, end_out, base) = (a.u32(), a.u32(), a.i32());
            let mut end: *mut libc::c_char = std::ptr::null_mut();
            let value = unsafe { libc::strtoull(host_c(text), &mut end, base) };
            if end_out != 0 {
                mem().write_u32(end_out, mem().guest(end as *const u8));
            }
            sync_errno(uc);
            Ret::U64(value)
        }),
        "strtod" => handler!(|uc, a| {
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
        "tolower" => handler!(|uc, a| Ret::U32(unsafe { libc::tolower(a.i32()) } as u32)),
        "toupper" => handler!(|uc, a| Ret::U32(unsafe { libc::toupper(a.i32()) } as u32)),
        "isspace" => handler!(|uc, a| Ret::U32((unsafe { libc::isspace(a.i32()) } != 0) as u32)),
        "isupper" => handler!(|uc, a| Ret::U32((unsafe { libc::isupper(a.i32()) } != 0) as u32)),
        "isblank" => handler!(|uc, a| Ret::U32((unsafe { libc::isblank(a.i32()) } != 0) as u32)),
        "wcscmp" => handler!(|uc, a| {
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
            let stream = unsafe { libc::fopen(host_c(path), host_c(mode)) };
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
                    shared().heap.free(file);
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
        "fputwc" => handler!(|uc, a| {
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
        "open" => handler!(|uc, a| {
            let (path, flags, mode) = (a.u32(), a.u32(), a.u32());
            checked(uc, unsafe { libc::open(host_c(path), host_open_flags(flags), mode) } as i64)
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
        "close" => handler!(|uc, a| checked(uc, unsafe { libc::close(a.i32()) } as i64)),
        "lseek" => handler!(|uc, a| {
            let (fd, offset, whence) = (a.i32(), a.i32(), a.i32());
            checked(uc, unsafe { libc::lseek(fd, offset as libc::off_t, whence) } as i64)
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
            checked(uc, unsafe { libc::poll(mem().host(fds).cast(), count as libc::nfds_t, timeout) } as i64)
        }),
        "opendir" => handler!(|uc, a| {
            let path = a.u32();
            let directory = unsafe { libc::opendir(host_c(path)) };
            if directory.is_null() {
                sync_errno(uc);
                return Ret::U32(0);
            }
            // The guest handle doubles as the buffer readdir() fills.
            let handle = shared().heap.calloc(1, 280);
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
            // bionic ARM32 dirent: u64 d_ino, i64 d_off, u16 d_reclen, u8 d_type, char d_name[256].
            let entry = unsafe { &*entry };
            let m = mem();
            m.write_u64(buffer, entry.d_ino as u64);
            m.write_u64(buffer + 8, entry.d_off as u64);
            m.write_u16(buffer + 16, 280);
            m.write_u8(buffer + 18, entry.d_type);
            let name = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
            let length = name.len().min(255);
            m.write_bytes(buffer + 19, &name[..length]);
            m.write_u8(buffer + 19 + length as u32, 0);
            Ret::U32(buffer)
        }),
        "closedir" => handler!(|uc, a| {
            let handle = a.u32();
            let removed = shared().state.directories.remove(&handle);
            match removed {
                Some((directory, _)) => {
                    unsafe { libc::closedir(directory as *mut libc::DIR) };
                    shared().heap.free(handle);
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
            let key = "locale:C".to_string();
            if let Some(&address) = shared().state.data_imports.get(&key) {
                return Ret::U32(address);
            }
            let address = shared().misc_cstr("C");
            shared().state.data_imports.insert(key, address);
            Ret::U32(address)
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
        "__cxa_finalize" => handler!(|uc, a| Ret::Void),

        // ---- signals: recorded, never installed on the host ----
        "sigaction" => handler!(|uc, a| {
            let (_signal, _action, old) = (a.u32(), a.u32(), a.u32());
            if old != 0 {
                unsafe { std::ptr::write_bytes(mem().host(old), 0, 16) };
            }
            Ret::U32(0)
        }),
        "bsd_signal" | "signal" => handler!(|uc, a| Ret::U32(0)),
        "sigprocmask" => handler!(|uc, a| {
            let (_how, _set, old) = (a.u32(), a.u32(), a.u32());
            if old != 0 {
                mem().write_u32(old, 0);
            }
            Ret::U32(0)
        }),
        "raise" | "kill" | "alarm" => handler!(|uc, a| Ret::U32(0)),

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
            let left = shared().heap.malloc(size);
            let right = shared().heap.malloc(size);
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
            shared().heap.free(left);
            shared().heap.free(right);
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
            // A game library's own symbol first (each part has its own JNI_OnLoad).
            let own = handle
                .checked_sub(FIRST_LIBRARY_HANDLE)
                .and_then(|index| {
                    crate::elf::with_loader(|loader| loader.libraries.get(index as usize).and_then(|l| l.export(uc, &name)))
                });
            Ret::U32(own.unwrap_or_else(|| lookup_symbol(&name)))
        }),
        "eglGetProcAddress" => handler!(|uc, a| {
            let name = mem().string(a.u32());
            Ret::U32(lookup_symbol(&name))
        }),
        "getpagesize" => handler!(|uc, a| Ret::U32(crate::mem::PAGE)),
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
                    let buffer = if buffer == 0 { shared().heap.malloc(size.max(needed)) } else { buffer };
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
