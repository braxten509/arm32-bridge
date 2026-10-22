//! Apple sockets and name lookups. Apple's socket addresses start with a
//! length byte, its IPv6 family is 30, and its option, flag, poll and
//! lookup-error numbers differ from Linux/Android, so these translate both ways.
//!
//! The game is offline (as everywhere in this layer), like an iPhone in
//! flight mode: this device itself (loopback, local sockets) works, every
//! other address is unreachable at once, and names other than "localhost"
//! are not found, without asking the network. Nothing here can wait on a
//! network that isn't there.

use crate::libsystem::threads::blocking;
use crate::symbols::Registry;
use std::sync::Mutex;

extern "C" {
    fn gethostbyname(name: *const libc::c_char) -> usize;
    fn inet_addr(text: *const libc::c_char) -> u32;
    fn inet_aton(text: *const libc::c_char, out: *mut u32) -> i32;
    fn inet_ntoa(address: u32) -> usize;
    fn inet_ntop(family: i32, source: *const u8, out: *mut libc::c_char, size: u32) -> usize;
    fn if_nametoindex(name: *const libc::c_char) -> u32;
}

const APPLE_AF_INET6: i32 = 30;

fn set_errno(linux: i32) {
    unsafe { *crate::libsystem::host_errno() = linux };
}

fn host_family(apple: i32) -> i32 {
    if apple == APPLE_AF_INET6 { libc::AF_INET6 } else { apple }
}

fn apple_family(host: i32) -> i32 {
    if host == libc::AF_INET6 { APPLE_AF_INET6 } else { host }
}

// ----- addresses -----

struct HostAddress {
    storage: libc::sockaddr_storage,
    length: u32,
}

impl HostAddress {
    fn empty() -> Self {
        HostAddress { storage: unsafe { std::mem::zeroed() }, length: std::mem::size_of::<libc::sockaddr_storage>() as u32 }
    }
    fn pointer(&self) -> *const libc::sockaddr {
        &self.storage as *const _ as *const libc::sockaddr
    }
    fn pointer_mut(&mut self) -> *mut libc::sockaddr {
        &mut self.storage as *mut _ as *mut libc::sockaddr
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer() as *const u8, self.length as usize) }
    }
}

/// An Apple socket address as the system's (None: errno is set).
unsafe fn host_address(apple: *const u8, length: u32) -> Option<HostAddress> {
    let mut out = HostAddress::empty();
    let place = out.pointer_mut() as *mut u8;
    let length = length as usize;
    if apple.is_null() || length < 2 {
        set_errno(libc::EINVAL);
        return None;
    }
    let family = *apple.add(1) as i32;
    let (needed, host) = match family {
        libc::AF_INET => (16, libc::AF_INET),
        APPLE_AF_INET6 => (28, libc::AF_INET6),
        libc::AF_UNIX => {
            let path = std::slice::from_raw_parts(apple.add(2), length.min(106) - 2);
            let path = &path[..path.iter().position(|&b| b == 0).unwrap_or(path.len())];
            std::ptr::copy_nonoverlapping(path.as_ptr(), place.add(2), path.len().min(107));
            *(place as *mut u16) = libc::AF_UNIX as u16;
            out.length = 2 + path.len() as u32 + 1;
            return Some(out);
        }
        libc::AF_UNSPEC => (length.min(16), libc::AF_UNSPEC),
        _ => {
            set_errno(libc::EAFNOSUPPORT);
            return None;
        }
    };
    if length < needed {
        set_errno(libc::EINVAL);
        return None;
    }
    std::ptr::copy_nonoverlapping(apple, place, needed);
    *(place as *mut u16) = host as u16;
    out.length = needed as u32;
    Some(out)
}

/// Whether an address is on this device: loopback (127.x, ::1, or 127.x
/// mapped into IPv6), a local socket, or "no address" (AF_UNSPEC).
fn on_this_device(host: &HostAddress) -> bool {
    let bytes = host.bytes();
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]) as i32;
    match family {
        libc::AF_INET => bytes.len() >= 8 && bytes[4] == 127,
        libc::AF_INET6 if bytes.len() >= 24 => {
            let address = &bytes[8..24];
            let loopback = address[..15].iter().all(|&b| b == 0) && address[15] == 1;
            let mapped = address[..10].iter().all(|&b| b == 0) && address[10] == 0xFF && address[11] == 0xFF && address[12] == 127;
            loopback || mapped
        }
        libc::AF_UNIX | libc::AF_UNSPEC => true,
        _ => false,
    }
}

