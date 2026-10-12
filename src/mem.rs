//! Guest memory: one 4 GiB host reservation that holds the whole 32-bit
//! address space, so guest address `g` lives at host address `base + g`.
//! Unicorn sees the same pages through `mem_map_ptr`, so writes from either
//! side are visible to the other with no copying.

use std::ffi::CStr;

pub const PAGE: u32 = 0x1000;

/// Stand-ins for the phone's functions (8 bytes each) and the return traps.
pub const STUB_BASE: u32 = 0x0001_0000;
pub const STUB_SIZE: u32 = 0x0010_0000;
/// Game libraries are loaded one after another from here.
pub const LIB_BASE: u32 = 0x0100_0000;
pub const LIB_END: u32 = 0x1000_0000;
/// The guest's malloc heap.
pub const HEAP_BASE: u32 = 0x1000_0000;
pub const HEAP_END: u32 = 0xC000_0000;
/// Small runtime-owned data: fake Java tables, data imports, thread data.
pub const MISC_BASE: u32 = 0xC000_0000;
pub const MISC_END: u32 = 0xD000_0000;
/// Guest stack (grows down from STACK_TOP).
pub const STACK_BASE: u32 = 0xE000_0000;
pub const STACK_TOP: u32 = 0xF000_0000;

/// Regions handed to Unicorn. Everything else faults when touched.
pub const MAPPED_REGIONS: [(u32, u32); 5] = [
    (STUB_BASE, STUB_BASE + STUB_SIZE),
    (LIB_BASE, LIB_END),
    (HEAP_BASE, HEAP_END),
    (MISC_BASE, MISC_END),
    (STACK_BASE, STACK_TOP),
];

#[derive(Clone, Copy)]
pub struct GuestMem {
    base: *mut u8,
}

impl GuestMem {
    pub fn reserve() -> GuestMem {
        let size = 1usize << 32;
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "could not reserve 4 GiB for guest memory");
        GuestMem { base: base as *mut u8 }
    }

    pub fn host(&self, guest: u32) -> *mut u8 {
        unsafe { self.base.add(guest as usize) }
    }

    /// Host pointer for a guest pointer, keeping NULL as NULL.
    pub fn host_or_null(&self, guest: u32) -> *mut u8 {
        if guest == 0 {
            std::ptr::null_mut()
        } else {
            self.host(guest)
        }
    }

    /// Guest address for a host pointer inside the reservation (NULL stays 0).
    pub fn guest(&self, host: *const u8) -> u32 {
        if host.is_null() {
            return 0;
        }
        let offset = (host as usize).wrapping_sub(self.base as usize);
        assert!(offset < (1usize << 32), "host pointer {host:p} is outside guest memory");
        offset as u32
    }

    pub fn read_u8(&self, guest: u32) -> u8 {
        unsafe { *self.host(guest) }
    }
    pub fn read_u16(&self, guest: u32) -> u16 {
        unsafe { (self.host(guest) as *const u16).read_unaligned() }
    }
    pub fn read_u32(&self, guest: u32) -> u32 {
        unsafe { (self.host(guest) as *const u32).read_unaligned() }
    }
    pub fn read_u64(&self, guest: u32) -> u64 {
        unsafe { (self.host(guest) as *const u64).read_unaligned() }
    }
    pub fn write_u8(&self, guest: u32, value: u8) {
        unsafe { *self.host(guest) = value }
    }
    pub fn write_u16(&self, guest: u32, value: u16) {
        unsafe { (self.host(guest) as *mut u16).write_unaligned(value) }
    }
    pub fn write_u32(&self, guest: u32, value: u32) {
        unsafe { (self.host(guest) as *mut u32).write_unaligned(value) }
    }
    pub fn write_u64(&self, guest: u32, value: u64) {
        unsafe { (self.host(guest) as *mut u64).write_unaligned(value) }
    }

    pub fn bytes(&self, guest: u32, length: usize) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.host(guest), length) }
    }
    pub fn write_bytes(&self, guest: u32, bytes: &[u8]) {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.host(guest), bytes.len()) }
    }

    pub fn c_str(&self, guest: u32) -> &CStr {
        unsafe { CStr::from_ptr(self.host(guest) as *const libc::c_char) }
    }
    pub fn string(&self, guest: u32) -> String {
        if guest == 0 {
            return "(null)".to_string();
        }
        self.c_str(guest).to_string_lossy().into_owned()
    }
}

