//! NSOperation, NSBlockOperation and NSOperationQueue.
//! The main queue runs its operations one at a time on the main thread's run
//! loop. Other queues run each ready operation on a thread of its own, up to
//! maxConcurrentOperationCount at once (no limit by default, as on Apple).
//! As on Apple, everything goes through the operation's own methods (isReady,
//! isFinished, queuePriority, start), so subclasses can override them: an
//! operation is done when its isFinished says so, which the queue learns from
//! didChangeValueForKey:@"isFinished" (the KVO notice Apple requires) or,
//! for subclasses that don't send it, by asking isFinished every 20 ms.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, object, runloop, string};
use crate::libsystem::mach::{block_copy, block_release, call_block, BlockHeader};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// NSOperationQueueDefaultMaxConcurrentOperationCount.
const NO_LIMIT: isize = -1;
const POLL: Duration = Duration::from_millis(20);

// ----- NSOperation: { isa, *mut Operation } -----

#[derive(Default)]
struct Operation {
    cancelled: bool,
    /// -start was called (a second -start is refused).
    started: bool,
    executing: bool,
    finished: bool,
    /// The finish was handled: queue slot freed, waiters woken, completion run.
    done: bool,
    /// The queue it was added to (0: none).
    queue: Id,
    completion: usize,
    dependencies: Vec<Id>,
    priority: isize,
    /// NSBlockOperation's blocks.
    blocks: Vec<usize>,
    name: Id,
}

/// Every operation's state sits behind this one lock. Lock order: QUEUE_LOCK,
/// then STATE; no game code runs while either is held.
static STATE: Mutex<()> = Mutex::new(());
/// Signalled (with STATE) whenever an operation's finish is handled.
static FINISHED: Condvar = Condvar::new();

fn operation(object: Id) -> &'static mut Operation {
    unsafe {
        let slot = (object + 8) as *mut *mut Operation;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::default());
        }
        &mut **slot
    }
}