/// The offline answer for an address off this device (ENETUNREACH), logged once per kind.
fn unreachable(what: &'static str) {
    log_once(what, "to an address off this device: answered 'network unreachable' (offline)");
    set_errno(libc::ENETUNREACH);
}

fn log_once(what: &'static str, message: &str) {
    static SAID: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if !said.contains(&what) {
        said.push(what);
        crate::log!("{what} {message}");
    }
}

/// The system's socket address written the Apple way (cut to the room, the
/// full length reported, as Apple does).
unsafe fn write_apple_address(host: &HostAddress, out: *mut u8, room: *mut u32) {
    if out.is_null() || room.is_null() {
        return;
    }
    let bytes = host.bytes();
    let family = if bytes.len() >= 2 { u16::from_ne_bytes([bytes[0], bytes[1]]) as i32 } else { libc::AF_UNSPEC };
    let mut apple = match family {
        libc::AF_INET => bytes[..16.min(bytes.len())].to_vec(),
        libc::AF_INET6 => bytes[..28.min(bytes.len())].to_vec(),
        libc::AF_UNIX => {
            let path = &bytes[2..];
            let path = &path[..path.iter().position(|&b| b == 0).unwrap_or(path.len()).min(103)];
            let mut apple = vec![0u8, 0];
            apple.extend_from_slice(path);
            apple.push(0);
            apple
        }
        _ => bytes.to_vec(),
    };
    if apple.len() >= 2 {
        apple[0] = apple.len() as u8;
        apple[1] = apple_family(family) as u8;
    }
    let count = apple.len().min(*room as usize);
    std::ptr::copy_nonoverlapping(apple.as_ptr(), out, count);
    *room = apple.len() as u32;
}

// ----- sockets -----

extern "C" fn ios_socket(family: i32, kind: i32, protocol: i32) -> i32 {
    unsafe { libc::socket(host_family(family), kind, protocol) }
}

extern "C" fn ios_bind(fd: i32, address: *const u8, length: u32) -> i32 {
    match unsafe { host_address(address, length) } {
        Some(host) => unsafe { libc::bind(fd, host.pointer(), host.length) },
        None => -1,
    }
}

extern "C" fn ios_connect(fd: i32, address: *const u8, length: u32) -> i32 {
    let Some(host) = (unsafe { host_address(address, length) }) else { return -1 };
    if !on_this_device(&host) {
        unreachable("connect");
        return -1;
    }
    let result = blocking(|| unsafe { libc::connect(fd, host.pointer(), host.length) });
    result
}

extern "C" fn ios_accept(fd: i32, address: *mut u8, length: *mut u32) -> i32 {
    let mut host = HostAddress::empty();
    let result = blocking(|| unsafe { libc::accept(fd, host.pointer_mut(), &mut host.length) });
    if result >= 0 {
        unsafe { write_apple_address(&host, address, length) };
    }
    result
}

extern "C" fn ios_getsockname(fd: i32, address: *mut u8, length: *mut u32) -> i32 {
    let mut host = HostAddress::empty();
    let result = unsafe { libc::getsockname(fd, host.pointer_mut(), &mut host.length) };
    if result == 0 {
        unsafe { write_apple_address(&host, address, length) };
    }
    result
}

const APPLE_MSG: [(i32, i32); 9] = [
    (0x1, libc::MSG_OOB),
    (0x2, libc::MSG_PEEK),
    (0x4, libc::MSG_DONTROUTE),
    (0x8, libc::MSG_EOR),
    (0x10, libc::MSG_TRUNC),
    (0x20, libc::MSG_CTRUNC),
    (0x40, libc::MSG_WAITALL),
    (0x80, libc::MSG_DONTWAIT),
    (0x80000, libc::MSG_NOSIGNAL),
];

/// Apple message flags as the system's. A broken connection always answers
/// EPIPE instead of a signal: the game's own "ignore SIGPIPE" is not installed here.
fn host_message_flags(apple: i32) -> i32 {
    APPLE_MSG.iter().filter(|(a, _)| apple & a != 0).fold(libc::MSG_NOSIGNAL, |flags, (_, host)| flags | host)
}

extern "C" fn ios_send(fd: i32, data: *const libc::c_void, length: usize, flags: i32) -> isize {
    let result = blocking(|| unsafe { libc::send(fd, data, length, host_message_flags(flags)) });
    result
}

