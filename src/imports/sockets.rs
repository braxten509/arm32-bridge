use crate::handler;
use crate::runtime::{mem, shared, without_gil, ArgSource, Handler, Ret, Uc};

const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;
const SO_SNDTIMEO: i32 = 21;
const ADDRINFO_SIZE: u32 = 32;

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
        let node = shared().heap.calloc(1, ADDRINFO_SIZE + address_length);
        let memory = mem();
        memory.write_u32(node, entry.ai_flags as u32);
        memory.write_u32(node + 4, entry.ai_family as u32);
        memory.write_u32(node + 8, entry.ai_socktype as u32);
        memory.write_u32(node + 12, entry.ai_protocol as u32);
        memory.write_u32(node + 16, address_length);
        if !entry.ai_canonname.is_null() {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.ai_canonname) }.to_bytes().to_vec();
            let copy = shared().heap.strdup(&name);
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
                    shared().heap.free(name);
                }
                shared().heap.free(node);
                node = next;
            }
            Ret::Void
        }),
        "gai_strerror" => handler!(|uc, a| {
            let text = unsafe { std::ffi::CStr::from_ptr(libc::gai_strerror(a.i32())) }.to_bytes().to_vec();
            Ret::U32(shared().heap.strdup(&text))
        }),
        _ => None,
    }
}
