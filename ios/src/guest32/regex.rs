//! POSIX regular expressions for 32-bit games (regcomp, regexec, regfree)
//! on the host's C library, in Darwin's layouts and numbers.
//!
//! Darwin armv7's regex_t is { re_magic, re_nsub, re_endp, re_g } (16
//! bytes) and regmatch_t { off_t rm_so, rm_eo } (two 64-bit offsets, 16
//! bytes); the host's are bigger or narrower, so the host's regex_t lives
//! here, named by re_g. Darwin's flags are the BSD ones (REG_NOSUB 4,
//! REG_NEWLINE 8; glibc has those two the other way round), with
//! REG_NOSPEC and REG_PEND done here for C libraries that lack them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

use super::function;

const REG_EXTENDED: u32 = 1;
const REG_ICASE: u32 = 2;
const REG_NOSUB: u32 = 4;
const REG_NEWLINE: u32 = 8;
const REG_NOSPEC: u32 = 0x10;
const REG_PEND: u32 = 0x20;
/// regexec: REG_NOTBOL 1, REG_NOTEOL 2, REG_STARTEND 4 (the same on the hosts).
const REG_STARTEND: u32 = 4;
const EXEC_FLAGS: u32 = 7;

const REG_NOMATCH: u32 = 1;
const REG_BADPAT: u32 = 2;
#[cfg(not(target_os = "android"))]
const REG_EPAREN: u32 = 8;
#[cfg(not(target_os = "android"))]
const REG_ESPACE: u32 = 12;
const REG_INVARG: u32 = 16;
/// Libc's MAGIC1 in a compiled regex_t.
const MAGIC: u32 = 0xF265;

struct Compiled {
    host: Box<libc::regex_t>,
    no_sub: bool,
}

// The host's regex_t is only used by one call at a time per pattern (as in C).
unsafe impl Send for Compiled {}

static COMPILED: Mutex<Option<HashMap<u32, Compiled>>> = Mutex::new(None);
static NEXT: AtomicU32 = AtomicU32::new(1);

