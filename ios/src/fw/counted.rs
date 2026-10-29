//! Real reference counts for the few objects that hold a lot of memory
//! (pictures: UIImage, CGImage, CGContext) or must stop when let go
//! (AVAudioPlayer: Apple's -dealloc stops its sound).
//!
//! The rest of the layer never frees objects, so retain/release do nothing
//! for them. Objects listed here count every retain and release as Apple's
//! runtime does (messages, ARC calls, CFRetain/CFRelease, autorelease pools)
//! and run their clean-up at zero. Places that keep an object without a
//! retain of their own on Apple's side (property setters, collections,
//! blocks, queued messages) call `keep` so a listed object isn't cleaned up
//! while still held; collections never let go of a picture (it stays alive
//! rather than vanish under the game), only of a 32-bit game's objects
//! (below).
//!
//! Only objects (or nil) may be passed in: whether one is counted is read
//! from its class pointer without any lock, so the many retains and
//! releases of every other object cost two loads and a compare or two.

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

/// The classes whose objects count (exact classes: only our code makes them).
static CLASSES: [AtomicUsize; 12] = [const { AtomicUsize::new(0) }; 12];

fn live() -> std::sync::MutexGuard<'static, Option<HashMap<Id, Entry>>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Objects of `class` count references from now on (see `track`).
pub fn count_class(class: usize) {
    for slot in &CLASSES {
        match slot.compare_exchange(0, class, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(existing) if existing == class => return,
            Err(_) => {}
        }
    }
    panic!("counted: too many counted classes");
}

/// Whether `object` is of a counted class, without any lock.
#[inline]
fn counted_class(object: Id) -> bool {
    // Not nil, a plain aligned object pointer (not a tagged one).
    if object == 0 || object & 7 != 0 || (object as isize) < 0 {
        return false;
    }
    let isa = unsafe { *(object as *const usize) };
    CLASSES.iter().any(|c| {
        let c = c.load(Ordering::Relaxed);
        c != 0 && c == isa
    })
}

/// Lists a new object of a counted class with one reference; `clean_up`
/// runs when the last goes.
pub fn track(object: Id, clean_up: fn(Id)) {
    if object == 0 {
        return;
    }
    live().get_or_insert_with(HashMap::new).insert(object, Entry { count: 1, clean_up });
}

pub fn is_tracked(object: Id) -> bool {
    counted_class(object) && live().as_ref().is_some_and(|m| m.contains_key(&object))
}

/// The reference count of a listed object.
pub fn count(object: Id) -> Option<usize> {
    live().as_ref().and_then(|m| m.get(&object)).map(|e| e.count)
}

/// +1 for a listed object or a game object (others: nothing). Returns the object.
pub fn retain(object: Id) -> Id {
    if counted_class(object) {
        if let Some(entry) = live().as_mut().and_then(|m| m.get_mut(&object)) {
            entry.count += 1;
        }
    } else {
        game_retain(object);
    }
    object
}

/// Called by `keep` for every kept object (see `set_keep_hook`).
static KEEP_HOOK: std::sync::OnceLock<fn(Id)> = std::sync::OnceLock::new();

/// Lets the 32-bit bridge hear of every kept object: a game block it lent
/// the host for one call must outlive that call once something keeps it.
pub fn set_keep_hook(hook: fn(Id)) {
    let _ = KEEP_HOOK.set(hook);
}

/// What a holder that Apple would retain for does here (see the top).
pub fn keep(object: Id) {
    retain(object);
    if let Some(hook) = KEEP_HOOK.get() {
        hook(object);
    }
}

/// -1 for a listed object; at zero it leaves the list and is cleaned up.
/// A game object's -dealloc waits for the next safe point (see below).
pub fn release(object: Id) {
    if !counted_class(object) {
        return game_release(object);
    }
    let gone = {
        let mut map = live();
        let Some(m) = map.as_mut() else { return };
        match m.get_mut(&object) {
            Some(entry) if entry.count > 1 => {
                entry.count -= 1;
                None
            }
            Some(_) => m.remove(&object),
            None => None,
        }
    };
    // Outside the lock: cleaning up may release what the object held.
    if let Some(entry) = gone {
        clear_weak(object);
        (entry.clean_up)(object);
    }
}

