//! One stretch of addresses for the game's memory, the way an iPhone keeps a
//! game's heap close together. Android's allocator spreads blocks over hundreds
//! of GiB, and Unity keeps a small table of the 4 GiB ranges its memory comes
//! from and gives up past 16 GiB. So malloc (through dlmalloc, in heap.rs),
//! anonymous mmap and vm_allocate all take pages from here.
//!
//! The stretch is reserved once (no memory until pages are used) and starts on
//! a 4 GiB line, so it covers as few of those ranges as it can. Free pages are
//! kept as (start, length) runs; pages given back are reserved again, which
//! returns their memory to the system. When the stretch is used up, callers
//! fall back to the system.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const SIZE: usize = 12 << 30;
const LINE: usize = 4 << 30;

struct Area {
    start: usize,
    page: usize,
    /// Free runs by start address; neighbours are always joined.
    free: Mutex<BTreeMap<usize, usize>>,
}

fn area() -> Option<&'static Area> {
    static AREA: OnceLock<Option<Area>> = OnceLock::new();
    AREA.get_or_init(|| {
        let raw = reserve(0, SIZE + LINE, false)?;
        let start = (raw + LINE - 1) & !(LINE - 1);
        unsafe {
            if start > raw {
                libc::munmap(raw as *mut _, start - raw);
            }
            libc::munmap((start + SIZE) as *mut _, raw + LINE - start);
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as usize;
        Some(Area { start, page, free: Mutex::new(BTreeMap::from([(start, SIZE)])) })
    })
    .as_ref()
}

/// Maps `length` bytes of inaccessible, uncommitted pages (at `address` exactly
/// when it isn't 0).
fn reserve(address: usize, length: usize, fixed: bool) -> Option<usize> {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | if fixed { libc::MAP_FIXED } else { 0 };
    let raw = unsafe { libc::mmap(address as *mut _, length, libc::PROT_NONE, flags, -1, 0) };
    (raw != libc::MAP_FAILED).then_some(raw as usize)
}

/// The free runs, with every signal held back while they are locked: a thread
/// that Mono's collector stops (thread_suspend uses a signal) must never sit on
/// this lock while the collector maps pages itself.
fn lock_free(area: &Area) -> (Quiet, MutexGuard<'_, BTreeMap<usize, usize>>) {
    let quiet = Quiet::new();
    (quiet, area.free.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Signals held back until dropped (bound before the lock guard, so dropped after it).
struct Quiet(libc::sigset_t);

impl Quiet {
    fn new() -> Quiet {
        unsafe {
            let mut all: libc::sigset_t = std::mem::zeroed();
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::sigfillset(&mut all);
            libc::pthread_sigmask(libc::SIG_BLOCK, &all, &mut old);
            Quiet(old)
        }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
    }
}

/// Whether `address` lies in the stretch.
pub fn contains(address: usize) -> bool {
    area().is_some_and(|area| (area.start..area.start + SIZE).contains(&address))
}

/// Fresh zeroed pages with `protection`, starting on `align` (a power of two;
/// at least the system page), or None when the stretch has no room.
pub fn map(length: usize, protection: i32, align: usize) -> Option<usize> {
    let area = area()?;
    let align = align.max(area.page);
    let length = length.checked_add(area.page - 1)? & !(area.page - 1);
    if length == 0 || length > SIZE {
        return None;
    }
    let address = {
        let (_quiet, mut free) = lock_free(area);
        let Some((run, run_length, address)) = free.iter().find_map(|(&run, &run_length)| {
            let address = (run + align - 1) & !(align - 1);
            (address + length <= run + run_length).then_some((run, run_length, address))
        }) else {
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                crate::log!("near memory: no room for {length} more bytes in the {} GiB stretch; using the system's", SIZE >> 30);
            }
            return None;
        };
        free.remove(&run);
        if address > run {
            free.insert(run, address - run);
        }
        if address + length < run + run_length {
            free.insert(address + length, run + run_length - address - length);
        }
        address
    };
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED;
    if unsafe { libc::mmap(address as *mut _, length, protection, flags, -1, 0) } == libc::MAP_FAILED {
        join(&mut lock_free(area).1, address, length);
        return None;
    }
    Some(address)
}

/// Returns pages to the stretch (whole system pages inside it only), dropping
/// their memory. Returns false when `address` isn't in the stretch.
pub fn unmap(address: usize, length: usize) -> bool {
    let Some(area) = area() else { return false };
    if !contains(address) {
        return false;
    }
    let start = address & !(area.page - 1);
    let end = (address.saturating_add(length).saturating_add(area.page - 1) & !(area.page - 1)).min(area.start + SIZE);
    if end > start && reserve(start, end - start, true).is_some() {
        join(&mut lock_free(area).1, start, end - start);
    }
    true
}

/// Adds a run of free pages, joined with any runs it touches or overlaps.
fn join(free: &mut BTreeMap<usize, usize>, mut start: usize, mut length: usize) {
    if let Some((&before, &before_length)) = free.range(..=start).next_back() {
        if before + before_length >= start {
            // Joins the run before (or lies inside it already: pages freed twice).
            let end = (start + length).max(before + before_length);
            start = before;
            length = end - before;
        }
    }
    // Swallows every run that starts inside or right after this one.
    while let Some((&after, &after_length)) = free.range(start + 1..=start + length).next() {
        free.remove(&after);
        length = length.max(after + after_length - start);
    }
    free.insert(start, length);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_come_back_and_join() {
        let a = map(100_000, libc::PROT_READ | libc::PROT_WRITE, 16384).unwrap();
        assert_eq!(a % 16384, 0);
        unsafe { *(a as *mut u8) = 7 };
        let b = map(4096, libc::PROT_READ | libc::PROT_WRITE, 0).unwrap();
        assert!(contains(a) && contains(b) && !contains(0));
        assert!(unmap(a, 100_000) && unmap(b, 4096));
        let again = map(100_000, libc::PROT_READ, 16384).unwrap();
        assert_eq!(unsafe { *(again as *const u8) }, 0, "reused pages were not cleared");
        unmap(again, 100_000);
        // A length near the top of the address space is refused, not wrapped.
        assert_eq!(map(usize::MAX - 8192, libc::PROT_READ, 0), None);
    }

    /// Pages given back twice, or inside or across free runs, keep every free
    /// page listed once, in joined runs.
    #[test]
    fn free_runs_join_and_never_lose_pages() {
        let mut free = BTreeMap::from([(0x10000, 0x10000), (0x40000, 0x10000)]);
        join(&mut free, 0x10000, 0x1000);
        join(&mut free, 0x18000, 0x1000);
        assert_eq!(free, BTreeMap::from([(0x10000, 0x10000), (0x40000, 0x10000)]));
        join(&mut free, 0x20000, 0x20000);
        assert_eq!(free, BTreeMap::from([(0x10000, 0x40000)]));
        join(&mut free, 0x8000, 0x60000);
        assert_eq!(free, BTreeMap::from([(0x8000, 0x60000)]));
    }
}