fn with_state<R>(f: impl FnOnce() -> R) -> R {
    let _guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// A BOOL answer from the operation's own method (overridable).
fn ask(op: Id, selector: &str) -> bool {
    runtime::send(op, selector, &[]) & 0xFF != 0
}

/// The operation's own change of `key`, announced around the change as a
/// setter's is (Apple's NSOperation sends will/didChangeValueForKey:): a
/// Prior notice sees the old state, and Old values reach dependent paths.
/// Called without STATE held (observers read the state).
fn changing<R>(op: Id, key: &str, change: impl FnOnce() -> R) -> R {
    object::will_change(op, key);
    let result = change();
    object::did_change(op, key);
    result
}

/// Called whenever the operation may have finished: once its isFinished says
/// YES, frees its queue slot, wakes waiters, starts what can start, and then
/// runs its completion block on a thread of its own (as Apple usually does).
fn finished_now(op: Id) -> bool {
    if !ask(op, "isFinished") {
        return false;
    }
    let first = with_state(|| {
        let state = operation(op);
        let first = !state.done;
        state.done = true;
        // Held while it runs (taken under the lock: a new completion block
        // may replace and release it meanwhile).
        let completion = if first && state.completion != 0 { block_copy(state.completion as *mut BlockHeader) as usize } else { 0 };
        first.then_some((state.queue, completion))
    });
    let Some((queue_object, completion)) = first else { return true };
    if queue_object != 0 {
        with_queue(queue_object, |q| q.running.retain(|&o| o != op));
        QUEUE_CHANGED.notify_all();
    }
    drop(STATE.lock().unwrap_or_else(|e| e.into_inner()));
    FINISHED.notify_all();
    // Operations waiting on this one may be ready now.
    for queue_object in all_queues() {
        schedule(queue_object);
    }
    if completion != 0 {
        // The operation too, as Apple's completion call captures it.
        crate::fw::counted::retain(op);
        std::thread::spawn(move || {
            unsafe { call_block(completion) };
            block_release(completion as *mut BlockHeader);
            crate::fw::counted::release(op);
            // The thread's only safe point.
            crate::fw::counted::dealloc_pending();
        });
    }
    // Its queue lets go of it. Whoever is still working with it holds it
    // too (the queue's runner during -start, the poller while it watches),
    // so it outlives the -finish that told us, as Apple's queue lets go of
    // it later (___NSOQDelayedFinishOperations).
    if queue_object != 0 {
        crate::fw::counted::release(op);
    }
    true
}

/// Started operations that haven't said they're finished yet (they finish
/// later on their own: asynchronous ones, or subclasses with their own start),
/// each with when to ask it next and how long to wait after that. Operations
/// that send the isFinished notice are handled at once; for the ones that
/// don't, the asking slows from every 20 ms to once a second.
static PENDING: Mutex<Vec<(Id, Instant, Duration)>> = Mutex::new(Vec::new());
const SLOWEST_POLL: Duration = Duration::from_secs(1);

/// The list holds what it watches, so a finished one is still there to ask.
fn watch(op: Id) {
    crate::fw::counted::retain(op);
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let start_poller = pending.is_empty();
    pending.push((op, Instant::now() + POLL, POLL));
    drop(pending);
    if start_poller {
        std::thread::spawn(|| loop {
            let next = PENDING.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, at, _)| *at).min();
            let Some(next) = next else { return };
            std::thread::sleep(next.saturating_duration_since(Instant::now()).max(POLL));
            let now = Instant::now();
            let due: Vec<Id> = PENDING.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, at, _)| *at <= now).map(|(op, _, _)| *op).collect();
            let done: Vec<Id> = due.iter().copied().filter(|&op| finished_now(op)).collect();
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|(op, _, _)| !done.contains(op));
            for (op, at, wait) in pending.iter_mut() {
                if due.contains(op) {
                    *wait = (*wait * 2).min(SLOWEST_POLL);
                    *at = now + *wait;
                }
            }
            let empty = pending.is_empty();
            drop(pending);
            done.iter().for_each(|&op| crate::fw::counted::release(op));
            // A safe point: what this round let go of deallocs here.
            crate::fw::counted::dealloc_pending();
            if empty {
                return;
            }
        });
    }
}

extern "C" fn op_init(this: Id, _: Sel) -> Id {
    operation(this);
    this
}

/// -start: runs -main unless cancelled, then marks the operation finished
/// (with the KVO notices Apple sends).
extern "C" fn op_start(this: Id, _: Sel) {
    let cancelled = with_state(|| {
        let op = operation(this);
        if op.started || op.finished || op.executing {
            return None;
        }
        op.started = true;
        Some(op.cancelled)
    });
    let finish = || {
        with_state(|| {
            let op = operation(this);
            op.executing = false;
            op.finished = true;
        })
    };
    match cancelled {
        None => {
            crate::log!("NSOperation started twice (NSInvalidArgumentException on iOS); ignored");
            return;
        }
        Some(false) => {
            changing(this, "isExecuting", || with_state(|| operation(this).executing = true));
            runtime::send(this, "main", &[]);
            changing(this, "isFinished", || changing(this, "isExecuting", finish));
        }
        Some(true) => changing(this, "isFinished", finish),
    }
    finished_now(this);
}

/// Subclasses tell the queue about their own state through KVO notices,
/// which also go on to the operation's observers (NSObject's
/// -didChangeValueForKey:, paired with their -willChangeValueForKey:).
extern "C" fn op_did_change(this: Id, _: Sel, key: Id) {
    let key = string::get(key);
    object::did_change(this, &key);
    match key.as_str() {
        "isFinished" => {
            finished_now(this);
        }
        "isReady" => {
            for queue_object in all_queues() {
                schedule(queue_object);
            }
        }
        _ => {}
    }
}