fn compiled<R>(work: impl FnOnce(&mut HashMap<u32, Compiled>) -> R) -> R {
    work(COMPILED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// Where re_nsub sits in the host's regex_t (the libc crate keeps its fields
/// private): bionic's is { int re_magic; size_t re_nsub; ... }, glibc's has
/// six words before it.
#[cfg(target_os = "android")]
const HOST_NSUB_OFFSET: usize = std::mem::size_of::<usize>();
#[cfg(not(target_os = "android"))]
const HOST_NSUB_OFFSET: usize = 6 * std::mem::size_of::<usize>();

fn host_subexpressions(host: &libc::regex_t) -> usize {
    unsafe { ((host as *const libc::regex_t as *const u8).add(HOST_NSUB_OFFSET) as *const usize).read() }
}

fn host_compile_flags(darwin: u32) -> libc::c_int {
    let mut host = 0;
    for (apple, own) in [(REG_EXTENDED, libc::REG_EXTENDED), (REG_ICASE, libc::REG_ICASE), (REG_NOSUB, libc::REG_NOSUB), (REG_NEWLINE, libc::REG_NEWLINE)] {
        if darwin & apple != 0 {
            host |= own;
        }
    }
    host
}

/// The host's error number in Darwin's (1-13 agree everywhere; bionic's
/// 14-16 are Darwin's too, glibc's 14-16 are its own).
fn darwin_error(host: libc::c_int) -> u32 {
    match host {
        0..=13 => host as u32,
        #[cfg(target_os = "android")]
        14..=16 => host as u32,
        #[cfg(not(target_os = "android"))]
        15 => REG_ESPACE, // REG_ESIZE
        #[cfg(not(target_os = "android"))]
        16 => REG_EPAREN, // REG_ERPAREN
        _ => REG_BADPAT,
    }
}

/// REG_NOSPEC: every character stands for itself.
fn literal(pattern: &[u8], extended: bool) -> Vec<u8> {
    let special: &[u8] = if extended { b"\\^$.[]|()?*+{}" } else { b"\\^$.[]*" };
    let mut out = Vec::with_capacity(pattern.len() * 2);
    for &byte in pattern {
        if special.contains(&byte) {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out
}

pub fn register() {
    // regcomp(regex_t *, const char *pattern, int flags)
    function(
        "regcomp",
        handler!(|uc, a| {
            let (re, pattern, flags) = (a.u32(), a.u32(), a.u32());
            let m = mem();
            let mut text = if flags & REG_PEND != 0 {
                let end = m.read_u32(re + 8);
                if end < pattern {
                    return Ret::U32(REG_INVARG);
                }
                m.bytes(pattern, (end - pattern) as usize).to_vec()
            } else {
                m.bytes(pattern, unsafe { libc::strlen(m.host(pattern).cast()) }).to_vec()
            };
            if flags & REG_NOSPEC != 0 {
                if flags & REG_EXTENDED != 0 {
                    return Ret::U32(REG_INVARG);
                }
                text = literal(&text, false);
            }
            if text.contains(&0) {
                return Ret::U32(REG_BADPAT);
            }
            text.push(0);
            let mut host: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
            let result = unsafe { libc::regcomp(&mut *host, text.as_ptr().cast(), host_compile_flags(flags)) };
            if result != 0 {
                return Ret::U32(darwin_error(result));
            }
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            m.write_u32(re, MAGIC);
            m.write_u32(re + 4, host_subexpressions(&host) as u32);
            m.write_u32(re + 12, id);
            compiled(|table| table.insert(id, Compiled { host, no_sub: flags & REG_NOSUB != 0 }));
            Ret::U32(0)
        })
        .unwrap(),
    );
    // regexec(const regex_t *, const char *, size_t nmatch, regmatch_t pmatch[], int flags)
    function(
        "regexec",
        handler!(|uc, a| {
            let (re, string, count, matches, flags) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let m = mem();
            let id = m.read_u32(re + 12);
            let found = compiled(|table| table.get(&id).map(|c| (&*c.host as *const libc::regex_t, c.no_sub)));
            let Some((host, no_sub)) = found.filter(|_| m.read_u32(re) == MAGIC) else {
                return Ret::U32(REG_BADPAT);
            };
            let count = if no_sub { 0 } else { count as usize };
            let mut host_matches: Vec<libc::regmatch_t> = vec![unsafe { std::mem::zeroed() }; count.max(1)];
            if flags & REG_STARTEND != 0 {
                host_matches[0].rm_so = m.read_u64(matches) as i64 as _;
                host_matches[0].rm_eo = m.read_u64(matches + 8) as i64 as _;
            }
            let given = if flags & REG_STARTEND != 0 { count.max(1) } else { count };
            let result = unsafe {
                libc::regexec(host, m.host(string).cast(), given, host_matches.as_mut_ptr(), (flags & EXEC_FLAGS) as libc::c_int)
            };
            if result != 0 {
                return Ret::U32(if result == libc::REG_NOMATCH { REG_NOMATCH } else { darwin_error(result) });
            }
            for (i, found) in host_matches.iter().take(count).enumerate() {
                let at = matches + i as u32 * 16;
                m.write_u64(at, found.rm_so as i64 as u64);
                m.write_u64(at + 8, found.rm_eo as i64 as u64);
            }
            Ret::U32(0)
        })
        .unwrap(),
    );
    function(
        "regfree",
        handler!(|uc, a| {
            let re = a.u32();
            let m = mem();
            if let Some(mut gone) = compiled(|table| table.remove(&m.read_u32(re + 12))) {
                unsafe { libc::regfree(&mut *gone.host) };
            }
            m.write_u32(re, 0);
            m.write_u32(re + 12, 0);
            Ret::Void
        })
        .unwrap(),
    );
}