// ----- a 32-bit game's objects: counted, and sent -dealloc at zero -----
//
// As Apple's runtime, a game object's -dealloc runs when its count reaches
// zero. The bridge lists each game object it makes (one reference: its
// alloc); NSObject's -retain/-release/-autorelease count it, and so do our
// holders that Apple's frameworks retain for (superviews, collections,
// controllers: `retain`/`release`/`keep` above). At zero it waits on its
// thread's list for the next safe point, where no host lock is held: when a
// host method the game called returns (so the game's own last -release and
// a holder letting go dealloc before the game goes on, as on Apple), when a
// pool drains, and at the end of each main-loop turn (`dealloc_pending`).
// Then the bridge sends it -dealloc as the game would (`set_dealloc_hook`):
// the game's -dealloc, whose [super dealloc] frees its guest memory. The
// host form's memory is kept; the bridge treats it as nil from then on, so
// a late message from a holder that didn't retain it does nothing.
//
// Every NSObject retain and release asks, so the common answer ("not a game
// object") takes no lock: the game's classes (the bridge's mirror classes)
// are in a fixed table read with plain loads, and an object counts only if
// its class pointer is there. Game objects' counts are in 64 shards, each
// with its own lock, picked by address.

struct GameEntry {
    count: usize,
    noted: bool,
}

/// The game's classes: an open-addressing table of class pointers, filled
/// once per class (by compare-exchange) and never emptied.
const GAME_CLASS_SLOTS: usize = 16384;
static GAME_CLASSES: [AtomicUsize; GAME_CLASS_SLOTS] = [const { AtomicUsize::new(0) }; GAME_CLASS_SLOTS];

const GAME_SHARDS: usize = 64;
static GAME: [Mutex<Option<HashMap<Id, GameEntry>>>; GAME_SHARDS] = [const { Mutex::new(None) }; GAME_SHARDS];

thread_local! {
    /// This thread's game objects whose count reached zero, oldest first.
    static TO_DEALLOC: RefCell<std::collections::VecDeque<Id>> = const { RefCell::new(std::collections::VecDeque::new()) };
}

/// How a game object is sent -dealloc (the bridge's, see set_dealloc_hook),
/// and whether one can be sent from here now.
type DeallocHook = std::sync::Arc<dyn Fn(Id) + Send + Sync>;
type DeallocReady = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;
static DEALLOC_HOOK: Mutex<Option<(DeallocHook, DeallocReady)>> = Mutex::new(None);

/// The 32-bit bridge sends game objects -dealloc through `hook`. Without
/// one (no 32-bit game) an object at zero just stays. While `ready` says no
/// (guest calls nested near the translator's limit, where Apple's -dealloc
/// would simply nest deeper on the stack), the objects wait on this
/// thread's list for the next safe point: the one a level up, in a chain of
/// -deallocs each freeing the next.
pub fn set_dealloc_hook(hook: DeallocHook, ready: DeallocReady) {
    *DEALLOC_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some((hook, ready));
}

fn class_slot(class: usize) -> usize {
    (class >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - 14)
}

/// Objects of `class` (a mirror of a game class) are counted from now on.
pub fn count_game_class(class: usize) {
    let mut at = class_slot(class);
    for _ in 0..GAME_CLASS_SLOTS {
        match GAME_CLASSES[at].compare_exchange(0, class, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(existing) if existing == class => return,
            Err(_) => at = (at + 1) % GAME_CLASS_SLOTS,
        }
    }
    static FULL: std::sync::Once = std::sync::Once::new();
    FULL.call_once(|| crate::log!("counted: more than {GAME_CLASS_SLOTS} game classes; the rest are not counted"));
}

