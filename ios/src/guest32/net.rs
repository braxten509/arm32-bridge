//! BSD sockets and name lookups for 32-bit (armv7) iPhone games.
//!
//! Where Darwin's 32-bit structures are the host's, the work is done by the
//! 64-bit implementation in `libsystem::net`: a socket address starts with
//! the same length and family bytes at both widths, so bind, connect, accept,
//! getsockname/getpeername and send/receive pass their structures through it
//! unchanged. Converted here are the things that differ: `struct addrinfo`
//! and `struct hostent` have 4-byte pointers (addrinfo's canonname comes
//! before addr), `struct timeval` is two 32-bit fields, fcntl's O_* flags are
//! Darwin's numbers, and errno goes back as Darwin's number.
//!
//! The game is offline, like an iPhone in flight mode: net.rs answers only
//! for loopback and numbers, so nothing here can wait on a network.

use arm32_bridge::handler;
use arm32_bridge::ios32::darwin_errno;
use arm32_bridge::runtime::{self, mem, ArgSource, Ret, Uc};

use super::function;
use super::libsystem::{apple_status_flags, host_open_flags};
use crate::libsystem::net::{self, AddressEntry, Hints};

const APPLE_SOL_SOCKET: i32 = 0xFFFF;
const APPLE_SO_SNDTIMEO: i32 = 0x1005;
const APPLE_SO_RCVTIMEO: i32 = 0x1006;

/// A guest pointer as a host pointer (NULL stays NULL).
fn host_ptr(guest: u32) -> *mut u8 {
    mem().host_or_null(guest)
}

fn host_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `result` and the host's errno as Darwin's when it failed.
fn finish(uc: &Uc, result: i64, error: i32) -> Ret {
    if result < 0 {
        mem().write_u32(uc.get_data().errno_slot, darwin_errno(error) as u32);
    }
    Ret::U32(result as i32 as u32)
}

/// A system call that may wait: the guest lock is let go while it does, as
/// for read and sleep.
fn blocking(uc: &Uc, work: impl FnOnce() -> i64) -> Ret {
    let (result, error) = runtime::without_gil(|| {
        let result = work();
        (result, host_errno())
    });
    finish(uc, result, error)
}

extern "C" {
    fn inet_addr(text: *const libc::c_char) -> u32;
}

