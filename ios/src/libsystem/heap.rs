//! The game's malloc zone: dlmalloc on pages from near.rs (so the game's memory
//! sits close together, as on an iPhone), the system allocator when that
//! stretch is full, plus a note of every block handed out, so malloc_size can
//! tell a real block from any other pointer the way Apple's zones do (0 for
//! anything malloc didn't give out, never a crash). Unity, for one, asks
//! malloc_size whether a pointer is a malloc block before freeing it, and
//! passes pointers from its own pools.
//!
//! The note is one bit per 16 bytes of address space, in 32 MiB bitmaps (one
//! per 4 GiB) mapped the first time a block lands there; only touched pages
//! take memory. Marking and clearing are single atomic operations.

use super::near;
use crate::symbols::Registry;
use dlmalloc::Dlmalloc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Every malloc block starts on 16 bytes, on Apple and here.
const QUANTUM_BITS: u32 = 4;
const REGION_BITS: u32 = 32;
/// User addresses fit in 48 bits; the top bits can hold a pointer tag (Android).
const ADDRESS_BITS: u32 = 48;
const REGIONS: usize = 1 << (ADDRESS_BITS - REGION_BITS);
const WORDS_PER_REGION: usize = 1 << (REGION_BITS - QUANTUM_BITS - 6);

static MAPS: [AtomicPtr<AtomicU64>; REGIONS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; REGIONS];

fn place(pointer: usize) -> (usize, usize, u64) {
    let address = pointer & ((1 << ADDRESS_BITS) - 1);
    let bit = (address & ((1 << REGION_BITS) - 1)) >> QUANTUM_BITS;
    (address >> REGION_BITS, bit / 64, 1 << (bit % 64))
}

fn region(index: usize) -> *mut AtomicU64 {
    let map = MAPS[index].load(Ordering::Acquire);
    if !map.is_null() {
        return map;
    }
    let size = WORDS_PER_REGION * 8;
    let fresh = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE, -1, 0)
    };
    if fresh == libc::MAP_FAILED {
        return std::ptr::null_mut();
    }
    match MAPS[index].compare_exchange(std::ptr::null_mut(), fresh as *mut AtomicU64, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh as *mut AtomicU64,
        Err(winner) => {
            unsafe { libc::munmap(fresh, size) };
            winner
        }
    }
}

fn mark(pointer: *mut libc::c_void) {
    if pointer.is_null() {
        return;
    }
    let (index, word, bit) = place(pointer as usize);
    let map = region(index);
    if !map.is_null() {
        unsafe { (*map.add(word)).fetch_or(bit, Ordering::Relaxed) };
    }
}

/// Clears the note before the block goes back, so a block another thread gets at
/// the same address right after keeps its note.
fn unmark(pointer: *mut libc::c_void) -> bool {
    let (index, word, bit) = place(pointer as usize);
    let map = MAPS[index].load(Ordering::Acquire);
    !map.is_null() && unsafe { (*map.add(word)).fetch_and(!bit, Ordering::Relaxed) } & bit != 0
}

fn is_block(pointer: *const libc::c_void) -> bool {
    if pointer.is_null() || pointer as usize & ((1 << QUANTUM_BITS) - 1) != 0 {
        return false;
    }
    let (index, word, bit) = place(pointer as usize);
    let map = MAPS[index].load(Ordering::Acquire);
    !map.is_null() && unsafe { (*map.add(word)).load(Ordering::Relaxed) } & bit != 0
}

/// dlmalloc's pages: whole 16 KiB pages from the near stretch.
struct Pages;

const PAGE: usize = 16384;