extern "C" fn op_main(_this: Id, _: Sel) {}

extern "C" fn op_cancel(this: Id, _: Sel) {
    if with_state(|| operation(this).cancelled) {
        return;
    }
    // A cancelled operation still waiting is ready at once (it only finishes).
    let first = changing(this, "isReady", || changing(this, "isCancelled", || with_state(|| !std::mem::replace(&mut operation(this).cancelled, true))));
    if first {
        for queue_object in all_queues() {
            schedule(queue_object);
        }
    }
}

extern "C" fn op_is_cancelled(this: Id, _: Sel) -> bool {
    with_state(|| operation(this).cancelled)
}

extern "C" fn op_is_executing(this: Id, _: Sel) -> bool {
    with_state(|| operation(this).executing)
}

extern "C" fn op_is_finished(this: Id, _: Sel) -> bool {
    with_state(|| operation(this).finished)
}

/// Ready when every dependency's isFinished says YES, or once cancelled.
extern "C" fn op_is_ready(this: Id, _: Sel) -> bool {
    let (cancelled, dependencies) = with_state(|| (operation(this).cancelled, operation(this).dependencies.clone()));
    cancelled || dependencies.iter().all(|&d| ask(d, "isFinished"))
}

extern "C" fn op_is_concurrent(_this: Id, _: Sel) -> bool {
    false
}

/// The dependencies are held, as Apple's array retains them.
extern "C" fn op_add_dependency(this: Id, _: Sel, other: Id) {
    if other != 0 {
        crate::fw::counted::retain(other);
        with_state(|| operation(this).dependencies.push(other));
    }
}

extern "C" fn op_remove_dependency(this: Id, _: Sel, other: Id) {
    let removed = with_state(|| {
        let list = &mut operation(this).dependencies;
        let before = list.len();
        list.retain(|&d| d != other);
        before - list.len()
    });
    (0..removed).for_each(|_| crate::fw::counted::release(other));
    for queue_object in all_queues() {
        schedule(queue_object);
    }
}

extern "C" fn op_dependencies(this: Id, _: Sel) -> Id {
    let list = with_state(|| operation(this).dependencies.clone());
    collections::array_autoreleased(&list)
}

/// Retained under the lock and autoreleased, as Apple's: a completion block
/// set meanwhile can't free it under the caller.
extern "C" fn op_completion_block(this: Id, _: Sel) -> usize {
    let block = with_state(|| block_copy(operation(this).completion as *mut BlockHeader) as usize);
    crate::fw::counted::autorelease_block(block)
}

extern "C" fn op_set_completion_block(this: Id, _: Sel, block: usize) {
    let block = if block == 0 { 0 } else { block_copy(block as *mut BlockHeader) as usize };
    let old = with_state(|| std::mem::replace(&mut operation(this).completion, block));
    // A copy property: the block it replaces is let go of (outside the lock:
    // its dispose may run game code).
    if old != 0 {
        block_release(old as *mut BlockHeader);
    }
}

extern "C" fn op_priority(this: Id, _: Sel) -> isize {
    with_state(|| operation(this).priority)
}

extern "C" fn op_set_priority(this: Id, _: Sel, priority: isize) {
    with_state(|| operation(this).priority = priority);
}

extern "C" fn op_name(this: Id, _: Sel) -> Id {
    with_state(|| operation(this).name)
}

extern "C" fn op_set_name(this: Id, _: Sel, name: Id) {
    let name = if name == 0 { 0 } else { runtime::send(name, "copy", &[]) };
    with_state(|| operation(this).name = name);
}

/// -waitUntilFinished: until the operation's isFinished says YES.
extern "C" fn op_wait(this: Id, _: Sel) {
    loop {
        if ask(this, "isFinished") {
            finished_now(this);
            return;
        }
        let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if operation(this).done {
            return;
        }
        // A 32-bit game's thread waiting here must let the queues' threads
        // run game code (GIL mode: ARM32_PARALLEL=0, Unicorn), as runloop's
        // waits do: the notification post would otherwise hold the lock the
        // observer's block needs and wait for ever.
        let waited = waiting(|| FINISHED.wait_timeout(guard, POLL).unwrap_or_else(|e| e.into_inner()));
        drop(waited);
    }
}