fn sockets() {
    function(
        "socket",
        handler!(|uc, a| {
            let (family, kind, protocol) = (a.i32(), a.i32(), a.i32());
            let result = net::ios_socket(family, kind, protocol);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "bind",
        handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let result = net::ios_bind(fd, host_ptr(address), length);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "connect",
        handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let address = host_ptr(address);
            blocking(uc, move || net::ios_connect(fd, address, length) as i64)
        })
        .unwrap(),
    );
    function(
        "accept",
        handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let (address, length) = (host_ptr(address), host_ptr(length).cast::<u32>());
            blocking(uc, move || net::ios_accept(fd, address, length) as i64)
        })
        .unwrap(),
    );
    function(
        "listen",
        handler!(|uc, a| {
            let (fd, backlog) = (a.i32(), a.i32());
            let result = net::ios_listen(fd, backlog);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "shutdown",
        handler!(|uc, a| {
            let (fd, how) = (a.i32(), a.i32());
            let result = unsafe { libc::shutdown(fd, how) };
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "getsockname",
        handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let (address, length) = (host_ptr(address), host_ptr(length).cast::<u32>());
            let result = net::ios_getsockname(fd, address, length);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "getpeername",
        handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let (address, length) = (host_ptr(address), host_ptr(length).cast::<u32>());
            let result = net::ios_getpeername(fd, address, length);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "send",
        handler!(|uc, a| {
            let (fd, data, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            let data = host_ptr(data).cast();
            blocking(uc, move || net::ios_send(fd, data, length as usize, flags) as i64)
        })
        .unwrap(),
    );
    function(
        "sendto",
        handler!(|uc, a| {
            let (fd, data, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            let (address, address_length) = (a.u32(), a.u32());
            let (data, address) = (host_ptr(data).cast(), host_ptr(address));
            blocking(uc, move || net::ios_sendto(fd, data, length as usize, flags, address, address_length) as i64)
        })
        .unwrap(),
    );
    function(
        "recv",
        handler!(|uc, a| {
            let (fd, data, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            let data = host_ptr(data).cast();
            blocking(uc, move || net::ios_recv(fd, data, length as usize, flags) as i64)
        })
        .unwrap(),
    );
    function(
        "recvfrom",
        handler!(|uc, a| {
            let (fd, data, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            let (address, address_length) = (a.u32(), a.u32());
            let (data, address, address_length) = (host_ptr(data).cast(), host_ptr(address), host_ptr(address_length).cast::<u32>());
            blocking(uc, move || net::ios_recvfrom(fd, data, length as usize, flags, address, address_length) as i64)
        })
        .unwrap(),
    );
}

/// Darwin's armv7 `struct timeval` (two 32-bit fields) as the 64-bit
/// implementation's (64-bit seconds).
pub(crate) fn timeval(value: u32) -> [u8; 16] {
    let mut time = [0u8; 16];
    time[..8].copy_from_slice(&(mem().read_u32(value) as i32 as i64).to_ne_bytes());
    time[8..12].copy_from_slice(&mem().read_u32(value + 4).to_ne_bytes());
    time
}

fn socket_options() {
    function(
        "setsockopt",
        handler!(|uc, a| {
            let (fd, level, name, value, length) = (a.i32(), a.i32(), a.i32(), a.u32(), a.u32());
            let guest = value;
            let value = host_ptr(value).cast::<u8>();
            if level == APPLE_SOL_SOCKET && matches!(name, APPLE_SO_SNDTIMEO | APPLE_SO_RCVTIMEO) {
                if value.is_null() || length < 8 {
                    return finish(uc, -1, libc::EINVAL);
                }
                let time = timeval(guest);
                let result = net::ios_setsockopt(fd, level, name, time.as_ptr(), time.len() as u32);
                return finish(uc, result as i64, host_errno());
            }
            let result = net::ios_setsockopt(fd, level, name, value, length);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
    function(
        "getsockopt",
        handler!(|uc, a| {
            let (fd, level, name, value, length) = (a.i32(), a.i32(), a.i32(), a.u32(), a.u32());
            let (value, length) = (host_ptr(value), host_ptr(length).cast::<u32>());
            if level == APPLE_SOL_SOCKET && matches!(name, APPLE_SO_SNDTIMEO | APPLE_SO_RCVTIMEO) {
                if length.is_null() || value.is_null() || unsafe { *length } < 8 {
                    return finish(uc, -1, libc::EINVAL);
                }
                let mut time = [0u8; 16];
                let mut size = time.len() as u32;
                let result = net::ios_getsockopt(fd, level, name, time.as_mut_ptr(), &mut size);
                if result < 0 {
                    return finish(uc, result as i64, host_errno());
                }
                unsafe {
                    (value as *mut u32).write_unaligned(i64::from_ne_bytes(time[..8].try_into().unwrap()) as i32 as u32);
                    (value.add(4) as *mut u32).write_unaligned(i32::from_ne_bytes(time[8..12].try_into().unwrap()) as u32);
                    *length = 8;
                }
                return Ret::U32(0);
            }
            let result = net::ios_getsockopt(fd, level, name, value, length);
            finish(uc, result as i64, host_errno())
        })
        .unwrap(),
    );
}

fn control() {
    // Darwin's fcntl commands and O_NONBLOCK (0x4); the other values are the
    // host's too, but its O_* flags are not.
    const F_GETFD: i32 = 1;
    const F_SETFD: i32 = 2;
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    function(
        "fcntl",
        handler!(|uc, a| {
            let (fd, command) = (a.i32(), a.i32());
            let argument = a.u32();
            match command {
                F_GETFD | F_SETFD | 0 => {
                    let result = unsafe { libc::fcntl(fd, command, argument as i32) };
                    finish(uc, result as i64, host_errno())
                }
                67 => {
                    // Darwin's F_DUPFD_CLOEXEC.
                    let result = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, argument as i32) };
                    finish(uc, result as i64, host_errno())
                }
                F_GETFL => {
                    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                    if flags < 0 {
                        finish(uc, -1, host_errno())
                    } else {
                        Ret::U32(apple_status_flags(flags))
                    }
                }
                F_SETFL => {
                    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, host_open_flags(argument)) };
                    finish(uc, result as i64, host_errno())
                }
                // F_RDAHEAD (45), F_NOCACHE (48), F_FULLFSYNC (51): hints only.
                45 | 48 | 51 => Ret::U32(0),
                _ => {
                    crate::log!("32-bit fcntl command {command} not translated, refusing");
                    mem().write_u32(uc.get_data().errno_slot, darwin_errno(libc::EINVAL) as u32);
                    Ret::U32(u32::MAX)
                }
            }
        })
        .unwrap(),
    );
    let select = || {
        handler!(|uc, a| {
            let (count, read, write, error) = (a.i32(), a.u32(), a.u32(), a.u32());
            let timeout = a.u32();
            let (read, write, error) = (
                host_ptr(read).cast::<libc::fd_set>(),
                host_ptr(write).cast::<libc::fd_set>(),
                host_ptr(error).cast::<libc::fd_set>(),
            );
            let time = (timeout != 0).then(|| timeval(timeout));
            blocking(uc, move || {
                let time = time.as_ref().map_or(std::ptr::null(), |time| time.as_ptr());
                net::ios_select(count, read, write, error, time) as i64
            })
        })
        .unwrap()
    };
    function("select", select());
    function("select$DARWIN_EXTSN", select());
}

/// Apple's addrinfo, 32-bit: flags, family, socktype, protocol, addrlen,
/// canonname, addr, next (canonname comes before addr on Darwin).
const ADDRINFO_SIZE: u32 = 32;

/// The 32-bit list for `entries`, from the guest heap (freeaddrinfo's to free).
fn guest_chain(entries: &[AddressEntry]) -> u32 {
    let mut head = 0;
    let mut tail = 0u32;
    for entry in entries {
        let name = entry.canonname.as_deref().unwrap_or(&[]);
        let node = super::alloc(ADDRINFO_SIZE + entry.address.len() as u32 + name.len() as u32);
        if node == 0 {
            // Out of memory: give back what was made, as Apple's list would
            // be freed by the caller on an error.
            let mut rest = head;
            while rest != 0 {
                let next = mem().read_u32(rest + 28);
                super::free(rest);
                rest = next;
            }
            return 0;
        }
        let address_at = node + ADDRINFO_SIZE;
        let name_at = address_at + entry.address.len() as u32;
        let m = mem();
        m.write_u32(node, entry.flags as u32);
        m.write_u32(node + 4, entry.family as u32);
        m.write_u32(node + 8, entry.socktype as u32);
        m.write_u32(node + 12, entry.protocol as u32);
        m.write_u32(node + 16, entry.address.len() as u32);
        m.write_u32(node + 20, if name.is_empty() { 0 } else { name_at });
        m.write_u32(node + 24, address_at);
        m.write_u32(node + 28, 0);
        m.write_bytes(address_at, &entry.address);
        if !name.is_empty() {
            m.write_bytes(name_at, name);
        }
        if tail == 0 {
            head = node;
        } else {
            m.write_u32(tail + 28, node);
        }
        tail = node;
    }
    head
}

fn guest_hints(guest: u32) -> Option<Hints> {
    if guest == 0 {
        return None;
    }
    Some(Hints {
        flags: mem().read_u32(guest) as i32,
        family: mem().read_u32(guest + 4) as i32,
        socktype: mem().read_u32(guest + 8) as i32,
        protocol: mem().read_u32(guest + 12) as i32,
    })
}

/// A per-thread area holding Darwin armv7's `struct hostent` (20 bytes, all
/// pointers 4 bytes) at the start, then its name (up to 400 bytes), the empty
/// alias list, the address list and the address. One per thread, as
/// gethostbyname's answer is.
const HOSTENT_AREA: u32 = 512;

fn guest_hostent(host: usize) -> u32 {
    if host == 0 {
        return 0;
    }
    // The host's hostent is a per-thread static too, so each thread keeps its
    // own guest copy to overwrite.
    thread_local! {
        static AREA: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let area = AREA.with(|slot| {
        if slot.get() == 0 {
            slot.set(super::alloc_permanent(HOSTENT_AREA, 4));
        }
        slot.get()
    });
    #[repr(C)]
    struct Hostent {
        name: *const libc::c_char,
        aliases: *const *const libc::c_char,
        addrtype: i32,
        length: i32,
        addresses: *const *const u8,
    }
    let entry = unsafe { &*(host as *const Hostent) };
    if entry.addresses.is_null() || entry.length < 4 {
        return 0;
    }
    let address = unsafe { *entry.addresses };
    if address.is_null() {
        return 0;
    }
    let name = if entry.name.is_null() { &b""[..] } else { unsafe { std::ffi::CStr::from_ptr(entry.name) }.to_bytes() };
    let name = &name[..name.len().min((HOSTENT_AREA - 40) as usize)];
    let m = mem();
    m.write_u32(area, area + 36); // h_name
    m.write_u32(area + 4, area + 20); // h_aliases: one NULL
    m.write_u32(area + 8, 2); // h_addrtype: AF_INET
    m.write_u32(area + 12, 4); // h_length
    m.write_u32(area + 16, area + 24); // h_addr_list: one address, then NULL
    m.write_u32(area + 20, 0);
    m.write_u32(area + 24, area + 32);
    m.write_u32(area + 28, 0);
    m.write_bytes(area + 32, unsafe { std::slice::from_raw_parts(address, 4) });
    m.write_bytes(area + 36, name);
    m.write_u8(area + 36 + name.len() as u32, 0);
    area
}

fn names() {
    function(
        "getaddrinfo",
        handler!(|uc, a| {
            let (node, service, hints, out) = (a.u32(), a.u32(), a.u32(), a.u32());
            let result = net::find_addresses(host_ptr(node).cast(), host_ptr(service).cast(), guest_hints(hints));
            match result {
                Ok(entries) => {
                    if out != 0 {
                        mem().write_u32(out, guest_chain(&entries));
                    }
                    Ret::U32(0)
                }
                Err(error) => Ret::U32(net::apple_lookup_error(error) as u32),
            }
        })
        .unwrap(),
    );
    function(
        "freeaddrinfo",
        handler!(|_uc, a| {
            let mut node = a.u32();
            while node != 0 {
                let next = mem().read_u32(node + 28);
                super::free(node);
                node = next;
            }
            Ret::Void
        })
        .unwrap(),
    );
    function(
        "gethostbyname",
        handler!(|_uc, a| Ret::U32(guest_hostent(net::ios_gethostbyname(host_ptr(a.u32()).cast())))).unwrap(),
    );
    function(
        "getnameinfo",
        handler!(|_uc, a| {
            let (address, length) = (a.u32(), a.u32());
            let (name, name_length, service, service_length, flags) =
                (a.u32(), a.u32(), a.u32(), a.u32(), a.i32());
            let result = net::ios_getnameinfo(
                host_ptr(address),
                length,
                host_ptr(name).cast(),
                name_length,
                host_ptr(service).cast(),
                service_length,
                flags,
            );
            // getnameinfo answers Apple's EAI_* numbers (no errno).
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    function("inet_addr", handler!(|_uc, a| Ret::U32(unsafe { inet_addr(host_ptr(a.u32()).cast()) })).unwrap());
    function(
        "inet_ntop",
        handler!(|uc, a| {
            let (family, source, out, size) = (a.i32(), a.u32(), a.u32(), a.u32());
            let result = net::ios_inet_ntop(family, host_ptr(source), host_ptr(out).cast(), size);
            if result == 0 {
                mem().write_u32(uc.get_data().errno_slot, darwin_errno(host_errno()) as u32);
                Ret::U32(0)
            } else {
                Ret::U32(out)
            }
        })
        .unwrap(),
    );
    function(
        "inet_pton",
        handler!(|uc, a| {
            let (family, text, out) = (a.i32(), a.u32(), a.u32());
            let result = net::ios_inet_pton(family, host_ptr(text).cast(), host_ptr(out));
            if result < 0 {
                finish(uc, result as i64, host_errno())
            } else {
                Ret::U32(result as u32)
            }
        })
        .unwrap(),
    );
}

pub fn register() {
    sockets();
    socket_options();
    control();
    names();
}