/// Gives dlmalloc pages from the guest heap region (bump only; the region
/// is huge and the proof is short-lived, so pages are never handed back).
pub struct GuestHeapPages {
    next: std::cell::Cell<usize>,
    end: usize,
}

unsafe impl Send for GuestHeapPages {}

impl GuestHeapPages {
    pub fn new(mem: &GuestMem) -> GuestHeapPages {
        GuestHeapPages {
            next: std::cell::Cell::new(mem.host(HEAP_BASE) as usize),
            end: mem.host(0).wrapping_add(HEAP_END as usize) as usize,
        }
    }
}

unsafe impl dlmalloc::Allocator for GuestHeapPages {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let size = (size + PAGE as usize - 1) & !(PAGE as usize - 1);
        let start = self.next.get();
        if start + size > self.end {
            return (std::ptr::null_mut(), 0, 0);
        }
        self.next.set(start + size);
        (start as *mut u8, size, 0)
    }
    fn remap(&self, _ptr: *mut u8, _old: usize, _new: usize, _can_move: bool) -> *mut u8 {
        std::ptr::null_mut()
    }
    fn free_part(&self, _ptr: *mut u8, _old: usize, _new: usize) -> bool {
        false
    }
    fn free(&self, _ptr: *mut u8, _size: usize) -> bool {
        false
    }
    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }
    fn allocates_zeros(&self) -> bool {
        true
    }
    fn page_size(&self) -> usize {
        PAGE as usize
    }
}

/// The guest's malloc. Pointers it returns are guest addresses.
pub struct GuestHeap {
    mem: GuestMem,
    inner: dlmalloc::Dlmalloc<GuestHeapPages>,
}

impl GuestHeap {
    pub fn new(mem: GuestMem) -> GuestHeap {
        GuestHeap { mem, inner: dlmalloc::Dlmalloc::new_with_allocator(GuestHeapPages::new(&mem)) }
    }
    pub fn malloc(&mut self, size: u32) -> u32 {
        let host = unsafe { self.inner.c_malloc(size.max(1) as usize) };
        self.mem.guest(host)
    }
    pub fn memalign(&mut self, align: u32, size: u32) -> u32 {
        let host = unsafe { self.inner.c_memalign(align.max(8) as usize, size.max(1) as usize) };
        self.mem.guest(host)
    }
    pub fn calloc(&mut self, count: u32, size: u32) -> u32 {
        let Some(total) = count.checked_mul(size) else { return 0 };
        let guest = self.malloc(total);
        if guest != 0 {
            unsafe { std::ptr::write_bytes(self.mem.host(guest), 0, total as usize) };
        }
        guest
    }
    pub fn realloc(&mut self, guest: u32, size: u32) -> u32 {
        if guest == 0 {
            return self.malloc(size);
        }
        let host = unsafe { self.inner.c_realloc(self.mem.host(guest), size.max(1) as usize) };
        self.mem.guest(host)
    }
    pub fn free(&mut self, guest: u32) {
        if guest != 0 {
            unsafe { self.inner.c_free(self.mem.host(guest)) }
        }
    }
    pub fn strdup(&mut self, text: &[u8]) -> u32 {
        let guest = self.malloc(text.len() as u32 + 1);
        self.mem.write_bytes(guest, text);
        self.mem.write_u8(guest + text.len() as u32, 0);
        guest
    }
}

/// Bump allocator for runtime-owned guest data that lives for the whole run.
pub struct Bump {
    next: u32,
    end: u32,
}

impl Bump {
    pub fn new(start: u32, end: u32) -> Bump {
        Bump { next: start, end }
    }
    pub fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let start = (self.next + align - 1) & !(align - 1);
        assert!(start + size <= self.end, "runtime data area is full");
        self.next = start + size;
        start
    }
}