/// Runs a wait without the guest lock (see runloop.rs `waiting`).
fn waiting<R>(wait: impl FnOnce() -> R) -> R {
    use arm32_bridge::runtime::{gil_held, parallel, without_gil};
    if !parallel() && gil_held() {
        without_gil(wait)
    } else {
        wait()
    }
}

// ----- NSBlockOperation -----

fn block_operation(class: Class, block: usize) -> Id {
    let object = runtime::alloc_instance(class);
    if block != 0 {
        let block = block_copy(block as *mut BlockHeader) as usize;
        with_state(|| operation(object).blocks.push(block));
    }
    object
}

extern "C" fn block_op_with_block(class: Class, _: Sel, block: usize) -> Id {
    block_operation(class, block)
}

extern "C" fn block_op_add(this: Id, _: Sel, block: usize) {
    if block != 0 {
        let block = block_copy(block as *mut BlockHeader) as usize;
        with_state(|| operation(this).blocks.push(block));
    }
}

extern "C" fn block_op_blocks(this: Id, _: Sel) -> Id {
    let blocks = with_state(|| operation(this).blocks.clone());
    collections::array_from(&blocks)
}

/// Runs the blocks in order (Apple may run them at once; order is one valid choice).
extern "C" fn block_op_main(this: Id, _: Sel) {
    let blocks = with_state(|| operation(this).blocks.clone());
    for block in blocks {
        unsafe { call_block(block) };
    }
}

// ----- NSOperationQueue: { isa, *mut Queue } -----

struct Queue {
    main: bool,
    name: Id,
    limit: isize,
    suspended: bool,
    /// Added and not yet started, in the order added.
    waiting: VecDeque<Id>,
    /// Started and not yet finished.
    running: Vec<Id>,
}

static QUEUES: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Every queue's state sits behind this lock.
static QUEUE_LOCK: Mutex<()> = Mutex::new(());
/// Signalled (with QUEUE_LOCK) whenever a queue's operation finishes.
static QUEUE_CHANGED: Condvar = Condvar::new();

thread_local! {
    static CURRENT: Cell<Id> = const { Cell::new(0) };
}

/// Call with QUEUE_LOCK held. A queue the game allocated gets its state on first use.
fn queue(object: Id) -> &'static mut Queue {
    unsafe {
        let slot = (object + 8) as *mut *mut Queue;
        if (*slot).is_null() {
            let state = Queue { main: false, name: 0, limit: NO_LIMIT, suspended: false, waiting: VecDeque::new(), running: Vec::new() };
            *slot = Box::into_raw(Box::new(state));
            QUEUES.lock().unwrap_or_else(|e| e.into_inner()).push(object);
        }
        &mut **slot
    }
}