extern "C" fn ios_sendto(fd: i32, data: *const libc::c_void, length: usize, flags: i32, address: *const u8, address_length: u32) -> isize {
    let host = if address.is_null() {
        None
    } else {
        match unsafe { host_address(address, address_length) } {
            Some(host) if on_this_device(&host) => Some(host),
            Some(_) => {
                unreachable("sendto");
                return -1;
            }
            None => return -1,
        }
    };
    let (pointer, size) = host.as_ref().map_or((std::ptr::null(), 0), |h| (h.pointer(), h.length));
    let result = blocking(|| unsafe { libc::sendto(fd, data, length, host_message_flags(flags), pointer, size) });
    result
}

extern "C" fn ios_recv(fd: i32, data: *mut libc::c_void, length: usize, flags: i32) -> isize {
    let result = blocking(|| unsafe { libc::recv(fd, data, length, host_message_flags(flags)) });
    result
}

extern "C" fn ios_recvfrom(fd: i32, data: *mut libc::c_void, length: usize, flags: i32, address: *mut u8, address_length: *mut u32) -> isize {
    let mut host = HostAddress::empty();
    let result = blocking(|| unsafe { libc::recvfrom(fd, data, length, host_message_flags(flags), host.pointer_mut(), &mut host.length) });
    if result >= 0 {
        unsafe { write_apple_address(&host, address, address_length) };
    }
    result
}

// ----- socket options -----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Value {
    /// The same bytes on both systems.
    Same,
    /// A time span (Apple's microseconds field is 32 bits).
    Time,
    /// An error number (SO_ERROR).
    Error,
    /// Apple's "no SIGPIPE" switch: always on here.
    NoSigpipe,
    /// Bytes waiting to be read (SO_NREAD).
    Waiting,
}

const APPLE_SOL_SOCKET: i32 = 0xFFFF;

/// Apple (level, option) -> the system's (level, option) and how the value reads.
fn host_option(level: i32, name: i32) -> Option<(i32, i32, Value)> {
    use Value::*;
    let (host_name, value) = match level {
        APPLE_SOL_SOCKET => {
            let found = match name {
                0x1 => (libc::SO_DEBUG, Same),
                0x2 => (libc::SO_ACCEPTCONN, Same),
                0x4 => (libc::SO_REUSEADDR, Same),
                0x8 => (libc::SO_KEEPALIVE, Same),
                0x10 => (libc::SO_DONTROUTE, Same),
                0x20 => (libc::SO_BROADCAST, Same),
                0x80 | 0x1080 => (libc::SO_LINGER, Same),
                0x100 => (libc::SO_OOBINLINE, Same),
                0x200 => (libc::SO_REUSEPORT, Same),
                0x1001 => (libc::SO_SNDBUF, Same),
                0x1002 => (libc::SO_RCVBUF, Same),
                0x1003 => (libc::SO_SNDLOWAT, Same),
                0x1004 => (libc::SO_RCVLOWAT, Same),
                0x1005 => (libc::SO_SNDTIMEO, Time),
                0x1006 => (libc::SO_RCVTIMEO, Time),
                0x1007 => (libc::SO_ERROR, Error),
                0x1008 => (libc::SO_TYPE, Same),
                0x1020 => (0, Waiting),
                0x1022 => (0, NoSigpipe),
                _ => return None,
            };
            return Some((libc::SOL_SOCKET, found.0, found.1));
        }
        libc::IPPROTO_TCP => match name {
            0x1 => (libc::TCP_NODELAY, Same),
            0x2 => (libc::TCP_MAXSEG, Same),
            0x10 => (libc::TCP_KEEPIDLE, Same),
            0x101 => (libc::TCP_KEEPINTVL, Same),
            0x102 => (libc::TCP_KEEPCNT, Same),
            _ => return None,
        },
        libc::IPPROTO_IP => match name {
            2 => (libc::IP_HDRINCL, Same),
            3 => (libc::IP_TOS, Same),
            4 => (libc::IP_TTL, Same),
            9 => (libc::IP_MULTICAST_IF, Same),
            10 => (libc::IP_MULTICAST_TTL, Same),
            11 => (libc::IP_MULTICAST_LOOP, Same),
            12 => (libc::IP_ADD_MEMBERSHIP, Same),
            13 => (libc::IP_DROP_MEMBERSHIP, Same),
            _ => return None,
        },
        libc::IPPROTO_IPV6 => match name {
            4 => (libc::IPV6_UNICAST_HOPS, Same),
            9 => (libc::IPV6_MULTICAST_IF, Same),
            10 => (libc::IPV6_MULTICAST_HOPS, Same),
            11 => (libc::IPV6_MULTICAST_LOOP, Same),
            12 => (libc::IPV6_ADD_MEMBERSHIP, Same),
            13 => (libc::IPV6_DROP_MEMBERSHIP, Same),
            27 => (libc::IPV6_V6ONLY, Same),
            _ => return None,
        },
        _ => return None,
    };
    Some((level, host_name, value))
}

