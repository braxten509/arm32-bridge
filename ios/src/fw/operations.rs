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
use crate::libsystem::mach::{block_copy, call_block, BlockHeader};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// NSOperationQueueDefaultMaxConcurrentOperationCount.
const NO_LIMIT: isize = -1;
const POLL: Duration = Duration::from_millis(20);

// ----- NSOperation: { isa, *mut Operation } -----

#[derive(Default)]
struct Operation {
    cancelled: bool,
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

fn kvo(op: Id, key: &str, value: bool) {
    object::kvo_notify(op, key, collections::number_bool(value));
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
        first.then_some((state.queue, state.completion))
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
        std::thread::spawn(move || unsafe { call_block(completion) });
    }
    true
}

/// Started operations that haven't said they're finished yet (they finish
/// later on their own: asynchronous ones, or subclasses with their own start).
static PENDING: Mutex<Vec<Id>> = Mutex::new(Vec::new());

fn watch(op: Id) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let start_poller = pending.is_empty();
    pending.push(op);
    drop(pending);
    if start_poller {
        std::thread::spawn(|| loop {
            std::thread::sleep(POLL);
            let list: Vec<Id> = PENDING.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let done: Vec<Id> = list.into_iter().filter(|&op| finished_now(op)).collect();
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|op| !done.contains(op));
            if pending.is_empty() {
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
        if op.finished || op.executing {
            return None;
        }
        op.executing = !op.cancelled;
        Some(op.cancelled)
    });
    match cancelled {
        None => {
            crate::log!("NSOperation started twice (NSInvalidArgumentException on iOS); ignored");
            return;
        }
        Some(false) => {
            kvo(this, "isExecuting", true);
            runtime::send(this, "main", &[]);
        }
        Some(true) => {}
    }
    with_state(|| {
        let op = operation(this);
        op.executing = false;
        op.finished = true;
    });
    if cancelled == Some(false) {
        kvo(this, "isExecuting", false);
    }
    kvo(this, "isFinished", true);
    finished_now(this);
}

/// Subclasses tell the queue about their own state through KVO notices,
/// which also go on to the operation's observers.
extern "C" fn op_did_change(this: Id, _: Sel, key: Id) {
    let key = string::get(key);
    if matches!(key.as_str(), "isFinished" | "isExecuting" | "isCancelled" | "isReady") {
        kvo(this, &key, ask(this, &key));
    }
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
    let first = with_state(|| !std::mem::replace(&mut operation(this).cancelled, true));
    if first {
        kvo(this, "isCancelled", true);
        // A cancelled operation still waiting is ready at once (it only finishes).
        kvo(this, "isReady", true);
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

extern "C" fn op_add_dependency(this: Id, _: Sel, other: Id) {
    if other != 0 {
        with_state(|| operation(this).dependencies.push(other));
    }
}

extern "C" fn op_remove_dependency(this: Id, _: Sel, other: Id) {
    with_state(|| operation(this).dependencies.retain(|&d| d != other));
    for queue_object in all_queues() {
        schedule(queue_object);
    }
}

extern "C" fn op_dependencies(this: Id, _: Sel) -> Id {
    let list = with_state(|| operation(this).dependencies.clone());
    collections::array_from(&list)
}

extern "C" fn op_completion_block(this: Id, _: Sel) -> usize {
    with_state(|| operation(this).completion)
}

extern "C" fn op_set_completion_block(this: Id, _: Sel, block: usize) {
    let block = if block == 0 { 0 } else { block_copy(block as *mut BlockHeader) as usize };
    with_state(|| operation(this).completion = block);
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
        drop(FINISHED.wait_timeout(guard, POLL).unwrap_or_else(|e| e.into_inner()));
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
            let before = CURRENT.with(|c| c.replace(object));
            runtime::send(op, "start", &[]);
            CURRENT.with(|c| c.set(before));
            if !finished_now(op) {
                watch(op);
            }
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
    collections::array_from(&list)
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