unsafe impl dlmalloc::Allocator for Pages {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let Some(size) = size.checked_add(PAGE - 1).map(|size| size & !(PAGE - 1)) else { return (std::ptr::null_mut(), 0, 0) };
        match near::map(size, libc::PROT_READ | libc::PROT_WRITE, PAGE) {
            Some(address) => (address as *mut u8, size, 0),
            None => (std::ptr::null_mut(), 0, 0),
        }
    }
    fn remap(&self, _: *mut u8, _: usize, _: usize, _: bool) -> *mut u8 {
        std::ptr::null_mut()
    }
    fn free_part(&self, pointer: *mut u8, old_size: usize, new_size: usize) -> bool {
        // Only whole pages past the part that stays.
        let start = (pointer as usize + new_size + PAGE - 1) & !(PAGE - 1);
        let end = pointer as usize + old_size;
        end <= start || near::unmap(start, end - start)
    }
    fn free(&self, pointer: *mut u8, size: usize) -> bool {
        near::unmap(pointer as usize, size)
    }
    fn can_release_part(&self, _: u32) -> bool {
        true
    }
    fn allocates_zeros(&self) -> bool {
        true
    }
    fn page_size(&self) -> usize {
        PAGE
    }
}

static NEAR: Mutex<Dlmalloc<Pages>> = Mutex::new(Dlmalloc::new_with_allocator(Pages));

fn near_heap() -> MutexGuard<'static, Dlmalloc<Pages>> {
    NEAR.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Blocks this big get pages of their own (given back whole when freed, already
/// zero for calloc), with a 16-byte header shaped like one of dlmalloc's own
/// mapped chunks: offset 0, then the mapping's length with the in-use bits clear.
const BIG: usize = 256 << 10;

fn big_alloc(size: usize) -> *mut libc::c_void {
    let Some(length) = size.checked_add(16 + PAGE - 1).map(|length| length & !(PAGE - 1)) else { return std::ptr::null_mut() };
    match near::map(length, libc::PROT_READ | libc::PROT_WRITE, PAGE) {
        Some(base) => unsafe {
            *(base as *mut usize) = 0;
            *((base + 8) as *mut usize) = length;
            (base + 16) as *mut libc::c_void
        },
        None => std::ptr::null_mut(),
    }
}

/// Whether a near block has pages of its own (dlmalloc's chunks are always
/// marked in use).
fn is_big(pointer: *const libc::c_void) -> bool {
    pointer as usize % PAGE == 16 && unsafe { *(pointer as *const usize).sub(1) } & 3 == 0
}

fn big_free(pointer: *mut libc::c_void) {
    let length = unsafe { *(pointer as *const usize).sub(1) } & !7;
    near::unmap(pointer as usize - 16, length);
}

/// A new near block: its own pages when big, else from dlmalloc.
fn near_alloc(size: usize) -> *mut libc::c_void {
    if size >= BIG {
        big_alloc(size)
    } else {
        unsafe { near_heap().c_malloc(size) as *mut libc::c_void }
    }
}

fn near_free(pointer: *mut libc::c_void) {
    if is_big(pointer) {
        big_free(pointer);
    } else {
        unsafe { near_heap().c_free(pointer as *mut u8) };
    }
}

/// Whether a block came from the near heap (else from the system's malloc).
fn is_near(pointer: *const libc::c_void) -> bool {
    near::contains(pointer as usize)
}

/// A near block's usable size, from dlmalloc's chunk header just before it.
fn near_size(pointer: *const libc::c_void) -> usize {
    let head = unsafe { *(pointer as *const usize).sub(1) };
    // Big blocks (in-use bits clear) carry 16 bytes of header, dlmalloc's chunks 8.
    (head & !7) - if head & 3 == 0 { 16 } else { 8 }
}

pub extern "C" fn malloc(size: usize) -> *mut libc::c_void {
    let mut block = near_alloc(size);
    if block.is_null() {
        block = unsafe { libc::malloc(size) };
    }
    mark(block);
    block
}

pub extern "C" fn calloc(count: usize, size: usize) -> *mut libc::c_void {
    let Some(total) = count.checked_mul(size) else { return std::ptr::null_mut() };
    let mut block = if total >= BIG { big_alloc(total) } else { unsafe { near_heap().calloc(total, 16) as *mut libc::c_void } };
    if block.is_null() {
        block = unsafe { libc::calloc(count, size) };
    }
    mark(block);
    block
}

/// Like Apple, a size of 0 still gives a (smallest) block back.
pub extern "C" fn realloc(pointer: *mut libc::c_void, size: usize) -> *mut libc::c_void {
    if pointer.is_null() {
        return malloc(size);
    }
    let was_marked = unmark(pointer);
    let block = if is_near(pointer) {
        let size = size.max(1);
        let old_size = near_size(pointer);
        let mut block = if is_big(pointer) {
            // Shrinking a little keeps the pages.
            if size <= old_size && size >= BIG { pointer } else { std::ptr::null_mut() }
        } else if size < BIG {
            unsafe { near_heap().c_realloc(pointer as *mut u8, size) as *mut libc::c_void }
        } else {
            std::ptr::null_mut()
        };
        if block.is_null() {
            // Moves to a new block: another kind of near block, or the system's
            // malloc when the stretch is full.
            block = near_alloc(size);
            if block.is_null() {
                block = unsafe { libc::malloc(size) };
            }
            if !block.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(pointer as *const u8, block as *mut u8, old_size.min(size)) };
                near_free(pointer);
            }
        }
        block
    } else {
        unsafe { libc::realloc(pointer, size.max(1)) }
    };
    if block.is_null() {
        if was_marked {
            mark(pointer);
        }
    } else {
        mark(block);
    }
    block
}

