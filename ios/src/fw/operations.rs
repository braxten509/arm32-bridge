//! NSOperation, NSBlockOperation and NSOperationQueue.
//! The main queue runs its operations one at a time on the main thread's run
//! loop. Other queues run each ready operation on a thread of its own, up to
//! maxConcurrentOperationCount at once (no limit by default, as on Apple).
//! An operation is ready when every operation it depends on has finished.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, runloop, string};
use crate::libsystem::mach::{block_copy, call_block, BlockHeader};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};

/// NSOperationQueueDefaultMaxConcurrentOperationCount.
const NO_LIMIT: isize = -1;

// ----- NSOperation: { isa, *mut Operation } -----

#[derive(Default)]
struct Operation {
    cancelled: bool,
    executing: bool,
    finished: bool,
    completion: usize,
    dependencies: Vec<Id>,
    priority: isize,
    /// NSBlockOperation's blocks.
    blocks: Vec<usize>,
    name: Id,
}

/// Every operation's state sits behind this one lock.
static STATE: Mutex<()> = Mutex::new(());
/// Signalled whenever an operation finishes.
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

fn is_finished(object: Id) -> bool {
    with_state(|| operation(object).finished)
}

fn is_ready(object: Id) -> bool {
    let dependencies = with_state(|| operation(object).dependencies.clone());
    dependencies.iter().all(|&d| is_finished(d))
}

extern "C" fn op_init(this: Id, _: Sel) -> Id {
    operation(this);
    this
}

/// -start: runs -main unless cancelled, then marks the operation finished.
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
            runtime::send(this, "main", &[]);
        }
        Some(true) => {}
    }
    finish(this);
}

fn finish(this: Id) {
    let completion = with_state(|| {
        let op = operation(this);
        op.executing = false;
        op.finished = true;
        op.completion
    });
    FINISHED.notify_all();
    if completion != 0 {
        unsafe { call_block(completion) };
    }
    // Operations waiting on this one may be ready now.
    for queue in all_queues() {
        schedule(queue);
    }
}

extern "C" fn op_main(_this: Id, _: Sel) {}

extern "C" fn op_cancel(this: Id, _: Sel) {
    with_state(|| operation(this).cancelled = true);
    // A cancelled operation still waiting is ready at once (it only finishes).
    for queue in all_queues() {
        schedule(queue);
    }
}

extern "C" fn op_is_cancelled(this: Id, _: Sel) -> bool {
    with_state(|| operation(this).cancelled)
}

extern "C" fn op_is_executing(this: Id, _: Sel) -> bool {
    with_state(|| operation(this).executing)
}

extern "C" fn op_is_finished(this: Id, _: Sel) -> bool {
    is_finished(this)
}

extern "C" fn op_is_ready(this: Id, _: Sel) -> bool {
    is_ready(this)
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
    for queue in all_queues() {
        schedule(queue);
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

/// -waitUntilFinished
extern "C" fn op_wait(this: Id, _: Sel) {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    while !operation(this).finished {
        guard = FINISHED.wait(guard).unwrap_or_else(|e| e.into_inner());
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
    /// Added and not yet finished, in the order added.
    waiting: VecDeque<Id>,
    running: Vec<Id>,
}

static QUEUES: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Every queue's state sits behind this lock. Order: this lock, then STATE.
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

/// Starts every operation the queue may run now.
fn schedule(object: Id) {
    loop {
        let next = with_queue(object, |q| {
            if q.suspended {
                return None;
            }
            let limit = if q.limit < 0 { usize::MAX } else { q.limit as usize };
            if q.running.len() >= limit {
                return None;
            }
            // Highest priority first among the ready ones, then the order added.
            let mut best: Option<(usize, isize)> = None;
            for (index, &op) in q.waiting.iter().enumerate() {
                let cancelled = with_state(|| operation(op).cancelled);
                if !cancelled && !is_ready(op) {
                    continue;
                }
                let priority = with_state(|| operation(op).priority);
                if best.map(|(_, p)| priority > p).unwrap_or(true) {
                    best = Some((index, priority));
                }
            }
            let (index, _) = best?;
            let op = q.waiting.remove(index)?;
            q.running.push(op);
            Some((op, q.main))
        });
        let Some((op, main)) = next else { return };
        let run = move || {
            let before = CURRENT.with(|c| c.replace(object));
            runtime::send(op, "start", &[]);
            CURRENT.with(|c| c.set(before));
            with_queue(object, |q| q.running.retain(|&o| o != op));
            QUEUE_CHANGED.notify_all();
            schedule(object);
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
    let (finished, executing) = with_state(|| (operation(op).finished, operation(op).executing));
    let already_queued = all_queues().into_iter().any(|q| with_queue(q, |q| q.waiting.contains(&op) || q.running.contains(&op)));
    if finished || executing || already_queued {
        crate::log!("NSOperationQueue: operation already queued, running or finished (NSInvalidArgumentException on iOS); not added");
        return false;
    }
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

extern "C" fn queue_cancel_all(this: Id, s: Sel) {
    let list: Vec<Id> = with_queue(this, |q| q.running.iter().chain(q.waiting.iter()).copied().collect());
    for op in list {
        runtime::send(op, "cancel", &[]);
    }
    let _ = s;
}


/// Waits until the queue has nothing left (the main queue can't wait for
/// itself on the main thread, so there it runs nothing and returns).
extern "C" fn queue_wait_all(this: Id, _: Sel) {
    if with_queue(this, |q| q.main) && runloop::is_main_thread() {
        crate::log!("NSOperationQueue waitUntilAllOperationsAreFinished on the main queue from the main thread; returned at once");
        return;
    }
    let mut guard = QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let q = queue(this);
        if q.running.is_empty() && (q.waiting.is_empty() || q.suspended) {
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