extern "C" fn ios_setsockopt(fd: i32, level: i32, name: i32, value: *const u8, length: u32) -> i32 {
    let Some((host_level, host_name, kind)) = host_option(level, name) else {
        crate::log!("setsockopt level {level:#x} option {name:#x}: not translated, refusing");
        set_errno(libc::ENOPROTOOPT);
        return -1;
    };
    unsafe {
        match kind {
            Value::NoSigpipe | Value::Waiting => 0,
            Value::Time => {
                if value.is_null() || length < 12 {
                    set_errno(libc::EINVAL);
                    return -1;
                }
                let time = libc::timeval { tv_sec: (value as *const i64).read_unaligned(), tv_usec: (value.add(8) as *const i32).read_unaligned() as _ };
                libc::setsockopt(fd, host_level, host_name, &time as *const _ as *const _, std::mem::size_of::<libc::timeval>() as u32)
            }
            _ => libc::setsockopt(fd, host_level, host_name, value as *const _, length),
        }
    }
}

extern "C" fn ios_getsockopt(fd: i32, level: i32, name: i32, value: *mut u8, length: *mut u32) -> i32 {
    let Some((host_level, host_name, kind)) = host_option(level, name) else {
        crate::log!("getsockopt level {level:#x} option {name:#x}: not translated, refusing");
        set_errno(libc::ENOPROTOOPT);
        return -1;
    };
    unsafe {
        let write_int = |number: i32| {
            if value.is_null() || length.is_null() || *length < 4 {
                set_errno(libc::EINVAL);
                return -1;
            }
            (value as *mut i32).write_unaligned(number);
            *length = 4;
            0
        };
        match kind {
            Value::NoSigpipe => write_int(1),
            Value::Waiting => {
                let mut waiting = 0i32;
                if libc::ioctl(fd, libc::FIONREAD as _, &mut waiting) != 0 {
                    return -1;
                }
                write_int(waiting)
            }
            Value::Error => {
                let mut error = 0i32;
                let mut size = 4u32;
                if libc::getsockopt(fd, host_level, host_name, &mut error as *mut _ as *mut _, &mut size) != 0 {
                    return -1;
                }
                write_int(crate::libsystem::io::apple_errno(error))
            }
            Value::Time => {
                let mut time: libc::timeval = std::mem::zeroed();
                let mut size = std::mem::size_of::<libc::timeval>() as u32;
                if libc::getsockopt(fd, host_level, host_name, &mut time as *mut _ as *mut _, &mut size) != 0 {
                    return -1;
                }
                if value.is_null() || length.is_null() || *length < 16 {
                    set_errno(libc::EINVAL);
                    return -1;
                }
                std::ptr::write_bytes(value, 0, 16);
                (value as *mut i64).write_unaligned(time.tv_sec as i64);
                (value.add(8) as *mut i32).write_unaligned(time.tv_usec as i32);
                *length = 16;
                0
            }
            Value::Same => libc::getsockopt(fd, host_level, host_name, value as *mut _, length),
        }
    }
}

// ----- control, waiting -----

crate::libsystem::libc::variadic!(ios_ioctl, 2, ios_ioctl_impl);

#[no_mangle]
unsafe extern "C" fn ios_ioctl_impl(fd: i32, request: u64, va: *const u64) -> i32 {
    let argument = *va as usize;
    let host = match request {
        0x8004_667E => libc::FIONBIO as u64,
        0x4004_667F => libc::FIONREAD as u64,
        0x8004_667D => 0x5452, // FIOASYNC (the libc crate lacks it on Android)
        0x2000_6601 => libc::FIOCLEX as u64,
        0x2000_6602 => libc::FIONCLEX as u64,
        _ => {
            crate::log!("ioctl request {request:#x}: not translated, refusing");
            set_errno(libc::ENOTTY);
            return -1;
        }
    };
    libc::ioctl(fd, host as _, argument)
}

/// Apple's timeval (32-bit microseconds) as the system's.
unsafe fn host_time(apple: *const u8) -> Option<libc::timeval> {
    (!apple.is_null()).then(|| libc::timeval {
        tv_sec: (apple as *const i64).read_unaligned(),
        tv_usec: (apple.add(8) as *const i32).read_unaligned() as _,
    })
}