extern "C" fn reallocf(pointer: *mut libc::c_void, size: usize) -> *mut libc::c_void {
    let block = realloc(pointer, size);
    if block.is_null() {
        free(pointer);
    }
    block
}

/// Frees a block from the game's malloc (or the system's); use this, not
/// libc::free, for memory the game hands over.
pub extern "C" fn free(pointer: *mut libc::c_void) {
    if pointer.is_null() {
        return;
    }
    unmark(pointer);
    if is_near(pointer) {
        near_free(pointer);
    } else {
        unsafe { libc::free(pointer) };
    }
}

extern "C" fn posix_memalign(out: *mut *mut libc::c_void, alignment: usize, size: usize) -> i32 {
    if !alignment.is_power_of_two() || alignment < std::mem::size_of::<usize>() {
        return libc::EINVAL;
    }
    let block = unsafe { near_heap().c_memalign(alignment, size) } as *mut libc::c_void;
    if !block.is_null() {
        unsafe { *out = block };
        mark(block);
        return 0;
    }
    let result = unsafe { libc::posix_memalign(out, alignment, size) };
    if result == 0 {
        mark(unsafe { *out });
    }
    result
}

extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut libc::c_void {
    let mut block = std::ptr::null_mut();
    if posix_memalign(&mut block, alignment.max(std::mem::size_of::<usize>()), size) != 0 {
        return std::ptr::null_mut();
    }
    block
}

/// Page-aligned, with Apple's 16 KiB pages.
extern "C" fn valloc(size: usize) -> *mut libc::c_void {
    aligned_alloc(16384, size)
}

/// The usable size of a malloc block, and 0 for any other pointer.
pub extern "C" fn malloc_size(pointer: *const libc::c_void) -> usize {
    if !is_block(pointer) {
        return 0;
    }
    if is_near(pointer) {
        near_size(pointer)
    } else {
        unsafe { libc::malloc_usable_size(pointer as *mut _) }
    }
}

/// The size malloc would really hand out for a request (16-byte steps).
extern "C" fn malloc_good_size(size: usize) -> usize {
    size.max(1).saturating_add(15) & !15
}

