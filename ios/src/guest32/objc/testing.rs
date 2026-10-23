//! A made-up guest for tests: a memory buffer (so guest addresses have real
//! host addresses) and Rust closures standing in for guest code.

use super::bridge::GuestSide;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const BASE: u32 = 0x10_0000;
const SIZE: usize = 0x10_0000;

type Code = Arc<dyn Fn(&[u32]) -> u64 + Send + Sync>;

pub struct Fake {
    memory: *mut u8,
    next: Mutex<u32>,
    code: Mutex<HashMap<u32, Code>>,
    pub freed: Mutex<Vec<u32>>,
}

unsafe impl Sync for Fake {}
unsafe impl Send for Fake {}

impl Fake {
    pub fn new() -> &'static Fake {
        let memory = Box::leak(vec![0u8; SIZE].into_boxed_slice()).as_mut_ptr();
        Box::leak(Box::new(Fake {
            memory,
            next: Mutex::new(BASE + 0x1000),
            code: Mutex::new(HashMap::new()),
            freed: Mutex::new(Vec::new()),
        }))
    }
    pub fn code(&self, imp: u32, f: impl Fn(&[u32]) -> u64 + Send + Sync + 'static) {
        self.code.lock().unwrap().insert(imp, Arc::new(f));
    }
    pub fn u32_at(&self, addr: u32) -> u32 {
        u32::from_le_bytes(self.read(addr, 4).try_into().unwrap())
    }
    pub fn put_words(&self, addr: u32, words: &[u32]) {
        self.write(addr, &words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>());
    }
    pub fn words(&self, words: &[u32]) -> u32 {
        let at = self.alloc_permanent(words.len() as u32 * 4);
        self.put_words(at, words);
        at
    }
    pub fn text(&self, s: &str) -> u32 {
        let at = self.alloc_permanent(s.len() as u32 + 1);
        self.write(at, &[s.as_bytes(), &[0]].concat());
        at
    }
}

impl GuestSide for Fake {
    fn read(&self, addr: u32, len: u32) -> Vec<u8> {
        assert!(addr >= BASE && (addr - BASE) as usize + len as usize <= SIZE, "fake read at {addr:#x}");
        unsafe { std::slice::from_raw_parts(self.memory.add((addr - BASE) as usize), len as usize).to_vec() }
    }
    fn write(&self, addr: u32, data: &[u8]) {
        assert!(addr >= BASE && (addr - BASE) as usize + data.len() <= SIZE, "fake write at {addr:#x}");
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.memory.add((addr - BASE) as usize), data.len()) }
    }
    fn alloc(&self, size: u32) -> u32 {
        self.alloc_permanent(size)
    }
    fn free(&self, addr: u32) {
        self.freed.lock().unwrap().push(addr);
    }
    fn alloc_permanent(&self, size: u32) -> u32 {
        let mut next = self.next.lock().unwrap();
        let at = *next;
        *next += size.max(1).div_ceil(8) * 8;
        at
    }
    fn host_address(&self, addr: u32) -> usize {
        self.memory as usize + (addr - BASE) as usize
    }
    fn guest_address(&self, host: usize) -> Option<u32> {
        let start = self.memory as usize;
        (host >= start && host < start + SIZE).then(|| BASE + (host - start) as u32)
    }
    fn call(&self, imp: u32, words: &[u32]) -> Result<u64, String> {
        let code = self.code.lock().unwrap().get(&imp).cloned().ok_or(format!("no fake code at {imp:#x}"))?;
        Ok(code(words))
    }
}
