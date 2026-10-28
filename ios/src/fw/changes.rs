//! A change counter for the phone-screen pictures: anything that could alter
//! what `pictures::tick` draws marks a change, and the tick rebuilds the list
//! when the counter has moved since the last list it committed (Apple's
//! CA::Layer::mark, roughly). The changed object also joins a DIRTY set so the
//! tick can rebuild just the touched part of the picture tree (A2).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The counter (bumped whenever something that could change the pictures
/// happens) and, in the app, the DIRTY set.
static CHANGES: Changes = Changes::new();

#[cfg(test)]
thread_local! {
    /// Each thread's own counter (tests): bumped alongside the global one so
    /// one test's marks don't break another test's "nothing changed" checks.
    static LOCAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The changed objects on this thread (tests), for the DIRTY rebuild set.
    static DIRTY: std::cell::RefCell<Dirty> = const { std::cell::RefCell::new(Dirty { objects: Vec::new(), overflowed: false }) };
}

/// The change counter and the DIRTY set it goes with.
struct Changes {
    generation: AtomicU64,
    dirty: Mutex<Dirty>,
}

impl Changes {
    const fn new() -> Changes {
        Changes { generation: AtomicU64::new(0), dirty: Mutex::new(Dirty { objects: Vec::new(), overflowed: false }) }
    }

    /// Marks `object` (`between` runs between the counter bump and the push:
    /// a pause there, for tests). Both happen under the set's lock: a commit
    /// reads the counter and then takes the set, so one that read this bump
    /// takes the set after the lock is let go, with the mark in it (bumped
    /// outside the lock, a thread paused before the push left the set short
    /// while the counter already matched, and the stale node stayed).
    fn mark(&self, object: Id, between: impl FnOnce()) {
        let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::Relaxed);
        between();
        dirty.push(object);
    }

    fn take(&self) -> Option<Vec<Id>> {
        self.dirty.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// The objects marked since the last commit took them. Past MOST_DIRTY
/// marks it stops listing and only says so (the commit rebuilds everything).
struct Dirty {
    objects: Vec<Id>,
    overflowed: bool,
}

const MOST_DIRTY: usize = 8192;

impl Dirty {
    fn push(&mut self, object: Id) {
        if self.objects.len() < MOST_DIRTY {
            self.objects.push(object);
        } else {
            self.overflowed = true;
        }
    }

    fn take(&mut self) -> Option<Vec<Id>> {
        let objects = std::mem::take(&mut self.objects);
        (!std::mem::replace(&mut self.overflowed, false)).then_some(objects)
    }
}

use crate::objc::runtime::Id;

/// Mark that `object` changed: it joins the DIRTY set so `pictures` can
/// rebuild just its part of the tree.
pub fn changed(object: Id) {
    #[cfg(test)]
    {
        CHANGES.generation.fetch_add(1, Ordering::Relaxed);
        LOCAL.with(|g| g.set(g.get() + 1));
        DIRTY.with(|d| d.borrow_mut().push(object));
    }
    #[cfg(not(test))]
    {
        CHANGES.mark(object, || {});
        wake_if_elsewhere();
    }
}

/// A mark from another thread (a picture file finished) wakes the main loop,
/// which waits up to a quarter second on a still screen (pictures::gap).
#[cfg(not(test))]
fn wake_if_elsewhere() {
    if !crate::fw::runloop::is_main_thread() {
        ELSEWHERE.fetch_add(1, Ordering::Relaxed);
        crate::fw::runloop::wake_main();
    }
}

/// Marks made on other threads.
static ELSEWHERE: AtomicU64 = AtomicU64::new(0);

/// How many marks other threads have made: one between the main loop's
/// look at the pictures and its wait keeps it from waiting.
pub fn elsewhere() -> u64 {
    ELSEWHERE.load(Ordering::Relaxed)
}

/// Bump the counter without marking any object: a change that only needs the
/// list made again (a picture file finished on its own thread, a scroll
/// step, a new GL frame), not any part of the picture tree rebuilt.
pub fn touch() {
    CHANGES.generation.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    {
        LOCAL.with(|g| g.set(g.get() + 1));
    }
    #[cfg(not(test))]
    wake_if_elsewhere();
}

/// The global counter's value now: marks from any thread (a picture file's
/// own thread, say) count.
pub fn generation() -> u64 {
    CHANGES.generation.load(Ordering::Relaxed)
}

/// The counter the skip key compares: the global one in the app (any thread's
/// mark counts); in tests this thread's, so one test's marks don't break
/// another test's "nothing changed" check.
pub fn commit_generation() -> u64 {
    #[cfg(test)]
    {
        LOCAL.with(|g| g.get())
    }
    #[cfg(not(test))]
    {
        CHANGES.generation.load(Ordering::Relaxed)
    }
}

/// Takes the DIRTY set: the objects changed since the last commit (None:
/// too many to list, everything counts as changed).
pub fn take_dirty() -> Option<Vec<Id>> {
    #[cfg(test)]
    {
        DIRTY.with(|d| d.borrow_mut().take())
    }
    #[cfg(not(test))]
    {
        CHANGES.take()
    }
}

/// The first `most` objects in the DIRTY set, left in it (for a log line).
pub fn peek_dirty(most: usize) -> Vec<Id> {
    #[cfg(test)]
    {
        DIRTY.with(|d| d.borrow().objects.iter().take(most).copied().collect())
    }
    #[cfg(not(test))]
    {
        CHANGES.dirty.lock().unwrap_or_else(|e| e.into_inner()).objects.iter().take(most).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A commit reads the counter, then takes the DIRTY set (pictures::tick).
    /// A mark on another thread that has bumped the counter must already be
    /// in the set by the take, or the commit reuses a stale node and later
    /// ticks skip because the counter matches.
    #[test]
    fn a_counted_mark_is_in_the_set_the_commit_takes() {
        let changes = &Changes::new();
        let (bumped, on_bump) = mpsc::channel();
        let (took, on_take) = mpsc::channel();
        let taken = std::thread::scope(|s| {
            s.spawn(move || {
                changes.mark(0x10, || {
                    bumped.send(()).unwrap();
                    // Paused until the commit has taken the set (or, when the
                    // take has to wait for this mark, a moment).
                    let _ = on_take.recv_timeout(Duration::from_millis(300));
                })
            });
            on_bump.recv().unwrap();
            let generation = changes.generation.load(Ordering::Relaxed);
            let taken = changes.take();
            took.send(()).ok();
            (generation, taken)
        });
        assert_eq!(taken, (1, Some(vec![0x10])));
    }
}