/// Apple's descriptor sets have the same bit layout as the system's.
extern "C" fn ios_select(count: i32, read: *mut libc::fd_set, write: *mut libc::fd_set, error: *mut libc::fd_set, timeout: *const u8) -> i32 {
    let mut time = unsafe { host_time(timeout) };
    let time_pointer = time.as_mut().map_or(std::ptr::null_mut(), |t| t as *mut _);
    let result = blocking(|| unsafe { libc::select(count, read, write, error, time_pointer) });
    result
}

/// Poll event bits that differ: Apple POLLWRNORM is POLLOUT (4), POLLWRBAND 0x100.
const APPLE_POLL: [(i16, i16); 9] = [
    (0x1, libc::POLLIN),
    (0x2, libc::POLLPRI),
    (0x4, libc::POLLOUT),
    (0x8, libc::POLLERR),
    (0x10, libc::POLLHUP),
    (0x20, libc::POLLNVAL),
    (0x40, libc::POLLRDNORM),
    (0x80, libc::POLLRDBAND),
    (0x100, libc::POLLWRBAND),
];

fn host_poll_events(apple: i16) -> i16 {
    APPLE_POLL.iter().filter(|(a, _)| apple & a != 0).fold(0, |events, (_, host)| events | host)
}

fn apple_poll_events(host: i16) -> i16 {
    let mut apple = APPLE_POLL.iter().filter(|(_, h)| host & h != 0).fold(0, |events, (a, _)| events | a);
    if host & libc::POLLWRNORM != 0 {
        apple |= 0x4;
    }
    apple
}

extern "C" fn ios_poll(fds: *mut libc::pollfd, count: u32, timeout: i32) -> i32 {
    let entries = if fds.is_null() { &mut [][..] } else { unsafe { std::slice::from_raw_parts_mut(fds, count as usize) } };
    let mut host: Vec<libc::pollfd> = entries.iter().map(|e| libc::pollfd { fd: e.fd, events: host_poll_events(e.events), revents: 0 }).collect();
    let result = blocking(|| unsafe { libc::poll(host.as_mut_ptr(), host.len() as _, timeout) });
    for (entry, done) in entries.iter_mut().zip(&host) {
        entry.revents = apple_poll_events(done.revents);
    }
    result
}

// ----- name lookups -----

/// Apple's addrinfo: flags, family, socktype, protocol, addrlen, canonname, addr, next.
#[repr(C)]
struct AppleAddrinfo {
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
    addrlen: u32,
    canonname: *mut libc::c_char,
    addr: *mut u8,
    next: *mut AppleAddrinfo,
}

const APPLE_AI: [(i32, i32); 7] = [
    (0x1, libc::AI_PASSIVE),
    (0x2, libc::AI_CANONNAME),
    (0x4, libc::AI_NUMERICHOST),
    (0x100, libc::AI_ALL),
    (0x400, libc::AI_ADDRCONFIG),
    (0x800, libc::AI_V4MAPPED),
    (0x1000, libc::AI_NUMERICSERV),
];

/// The system's lookup error as Apple's (Android already uses Apple's numbers).
fn apple_lookup_error(host: i32) -> i32 {
    match host {
        0 => 0,
        libc::EAI_AGAIN => 2,
        libc::EAI_BADFLAGS => 3,
        libc::EAI_FAIL => 4,
        libc::EAI_FAMILY => 5,
        libc::EAI_MEMORY => 6,
        libc::EAI_NONAME => 8,
        libc::EAI_SERVICE => 9,
        libc::EAI_SOCKTYPE => 10,
        libc::EAI_SYSTEM => 11,
        libc::EAI_OVERFLOW => 14,
        other if other > 0 => other,
        -5 => 7,
        -9 => 1,
        _ => 4,
    }
}

/// "localhost" (any case, with or without the final dot) is on this device.
fn is_localhost(name: &[u8]) -> bool {
    let name = name.strip_suffix(b".").unwrap_or(name);
    name.eq_ignore_ascii_case(b"localhost")
}

fn hints_flags(hints: *const AppleAddrinfo) -> i32 {
    unsafe { hints.as_ref() }.map_or(0, |h| h.flags)
}

/// gethostbyname: numbers and "localhost" only (offline: other names aren't found).
extern "C" fn ios_gethostbyname(name: *const libc::c_char) -> usize {
    if name.is_null() {
        return 0;
    }
    let mut number = 0u32;
    let text = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes();
    if !is_localhost(text) && unsafe { inet_aton(name, &mut number) } == 0 {
        log_once("gethostbyname", "of a name: answered 'not found' (offline)");
        return 0;
    }
    unsafe { gethostbyname(name) }
}

