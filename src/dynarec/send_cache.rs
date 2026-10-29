//! Message caches for translated code: a phone function that sends a message
//! (objc_msgSend) can get a lane (thumb::send_lane) that looks up (the word
//! at r0, r1) — the receiver's class and the selector — in a table and, when
//! it is there, jumps straight to the remembered guest code with every
//! register as the caller left it, as Apple's objc_msgSend does. Anything
//! else goes to the phone function as before. The bridge fills the table
//! with answers that are guest code and empties it whenever they may change.
//!
//! Translated code reads the table without a lock, so a slot is written
//! once: it holds one (class, selector) for the table's whole life. Emptying
//! swaps in a fresh table; a lane already reading the old one finishes with
//! an answer that was right a moment ago (as with Apple's cache flushes).
//! Old tables are kept (a lane may still read them); past MAX_TABLES the
//! cache stays empty, so a game that keeps changing its methods still runs,
//! just without the shortcut.

use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;

pub const BITS: u32 = 12;
const SLOTS: usize = 1 << BITS;
const MAX_TABLES: u32 = 256;

/// (class, selector, guest code) in one 16-byte slot; an empty slot is zeros.
#[repr(C, align(16))]
struct Slot {
    class: AtomicU32,
    sel: AtomicU32,
    target: AtomicU32,
    _pad: u32,
}

struct Table {
    slots: [Slot; SLOTS],
}

fn new_table() -> *mut Table {
    // Zeroed memory is a table of empty slots.
    let layout = std::alloc::Layout::new::<Table>();
    let table = unsafe { std::alloc::alloc_zeroed(layout) } as *mut Table;
    assert!(!table.is_null(), "out of memory for a message cache");
    table
}

/// The slot a (class, selector) goes in; the lane computes the same.
pub fn slot_of(class: u32, sel: u32) -> usize {
    ((class >> 3 ^ sel >> 2) as usize) & (SLOTS - 1)
}

pub struct SendCache {
    /// The live table (the lane loads it from here).
    table: AtomicPtr<Table>,
    /// The live table has an entry (emptying an empty one makes no new table).
    filled: AtomicBool,
    tables: AtomicU32,
    /// Past MAX_TABLES: nothing goes in any more.
    retired: AtomicBool,
    writer: Mutex<()>,
}

impl SendCache {
    /// A new cache, never freed.
    pub fn leaked() -> &'static SendCache {
        Box::leak(Box::new(SendCache {
            table: AtomicPtr::new(new_table()),
            filled: AtomicBool::new(false),
            tables: AtomicU32::new(1),
            retired: AtomicBool::new(false),
            writer: Mutex::new(()),
        }))
    }

    /// Where the lane finds the live table.
    pub(crate) fn table_address(&self) -> usize {
        &self.table as *const AtomicPtr<Table> as usize
    }

    /// Remembers that (class, sel) goes to `target` (bit 0 = Thumb). A slot
    /// already taken (by another pair) keeps its pair.
    pub fn put(&self, class: u32, sel: u32, target: u32) {
        if class == 0 || sel == 0 || target == 0 || self.retired.load(Ordering::Relaxed) {
            return;
        }
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let table = self.table.load(Ordering::Acquire);
        let slot = unsafe { &(*table).slots[slot_of(class, sel)] };
        if slot.class.load(Ordering::Relaxed) != 0 {
            return;
        }
        // The class goes in last: a lane that sees it also sees the rest
        // (or reads a zero target and takes the slow way).
        slot.target.store(target, Ordering::Relaxed);
        slot.sel.store(sel, Ordering::Relaxed);
        slot.class.store(class, Ordering::Release);
        self.filled.store(true, Ordering::Relaxed);
    }

    /// Forgets every pair.
    pub fn clear(&self) {
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if !self.filled.swap(false, Ordering::Relaxed) {
            return;
        }
        if self.tables.fetch_add(1, Ordering::Relaxed) + 1 > MAX_TABLES {
            if !self.retired.swap(true, Ordering::Relaxed) {
                crate::log::line(&format!("[jit] message cache emptied {MAX_TABLES} times: no longer used"));
            }
            // An empty table that nothing goes into again.
            self.table.store(new_table(), Ordering::Release);
            return;
        }
        self.table.store(new_table(), Ordering::Release);
    }

    /// Where (class, sel) goes, if remembered.
    pub fn get(&self, class: u32, sel: u32) -> Option<u32> {
        let table = self.table.load(Ordering::Acquire);
        let slot = unsafe { &(*table).slots[slot_of(class, sel)] };
        (slot.class.load(Ordering::Acquire) == class && slot.sel.load(Ordering::Relaxed) == sel)
            .then(|| slot.target.load(Ordering::Relaxed))
            .filter(|&target| target != 0)
    }

    /// Times it was emptied (for the stats line).
    pub fn times_emptied(&self) -> u32 {
        self.tables.load(Ordering::Relaxed) - 1
    }
}
