//! The game's malloc zone: the system allocator, plus a note of every block it
//! handed out, so malloc_size can tell a real block from any other pointer the
//! way Apple's zones do (0 for anything malloc didn't give out, never a crash).
//! Unity, for one, asks malloc_size whether a pointer is a malloc block before
//! freeing it, and passes pointers from its own pools.
//!
//! The note is one bit per 16 bytes of address space, in 32 MiB bitmaps (one
//! per 4 GiB) mapped the first time a block lands there; only touched pages
//! take memory. Marking and clearing are single atomic operations.

use crate::symbols::Registry;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

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

pub extern "C" fn malloc(size: usize) -> *mut libc::c_void {
    let block = unsafe { libc::malloc(size) };
    mark(block);
    block
}

pub extern "C" fn calloc(count: usize, size: usize) -> *mut libc::c_void {
    let block = unsafe { libc::calloc(count, size) };
    mark(block);
    block
}

/// Like Apple, a size of 0 still gives a (smallest) block back.
pub extern "C" fn realloc(pointer: *mut libc::c_void, size: usize) -> *mut libc::c_void {
    if pointer.is_null() {
        return malloc(size);
    }
    let was_marked = unmark(pointer);
    let block = unsafe { libc::realloc(pointer, size.max(1)) };
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
    unsafe { libc::free(pointer) };
}

extern "C" fn posix_memalign(out: *mut *mut libc::c_void, alignment: usize, size: usize) -> i32 {
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
    unsafe { libc::malloc_usable_size(pointer as *mut _) }
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
}