extern "C" fn ios_getaddrinfo(node: *const libc::c_char, service: *const libc::c_char, hints: *const AppleAddrinfo, out: *mut *mut AppleAddrinfo) -> i32 {
    let mut host_hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    if let Some(hints) = unsafe { hints.as_ref() } {
        host_hints.ai_flags = APPLE_AI.iter().filter(|(a, _)| hints.flags & a != 0).fold(0, |f, (_, h)| f | h);
        host_hints.ai_family = host_family(hints.family);
        host_hints.ai_socktype = hints.socktype;
        host_hints.ai_protocol = hints.protocol;
    } else {
        host_hints.ai_flags = libc::AI_V4MAPPED | libc::AI_ADDRCONFIG;
    }
    let apple_flags = hints_flags(hints);
    // Offline: only numbers are read, so the system never asks the network.
    host_hints.ai_flags |= libc::AI_NUMERICHOST;
    let localhost = !node.is_null() && is_localhost(unsafe { std::ffi::CStr::from_ptr(node) }.to_bytes());
    if !localhost {
        let result = host_lookup(node, service, &host_hints, apple_flags);
        if result == Err(libc::EAI_NONAME) && apple_flags & 0x4 == 0 {
            log_once("getaddrinfo", "of a name: answered 'not known' (offline)");
        }
        return finish_lookup(result, out);
    }
    // "localhost" is this device's loopback addresses (IPv6 first, as on Apple),
    // whether or not any network is set up.
    host_hints.ai_flags &= !libc::AI_ADDRCONFIG;
    let numbers: &[&std::ffi::CStr] = match host_hints.ai_family {
        libc::AF_INET => &[c"127.0.0.1"],
        libc::AF_INET6 => &[c"::1"],
        _ => &[c"::1", c"127.0.0.1"],
    };
    let mut head: *mut AppleAddrinfo = std::ptr::null_mut();
    let mut first_error = None;
    for number in numbers.iter().rev() {
        match host_lookup(number.as_ptr(), service, &host_hints, apple_flags) {
            Ok(list) => unsafe {
                let mut last = list;
                while !(*last).next.is_null() {
                    last = (*last).next;
                }
                (*last).next = head;
                head = list;
            },
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }
    match (head.is_null(), first_error) {
        (true, Some(error)) => finish_lookup(Err(error), out),
        _ => finish_lookup(Ok(head), out),
    }
}

fn finish_lookup(result: Result<*mut AppleAddrinfo, i32>, out: *mut *mut AppleAddrinfo) -> i32 {
    match result {
        Ok(list) => {
            unsafe { *out = list };
            0
        }
        Err(error) => apple_lookup_error(error),
    }
}

/// The system's getaddrinfo, its answer copied into Apple's layout.
fn host_lookup(node: *const libc::c_char, service: *const libc::c_char, hints: &libc::addrinfo, apple_flags: i32) -> Result<*mut AppleAddrinfo, i32> {
    let mut found: *mut libc::addrinfo = std::ptr::null_mut();
    let error = unsafe { libc::getaddrinfo(node, service, hints, &mut found) };
    if error != 0 {
        return Err(error);
    }
    let mut head: *mut AppleAddrinfo = std::ptr::null_mut();
    let mut tail: *mut *mut AppleAddrinfo = &mut head;
    let mut at = found;
    while let Some(entry) = unsafe { at.as_ref() } {
        unsafe {
            let node = libc::calloc(1, std::mem::size_of::<AppleAddrinfo>() + 128) as *mut AppleAddrinfo;
            let place = (node as *mut u8).add(std::mem::size_of::<AppleAddrinfo>());
            let mut host = HostAddress::empty();
            let size = (entry.ai_addrlen as usize).min(std::mem::size_of::<libc::sockaddr_storage>());
            if !entry.ai_addr.is_null() {
                std::ptr::copy_nonoverlapping(entry.ai_addr as *const u8, host.pointer_mut() as *mut u8, size);
            }
            host.length = size as u32;
            let mut room = 128u32;
            write_apple_address(&host, place, &mut room);
            (*node).flags = apple_flags;
            (*node).family = apple_family(entry.ai_family);
            (*node).socktype = entry.ai_socktype;
            (*node).protocol = entry.ai_protocol;
            (*node).addrlen = room;
            (*node).addr = place;
            (*node).canonname = if entry.ai_canonname.is_null() { std::ptr::null_mut() } else { libc::strdup(entry.ai_canonname) };
            *tail = node;
            tail = &mut (*node).next;
        }
        at = entry.ai_next;
    }
    unsafe { libc::freeaddrinfo(found) };
    Ok(head)
}

extern "C" fn ios_freeaddrinfo(mut list: *mut AppleAddrinfo) {
    while !list.is_null() {
        unsafe {
            let next = (*list).next;
            libc::free((*list).canonname as *mut _);
            libc::free(list as *mut _);
            list = next;
        }
    }
}

extern "C" fn ios_gai_strerror(code: i32) -> usize {
    let text: &'static [u8] = match code {
        0 => b"Success\0",
        1 => b"Address family for hostname not supported\0",
        2 => b"Temporary failure in name resolution\0",
        3 => b"Invalid value for ai_flags\0",
        4 => b"Non-recoverable failure in name resolution\0",
        5 => b"ai_family not supported\0",
        6 => b"Memory allocation failure\0",
        7 => b"No address associated with hostname\0",
        8 => b"nodename nor servname provided, or not known\0",
        9 => b"servname not supported for ai_socktype\0",
        10 => b"ai_socktype not supported\0",
        11 => b"System error\0",
        12 => b"Invalid value for hints\0",
        13 => b"Resolved protocol is unknown\0",
        14 => b"Argument buffer overflow\0",
        _ => b"Unknown error\0",
    };
    text.as_ptr() as usize
}

