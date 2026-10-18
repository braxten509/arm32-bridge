//! Stand-ins for functions the game imports that are not written yet.
//! Each gets a tiny code stub that names it when called. By default the run
//! stops there; with keep-going the call returns 0 and the run continues.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub static KEEP_GOING: AtomicBool = AtomicBool::new(false);

struct Missing {
    names: Vec<String>,
    hits: BTreeMap<String, u64>,
}

static MISSING: Mutex<Missing> = Mutex::new(Missing { names: Vec::new(), hits: BTreeMap::new() });

std::arch::global_asm!(
    ".globl ios_missing_entry",
    ".p2align 2",
    "ios_missing_entry:",
    "stp x29, x30, [sp, #-16]!",
    "mov x29, sp",
    "mov x0, x16",
    "mov x1, x30",
    "bl ios_missing_called",
    "ldp x29, x30, [sp], #16",
    "ret",
);

extern "C" {
    fn ios_missing_entry();
}

#[no_mangle]
extern "C" fn ios_missing_called(index: usize, return_address: usize) -> usize {
    let name = {
        let mut missing = MISSING.lock().unwrap();
        let name = missing.names.get(index).cloned().unwrap_or_else(|| format!("#{index}"));
        *missing.hits.entry(name.clone()).or_default() += 1;
        name
    };
    let place = crate::loader::describe_address(return_address);
    if KEEP_GOING.load(Ordering::Relaxed) {
        if first_hit(&name) {
            crate::log!("missing function {name} (called from {place}), returning 0");
        }
        return 0;
    }
    crate::log!("STOP: missing function {name} (called from {place})");
    crate::report::finish(1);
}

fn first_hit(name: &str) -> bool {
    MISSING.lock().unwrap().hits.get(name).copied() == Some(1)
}

/// Every missing function reached so far, with call counts.
pub fn hits() -> Vec<(String, u64)> {
    MISSING.lock().unwrap().hits.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

/// Code region holding the stubs, filled at load time.
pub struct StubPage {
    base: *mut u32,
    capacity: usize,
    used: usize,
}

const STUB_WORDS: usize = 6;

impl StubPage {
    pub fn new(capacity: usize) -> StubPage {
        let bytes = (capacity * STUB_WORDS * 4 + 0xFFFF) & !0xFFFF;
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "can't reserve memory for missing-function stubs");
        StubPage { base: base as *mut u32, capacity, used: 0 }
    }

    /// Makes a stub that reports `name` when called.
    pub fn stub(&mut self, name: &str) -> usize {
        assert!(self.used < self.capacity, "too many missing functions");
        let index = {
            let mut missing = MISSING.lock().unwrap();
            missing.names.push(name.to_string());
            missing.names.len() - 1
        };
        assert!(index < 1 << 32);
        let low = (index & 0xFFFF) as u32;
        let high = ((index >> 16) & 0xFFFF) as u32;
        let target = ios_missing_entry as usize as u64;
        unsafe {
            let code = self.base.add(self.used * STUB_WORDS);
            code.add(0).write(0xD280_0010 | (low << 5)); // movz x16, #low
            code.add(1).write(0xF2A0_0010 | (high << 5)); // movk x16, #high, lsl 16
            code.add(2).write(0x5800_0051); // ldr x17, #8
            code.add(3).write(0xD61F_0220); // br x17
            code.add(4).write(target as u32);
            code.add(5).write((target >> 32) as u32);
            self.used += 1;
            code as usize
        }
    }

    /// Turns the stubs into runnable code.
    pub fn seal(&self) {
        let bytes = (self.capacity * STUB_WORDS * 4 + 0xFFFF) & !0xFFFF;
        unsafe {
            crate::loader::clear_cache(self.base as usize, self.base as usize + bytes);
            libc::mprotect(self.base as *mut _, bytes, libc::PROT_READ | libc::PROT_EXEC);
        }
    }
}