/// Whether `object` is of a game class, without any lock.
#[inline]
fn game_class_of(object: Id) -> bool {
    if object == 0 || object & 7 != 0 || (object as isize) < 0 {
        return false;
    }
    let isa = unsafe { *(object as *const usize) };
    let mut at = class_slot(isa);
    for _ in 0..GAME_CLASS_SLOTS {
        match GAME_CLASSES[at].load(Ordering::Relaxed) {
            0 => return false,
            c if c == isa => return true,
            _ => at = (at + 1) % GAME_CLASS_SLOTS,
        }
    }
    false
}

fn game(object: Id) -> std::sync::MutexGuard<'static, Option<HashMap<Id, GameEntry>>> {
    let shard = ((object >> 4) ^ (object >> 12)) % GAME_SHARDS;
    GAME[shard].lock().unwrap_or_else(|e| e.into_inner())
}

/// Lists a new game object (its host form) with one reference (its alloc).
pub fn watch_game_object(object: Id) {
    if object == 0 {
        return;
    }
    game(object).get_or_insert_with(HashMap::new).insert(object, GameEntry { count: 1, noted: false });
}

/// A game object that was deallocated (by the game's own -dealloc).
pub fn forget_game_object(object: Id) {
    if let Some(map) = game(object).as_mut() {
        map.remove(&object);
    }
}

/// The count of a listed game object.
pub fn game_count(object: Id) -> Option<usize> {
    game(object).as_ref().and_then(|m| m.get(&object)).map(|e| e.count)
}

/// -[NSObject retain] of a listed game object.
pub fn game_retain(object: Id) {
    if !game_class_of(object) {
        return;
    }
    if let Some(entry) = game(object).as_mut().and_then(|m| m.get_mut(&object)) {
        entry.count += 1;
    }
}

/// -[NSObject release] of a listed game object; at zero it waits for
/// -dealloc at this thread's next safe point.
pub fn game_release(object: Id) {
    if !game_class_of(object) {
        return;
    }
    let zero = match game(object).as_mut().and_then(|m| m.get_mut(&object)) {
        Some(entry) if entry.count > 0 => {
            entry.count -= 1;
            entry.count == 0 && !std::mem::replace(&mut entry.noted, true)
        }
        _ => false,
    };
    if zero {
        TO_DEALLOC.with(|d| d.borrow_mut().push_back(object));
    }
}

/// -[NSObject autorelease] of a listed game object: released when the pool drains.
pub fn game_autorelease(object: Id) {
    if game_class_of(object) && game(object).as_ref().is_some_and(|m| m.contains_key(&object)) {
        POOL.with(|p| p.borrow_mut().push(object));
    }
}

/// A safe point (no host lock held): sends -dealloc to this thread's game
/// objects whose count reached zero, and to those their -dealloc lets go of
/// in turn. One retained again since (on Apple a use of a freed object)
/// lives on. Each is logged (the first 200, then every 1000th) with how many
/// notification registrations, KVO registrations and timers still name it,
/// which Apple's frameworks don't retain either (the game should have
/// removed them).
pub fn dealloc_pending() {
    static LOGGED: AtomicUsize = AtomicUsize::new(0);
    // Nothing waiting (nearly every call, one after each host message).
    if TO_DEALLOC.with(|d| d.borrow().is_empty()) {
        return;
    }
    let hook = DEALLOC_HOOK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if hook.as_ref().is_some_and(|(_, ready)| !ready()) {
        return;
    }
    while let Some(object) = TO_DEALLOC.with(|d| d.borrow_mut().pop_front()) {
        let zero = match game(object).as_mut().and_then(|m| m.get_mut(&object)) {
            Some(entry) if entry.count == 0 => true,
            Some(entry) => {
                entry.noted = false;
                false
            }
            None => false,
        };
        if !zero {
            continue;
        }
        let total = LOGGED.fetch_add(1, Ordering::Relaxed) + 1;
        if total <= 200 || total % 1000 == 0 {
            crate::log!(
                "dealloc {} {object:#x} (#{total}): named by {} notification registrations, {} KVO registrations, {} timers",
                crate::fw::string::describe_class_of(object),
                crate::fw::foundation::notification_registrations_naming(object),
                crate::fw::object::kvo_registrations_naming(object),
                crate::fw::runloop::timers_holding(object),
            );
        }
        if let Some((dealloc, _)) = &hook {
            dealloc(object);
        }
    }
}

