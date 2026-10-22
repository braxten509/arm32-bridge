//! Real reference counts for the few objects that hold a lot of memory
//! (pictures: UIImage, CGImage, CGContext).
//!
//! The rest of the layer never frees objects, so retain/release do nothing
//! for them. Objects listed here count every retain and release as Apple's
//! runtime does (messages, ARC calls, CFRetain/CFRelease, autorelease pools)
//! and run their clean-up at zero. Places that keep an object without a
//! retain of their own on Apple's side (property setters, collections,
//! blocks, queued messages) call `keep` so a listed object isn't cleaned up
//! while still held; collections never let go (a picture stays alive rather
//! than vanish under the game).
//!
//! Objects are looked up by address, so any value can be passed in safely.

use crate::objc::runtime::Id;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

struct Entry {
    count: usize,
    clean_up: fn(Id),
}

static LIVE: Mutex<Option<HashMap<Id, Entry>>> = Mutex::new(None);
/// How many objects are listed (lets the common case skip the lock).
static LISTED: AtomicUsize = AtomicUsize::new(0);

fn live() -> std::sync::MutexGuard<'static, Option<HashMap<Id, Entry>>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lists a new object with one reference; `clean_up` runs when the last goes.
pub fn track(object: Id, clean_up: fn(Id)) {
    if object == 0 {
        return;
    }
    if live().get_or_insert_with(HashMap::new).insert(object, Entry { count: 1, clean_up }).is_none() {
        LISTED.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn is_tracked(object: Id) -> bool {
    object != 0 && LISTED.load(Ordering::Relaxed) != 0 && live().as_ref().is_some_and(|m| m.contains_key(&object))
}

/// The reference count of a listed object.
pub fn count(object: Id) -> Option<usize> {
    live().as_ref().and_then(|m| m.get(&object)).map(|e| e.count)
}

/// +1 for a listed object (others: nothing). Returns the object.
pub fn retain(object: Id) -> Id {
    if object != 0 && LISTED.load(Ordering::Relaxed) != 0 {
        if let Some(entry) = live().as_mut().and_then(|m| m.get_mut(&object)) {
            entry.count += 1;
        }
    }
    object
}

/// What a holder that Apple would retain for does here (see the top).
pub fn keep(object: Id) {
    retain(object);
}

/// -1 for a listed object; at zero it leaves the list and is cleaned up.
pub fn release(object: Id) {
    if object == 0 || LISTED.load(Ordering::Relaxed) == 0 {
        return;
    }
    let gone = {
        let mut map = live();
        let Some(m) = map.as_mut() else { return };
        match m.get_mut(&object) {
            Some(entry) if entry.count > 1 => {
                entry.count -= 1;
                None
            }
            Some(_) => {
                LISTED.fetch_sub(1, Ordering::Relaxed);
                m.remove(&object)
            }
            None => None,
        }
    };
    // Outside the lock: cleaning up may release what the object held.
    if let Some(entry) = gone {
        (entry.clean_up)(object);
    }
}

/// Swaps what a strong slot holds: retains the new object, releases the old.
pub fn replace(old: Id, new: Id) {
    if old != new {
        retain(new);
        release(old);
    }
}

// ----- autorelease pools (per thread, as on Apple) -----

thread_local! {
    static POOL: RefCell<Vec<Id>> = const { RefCell::new(Vec::new()) };
    /// NSAutoreleasePool objects open on this thread, with where each began.
    static POOL_OBJECTS: RefCell<Vec<(Id, usize)>> = const { RefCell::new(Vec::new()) };
}

/// Puts a listed object in the innermost pool. Returns the object.
pub fn autorelease(object: Id) -> Id {
    if is_tracked(object) {
        POOL.with(|p| p.borrow_mut().push(object));
    }
    object
}

/// Where a pool opened now begins.
pub fn pool_mark() -> usize {
    POOL.with(|p| p.borrow().len())
}

/// Releases everything put in pools since `mark`, newest first.
pub fn pool_drain(mark: usize) {
    loop {
        // One at a time: a clean-up may autorelease more.
        let next = POOL.with(|p| {
            let mut p = p.borrow_mut();
            if p.len() > mark { p.pop() } else { None }
        });
        match next {
            Some(object) => release(object),
            None => break,
        }
    }
}

/// objc_autoreleasePoolPush's token (never 0).
pub fn pool_push() -> usize {
    pool_mark() + 1
}

pub fn pool_pop(token: usize) {
    pool_drain(token.saturating_sub(1));
}

/// -[NSAutoreleasePool init].
pub fn pool_object_open(pool: Id) {
    let mark = pool_mark();
    POOL_OBJECTS.with(|p| p.borrow_mut().push((pool, mark)));
}

/// -[NSAutoreleasePool drain/release]: drains it and any opened inside it.
pub fn pool_object_close(pool: Id) {
    let mark = POOL_OBJECTS.with(|p| {
        let mut p = p.borrow_mut();
        let at = p.iter().rposition(|&(o, _)| o == pool)?;
        let mark = p[at].1;
        p.truncate(at);
        Some(mark)
    });
    if let Some(mark) = mark {
        pool_drain(mark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static CLEANED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

    fn note(object: Id) {
        CLEANED.lock().unwrap().push(object);
    }

    fn cleaned(object: Id) -> usize {
        CLEANED.lock().unwrap().iter().filter(|&&o| o == object).count()
    }

    #[test]
    fn counts_pools_and_holders_free_once_at_the_right_time() {
        let (image, kept, other) = (0x7000_1000, 0x7000_2000, 0x7000_3000);
        track(image, note);
        // A factory's result: +1 and autoreleased, then retained by its user.
        let mark = pool_mark();
        autorelease(image);
        retain(image);
        pool_drain(mark);
        assert_eq!(count(image), Some(1));
        release(image);
        assert_eq!(cleaned(image), 1);
        release(image); // too many releases do nothing more
        assert_eq!(cleaned(image), 1);
        // Kept by a holder: survives its creator's release and the pool.
        track(kept, note);
        let token = pool_push();
        autorelease(kept);
        keep(kept);
        pool_pop(token);
        assert_eq!((count(kept), cleaned(kept)), (Some(1), 0));
        // Objects that aren't listed are left alone.
        retain(other);
        release(other);
        assert_eq!(count(other), None);
    }

    #[test]
    fn counting_from_many_threads_frees_exactly_once() {
        let object = 0x7000_4000;
        track(object, note);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        retain(object);
                        release(object);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!((count(object), cleaned(object)), (Some(1), 0));
        release(object);
        assert_eq!(cleaned(object), 1);
    }
}
