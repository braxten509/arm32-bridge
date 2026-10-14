use crate::handler;
use crate::runtime::{mem, shared, without_gil, ArgSource, Handler, Ret, Uc};

const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;
const SO_SNDTIMEO: i32 = 21;
const ADDRINFO_SIZE: u32 = 32;

const TIOCGWINSZ: u32 = 0x5413;
const FIONREAD: u32 = 0x541B;
const FIONBIO: u32 = 0x5421;
const SIOCGIFNAME: u32 = 0x8910;
const SIOCGIFCONF: u32 = 0x8912;
const SIOCSIFHWBROADCAST: u32 = 0x8937;
/// struct ifreq: a 16-byte name, then a union holding a sockaddr or a pointer (8 bytes on the phone).
const IFREQ_SIZE: usize = 32;
const HOST_IFREQ_SIZE: usize = 40;

/// The phone's struct ifconf.
#[repr(C)]
struct HostIfconf {
    length: i32,
    buffer: *mut u8,
}

/// SIOCGIFCONF: the network interfaces and their addresses, as an array of
/// 32-byte guest ifreqs (the phone's are 40 bytes).
fn interface_list(uc: &Uc, fd: i32, argument: u32) -> Ret {
    let guest_length = mem().read_u32(argument) as i32;
    let guest_buffer = mem().read_u32(argument + 4);
    let mut host = vec![0u8; guest_length.max(0) as usize / IFREQ_SIZE * HOST_IFREQ_SIZE];
    // With no buffer the call only reports the length needed.
    let buffer = if guest_buffer == 0 { std::ptr::null_mut() } else { host.as_mut_ptr() };
    let mut conf = HostIfconf { length: host.len() as i32, buffer };
    let result = unsafe { libc::ioctl(fd, SIOCGIFCONF as _, &mut conf) };
    if result < 0 {
        return finish(uc, -1, std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    let count = conf.length.max(0) as usize / HOST_IFREQ_SIZE;
    if guest_buffer != 0 {
        for i in 0..count {
            let entry = &host[i * HOST_IFREQ_SIZE..i * HOST_IFREQ_SIZE + IFREQ_SIZE];
            mem().write_bytes(guest_buffer + (i * IFREQ_SIZE) as u32, entry);
        }
    }
    mem().write_u32(argument, (count * IFREQ_SIZE) as u32);
    Ret::U32(0)
}

// getservbyport's result area: struct servent (16 bytes), the empty alias list, the name, the protocol.
const SERVENT_ALIASES: u32 = 16;
const SERVENT_NAME: u32 = 20;
const SERVENT_TEXT_SIZE: u32 = 64;
const SERVENT_PROTOCOL: u32 = SERVENT_NAME + SERVENT_TEXT_SIZE;

/// Fills getservbyport's shared result area; returns its struct servent.
fn write_servent(name: &[u8], port: i32, protocol: &[u8]) -> u32 {
    if shared().state.servent == 0 {
        let area = shared().misc_alloc(SERVENT_PROTOCOL + SERVENT_TEXT_SIZE, 4);
        shared().state.servent = area;
    }
    let base = shared().state.servent;
    let m = mem();
    for (at, text) in [(SERVENT_NAME, name), (SERVENT_PROTOCOL, protocol)] {
        let text = &text[..text.len().min(SERVENT_TEXT_SIZE as usize - 1)];
        m.write_bytes(base + at, text);
        m.write_u8(base + at + text.len() as u32, 0);
    }
    m.write_u32(base + SERVENT_ALIASES, 0);
    m.write_u32(base, base + SERVENT_NAME); // s_name
    m.write_u32(base + 4, base + SERVENT_ALIASES); // s_aliases
    m.write_u32(base + 8, port as u32); // s_port
    m.write_u32(base + 12, base + SERVENT_PROTOCOL); // s_proto
    base
}

const HOSTENT_ADDRESSES: usize = 16;
// The result area: struct hostent (20 bytes), the empty alias list, the
// address list, the addresses, then the name.
const HOSTENT_ALIASES: u32 = 20;
const HOSTENT_ADDRESS_LIST: u32 = 24;
const HOSTENT_ADDRESS_BYTES: u32 = HOSTENT_ADDRESS_LIST + (HOSTENT_ADDRESSES as u32 + 1) * 4;
const HOSTENT_NAME: u32 = HOSTENT_ADDRESS_BYTES + HOSTENT_ADDRESSES as u32 * 4;
const HOSTENT_NAME_SIZE: u32 = 256;

/// Fills gethostbyname's shared result area for IPv4 `addresses`; returns its struct hostent.
fn write_hostent(name: &[u8], addresses: &[[u8; 4]]) -> u32 {
    if shared().state.hostent == 0 {
        let area = shared().misc_alloc(HOSTENT_NAME + HOSTENT_NAME_SIZE, 8);
        shared().state.hostent = area;
    }
    let base = shared().state.hostent;
    let m = mem();
    let name = &name[..name.len().min(HOSTENT_NAME_SIZE as usize - 1)];
    m.write_bytes(base + HOSTENT_NAME, name);
    m.write_u8(base + HOSTENT_NAME + name.len() as u32, 0);
    m.write_u32(base + HOSTENT_ALIASES, 0);
    for (i, address) in addresses.iter().take(HOSTENT_ADDRESSES).enumerate() {
        let at = base + HOSTENT_ADDRESS_BYTES + i as u32 * 4;
        m.write_bytes(at, address);
        m.write_u32(base + HOSTENT_ADDRESS_LIST + i as u32 * 4, at);
    }
    m.write_u32(base + HOSTENT_ADDRESS_LIST + addresses.len().min(HOSTENT_ADDRESSES) as u32 * 4, 0);
    m.write_u32(base, base + HOSTENT_NAME); // h_name
    m.write_u32(base + 4, base + HOSTENT_ALIASES); // h_aliases
    m.write_u32(base + 8, libc::AF_INET as u32); // h_addrtype
    m.write_u32(base + 12, 4); // h_length
    m.write_u32(base + 16, base + HOSTENT_ADDRESS_LIST); // h_addr_list
    base
}

/// getnameinfo's NI_ flags: bionic uses the BSD numbers (as does the phone);
/// the PC's glibc numbers them differently.
fn name_flags(bionic: u32) -> i32 {
    #[cfg(target_os = "android")]
    return bionic as i32;
    #[cfg(not(target_os = "android"))]
    {
        const BIONIC: [(u32, i32); 5] = [
            (0x1, libc::NI_NOFQDN),
            (0x2, libc::NI_NUMERICHOST),
            (0x4, libc::NI_NAMEREQD),
            (0x8, libc::NI_NUMERICSERV),
            (0x10, libc::NI_DGRAM),
        ];
        BIONIC.iter().filter(|(bit, _)| bionic & bit != 0).fold(0, |flags, (_, host)| flags | host)
    }
}

fn pointer(guest: u32) -> *mut u8 {
    mem().host_or_null(guest)
}

fn finish(uc: &Uc, result: i64, error: i32) -> Ret {
    if result < 0 {
        mem().write_u32(uc.get_data().errno_slot, error as u32);
    }
    Ret::U32(result as i32 as u32)
}

fn host_call(uc: &Uc, call: impl FnOnce() -> i64) -> Ret {
    let result = call();
    let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    finish(uc, result, error)
}

fn blocking(uc: &Uc, call: impl FnOnce() -> i64) -> Ret {
    let (result, error) = without_gil(|| {
        let result = call();
        (result, std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    });
    finish(uc, result, error)
}

fn read_timeval(guest: u32) -> libc::timeval {
    libc::timeval {
        tv_sec: mem().read_u32(guest) as i32 as libc::time_t,
        tv_usec: mem().read_u32(guest + 4) as i32 as libc::suseconds_t,
    }
}

fn write_timeval(guest: u32, value: &libc::timeval) {
    mem().write_u32(guest, value.tv_sec as i32 as u32);
    mem().write_u32(guest + 4, value.tv_usec as i32 as u32);
}

fn is_timeout_option(level: i32, name: i32) -> bool {
    level == SOL_SOCKET && (name == SO_RCVTIMEO || name == SO_SNDTIMEO)
}

fn guest_addrinfo_chain(mut host: *const libc::addrinfo) -> u32 {
    let mut first = 0;
    let mut previous = 0;
    while !host.is_null() {
        let entry = unsafe { &*host };
        let address_length = entry.ai_addrlen as u32;
        let node = crate::runtime::heap().calloc(1, ADDRINFO_SIZE + address_length);
        let memory = mem();
        memory.write_u32(node, entry.ai_flags as u32);
        memory.write_u32(node + 4, entry.ai_family as u32);
        memory.write_u32(node + 8, entry.ai_socktype as u32);
        memory.write_u32(node + 12, entry.ai_protocol as u32);
        memory.write_u32(node + 16, address_length);
        if !entry.ai_canonname.is_null() {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.ai_canonname) }.to_bytes().to_vec();
            let copy = crate::runtime::heap().strdup(&name);
            memory.write_u32(node + 20, copy);
        }
        if !entry.ai_addr.is_null() && address_length > 0 {
            let bytes = unsafe { std::slice::from_raw_parts(entry.ai_addr as *const u8, address_length as usize) };
            memory.write_bytes(node + ADDRINFO_SIZE, bytes);
            memory.write_u32(node + 24, node + ADDRINFO_SIZE);
        }
        if previous == 0 {
            first = node;
        } else {
            memory.write_u32(previous + 28, node);
        }
        previous = node;
        host = entry.ai_next;
    }
    first
}

fn accept(uc: &Uc, fd: i32, address: u32, length: u32, flags: i32) -> Ret {
    let (address, length) = (pointer(address) as usize, pointer(length) as usize);
    blocking(uc, move || unsafe {
        libc::accept4(fd, address as *mut libc::sockaddr, length as *mut libc::socklen_t, flags)
    } as i64)
}

fn send_to(uc: &Uc, fd: i32, buffer: u32, length: u32, flags: i32, address: u32, address_length: u32) -> Ret {
    let (buffer, address) = (pointer(buffer) as usize, pointer(address) as usize);
    blocking(uc, move || unsafe {
        libc::sendto(fd, buffer as *const _, length as usize, flags, address as *const libc::sockaddr, address_length)
    } as i64)
}

fn receive_from(uc: &Uc, fd: i32, buffer: u32, length: u32, flags: i32, address: u32, address_length: u32) -> Ret {
    let (buffer, address, address_length) =
        (pointer(buffer) as usize, pointer(address) as usize, pointer(address_length) as usize);
    blocking(uc, move || unsafe {
        libc::recvfrom(fd, buffer as *mut _, length as usize, flags, address as *mut libc::sockaddr, address_length as *mut libc::socklen_t)
    } as i64)
}

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        "socket" => handler!(|uc, a| {
            let (domain, kind, protocol) = (a.i32(), a.i32(), a.i32());
            host_call(uc, || unsafe { libc::socket(domain, kind, protocol) } as i64)
        }),
        "socketpair" => handler!(|uc, a| {
            let (domain, kind, protocol, out) = (a.i32(), a.i32(), a.i32(), a.u32());
            let mut pair = [0i32; 2];
            let result = host_call(uc, || unsafe { libc::socketpair(domain, kind, protocol, pair.as_mut_ptr()) } as i64);
            mem().write_u32(out, pair[0] as u32);
            mem().write_u32(out + 4, pair[1] as u32);
            result
        }),
        "connect" => handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            let address = pointer(address) as usize;
            blocking(uc, move || unsafe { libc::connect(fd, address as *const libc::sockaddr, length) } as i64)
        }),
        "bind" => handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            host_call(uc, || unsafe { libc::bind(fd, pointer(address).cast(), length) } as i64)
        }),
        "listen" => handler!(|uc, a| {
            let (fd, backlog) = (a.i32(), a.i32());
            host_call(uc, || unsafe { libc::listen(fd, backlog) } as i64)
        }),
        "accept" => handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            accept(uc, fd, address, length, 0)
        }),
        "accept4" => handler!(|uc, a| {
            let (fd, address, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            accept(uc, fd, address, length, flags)
        }),
        "shutdown" => handler!(|uc, a| {
            let (fd, how) = (a.i32(), a.i32());
            host_call(uc, || unsafe { libc::shutdown(fd, how) } as i64)
        }),
        "getsockname" => handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            host_call(uc, || unsafe { libc::getsockname(fd, pointer(address).cast(), pointer(length).cast()) } as i64)
        }),
        "getpeername" => handler!(|uc, a| {
            let (fd, address, length) = (a.i32(), a.u32(), a.u32());
            host_call(uc, || unsafe { libc::getpeername(fd, pointer(address).cast(), pointer(length).cast()) } as i64)
        }),
        "send" => handler!(|uc, a| {
            let (fd, buffer, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            send_to(uc, fd, buffer, length, flags, 0, 0)
        }),
        "sendto" => handler!(|uc, a| {
            let (fd, buffer, length, flags, address, address_length) =
                (a.i32(), a.u32(), a.u32(), a.i32(), a.u32(), a.u32());
            send_to(uc, fd, buffer, length, flags, address, address_length)
        }),
        "recv" => handler!(|uc, a| {
            let (fd, buffer, length, flags) = (a.i32(), a.u32(), a.u32(), a.i32());
            receive_from(uc, fd, buffer, length, flags, 0, 0)
        }),
        "recvfrom" => handler!(|uc, a| {
            let (fd, buffer, length, flags, address, address_length) =
                (a.i32(), a.u32(), a.u32(), a.i32(), a.u32(), a.u32());
            receive_from(uc, fd, buffer, length, flags, address, address_length)
        }),
        "setsockopt" => handler!(|uc, a| {
            let (fd, level, option, value, length) = (a.i32(), a.i32(), a.i32(), a.u32(), a.u32());
            if is_timeout_option(level, option) && length >= 8 {
                let timeout = read_timeval(value);
                let size = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
                return host_call(uc, || unsafe {
                    libc::setsockopt(fd, level, option, (&timeout as *const libc::timeval).cast(), size)
                } as i64);
            }
            host_call(uc, || unsafe { libc::setsockopt(fd, level, option, pointer(value).cast(), length) } as i64)
        }),
        "getsockopt" => handler!(|uc, a| {
            let (fd, level, option, value, length) = (a.i32(), a.i32(), a.i32(), a.u32(), a.u32());
            if is_timeout_option(level, option) {
                let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
                let mut size = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
                let result = host_call(uc, || unsafe {
                    libc::getsockopt(fd, level, option, (&mut timeout as *mut libc::timeval).cast(), &mut size)
                } as i64);
                write_timeval(value, &timeout);
                mem().write_u32(length, 8);
                return result;
            }
            host_call(uc, || unsafe { libc::getsockopt(fd, level, option, pointer(value).cast(), pointer(length).cast()) } as i64)
        }),
        "select" => handler!(|uc, a| {
            let (count, read, write, except, timeout) = (a.i32(), a.u32(), a.u32(), a.u32(), a.u32());
            let mut host_timeout = (timeout != 0).then(|| read_timeval(timeout));
            let sets = [read, write, except].map(|set| pointer(set) as usize);
            let timeout_pointer = host_timeout.as_mut().map_or(0, |t| t as *mut libc::timeval as usize);
            let result = blocking(uc, move || unsafe {
                libc::select(
                    count,
                    sets[0] as *mut libc::fd_set,
                    sets[1] as *mut libc::fd_set,
                    sets[2] as *mut libc::fd_set,
                    timeout_pointer as *mut libc::timeval,
                )
            } as i64);
            if let Some(remaining) = host_timeout {
                write_timeval(timeout, &remaining);
            }
            result
        }),
        "inet_pton" => handler!(|uc, a| {
            let (family, text, out) = (a.i32(), a.u32(), a.u32());
            let text = mem().string(text);
            match family {
                libc::AF_INET => match text.parse::<std::net::Ipv4Addr>() {
                    Ok(address) => {
                        mem().write_bytes(out, &address.octets());
                        Ret::U32(1)
                    }
                    Err(_) => Ret::U32(0),
                },
                libc::AF_INET6 => match text.parse::<std::net::Ipv6Addr>() {
                    Ok(address) => {
                        mem().write_bytes(out, &address.octets());
                        Ret::U32(1)
                    }
                    Err(_) => Ret::U32(0),
                },
                _ => finish(uc, -1, libc::EAFNOSUPPORT),
            }
        }),
        "inet_ntop" => handler!(|uc, a| {
            let (family, source, out, size) = (a.i32(), a.u32(), a.u32(), a.u32());
            let text = match family {
                libc::AF_INET => {
                    let octets: [u8; 4] = mem().bytes(source, 4).try_into().unwrap();
                    std::net::Ipv4Addr::from(octets).to_string()
                }
                libc::AF_INET6 => {
                    let octets: [u8; 16] = mem().bytes(source, 16).try_into().unwrap();
                    std::net::Ipv6Addr::from(octets).to_string()
                }
                _ => {
                    finish(uc, -1, libc::EAFNOSUPPORT);
                    return Ret::U32(0);
                }
            };
            if text.len() + 1 > size as usize {
                finish(uc, -1, libc::ENOSPC);
                return Ret::U32(0);
            }
            mem().write_bytes(out, text.as_bytes());
            mem().write_u8(out + text.len() as u32, 0);
            Ret::U32(out)
        }),
        "inet_addr" => handler!(|uc, a| {
            let text = mem().string(a.u32());
            let address = text.parse::<std::net::Ipv4Addr>().map_or(u32::MAX, |ip| u32::from_ne_bytes(ip.octets()));
            Ret::U32(address)
        }),
        // Socket addresses have no pointers, so the guest's is used as is.
        "getnameinfo" => handler!(|uc, a| {
            let (address, length, host, host_length, service, service_length) =
                (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let flags = name_flags(a.u32());
            let (address, host, service) = (pointer(address) as usize, pointer(host) as usize, pointer(service) as usize);
            let result = without_gil(move || unsafe {
                libc::getnameinfo(
                    address as *const libc::sockaddr,
                    length,
                    host as *mut libc::c_char,
                    host_length as _,
                    service as *mut libc::c_char,
                    service_length as _,
                    flags,
                )
            });
            Ret::U32(result as u32)
        }),
        "getaddrinfo" => handler!(|uc, a| {
            let (node, service, hints, out) = (a.u32(), a.u32(), a.u32(), a.u32());
            let mut host_hints: libc::addrinfo = unsafe { std::mem::zeroed() };
            if hints != 0 {
                host_hints.ai_flags = mem().read_u32(hints) as i32;
                host_hints.ai_family = mem().read_u32(hints + 4) as i32;
                host_hints.ai_socktype = mem().read_u32(hints + 8) as i32;
                host_hints.ai_protocol = mem().read_u32(hints + 12) as i32;
            }
            let (node, service) = (pointer(node) as usize, pointer(service) as usize);
            let hints_pointer = if hints == 0 { 0 } else { &host_hints as *const libc::addrinfo as usize };
            let (result, list) = without_gil(move || {
                let mut list: *mut libc::addrinfo = std::ptr::null_mut();
                let result = unsafe {
                    libc::getaddrinfo(
                        node as *const libc::c_char,
                        service as *const libc::c_char,
                        hints_pointer as *const libc::addrinfo,
                        &mut list,
                    )
                };
                (result, list as usize)
            });
            if result == 0 {
                let list = list as *mut libc::addrinfo;
                mem().write_u32(out, guest_addrinfo_chain(list));
                unsafe { libc::freeaddrinfo(list) };
            }
            Ret::U32(result as u32)
        }),
        "freeaddrinfo" => handler!(|uc, a| {
            let mut node = a.u32();
            while node != 0 {
                let next = mem().read_u32(node + 28);
                let name = mem().read_u32(node + 20);
                if name != 0 {
                    crate::runtime::heap().free(name);
                }
                crate::runtime::heap().free(node);
                node = next;
            }
            Ret::Void
        }),
        "ioctl" => handler!(|uc, a| {
            let (fd, request, argument) = (a.i32(), a.u32(), a.u32());
            match request {
                // An int (or struct winsize) the same size on both sides.
                FIONREAD | FIONBIO | TIOCGWINSZ => host_call(uc, || unsafe { libc::ioctl(fd, request as _, pointer(argument)) } as i64),
                SIOCGIFCONF => interface_list(uc, fd, argument),
                // One struct ifreq with the answer inside it (name, address, flags, ...).
                SIOCGIFNAME..=SIOCSIFHWBROADCAST => {
                    let mut request_buffer = [0u8; HOST_IFREQ_SIZE];
                    request_buffer[..IFREQ_SIZE].copy_from_slice(mem().bytes(argument, IFREQ_SIZE));
                    let result = host_call(uc, || unsafe { libc::ioctl(fd, request as _, request_buffer.as_mut_ptr()) } as i64);
                    mem().write_bytes(argument, &request_buffer[..IFREQ_SIZE]);
                    result
                }
                _ => finish(uc, -1, libc::ENOTTY),
            }
        }),
        "inet_ntoa" => handler!(|uc, a| {
            // struct in_addr is passed by value: the address bytes in memory order.
            let text = std::net::Ipv4Addr::from(a.u32().to_ne_bytes()).to_string();
            Ret::U32(shared().cached_cstr(format!("inet_ntoa:{text}"), &text))
        }),
        "gethostbyname" => handler!(|uc, a| {
            let name_pointer = a.u32();
            let name = mem().c_str(name_pointer).to_owned();
            let addresses: Vec<[u8; 4]> = without_gil(move || {
                let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
                hints.ai_family = libc::AF_INET;
                hints.ai_socktype = libc::SOCK_STREAM;
                let mut list: *mut libc::addrinfo = std::ptr::null_mut();
                let mut found = Vec::new();
                if unsafe { libc::getaddrinfo(name.as_ptr(), std::ptr::null(), &hints, &mut list) } == 0 {
                    let mut node = list;
                    while !node.is_null() && found.len() < HOSTENT_ADDRESSES {
                        let address = unsafe { &*((*node).ai_addr as *const libc::sockaddr_in) };
                        found.push(address.sin_addr.s_addr.to_ne_bytes());
                        node = unsafe { (*node).ai_next };
                    }
                    unsafe { libc::freeaddrinfo(list) };
                }
                found
            });
            if addresses.is_empty() {
                return Ret::U32(0);
            }
            Ret::U32(write_hostent(&mem().c_str(name_pointer).to_bytes().to_vec(), &addresses))
        }),
        "if_nametoindex" => handler!(|uc, a| Ret::U32(unsafe { libc::if_nametoindex(pointer(a.u32()).cast()) })),
        "if_indextoname" => handler!(|uc, a| {
            let (index, out) = (a.u32(), a.u32());
            let result = unsafe { libc::if_indextoname(index, pointer(out).cast()) };
            if result.is_null() {
                finish(uc, -1, std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
                return Ret::U32(0);
            }
            Ret::U32(out)
        }),
        // getservbyport(port in network order, protocol or null) from the phone's service list.
        "getservbyport" => handler!(|uc, a| {
            let (port, protocol) = (a.i32(), a.u32());
            let found = unsafe { libc::getservbyport(port, pointer(protocol) as *const libc::c_char) };
            if found.is_null() {
                return Ret::U32(0);
            }
            let found = unsafe { &*found };
            let name = unsafe { std::ffi::CStr::from_ptr(found.s_name) }.to_bytes().to_vec();
            let protocol = unsafe { std::ffi::CStr::from_ptr(found.s_proto) }.to_bytes().to_vec();
            Ret::U32(write_servent(&name, found.s_port, &protocol))
        }),
        "gai_strerror" => handler!(|uc, a| {
            let text = unsafe { std::ffi::CStr::from_ptr(libc::gai_strerror(a.i32())) }.to_bytes().to_vec();
            Ret::U32(crate::runtime::heap().strdup(&text))
        }),
        _ => None,
    }
}