const APPLE_NI: [(i32, i32); 5] = [
    (0x1, libc::NI_NOFQDN),
    (0x2, libc::NI_NUMERICHOST),
    (0x4, libc::NI_NAMEREQD),
    (0x8, libc::NI_NUMERICSERV),
    (0x10, libc::NI_DGRAM),
];

extern "C" fn ios_getnameinfo(address: *const u8, length: u32, host: *mut libc::c_char, host_length: u32, service: *mut libc::c_char, service_length: u32, flags: i32) -> i32 {
    let Some(converted) = (unsafe { host_address(address, length) }) else { return 5 };
    let mut host_flags = APPLE_NI.iter().filter(|(a, _)| flags & a != 0).fold(0, |f, (_, h)| f | h);
    if !on_this_device(&converted) {
        // Offline: no name can be looked up for another machine's address.
        if host_flags & libc::NI_NAMEREQD != 0 && !host.is_null() && host_length > 0 {
            log_once("getnameinfo", "for an address off this device: answered 'not known' (offline)");
            return 8;
        }
        host_flags |= libc::NI_NUMERICHOST;
    }
    let error = unsafe { libc::getnameinfo(converted.pointer(), converted.length, host, host_length as _, service, service_length as _, host_flags) };
    apple_lookup_error(error)
}

extern "C" fn ios_inet_ntop(family: i32, source: *const u8, out: *mut libc::c_char, size: u32) -> usize {
    unsafe { inet_ntop(host_family(family), source, out, size) }
}

