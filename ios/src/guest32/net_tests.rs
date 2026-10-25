//! Darwin's 32-bit socket layouts: pointers in `struct addrinfo` and
//! `struct hostent` are 4 bytes, socket addresses start with a length byte,
//! `fd_set`/`struct timeval` are the armv7 sizes and numbers, and errno
//! comes back as Darwin's number. Each test calls the registered handler
//! the way translated game code would.

use super::tests::{alloc, call, cstring};
use arm32_bridge::runtime::{self, mem};

/// The guest's (Darwin) errno.
fn errno() -> u32 {
    let slot = call("__error", [0; 4], &[]).0;
    mem().read_u32(slot)
}

/// A guest word holding `value`.
fn word(value: u32) -> u32 {
    let at = alloc(4);
    mem().write_u32(at, value);
    at
}

fn close(fd: i32) {
    unsafe { libc::close(fd) };
}

#[test]
fn getaddrinfo_gives_an_armv7_list_that_freeaddrinfo_frees() {
    let hints = alloc(32);
    mem().write_bytes(hints, &[0; 32]);
    mem().write_u32(hints, 0x4); // AI_NUMERICHOST
    mem().write_u32(hints + 4, 2); // AF_INET
    mem().write_u32(hints + 8, 1); // SOCK_STREAM
    let out = alloc(4);
    mem().write_u32(out, 0);
    assert_eq!(call("getaddrinfo", [cstring("127.0.0.1"), cstring("80"), hints, out], &[]).0, 0);
    let node = mem().read_u32(out);
    assert_ne!(node, 0, "no list");
    // { int flags, family, socktype, protocol; socklen_t addrlen;
    //   char *canonname; struct sockaddr *addr; struct addrinfo *next }
    assert_eq!(mem().read_u32(node), 0x4, "ai_flags");
    assert_eq!(mem().read_u32(node + 4), 2, "ai_family");
    assert_eq!(mem().read_u32(node + 8), 1, "ai_socktype");
    assert_eq!(mem().read_u32(node + 12), 6, "ai_protocol");
    assert_eq!(mem().read_u32(node + 16), 16, "ai_addrlen");
    assert_eq!(mem().read_u32(node + 20), 0, "ai_canonname");
    let address = mem().read_u32(node + 24);
    assert_eq!(
        mem().bytes(address, 16),
        [16, 2, 0, 80, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        "armv7 sockaddr_in (length byte, family, big-endian port)"
    );
    assert_eq!(mem().read_u32(node + 28), 0, "ai_next");
    assert!(runtime::heap().not_owned(node).is_none(), "the list is not a guest heap block");
    call("freeaddrinfo", [node, 0, 0, 0], &[]);
    assert!(runtime::heap().not_owned(node).is_some(), "freeaddrinfo left the list allocated");
    // "localhost" (no hints) is a chain: IPv6 first, then IPv4, both heap
    // blocks with the next links that freeaddrinfo walks.
    mem().write_u32(out, 0);
    assert_eq!(call("getaddrinfo", [cstring("localhost"), cstring("80"), 0, out], &[]).0, 0, "localhost");
    let mut node = mem().read_u32(out);
    assert_ne!(node, 0, "no localhost list");
    let mut families = Vec::new();
    while node != 0 {
        let family = mem().read_u32(node + 4);
        families.push(family);
        assert_eq!(mem().read_u8(mem().read_u32(node + 24) + 1) as u32, family, "sockaddr family");
        node = mem().read_u32(node + 28);
    }
    assert!(families.len() >= 2 && families[0] == 30 && *families.last().unwrap() == 2, "IPv6 first, then IPv4: {families:?}");
    call("freeaddrinfo", [mem().read_u32(out), 0, 0, 0], &[]);
    // A name that is neither a number nor localhost: EAI_NONAME (8), nothing written.
    mem().write_u32(out, 0xAAAA);
    assert_eq!(call("getaddrinfo", [cstring("example.com"), cstring("80"), hints, out], &[]).0, 8);
    assert_eq!(mem().read_u32(out), 0xAAAA, "the answer pointer was written on failure");
}

#[test]
fn a_loopback_tcp_round_trip_uses_darwin_numbers() {
    let server = call("socket", [2, 1, 0, 0], &[]).0 as i32;
    assert!(server >= 0, "no AF_INET/SOCK_STREAM socket");
    // Darwin's SOL_SOCKET 0xffff and SO_REUSEADDR 0x4.
    assert_eq!(call("setsockopt", [server as u32, 0xffff, 0x4, word(1)], &[4]).0, 0, "SO_REUSEADDR");
    let address = alloc(16);
    mem().write_bytes(address, &[16, 2, 0, 0, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(call("bind", [server as u32, address, 16, 0], &[]).0, 0, "bind 127.0.0.1:0");
    assert_eq!(call("listen", [server as u32, 1, 0, 0], &[]).0, 0, "listen");
    let length = word(16);
    assert_eq!(call("getsockname", [server as u32, address, length, 0], &[]).0, 0, "getsockname");
    assert_eq!(mem().read_u32(length), 16, "armv7 sockaddr_in is 16 bytes");
    let bound = mem().bytes(address, 16).to_vec();
    assert_eq!(&bound[4..8], &[127, 0, 0, 1], "not bound to loopback");
    let port = u16::from_be_bytes([bound[2], bound[3]]);
    assert_ne!(port, 0, "no port was given");

    let client = call("socket", [2, 1, 0, 0], &[]).0 as i32;
    assert!(client >= 0, "no client socket");
    mem().write_bytes(address, &[16, 2, bound[2], bound[3], 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(call("connect", [client as u32, address, 16, 0], &[]).0, 0, "connect to loopback");
    let peer = alloc(64);
    let peer_length = word(64);
    let accepted = call("accept", [server as u32, peer, peer_length, 0], &[]).0 as i32;
    assert!(accepted >= 0, "accept");
    assert_eq!(mem().read_u32(peer_length), 16, "accept's peer length");
    let peer_bytes = mem().bytes(peer, 16).to_vec();
    assert_eq!((peer_bytes[0], peer_bytes[1]), (16, 2), "accept's peer sockaddr");
    assert_eq!(&peer_bytes[4..8], &[127, 0, 0, 1], "accept's peer address");
    assert_ne!(&peer_bytes[2..4], &[0, 0][..], "accept's peer port");
    // getpeername and getnameinfo read the same peer back.
    let named = alloc(16);
    let named_length = word(16);
    assert_eq!(call("getpeername", [accepted as u32, named, named_length, 0], &[]).0, 0, "getpeername");
    assert_eq!(mem().read_u32(named_length), 16);
    assert_eq!(&mem().bytes(named, 16)[4..8], &[127, 0, 0, 1]);
    let name = alloc(64);
    let service = alloc(16);
    // NI_NUMERICHOST 2; the service and its length are on the stack.
    assert_eq!(call("getnameinfo", [peer, 16, name, 64], &[service, 16, 2]).0, 0, "getnameinfo");
    assert_eq!(mem().string(name), "127.0.0.1");
    assert!(!mem().string(service).is_empty(), "no service port");

    let two = alloc(2);
    mem().write_bytes(two, b"hi");
    assert_eq!(call("send", [client as u32, two, 2, 0], &[]).0, 2, "send");
    let received = alloc(16);
    assert_eq!(call("recv", [accepted as u32, received, 16, 0], &[]).0, 2, "recv");
    assert_eq!(mem().bytes(received, 2), b"hi", "recv changed the bytes");

    // SO_RCVTIMEO takes an 8-byte armv7 timeval: { int32 seconds, int32
    // microseconds } becomes the host's 16-byte field. (A kernel round trip
    // can't be checked under qemu: it answers a zero timeval and length.)
    let timeout = alloc(8);
    mem().write_u32(timeout, 3);
    mem().write_u32(timeout + 4, 400_000);
    let converted = super::net::timeval(timeout);
    assert_eq!(i64::from_ne_bytes(converted[..8].try_into().unwrap()), 3, "seconds");
    assert_eq!(i32::from_ne_bytes(converted[8..12].try_into().unwrap()), 400_000, "microseconds");
    assert_eq!(call("setsockopt", [accepted as u32, 0xffff, 0x1006, timeout], &[8]).0, 0, "SO_RCVTIMEO");
    let got = alloc(8);
    mem().write_bytes(got, &[0xAA; 8]);
    let got_length = word(8);
    assert_eq!(call("getsockopt", [accepted as u32, 0xffff, 0x1006, got], &[got_length]).0, 0, "getsockopt");
    assert_eq!(mem().read_u32(got_length), 8, "getsockopt's armv7 length");

    // fcntl: Darwin's F_GETFL 3, F_SETFL 4, O_NONBLOCK 0x4.
    assert_eq!(call("fcntl", [accepted as u32, 3, 0, 0], &[]).0 & 0x4, 0, "non-blocking at first");
    assert_eq!(call("fcntl", [accepted as u32, 4, 4, 0], &[]).0, 0, "F_SETFL O_NONBLOCK");
    assert_eq!(call("fcntl", [accepted as u32, 3, 0, 0], &[]).0 & 0x4, 0x4, "O_NONBLOCK reads back as 0x4");
    assert_eq!(call("recv", [accepted as u32, received, 16, 0], &[]).0, u32::MAX, "an empty socket gave data");
    assert_eq!(errno(), 35, "Darwin's EAGAIN");

    close(accepted);
    close(client);
    close(server);
}

#[test]
fn gethostbyname_gives_a_32_bit_hostent() {
    let entry = call("gethostbyname", [cstring("127.0.0.1"), 0, 0, 0], &[]).0;
    assert_ne!(entry, 0, "no hostent for a number");
    // { char *h_name; char **h_aliases; int h_addrtype; int h_length;
    //   char **h_addr_list } with 4-byte pointers (20 bytes).
    assert_ne!(mem().read_u32(entry), 0, "h_name");
    assert_ne!(mem().read_u32(entry + 4), 0, "h_aliases");
    assert_eq!(mem().read_u32(entry + 8), 2, "h_addrtype: AF_INET");
    assert_eq!(mem().read_u32(entry + 12), 4, "h_length");
    let list = mem().read_u32(entry + 16);
    assert_ne!(list, 0, "h_addr_list");
    assert_eq!(mem().bytes(mem().read_u32(list), 4), [127, 0, 0, 1], "h_addr");
    assert_eq!(mem().read_u32(list + 4), 0, "a second address");
    // A name off this device isn't looked up (offline).
    assert_eq!(call("gethostbyname", [cstring("example.com"), 0, 0, 0], &[]).0, 0);
}

#[test]
fn inet_pton_and_ntop_use_darwin_families() {
    let address = alloc(16);
    let text = alloc(64);
    assert_eq!(call("inet_pton", [2, cstring("192.0.2.7"), address, 0], &[]).0, 1, "IPv4 inet_pton");
    assert_eq!(mem().bytes(address, 4), [192, 0, 2, 7]);
    assert_eq!(call("inet_ntop", [2, address, text, 64], &[]).0, text, "inet_ntop returns its buffer");
    assert_eq!(mem().string(text), "192.0.2.7");
    // Darwin's AF_INET6 is 30 (Linux's is 10).
    assert_eq!(call("inet_pton", [30, cstring("2001:db8::1"), address, 0], &[]).0, 1, "IPv6 inet_pton");
    assert_eq!(
        mem().bytes(address, 16),
        [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        "IPv6 address bytes"
    );
    assert_eq!(call("inet_ntop", [30, address, text, 64], &[]).0, text, "IPv6 inet_ntop");
    assert_eq!(mem().string(text), "2001:db8::1");
    // Malformed text is refused, as is a family this bridge doesn't have.
    assert_eq!(call("inet_pton", [2, cstring("not an address"), address, 0], &[]).0, 0, "bad IPv4 text");
    assert_eq!(call("inet_pton", [99, cstring("127.0.0.1"), address, 0], &[]).0, u32::MAX, "bad family");
}

#[test]
fn poll_and_select_read_armv7_fd_sets_and_timevals() {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) }, 0);
    let (read_fd, write_fd) = (fds[0], fds[1]);
    assert!(read_fd < 1024, "fd {read_fd} is outside Darwin's fd_set");
    assert_eq!(unsafe { libc::write(write_fd, b"x".as_ptr().cast(), 1) }, 1);
    // struct pollfd { int fd; short events; short revents } with POLLIN 1.
    let list = alloc(8);
    mem().write_u32(list, read_fd as u32);
    mem().write_u32(list + 4, 1);
    assert_eq!(call("poll", [list, 1, 0, 0], &[]).0, 1, "poll on a readable socket");
    assert_eq!(mem().read_u16(list + 6), 1, "poll's POLLIN");
    // fd_set is 32 int32 words (128 bytes); struct timeval is { int32, int32 }.
    let set = alloc(128);
    let timeout = alloc(8);
    let readable = (1u8 << (read_fd % 8)) as u32;
    let byte = (read_fd / 8) as u32;
    mem().write_bytes(set, &[0; 128]);
    mem().write_u8(set + byte, readable as u8);
    mem().write_u32(timeout, 0);
    mem().write_u32(timeout + 4, 200_000);
    assert_eq!(call("select$DARWIN_EXTSN", [(read_fd + 1) as u32, set, 0, 0], &[timeout]).0, 1, "select$DARWIN_EXTSN");
    assert_eq!(mem().read_u8(set + byte) as u32 & readable, readable, "select's read set lost the socket");
    mem().write_bytes(set, &[0; 128]);
    mem().write_u8(set + byte, readable as u8);
    mem().write_u32(timeout, 0);
    mem().write_u32(timeout + 4, 200_000);
    assert_eq!(call("select", [(read_fd + 1) as u32, set, 0, 0], &[timeout]).0, 1, "select");
    unsafe {
        libc::close(read_fd);
        libc::close(write_fd);
    }
}