fn all_queues() -> Vec<Id> {
    QUEUES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn make_main_queue(class: Class) -> Id {
    let object = runtime::alloc_instance(class);
    with_queue(object, |q| {
        q.main = true;
        q.limit = 1;
        q.name = string::make("NSOperationQueue Main Queue");
    });
    object
}

fn with_queue<R>(object: Id, f: impl FnOnce(&mut Queue) -> R) -> R {
    let _guard = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f(queue(object))
}

fn has_room(q: &Queue) -> bool {
    !q.suspended && (q.limit < 0 || q.running.len() < q.limit as usize)
}

/// Starts every operation the queue may run now: the ready ones (isReady),
/// highest queuePriority first, then in the order added.
fn schedule(object: Id) {
    loop {
        let candidates = with_queue(object, |q| if has_room(q) { q.waiting.iter().copied().collect() } else { Vec::new() });
        // The operations' own methods run with no lock held.
        let mut best: Option<(Id, isize)> = None;
        for op in candidates {
            if !ask(op, "isReady") {
                continue;
            }
            let priority = runtime::send(op, "queuePriority", &[]) as isize;
            if best.map(|(_, p)| priority > p).unwrap_or(true) {
                best = Some((op, priority));
            }
        }
        let Some((op, _)) = best else { return };
        let claimed = with_queue(object, |q| match q.waiting.iter().position(|&o| o == op) {
            Some(index) if has_room(q) => {
                q.waiting.remove(index);
                q.running.push(op);
                Some(q.main)
            }
            _ => None,
        });
        let Some(main) = claimed else { continue };
        let run = move || {
            // Held while we work with it: its -start may finish it, and the
            // queue then lets go of it, before we ask it again here.
            crate::fw::counted::retain(op);
            let before = CURRENT.with(|c| c.replace(object));
            runtime::send(op, "start", &[]);
            CURRENT.with(|c| c.set(before));
            if !finished_now(op) {
                watch(op);
            }
            crate::fw::counted::release(op);
            // A safe point: what finishing let go of deallocs here.
            crate::fw::counted::dealloc_pending();
        };
        if main {
            runloop::post(runloop::Work::Native(Box::new(run)));
        } else {
            std::thread::spawn(run);
        }
    }
}

extern "C" fn queue_init(this: Id, _: Sel) -> Id {
    with_queue(this, |_| ());
    this
}

extern "C" fn queue_main(class: Class, _: Sel) -> Id {
    static MAIN: OnceLock<usize> = OnceLock::new();
    *MAIN.get_or_init(|| make_main_queue(class))
}

extern "C" fn queue_current(class: Class, s: Sel) -> Id {
    let current = CURRENT.with(|c| c.get());
    if current != 0 {
        current
    } else if runloop::is_main_thread() {
        queue_main(class, s)
    } else {
        0
    }
}

fn add(this: Id, op: Id) -> bool {
    if op == 0 {
        return false;
    }
    let (finished, executing) = (ask(op, "isFinished"), ask(op, "isExecuting"));
    let already_queued = with_state(|| operation(op).queue != 0);
    if finished || executing || already_queued {
        crate::log!("NSOperationQueue: operation already queued, running or finished (NSInvalidArgumentException on iOS); not added");
        return false;
    }
    with_state(|| operation(op).queue = this);
    // The queue holds it until it has finished, as Apple's does.
    crate::fw::counted::retain(op);
    with_queue(this, |q| q.waiting.push_back(op));
    true
}

extern "C" fn queue_add_operation(this: Id, _: Sel, op: Id) {
    if add(this, op) {
        schedule(this);
    }
}

extern "C" fn queue_add_block(this: Id, _: Sel, block: usize) {
    let op = block_operation(runtime::class_named("NSBlockOperation"), block);
    queue_add_operation(this, 0, op);
}

extern "C" fn queue_add_operations(this: Id, s: Sel, list: Id, wait: u8) {
    let ops = collections::array_items(list);
    for &op in &ops {
        add(this, op);
    }
    schedule(this);
    if wait != 0 {
        for op in ops {
            op_wait(op, s);
        }
    }
}

extern "C" fn queue_operations(this: Id, _: Sel) -> Id {
    let list: Vec<Id> = with_queue(this, |q| q.running.iter().chain(q.waiting.iter()).copied().collect());
    collections::array_autoreleased(&list)
}

extern "C" fn queue_operation_count(this: Id, _: Sel) -> usize {
    with_queue(this, |q| q.running.len() + q.waiting.len())
}

extern "C" fn queue_cancel_all(this: Id, _: Sel) {
    let list: Vec<Id> = with_queue(this, |q| q.running.iter().chain(q.waiting.iter()).copied().collect());
    for op in list {
        runtime::send(op, "cancel", &[]);
    }
    schedule(this);
}

/// Waits until every operation the queue holds has finished, as Apple does
/// (a suspended queue with operations waits until it is resumed). The main
/// queue can't wait for itself on the main thread, so there it returns.
extern "C" fn queue_wait_all(this: Id, _: Sel) {
    if with_queue(this, |q| q.main) && runloop::is_main_thread() {
        crate::log!("NSOperationQueue waitUntilAllOperationsAreFinished on the main queue from the main thread; returned at once");
        return;
    }
    let mut guard = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let q = queue(this);
        if q.running.is_empty() && q.waiting.is_empty() {
            return;
        }
        // The timeout covers operations whose dependencies finish on another queue.
        guard = QUEUE_CHANGED.wait_timeout(guard, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

extern "C" fn queue_limit(this: Id, _: Sel) -> isize {
    with_queue(this, |q| q.limit)
}

extern "C" fn queue_set_limit(this: Id, _: Sel, limit: isize) {
    let main = with_queue(this, |q| {
        if !q.main {
            q.limit = if limit < 0 { NO_LIMIT } else { limit };
        }
        q.main
    });
    if !main {
        schedule(this);
    }
}

extern "C" fn queue_is_suspended(this: Id, _: Sel) -> bool {
    with_queue(this, |q| q.suspended)
}

extern "C" fn queue_set_suspended(this: Id, _: Sel, suspended: u8) {
    with_queue(this, |q| q.suspended = suspended != 0);
    if suspended == 0 {
        schedule(this);
    }
}

extern "C" fn queue_name(this: Id, _: Sel) -> Id {
    with_queue(this, |q| q.name)
}

extern "C" fn queue_set_name(this: Id, _: Sel, name: Id) {
    let name = if name == 0 { 0 } else { runtime::send(name, "copy", &[]) };
    with_queue(this, |q| q.name = name);
}

pub fn register(_: &mut Registry) {
    let op = define_class("NSOperation", "NSObject", 16);
    methods(
        op,
        &[
            ("init", op_init as usize),
            ("start", op_start as usize),
            ("main", op_main as usize),
            ("cancel", op_cancel as usize),
            ("isCancelled", op_is_cancelled as usize),
            ("isExecuting", op_is_executing as usize),
            ("isFinished", op_is_finished as usize),
            ("isReady", op_is_ready as usize),
            ("isConcurrent", op_is_concurrent as usize),
            ("isAsynchronous", op_is_concurrent as usize),
            ("addDependency:", op_add_dependency as usize),
            ("removeDependency:", op_remove_dependency as usize),
            ("dependencies", op_dependencies as usize),
            ("completionBlock", op_completion_block as usize),
            ("setCompletionBlock:", op_set_completion_block as usize),
            ("queuePriority", op_priority as usize),
            ("setQueuePriority:", op_set_priority as usize),
            ("name", op_name as usize),
            ("setName:", op_set_name as usize),
            ("waitUntilFinished", op_wait as usize),
            ("didChangeValueForKey:", op_did_change as usize),
        ],
    );
    let block_op = define_class("NSBlockOperation", "NSOperation", 16);
    class_methods(block_op, &[("blockOperationWithBlock:", block_op_with_block as usize)]);
    methods(
        block_op,
        &[("addExecutionBlock:", block_op_add as usize), ("executionBlocks", block_op_blocks as usize), ("main", block_op_main as usize)],
    );
    let queue_class = define_class("NSOperationQueue", "NSObject", 16);
    class_methods(queue_class, &[("mainQueue", queue_main as usize), ("currentQueue", queue_current as usize)]);
    methods(
        queue_class,
        &[
            ("init", queue_init as usize),
            ("addOperation:", queue_add_operation as usize),
            ("addOperationWithBlock:", queue_add_block as usize),
            ("addOperations:waitUntilFinished:", queue_add_operations as usize),
            ("operations", queue_operations as usize),
            ("operationCount", queue_operation_count as usize),
            ("cancelAllOperations", queue_cancel_all as usize),
            ("waitUntilAllOperationsAreFinished", queue_wait_all as usize),
            ("maxConcurrentOperationCount", queue_limit as usize),
            ("setMaxConcurrentOperationCount:", queue_set_limit as usize),
            ("isSuspended", queue_is_suspended as usize),
            ("setSuspended:", queue_set_suspended as usize),
            ("name", queue_name as usize),
            ("setName:", queue_set_name as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[repr(C)]
    struct TestBlock {
        header: BlockHeader,
        queue: Id,
    }

    fn block(invoke: extern "C" fn(usize), queue: Id) -> usize {
        // A global block: block_copy hands back the same one.
        let header = BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: invoke as usize, descriptor: std::ptr::null() };
        Box::leak(Box::new(TestBlock { header, queue })) as *mut TestBlock as usize
    }

    static SECOND_RAN: AtomicBool = AtomicBool::new(false);
    static COMPLETION_DONE: AtomicBool = AtomicBool::new(false);

    extern "C" fn nothing(_block: usize) {}

    extern "C" fn second(_block: usize) {
        SECOND_RAN.store(true, Ordering::SeqCst);
    }

    /// The first operation's completion block adds a second one to the same
    /// serial queue and waits for it: the first must have left its slot.
    extern "C" fn completion(block: usize) {
        let queue_object = unsafe { (*(block as *const TestBlock)).queue };
        let op = block_operation(runtime::class_named("NSBlockOperation"), self::block(second, 0));
        queue_add_operations(queue_object, 0, collections::array_from(&[op]), 1);
        COMPLETION_DONE.store(true, Ordering::SeqCst);
    }

    /// An operation holds its dependencies, as Apple's retains them: a
    /// game's operation it waits on can't dealloc under it.
    #[test]
    fn an_operation_holds_its_dependencies() {
        use crate::fw::counted;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = crate::fw::object::define_class("GameCountedDependency", "NSOperation", 8);
        counted::count_game_class(class);
        let new = |class: Id| runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let (op, dependency) = (new(runtime::class_named("NSOperation")), new(class));
        counted::watch_game_object(dependency);
        runtime::send(op, "addDependency:", &[dependency]);
        assert_eq!(counted::game_count(dependency), Some(2), "held by the operation");
        runtime::send(op, "removeDependency:", &[dependency]);
        assert_eq!(counted::game_count(dependency), Some(1), "let go of");
    }

    /// -completionBlock hands the block back retained and autoreleased, as
    /// Apple's (__NSOperationInternal completionBlock): replacing it meanwhile
    /// doesn't free it under the caller.
    #[test]
    fn the_completion_block_getter_outlives_a_replacement() {
        use crate::libsystem::mach::test_blocks::{counting, Counts};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static COUNTS: Counts = Counts { copies: std::sync::atomic::AtomicUsize::new(0), disposes: std::sync::atomic::AtomicUsize::new(0) };
        let op = runtime::send(runtime::send(runtime::class_named("NSOperation"), "alloc", &[]), "init", &[]);
        op_set_completion_block(op, 0, counting(&COUNTS));
        let mark = crate::fw::counted::pool_mark();
        let got = op_completion_block(op, 0);
        op_set_completion_block(op, 0, 0);
        assert_eq!(COUNTS.get(), (1, 0), "freed while the caller still had it");
        assert_ne!(got, 0);
        crate::fw::counted::pool_drain(mark);
        assert_eq!(COUNTS.get(), (1, 1), "let go of when the pool drained");
    }

    /// Operations of a game class whose own -start finishes them (at once,
    /// sending the isFinished notice) or not (a later isFinished tells).
    static FINISHED_OPS: Mutex<Vec<Id>> = Mutex::new(Vec::new());
    static COUNT_IN_START: Mutex<Vec<Option<usize>>> = Mutex::new(Vec::new());

    extern "C" fn own_is_finished(this: Id, _: Sel) -> bool {
        FINISHED_OPS.lock().unwrap().contains(&this)
    }

    extern "C" fn finish_in_start(this: Id, _: Sel) {
        let key = string::make("isFinished");
        runtime::send(this, "willChangeValueForKey:", &[key]);
        FINISHED_OPS.lock().unwrap().push(this);
        runtime::send(this, "didChangeValueForKey:", &[key]);
        // As the bridge does when a game's host call returns.
        crate::fw::counted::dealloc_pending();
        COUNT_IN_START.lock().unwrap().push(crate::fw::counted::game_count(this));
    }

    extern "C" fn start_and_return(_this: Id, _: Sel) {}

    fn game_operation(name: &str, start: extern "C" fn(Id, Sel)) -> Id {
        let class = crate::fw::object::define_class(name, "NSOperation", 8);
        methods(class, &[("start", start as usize), ("isFinished", own_is_finished as usize)]);
        crate::fw::counted::count_game_class(class);
        let op = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        crate::fw::counted::watch_game_object(op);
        op
    }

    /// Adds `op` to a new queue and lets go of the game's own reference
    /// before the queue starts it, so only the queue holds it.
    fn hand_to_queue(op: Id) {
        let queue_object = runtime::send(runtime::send(runtime::class_named("NSOperationQueue"), "alloc", &[]), "init", &[]);
        queue_set_suspended(queue_object, 0, 1);
        queue_add_operation(queue_object, 0, op);
        runtime::send(op, "release", &[]);
        queue_set_suspended(queue_object, 0, 0);
    }

    fn wait_until(what: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !what() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        what()
    }

    /// An operation the queue holds outlives the -start in which it finishes
    /// itself (Apple's queue lets go of a finished operation later, from
    /// ___NSOQDelayedFinishOperations), and is let go of after it.
    #[test]
    fn a_queued_operation_outlives_its_own_finish() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let op = game_operation("GameFinishesInStart", finish_in_start);
        hand_to_queue(op);
        assert!(wait_until(|| !COUNT_IN_START.lock().unwrap().is_empty()), "never started");
        assert_eq!(COUNT_IN_START.lock().unwrap()[0], Some(1), "let go of inside its own -start");
        assert!(wait_until(|| crate::fw::counted::game_count(op) == Some(0)), "never let go of");
    }

    /// An operation finished by its own later isFinished (no notice: the
    /// queue's poller learns it) deallocs on the poller's thread.
    #[test]
    fn an_operation_the_poller_finds_finished_deallocs() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let op = game_operation("GameFinishesLater", start_and_return);
        hand_to_queue(op);
        assert!(wait_until(|| PENDING.lock().unwrap().iter().any(|&(o, _, _)| o == op)), "never watched");
        let mut log = crate::report::LineWatch::now();
        FINISHED_OPS.lock().unwrap().push(op);
        assert!(log.wait_for(&format!("dealloc GameFinishesLater {op:#x}"), Duration::from_millis(500)), "never dealloced");
    }

    #[test]
    fn completion_runs_after_the_operation_leaves_its_serial_queue() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSOperationQueue");
        let queue_object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        queue_set_limit(queue_object, 0, 1);
        let first = block_operation(runtime::class_named("NSBlockOperation"), block(nothing, 0));
        op_set_completion_block(first, 0, block(completion, queue_object));
        queue_add_operation(queue_object, 0, first);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !COMPLETION_DONE.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(COMPLETION_DONE.load(Ordering::SeqCst), "the completion block waited forever on its own queue");
        assert!(SECOND_RAN.load(Ordering::SeqCst));
        queue_wait_all(queue_object, 0);
        assert_eq!(queue_operation_count(queue_object, 0), 0);
    }
}
