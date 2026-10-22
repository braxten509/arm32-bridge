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

/// The Android regions as (start, end, may hold code). Everything else
/// faults when touched. Writes to code regions pay for a self-modifying-code
/// check in Unicorn (see vendor/unicorn-engine-sys); the heap needs it for
/// code games make while running.
pub const MAPPED_REGIONS: [(u32, u32, bool); 6] = [
    (STUB_BASE, STUB_BASE + STUB_SIZE, true),
    (LIB_BASE, LIB_END, true),
    // Games with a JIT (Mono, ...) run code they write into heap blocks.
    (HEAP_BASE, HEAP_END, true),
    (MISC_BASE, MISC_END, false),
    (STACK_BASE, STACK_TOP, false),
    (KUSER_BASE, KUSER_END, true),
];

/// Where the guest address space is mapped and where the phone-function
/// stand-ins and return traps live. Mapping (host and Unicorn), the
/// "may code run here" check, the return traps and stub dispatch all read
/// this one value. Android programs use `MemoryLayout::android()` (the
/// constants above, unchanged); a program that must sit at fixed low
/// addresses (32-bit iPhone programs) gets its own regions and a stub area
/// out of its way. The heap, runtime data and stack keep their Android
/// places in every layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryLayout {
    /// (start, end, may hold code), page-aligned, sorted, not overlapping.
    pub regions: Vec<(u32, u32, bool)>,
    /// Return traps (8 bytes per nesting depth) then stand-ins (8 bytes each).
    pub stub_base: u32,
    pub stub_size: u32,
}

impl MemoryLayout {
    pub fn android() -> MemoryLayout {
        MemoryLayout { regions: MAPPED_REGIONS.to_vec(), stub_base: STUB_BASE, stub_size: STUB_SIZE }
    }

    /// The regions every layout needs besides its program: heap, runtime
    /// data and stack, plus a code region for the stub area at `stub_base`.
    pub fn with_program(program: &[(u32, u32, bool)], stub_base: u32, stub_size: u32) -> Result<MemoryLayout, String> {
        let mut regions = program.to_vec();
        regions.extend([
            (HEAP_BASE, HEAP_END, true),
            (MISC_BASE, MISC_END, false),
            (stub_base, stub_base.checked_add(stub_size).ok_or("stub area overflows")?, true),
            (STACK_BASE, STACK_TOP, false),
        ]);
        regions.sort_unstable();
        let layout = MemoryLayout { regions, stub_base, stub_size };
        layout.validate()?;
        Ok(layout)
    }

    /// Page alignment, order, no overlaps, and the stub area inside one code region.
    pub fn validate(&self) -> Result<(), String> {
        let mut previous_end = 0u64;
        for &(start, end, _) in &self.regions {
            if start % PAGE != 0 || end % PAGE != 0 || start >= end {
                return Err(format!("region {start:#x}..{end:#x} is empty or not page-aligned"));
            }
            if u64::from(start) < previous_end {
                return Err(format!("region {start:#x}..{end:#x} overlaps or is out of order"));
            }
            previous_end = u64::from(end);
        }
        if self.stub_size == 0 || self.stub_size % PAGE != 0 {
            return Err(format!("stub area size {:#x} is not whole pages", self.stub_size));
        }
        let stub_end = u64::from(self.stub_base) + u64::from(self.stub_size);
        let inside = self
            .regions
            .iter()
            .any(|&(start, end, code)| code && start <= self.stub_base && stub_end <= u64::from(end));
        if !inside {
            return Err(format!("stub area {:#x}..{stub_end:#x} is not inside one code region", self.stub_base));
        }
        Ok(())
    }

    pub fn executable(&self, address: u32) -> bool {
        self.regions.iter().any(|&(start, end, code)| code && (start..end).contains(&address))
    }

    pub fn in_stub_area(&self, address: u32) -> bool {
        address.wrapping_sub(self.stub_base) < self.stub_size
    }

    /// The end of the region holding `address`.
    pub fn region_end(&self, address: u32) -> Option<u32> {
        self.regions.iter().find(|&&(start, end, _)| (start..end).contains(&address)).map(|&(_, end, _)| end)
    }

    /// Whether every byte of [start, end) is mapped (it may cross from one
    /// region into the next when they touch).
    pub fn mapped(&self, start: u32, end: u64) -> bool {
        let mut at = u64::from(start);
        while at < end {
            match self.region_end(at as u32) {
                Some(region_end) => at = u64::from(region_end),
                None => return false,
            }
        }
        true
    }