// ----- weak references (ARC's __weak) to counted objects -----

/// Where weak references to each counted object are kept.
static WEAK: Mutex<Option<HashMap<Id, Vec<usize>>>> = Mutex::new(None);

fn weak() -> std::sync::MutexGuard<'static, Option<HashMap<Id, Vec<usize>>>> {
    WEAK.lock().unwrap_or_else(|e| e.into_inner())
}

/// objc_storeWeak / objc_initWeak: `location` now refers weakly to `object`.
pub fn store_weak(location: *mut Id, object: Id) -> Id {
    if location.is_null() {
        return object;
    }
    let old = unsafe { std::mem::replace(&mut *location, object) };
    if counted_class(old) || counted_class(object) {
        let mut map = weak();
        let map = map.get_or_insert_with(HashMap::new);
        if let Some(list) = map.get_mut(&old) {
            list.retain(|&l| l != location as usize);
        }
        if counted_class(object) && is_tracked(object) {
            map.entry(object).or_default().push(location as usize);
        } else if counted_class(object) {
            // Already gone: a weak reference to it reads nil, as on Apple.
            unsafe { *location = 0 };
            return 0;
        }
    }
    object
}

/// objc_loadWeakRetained: the object (+1), or nil once it is gone.
pub fn load_weak_retained(location: *const Id) -> Id {
    if location.is_null() {
        return 0;
    }
    // Under the weak lock, so the object can't be cleared halfway.
    let _guard = weak();
    let object = unsafe { *location };
    if counted_class(object) && !is_tracked(object) {
        return 0; // its last reference just went
    }
    retain(object)
}

/// Zeroes the weak references to an object whose last reference went.
fn clear_weak(object: Id) {
    if let Some(list) = weak().as_mut().and_then(|m| m.remove(&object)) {
        for location in list {
            let slot = location as *mut Id;
            unsafe {
                if *slot == object {
                    *slot = 0;
                }
            }
        }
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

/// Puts a listed object or game object in the innermost pool. Returns the object.
pub fn autorelease(object: Id) -> Id {
    if is_tracked(object) {
        POOL.with(|p| p.borrow_mut().push(object));
    } else {
        game_autorelease(object);
    }
    object
}

/// Marks a heap block in the pool (block pointers are aligned, so the low
/// bit is free).
const BLOCK_MARK: usize = 1;

/// Puts one reference to a heap block (one `block_copy` made) in the
/// innermost pool, released when it drains, as -[NSBlock autorelease].
pub fn autorelease_block(block: usize) -> usize {
    if block != 0 {
        POOL.with(|p| p.borrow_mut().push(block | BLOCK_MARK));
    }
    block
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
            Some(block) if block & BLOCK_MARK != 0 => {
                crate::libsystem::mach::block_release((block & !BLOCK_MARK) as *mut crate::libsystem::mach::BlockHeader)
            }
            Some(object) if counted_class(object) => release(object),
            Some(object) => game_release(object),
            None => break,
        }
    }
    dealloc_pending();
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

    /// A fake object of a counted class (only its class pointer is read).
    fn object() -> Id {
        count_class(0x7000_0000);
        Box::leak(Box::new(0x7000_0000usize)) as *mut usize as Id
    }

    #[test]
    fn counts_pools_and_holders_free_once_at_the_right_time() {
        let (image, kept) = (object(), object());
        let other = Box::leak(Box::new(0x1234usize)) as *mut usize as Id;
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
        // A weak reference: loading it gives +1; once the object goes it reads nil.
        let weak = object();
        track(weak, note);
        let mut slot: Id = 0;
        store_weak(&mut slot, weak);
        assert_eq!(load_weak_retained(&slot), weak);
        assert_eq!(count(weak), Some(2));
        release(weak);
        release(weak);
        assert_eq!((slot, load_weak_retained(&slot)), (0, 0));
        store_weak(&mut slot, 0);
    }

    #[test]
    fn counting_from_many_threads_frees_exactly_once() {
        let object = object();
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