pub fn register(r: &mut Registry) {
    r.function("socket", ios_socket as usize);
    r.function("bind", ios_bind as usize);
    r.function("connect", ios_connect as usize);
    r.function("accept", ios_accept as usize);
    r.function("listen", libc::listen as usize);
    r.function("shutdown", libc::shutdown as usize);
    r.function("getsockname", ios_getsockname as usize);
    r.function("send", ios_send as usize);
    r.function("sendto", ios_sendto as usize);
    r.function("recv", ios_recv as usize);
    r.function("recvfrom", ios_recvfrom as usize);
    r.function("setsockopt", ios_setsockopt as usize);
    r.function("getsockopt", ios_getsockopt as usize);
    r.function("ioctl", ios_ioctl as usize);
    r.function("pipe", libc::pipe as usize);
    r.function("select", ios_select as usize);
    r.function("poll", ios_poll as usize);
    r.function("getaddrinfo", ios_getaddrinfo as usize);
    r.function("freeaddrinfo", ios_freeaddrinfo as usize);
    r.function("gai_strerror", ios_gai_strerror as usize);
    r.function("getnameinfo", ios_getnameinfo as usize);
    r.function("gethostbyname", ios_gethostbyname as usize);
    r.function("inet_addr", inet_addr as usize);
    r.function("inet_aton", inet_aton as usize);
    r.function("inet_ntoa", inet_ntoa as usize);
    r.function("inet_ntop", ios_inet_ntop as usize);
    r.function("if_nametoindex", if_nametoindex as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Apple IPv6 loopback address goes out and comes back the Apple way,
    /// and a real socket bound with it reports it in Apple's form.
    #[test]
    fn apple_addresses_round_trip_through_a_socket() {
        let mut apple = [0u8; 28];
        apple[0] = 28;
        apple[1] = APPLE_AF_INET6 as u8;
        apple[23] = 1; // ::1
        let fd = ios_socket(APPLE_AF_INET6, libc::SOCK_DGRAM, 0);
        assert!(fd >= 0, "no IPv6 socket");
        assert_eq!(ios_bind(fd, apple.as_ptr(), 28), 0, "bind to [::1]:0 failed");
        let mut back = [0u8; 64];
        let mut length = 64u32;
        assert_eq!(ios_getsockname(fd, back.as_mut_ptr(), &mut length), 0);
        assert_eq!(length, 28);
        assert_eq!(back[0], 28, "Apple length byte");
        assert_eq!(back[1], APPLE_AF_INET6 as u8, "Apple family number");
        assert_ne!(u16::from_be_bytes([back[2], back[3]]), 0, "no port given");
        assert_eq!(&back[8..24], &apple[8..24], "address changed");
        let reuse = 1i32;
        assert_eq!(ios_setsockopt(fd, APPLE_SOL_SOCKET, 0x4, &reuse as *const i32 as *const u8, 4), 0);
        let (mut value, mut size) = (0i32, 4u32);
        assert_eq!(ios_getsockopt(fd, APPLE_SOL_SOCKET, 0x1008, &mut value as *mut i32 as *mut u8, &mut size), 0);
        assert_eq!(value, libc::SOCK_DGRAM, "SO_TYPE read through Apple's number");
        unsafe { libc::close(fd) };
    }

    /// Offline like flight mode: another machine is unreachable and a name
    /// isn't found, both at once; this device still answers.
    #[test]
    fn the_network_is_offline_without_waiting() {
        let started = std::time::Instant::now();
        let fd = ios_socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        let mut far = [0u8; 16];
        far[..8].copy_from_slice(&[16, 2, 0, 80, 192, 0, 2, 1]); // 192.0.2.1:80 (a documentation address)
        assert_eq!(ios_connect(fd, far.as_ptr(), 16), -1, "connected to another machine");
        assert_eq!(unsafe { *crate::libsystem::io::error_location() }, 51, "not Apple's ENETUNREACH");
        unsafe { libc::close(fd) };
        let mut list = std::ptr::null_mut();
        assert_eq!(ios_getaddrinfo(c"example.com".as_ptr(), c"80".as_ptr(), std::ptr::null(), &mut list), 8);
        assert_eq!(ios_gethostbyname(c"example.com".as_ptr()), 0);
        assert!(started.elapsed() < std::time::Duration::from_millis(100), "offline answers took {:?}", started.elapsed());
        assert_eq!(ios_getaddrinfo(c"localhost".as_ptr(), c"80".as_ptr(), std::ptr::null(), &mut list), 0, "localhost not found");
        ios_freeaddrinfo(list);
        // A loopback listener takes a loopback connection.
        let server = ios_socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        let mut near = [0u8; 16];
        near[..8].copy_from_slice(&[16, 2, 0, 0, 127, 0, 0, 1]);
        assert_eq!(ios_bind(server, near.as_ptr(), 16), 0);
        assert_eq!(unsafe { libc::listen(server, 1) }, 0);
        let mut length = 16u32;
        assert_eq!(ios_getsockname(server, near.as_mut_ptr(), &mut length), 0);
        let client = ios_socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        assert_eq!(ios_connect(client, near.as_ptr(), 16), 0, "loopback refused");
        unsafe {
            libc::close(client);
            libc::close(server);
        }
    }

    #[test]
    fn numeric_lookups_come_back_in_apple_layout() {
        let hints = AppleAddrinfo {
            flags: 0x4,
            family: libc::AF_INET,
            socktype: libc::SOCK_STREAM,
            protocol: 0,
            addrlen: 0,
            canonname: std::ptr::null_mut(),
            addr: std::ptr::null_mut(),
            next: std::ptr::null_mut(),
        };
        let mut list = std::ptr::null_mut();
        assert_eq!(ios_getaddrinfo(c"127.0.0.1".as_ptr(), c"80".as_ptr(), &hints, &mut list), 0);
        let entry = unsafe { &*list };
        assert_eq!(entry.family, libc::AF_INET);
        assert_eq!(entry.addrlen, 16);
        let address = unsafe { std::slice::from_raw_parts(entry.addr, 16) };
        assert_eq!(&address[..8], &[16, 2, 0, 80, 127, 0, 0, 1]);
        ios_freeaddrinfo(list);
        assert_eq!(ios_getaddrinfo(c"not an address".as_ptr(), std::ptr::null(), &hints, &mut list), 8, "EAI_NONAME in Apple's number");
    }
}