    pub fn has_kernel_helpers(&self) -> bool {
        self.regions.iter().any(|&(start, end, _)| start <= KUSER_BASE && KUSER_END <= end)
    }
}

static LAYOUT: std::sync::OnceLock<MemoryLayout> = std::sync::OnceLock::new();
/// Set once a layout other than Android's is chosen. While it is clear (every
/// Android run), the hot checks below use the constants, exactly as before
/// layouts existed; only other layouts walk `layout()`.
static CUSTOM_LAYOUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Picks the layout for this process. Must come before guest memory is
/// reserved (runtime::init); afterwards only the same layout is accepted.
pub fn set_layout(layout: MemoryLayout) -> Result<(), String> {
    layout.validate()?;
    let custom = layout != MemoryLayout::android();
    let current = LAYOUT.get_or_init(|| layout.clone());
    if *current != layout {
        return Err("guest memory layout was already chosen".into());
    }
    if custom {
        CUSTOM_LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// This process's layout (Android's unless `set_layout` chose another first).
pub fn layout() -> &'static MemoryLayout {
    LAYOUT.get_or_init(MemoryLayout::android)
}

/// Whether this process uses a layout other than Android's.
#[inline]
pub fn custom_layout() -> bool {
    CUSTOM_LAYOUT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `address` is in the Android layout's code regions (constant data).
#[inline]
pub fn android_executable(address: u32) -> bool {
    MAPPED_REGIONS.iter().any(|&(start, end, code)| code && (start..end).contains(&address))
}

#[inline]
pub fn in_stub_area(address: u32) -> bool {
    if custom_layout() {
        layout().in_stub_area(address)
    } else {
        address.wrapping_sub(STUB_BASE) < STUB_SIZE
    }
}

/// Where the return traps and stand-ins start in this process's layout.
#[inline]
pub fn stub_base() -> u32 {
    if custom_layout() {
        layout().stub_base
    } else {
        STUB_BASE
    }
}

/// Read-only segments of loaded libraries, as (start, end). Their bytes never
/// change, so translated code may use them as constants (literal pools) and
/// skip watching them for new code. Writable library parts (.data, .bss) are
/// not here: a game may build code there.
static FIXED: std::sync::RwLock<Vec<(u32, u32)>> = std::sync::RwLock::new(Vec::new());

pub fn add_fixed(start: u32, end: u32) {
    FIXED.write().unwrap().push((start, end));
}

/// The game made [start, end) writable (mprotect): its bytes may change from
/// now on, so that part of any read-only library segment stops being fixed.
/// Returns whether any of it was fixed.
pub fn unfix(start: u32, end: u32) -> bool {
    let mut fixed = FIXED.write().unwrap();
    let mut changed = false;
    let mut kept = Vec::with_capacity(fixed.len() + 1);
    for &(first, last) in fixed.iter() {
        if last <= start || first >= end {
            kept.push((first, last));
            continue;
        }
        changed = true;
        if first < start {
            kept.push((first, start));
        }
        if last > end {
            kept.push((end, last));
        }
    }
    *fixed = kept;
    changed
}

/// Whether all of [start, end) lies in one read-only library segment.
pub fn fixed_range(start: u32, end: u32) -> bool {
    FIXED.read().unwrap().iter().any(|&(first, last)| first <= start && end <= last)
}

/// Whether `address` is in a read-only library segment (see `FIXED`).
pub fn fixed(address: u32) -> bool {
    FIXED.read().unwrap().iter().any(|&(start, end)| (start..end).contains(&address))
}

/// Whether guest code may run at `address` (a region mapped as code).
#[inline]
pub fn executable(address: u32) -> bool {
    if custom_layout() {
        layout().executable(address)
    } else {
        android_executable(address)
    }
}

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
        for &(region_start, region_end, _) in &layout().regions {
            if (region_start as usize) > start {
                unsafe { libc::mprotect((base as *mut u8).add(start).cast(), region_start as usize - start, libc::PROT_NONE) };
            }
            start = region_end as usize;
        }
        unsafe { libc::mprotect((base as *mut u8).add(start).cast(), size - start, libc::PROT_NONE) };
        GuestMem { base: base as *mut u8 }
    }

    /// Writes the kuser helpers at the addresses Linux uses, as the kernel
    /// writes them for multi-core ARMv7 (guest threads run at the same time):
    /// ldrex/strex loops with barriers. Results match the kernel's: 0 and
    /// carry set on success.
    pub fn write_kernel_helpers(&self) {
        if !layout().has_kernel_helpers() {
            return; // not a Linux layout: the page isn't mapped
        }
        const HELPERS: [(u32, &[u32]); 4] = [
            // __kuser_cmpxchg64(const u64 *old, const u64 *new, u64 *ptr)
            (0xFFFF_0F60, &[
                0xE92D_00F0, // push {r4-r7}
                0xE1C0_40D0, // ldrd r4, r5, [r0]
                0xE1C1_60D0, // ldrd r6, r7, [r1]
                0xF57F_F05B, // dmb ish
                0xE1B2_0F9F, // 1: ldrexd r0, r1, [r2]
                0xE030_3004, // eors r3, r0, r4
                0x0031_3005, // eorseq r3, r1, r5
                0x01A2_3F96, // strexdeq r3, r6, r7, [r2]
                0x0333_0001, // teqeq r3, #1
                0x0AFF_FFF9, // beq 1b
                0xF57F_F05B, // dmb ish
                0xE273_0000, // rsbs r0, r3, #0
                0xE8BD_00F0, // pop {r4-r7}
                0xE12F_FF1E, // bx lr
            ]),
            // __kuser_memory_barrier
            (0xFFFF_0FA0, &[0xF57F_F05B, 0xE12F_FF1E]), // dmb ish; bx lr
            // __kuser_cmpxchg(u32 old, u32 new, u32 *ptr)
            (0xFFFF_0FC0, &[
                0xF57F_F05B, // dmb ish
                0xE192_3F9F, // 1: ldrex r3, [r2]
                0xE053_3000, // subs r3, r3, r0
                0x0182_3F91, // strexeq r3, r1, [r2]
                0x0333_0001, // teqeq r3, #1
                0x0AFF_FFFA, // beq 1b
                0xE273_0000, // rsbs r0, r3, #0
                0xEAFF_FFEF, // b __kuser_memory_barrier
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

    /// The guest word at `guest` (4-byte aligned) as an atomic, for words several
    /// threads change at once (mutexes, semaphores, futexes).
    pub fn atomic_u32(&self, guest: u32) -> &'static std::sync::atomic::AtomicU32 {
        assert!(guest % 4 == 0, "unaligned guest synchronization word at {guest:#x}");
        unsafe { &*(self.host(guest) as *const std::sync::atomic::AtomicU32) }
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
    /// Why `guest` can't be a live block from this heap (freeing it would corrupt
    /// the heap), or None when it looks like one. Reads dlmalloc's chunk header:
    /// the word before the block holds its size and in-use flags.
    pub fn not_owned(&self, guest: u32) -> Option<&'static str> {
        let word = std::mem::size_of::<usize>();
        let host = self.mem.host(guest) as usize;
        let top = self.inner.allocator().next.get();
        if host < self.mem.host(HEAP_BASE) as usize + 2 * word || host >= top {
            return Some("not from the heap");
        }
        if host % (2 * word) != 0 {
            return Some("not the start of a block");
        }
        let head = unsafe { *((host - word) as *const usize) };
        let size = head & !7;
        if head & 2 == 0 {
            return Some("already freed (or not the start of a block)");
        }
        if size < 4 * word || size % (2 * word) != 0 || host - 2 * word + size > top {
            return Some("its size field is damaged");
        }
        let next_head = unsafe { *((host - word + size) as *const usize) };
        if next_head & 1 == 0 {
            return Some("the next block's header is damaged (written past the end?)");
        }
        None
    }
    /// Bytes usable in a live block (at least what was asked for), or 0 for a
    /// pointer that isn't one. An in-use chunk's size word, minus its own word.
    pub fn usable_size(&self, guest: u32) -> u32 {
        if guest == 0 || self.not_owned(guest).is_some() {
            return 0;
        }
        let word = std::mem::size_of::<usize>();
        let head = unsafe { *((self.mem.host(guest) as usize - word) as *const usize) };
        ((head & !7) - word) as u32
    }
    /// Bytes of guest memory the heap has taken so far (used and free blocks).
    pub fn arena_size(&self) -> u32 {
        (self.inner.allocator().next.get() - self.mem.host(HEAP_BASE) as usize) as u32
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
