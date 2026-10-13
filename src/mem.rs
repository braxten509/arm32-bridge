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
/// The Linux "kuser helpers" page: small routines the kernel places at fixed
/// addresses that old (ARMv5, armeabi) code calls for atomics and the thread
/// pointer. See `write_kernel_helpers`.
pub const KUSER_BASE: u32 = 0xFFFF_0000;
pub const KUSER_END: u32 = 0xFFFF_1000;

/// Regions handed to Unicorn as (start, end, may hold code). Everything else
/// faults when touched. Writes to code regions pay for a self-modifying-code
/// check (see vendor/unicorn-engine-sys), so only stubs and libraries get it.
pub const MAPPED_REGIONS: [(u32, u32, bool); 6] = [
    (STUB_BASE, STUB_BASE + STUB_SIZE, true),
    (LIB_BASE, LIB_END, true),
    (HEAP_BASE, HEAP_END, false),
    (MISC_BASE, MISC_END, false),
    (STACK_BASE, STACK_TOP, false),
    (KUSER_BASE, KUSER_END, true),
];

#[derive(Clone, Copy)]
pub struct GuestMem {
    base: *mut u8,
}

// The reservation lives for the whole process and is only a base address.
unsafe impl Send for GuestMem {}
unsafe impl Sync for GuestMem {}

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
        // Addresses outside the mapped regions fault on the host too, as they
        // do in Unicorn (a NULL dereference in translated code must not read 0).
        let mut start = 0usize;
        for (region_start, region_end, _) in MAPPED_REGIONS {
            if (region_start as usize) > start {
                unsafe { libc::mprotect((base as *mut u8).add(start).cast(), region_start as usize - start, libc::PROT_NONE) };
            }
            start = region_end as usize;
        }
        unsafe { libc::mprotect((base as *mut u8).add(start).cast(), size - start, libc::PROT_NONE) };
        GuestMem { base: base as *mut u8 }
    }

    /// Writes the kuser helpers at the addresses Linux uses. Only one guest
    /// thread runs at a time, so plain loads and stores are atomic. Results
    /// match the kernel's: 0 and carry set on success.
    pub fn write_kernel_helpers(&self) {
        const HELPERS: [(u32, &[u32]); 4] = [
            // __kuser_cmpxchg64(const u64 *old, const u64 *new, u64 *ptr)
            (0xFFFF_0F60, &[
                0xE92D_00F0, // push {r4-r7}
                0xE1C0_40D0, // ldrd r4, r5, [r0]
                0xE1C2_60D0, // ldrd r6, r7, [r2]
                0xE156_0004, // cmp r6, r4
                0x0157_0005, // cmpeq r7, r5
                0x01C1_40D0, // ldrdeq r4, r5, [r1]
                0x01C2_40F0, // strdeq r4, r5, [r2]
                0x03A0_0000, // moveq r0, #0
                0x13A0_0001, // movne r0, #1
                0xE270_0000, // rsbs r0, r0, #0
                0xE8BD_00F0, // pop {r4-r7}
                0xE12F_FF1E, // bx lr
            ]),
            // __kuser_memory_barrier
            (0xFFFF_0FA0, &[0xF57F_F05B, 0xE12F_FF1E]), // dmb ish; bx lr
            // __kuser_cmpxchg(u32 old, u32 new, u32 *ptr)
            (0xFFFF_0FC0, &[
                0xE592_3000, // ldr r3, [r2]
                0xE053_3000, // subs r3, r3, r0
                0x0582_1000, // streq r1, [r2]
                0xE273_0000, // rsbs r0, r3, #0
                0xE12F_FF1E, // bx lr
            ]),
            // __kuser_get_tls
            (0xFFFF_0FE0, &[0xEE1D_0F70, 0xE12F_FF1E]), // mrc p15, 0, r0, c13, c0, 3; bx lr
        ];
        for (address, words) in HELPERS {
            for (i, word) in words.iter().enumerate() {
                self.write_u32(address + i as u32 * 4, *word);
            }
        }
        // __kuser_helper_version: the number of 32-byte helper slots.
        self.write_u32(0xFFFF_0FFC, 5);
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