pub fn register(r: &mut Registry) {
    r.function("malloc", malloc as usize);
    r.function("calloc", calloc as usize);
    r.function("realloc", realloc as usize);
    r.function("reallocf", reallocf as usize);
    r.function("free", free as usize);
    r.function("posix_memalign", posix_memalign as usize);
    r.function("aligned_alloc", aligned_alloc as usize);
    r.function("valloc", valloc as usize);
    r.function("malloc_size", malloc_size as usize);
    r.function("malloc_usable_size", malloc_size as usize);
    r.function("malloc_good_size", malloc_good_size as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's malloc_size gives 0 for a pointer malloc never handed out, like one
    /// inside another allocator's pool, even if the bytes before it look like a header.
    #[test]
    fn malloc_size_is_zero_inside_a_block() {
        unsafe {
            let pool = malloc(512) as *mut u8;
            libc::memset(pool as *mut _, 0, 512);
            // A made-up 64-byte in-use chunk at pool+48, followed by one more.
            *(pool.add(56) as *mut usize) = 0x41;
            *(pool.add(120) as *mut usize) = 0x21;
            assert_eq!(malloc_size(pool.add(64) as *mut _), 0, "a pointer inside a pool was taken for a malloc block");
            assert_eq!(malloc_size(pool.add(65) as *mut _), 0);
            assert_eq!(malloc_size(std::ptr::null_mut()), 0);
            assert!(malloc_size(pool as *mut _) >= 512);
            free(pool as *mut _);
        }
    }

    #[test]
    fn malloc_size_follows_every_way_to_get_a_block() {
        let mut aligned = std::ptr::null_mut();
        assert_eq!(posix_memalign(&mut aligned, 64, 100), 0);
        assert!(malloc_size(aligned) >= 100);
        let grown = realloc(calloc(4, 8), 5000);
        assert!(malloc_size(grown) >= 5000);
        let smallest = realloc(std::ptr::null_mut(), 0);
        assert!(malloc_size(smallest) > 0, "realloc(NULL, 0) gave no block");
        // Big blocks: zeroed by calloc, given back whole, and moved by realloc.
        let big = calloc(1, 3 << 20) as *mut u8;
        assert!(malloc_size(big as *mut _) >= 3 << 20);
        assert!(unsafe { std::slice::from_raw_parts(big, 3 << 20) }.iter().all(|&byte| byte == 0));
        unsafe { *big.add(5) = 9 };
        let small = realloc(big as *mut _, 100) as *mut u8;
        assert_eq!(unsafe { *small.add(5) }, 9);
        assert!(malloc_size(small as *mut _) >= 100);
        let regrown = realloc(small as *mut _, 2 << 20) as *mut u8;
        assert_eq!(unsafe { *regrown.add(5) }, 9);
        free(regrown as *mut _);
        let page = valloc(10);
        assert_eq!(page as usize % 16384, 0);
        assert!(malloc_size(page) >= 10);
        let stack = 0u128;
        assert_eq!(malloc_size(&stack as *const u128 as *const _), 0, "a stack address counted as a block");
        for block in [aligned, grown, smallest, page] {
            free(block);
        }
        assert_eq!(malloc_good_size(1), 16);
        assert_eq!(malloc_good_size(33), 48);
    }

    /// Unity keeps a table of the address ranges its memory comes from and gives
    /// up when they span more than 16 GiB; an iPhone keeps a game's memory close
    /// together, Android's allocator spreads it far apart. Blocks of every size,
    /// from several threads, plus pages from mmap and vm_allocate, must all sit
    /// in one stretch of addresses.
    #[test]
    fn game_memory_stays_close_together() {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    [16usize, 1000, 100_000, 1 << 20, 64 << 20].iter().map(|&size| malloc(size) as usize).collect::<Vec<_>>()
                })
            })
            .collect();
        let mut addresses: Vec<usize> = threads.into_iter().flat_map(|thread| thread.join().unwrap()).collect();
        addresses.push(crate::libsystem::io::ios_mmap(0, 1 << 20, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | 0x1000, -1, 0));
        let mut page = 0usize;
        assert_eq!(crate::libsystem::mach::vm_allocate(0, &mut page, 1 << 20, 1), 0);
        addresses.push(page);
        // Android's allocator puts a tag in the top byte; the address is below it.
        let addresses: Vec<usize> = addresses.iter().map(|address| address & ((1 << 56) - 1)).collect();
        let (low, high) = (addresses.iter().min().unwrap(), addresses.iter().max().unwrap());
        assert!(high - low < 12 << 30, "game memory spans {:#x}..{:#x}", low, high);
        // A block from the system allocator can still be handed to the game's free.
        let foreign = unsafe { libc::malloc(64) };
        assert_eq!(malloc_size(foreign), 0);
        free(foreign);
    }
}
