//! Apple kernel (Mach) calls, dispatch queues, blocks and dynamic loading.

use crate::symbols::{leak, Registry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

const KERN_SUCCESS: i32 = 0;
const KERN_FAILURE: i32 = 5;

pub(crate) fn now_nanos() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

pub(crate) fn wall_nanos() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// A dispatch time as a monotonic deadline (None: never). Apple's times are
/// 0 (now), ~0 (forever), a mach_absolute_time value, or a wall-clock time
/// from dispatch_walltime, stored negated.
pub(crate) fn deadline_nanos(when: u64) -> Option<u64> {
    match when {
        0 => Some(now_nanos()),
        u64::MAX => None,
        _ if (when as i64) < 0 => {
            let wall = (when as i64).wrapping_neg() as u64;
            Some(now_nanos().saturating_add(wall.saturating_sub(wall_nanos())))
        }
        _ => Some(when),
    }
}

extern "C" fn mach_absolute_time() -> u64 {
    now_nanos()
}

extern "C" fn mach_timebase_info(info: *mut [u32; 2]) -> i32 {
    unsafe { *info = [1, 1] };
    KERN_SUCCESS
}

extern "C" fn mach_wait_until(deadline: u64) -> i32 {
    let t = libc::timespec { tv_sec: (deadline / 1_000_000_000) as i64, tv_nsec: (deadline % 1_000_000_000) as i64 };
    unsafe { libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &t, std::ptr::null_mut()) };
    KERN_SUCCESS
}

extern "C" fn mach_host_self() -> u32 {
    0x203
}

extern "C" fn mach_thread_self() -> u32 {
    unsafe { libc::gettid() as u32 }
}

extern "C" fn host_page_size(_host: u32, size: *mut usize) -> i32 {
    unsafe { *size = 16384 };
    KERN_SUCCESS
}

fn meminfo(key: &str) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
        * 1024
}

/// host_statistics(HOST_VM_INFO): page counts, with the game told it has
/// the memory of the device it thinks it runs on.
extern "C" fn host_statistics(_host: u32, flavor: i32, info: *mut u32, count: *mut u32) -> i32 {
    if flavor != 2 {
        crate::log!("host_statistics flavor {flavor}: not known");
        return KERN_FAILURE;
    }
    let page = 16384u64;
    let total = 1u64 << 30;
    let available = meminfo("MemAvailable:").min(total / 2);
    let free_pages = (available / page) as u32;
    let used_pages = ((total - available) / page) as u32;
    unsafe {
        let words = (*count as usize).min(15);
        std::ptr::write_bytes(info, 0, words);
        let values = [free_pages, used_pages / 2, used_pages / 4, used_pages / 4];
        for (index, value) in values.iter().enumerate().take(words) {
            *info.add(index) = *value;
        }
    }
    KERN_SUCCESS
}

/// task_info: only the resident memory size is filled in.
extern "C" fn task_info(_task: u32, flavor: i32, info: *mut u8, count: *mut u32) -> i32 {
    let resident = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|t| t.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
        .unwrap_or(0)
        * 4096;
    unsafe {
        let bytes = (*count as usize) * 4;
        std::ptr::write_bytes(info, 0, bytes);
        match flavor {
            // TASK_BASIC_INFO_64 (5) on 64-bit: suspend_count i32, virtual u64, resident u64.
            5 | 18 if bytes >= 24 => {
                *(info.add(8) as *mut u64) = 1 << 30;
                *(info.add(16) as *mut u64) = resident;
            }
            // MACH_TASK_BASIC_INFO (20): virtual u64, resident u64, resident max u64.
            20 if bytes >= 24 => {
                *(info as *mut u64) = 1 << 30;
                *(info.add(8) as *mut u64) = resident;
                *(info.add(16) as *mut u64) = resident;
            }
            _ => {
                crate::log!("task_info flavor {flavor}: not known");
                return KERN_FAILURE;
            }
        }
    }
    KERN_SUCCESS
}

pub(crate) extern "C" fn vm_allocate(_task: u32, address: *mut usize, size: usize, flags: i32) -> i32 {
    unsafe {
        let fixed = flags & 1 == 0;
        if !fixed {
            if let Some(near) = super::near::map(size, libc::PROT_READ | libc::PROT_WRITE, 16384) {
                *address = near;
                return KERN_SUCCESS;
            }
        }
        let want = if fixed { *address } else { 0 };
        let extra = if fixed { 0 } else { 16384 };
        let raw = libc::mmap(
            want as *mut _,
            size + extra,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | if fixed { libc::MAP_FIXED } else { 0 },
            -1,
            0,
        );
        if raw == libc::MAP_FAILED {
            return 3;
        }
        let raw = raw as usize;
        let aligned = (raw + 16383) & !16383;
        if !fixed {
            if aligned > raw {
                libc::munmap(raw as *mut _, aligned - raw);
            }
            let tail = raw + size + extra - (aligned + size);
            if tail > 0 {
                libc::munmap((aligned + size) as *mut _, tail);
            }
        }
        *address = aligned;
    }
    KERN_SUCCESS
}

extern "C" fn vm_deallocate(_task: u32, address: usize, size: usize) -> i32 {
    super::io::ios_munmap(address, size);
    KERN_SUCCESS
}

extern "C" fn vm_protect(_task: u32, address: usize, size: usize, _set_max: i32, protection: i32) -> i32 {
    if unsafe { libc::mprotect(address as *mut _, size, protection & 7) } == 0 {
        KERN_SUCCESS
    } else {
        KERN_FAILURE
    }
}

extern "C" fn vm_read_overwrite(_task: u32, address: usize, size: usize, data: usize, out_size: *mut usize) -> i32 {
    unsafe {
        std::ptr::copy(address as *const u8, data as *mut u8, size);
        *out_size = size;
    }
    KERN_SUCCESS
}

macro_rules! fails {
    ($($name:ident),* $(,)?) => {
        $(
            extern "C" fn $name() -> i32 {
                crate::log!(concat!(stringify!($name), ": refused (crash reporting and Mach ports are not supported)"));
                KERN_FAILURE
            }
        )*
    };
}

fails!(
    mach_port_allocate, mach_port_insert_right, mach_port_move_member, mach_port_request_notification,
    mach_msg, task_get_exception_ports, task_swap_exception_ports, thread_get_exception_ports,
    thread_swap_exception_ports, task_threads, thread_info, exception_raise, exception_raise_state,
    exception_raise_state_identity, vm_map, mach_make_memory_entry_64
);

const MACH_PORT_RIGHT_SEND: u32 = 0;
const MACH_PORT_RIGHT_DEAD_NAME: u32 = 4;
const KERN_INVALID_RIGHT: i32 = 17;

/// Every port name we hand out (task, host, threads) is a plain send right
/// with nothing behind it, so letting go of one always works (as it does on
/// Apple for MACH_PORT_NULL and MACH_PORT_DEAD too).
extern "C" fn mach_port_deallocate(_task: u32, _name: u32) -> i32 {
    KERN_SUCCESS
}

/// Only send rights (and dead names) exist here; no receive rights or sets are ever made.
extern "C" fn mach_port_mod_refs(_task: u32, _name: u32, right: u32, _delta: i32) -> i32 {
    match right {
        MACH_PORT_RIGHT_SEND | MACH_PORT_RIGHT_DEAD_NAME => KERN_SUCCESS,
        _ => KERN_INVALID_RIGHT,
    }
}

extern "C" fn thread_policy_set() -> i32 {
    KERN_SUCCESS
}

// ----- blocks -----

const BLOCK_NEEDS_FREE: i32 = 1 << 24;
const BLOCK_HAS_COPY_DISPOSE: i32 = 1 << 25;
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

#[repr(C)]
pub struct BlockHeader {
    pub isa: usize,
    pub flags: i32,
    pub reserved: i32,
    pub invoke: usize,
    pub descriptor: *const [usize; 4],
}

pub static MALLOC_BLOCK_CLASS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub extern "C" fn block_copy(block: *mut BlockHeader) -> *mut BlockHeader {
    if block.is_null() {
        return block;
    }
    unsafe {
        let flags = (*block).flags;
        if flags & BLOCK_IS_GLOBAL != 0 || flags & BLOCK_NEEDS_FREE != 0 {
            return block;
        }
        let descriptor = &*(*block).descriptor;
        let size = descriptor[1];
        let copy = libc::malloc(size) as *mut BlockHeader;
        std::ptr::copy_nonoverlapping(block as *const u8, copy as *mut u8, size);
        (*copy).isa = MALLOC_BLOCK_CLASS.load(std::sync::atomic::Ordering::Relaxed);
        (*copy).flags = (flags & !0xFFFF) | BLOCK_NEEDS_FREE | 2;
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let helper: extern "C" fn(*mut BlockHeader, *mut BlockHeader) = std::mem::transmute(descriptor[2]);
            helper(copy, block);
        }
        copy
    }
}

extern "C" fn block_release(_block: *mut BlockHeader) {}

#[repr(C)]
struct Byref {
    isa: usize,
    forwarding: *mut Byref,
    flags: i32,
    size: i32,
    keep: usize,
    destroy: usize,
    layout: usize,
}

unsafe fn byref_copy(source: *mut Byref) -> *mut Byref {
    let source = (*source).forwarding;
    if (*source).flags & BLOCK_NEEDS_FREE != 0 {
        return source;
    }
    let size = ((*source).size as usize).max(24);
    let copy = libc::malloc(size) as *mut Byref;
    std::ptr::copy_nonoverlapping(source as *const u8, copy as *mut u8, size);
    (*copy).isa = 0;
    (*copy).flags = (*source).flags | BLOCK_NEEDS_FREE | 4;
    (*copy).forwarding = copy;
    (*source).forwarding = copy;
    if (*source).flags & BLOCK_HAS_COPY_DISPOSE != 0 {
        let keep: extern "C" fn(*mut Byref, *mut Byref) = std::mem::transmute((*source).keep);
        keep(copy, source);
    }
    copy
}

extern "C" fn block_object_assign(destination: *mut usize, object: usize, flags: i32) {
    unsafe {
        match flags & 0xFF {
            7 => *destination = block_copy(object as *mut BlockHeader) as usize,
            8 | 24 => *destination = byref_copy(object as *mut Byref) as usize,
            // A captured object: copying the block retains it (counted objects only).
            3 => *destination = crate::fw::counted::retain(object),
            _ => *destination = object,
        }
    }
}

extern "C" fn block_object_dispose(object: usize, flags: i32) {
    if flags & 0xFF == 3 {
        crate::fw::counted::release(object);
    }
}

pub unsafe fn call_block(block: usize) {
    let header = block as *const BlockHeader;
    let invoke: extern "C" fn(usize) = std::mem::transmute((*header).invoke);
    invoke(block);
}

// ----- dispatch -----

static ONCE_WAIT: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Runs `work` once per predicate. Apple's predicate: 0 = not yet, ~0 = done.
/// While running we store 1; other threads wait for ~0.
fn run_once(predicate: *mut i64, work: impl FnOnce()) {
    use std::sync::atomic::{AtomicI64, Ordering};
    let state = unsafe { &*(predicate as *const AtomicI64) };
    if state.load(Ordering::Acquire) == !0 {
        return;
    }
    if state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        work();
        let _guard = ONCE_WAIT.0.lock().unwrap_or_else(|e| e.into_inner());
        state.store(!0, Ordering::Release);
        ONCE_WAIT.1.notify_all();
        return;
    }
    let mut guard = ONCE_WAIT.0.lock().unwrap_or_else(|e| e.into_inner());
    while state.load(Ordering::Acquire) != !0 {
        guard = ONCE_WAIT.1.wait(guard).unwrap_or_else(|e| e.into_inner());
    }
}

extern "C" fn dispatch_once(predicate: *mut i64, block: usize) {
    run_once(predicate, || unsafe { call_block(block) });
}

extern "C" fn dispatch_once_f(predicate: *mut i64, context: usize, function: usize) {
    run_once(predicate, || {
        let f: extern "C" fn(usize) = unsafe { std::mem::transmute(function) };
        f(context);
    });
}

static MAIN_QUEUE_OBJECT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static GLOBAL_QUEUE_OBJECT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn main_queue() -> usize {
    *MAIN_QUEUE_OBJECT.get_or_init(|| leak([0usize; 16]))
}

pub(crate) extern "C" fn dispatch_get_global_queue(_priority: i64, _flags: u64) -> usize {
    *GLOBAL_QUEUE_OBJECT.get_or_init(|| leak([0usize; 16]))
}

// ----- queues -----
// The main queue runs on the main thread's run loop. Queues the game makes
// are serial (Apple's default): their blocks run one at a time, in order, on
// a worker thread. Global queues run each block on its own thread. A queue
// whose target (dispatch_set_target_queue) is serial or the main queue runs
// its blocks there, taking turns with that queue's own blocks.
// dispatch_suspend works on every queue the game made (serial or concurrent):
// a block bound for a suspended queue, or for a queue whose targets pass
// through one, doesn't start; it is held and sent on at resume, in the order
// the blocks were sent. While no queue is suspended, sending and running a
// block checks one counter and takes no shared lock for this.

type Done = std::sync::Arc<(Mutex<bool>, Condvar)>;

/// A block to run, the queue it was sent to (what dispatch_get_current_queue
/// answers while it runs), who waits for it, and its place in sending order.
struct Job {
    block: usize,
    sent_to: usize,
    done: Option<Done>,
    order: u64,
}

static NEXT_ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn job(block: usize, sent_to: usize, done: Option<Done>) -> Job {
    Job { block, sent_to, done, order: NEXT_ORDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
}

fn run(job: Job) {
    // Blocks sent with dispatch_async get a pool each, as GCD's worker
    // threads drain theirs (sync blocks use the caller's).
    let pool = job.done.is_none().then(crate::fw::counted::pool_mark);
    run_as(job.sent_to, job.block);
    if let Some(pool) = pool {
        crate::fw::counted::pool_drain(pool);
    }
    if let Some(done) = job.done {
        mark_done(&done);
    }
}

struct SerialQueue {
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    waiting: std::collections::VecDeque<Job>,
    /// A worker thread is running the queue's blocks.
    running: bool,
    /// dispatch_suspend count: no new block starts while above 0.
    suspended: u32,
}

static SERIAL_QUEUES: Mutex<Option<std::collections::HashMap<usize, &'static SerialQueue>>> = Mutex::new(None);

#[derive(Default)]
struct Suspensions {
    /// Concurrent queues the game made: their suspension counts.
    concurrent: std::collections::HashMap<usize, u32>,
    /// Blocks held back, by the suspended queue holding them, in sending order.
    held: std::collections::HashMap<usize, std::collections::VecDeque<Job>>,
}

/// Taken around every suspension change, holding and release.
static SUSPENSIONS: Mutex<Option<Suspensions>> = Mutex::new(None);

/// How many queues are suspended now: 0 lets blocks skip the check.
static ANY_SUSPENDED: AtomicUsize = AtomicUsize::new(0);

/// Queue -> its target queue.
static TARGETS: Mutex<Option<std::collections::HashMap<usize, usize>>> = Mutex::new(None);

thread_local! {
    static CURRENT_QUEUE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The serial queue whose worker this thread is (0: none).
    static CURRENT_RUNNER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn serial_queue(queue: usize) -> Option<&'static SerialQueue> {
    SERIAL_QUEUES.lock().unwrap().as_ref().and_then(|m| m.get(&queue).copied())
}

/// Where a queue's blocks run: the last serial (or main) queue along its
/// targets, or the queue itself.
fn runs_on(queue: usize) -> usize {
    let targets = TARGETS.lock().unwrap();
    let mut at = queue;
    for _ in 0..32 {
        match targets.as_ref().and_then(|t| t.get(&at)).copied() {
            Some(target) if target == main_queue() || serial_queue(target).is_some() => at = target,
            _ => break,
        }
    }
    at
}

/// Runs a block as if on `queue` (for blocks run by another queue's thread).
fn run_as(queue: usize, block: usize) {
    let before = CURRENT_QUEUE.with(|c| c.replace(queue));
    unsafe { call_block(block) };
    CURRENT_QUEUE.with(|c| c.set(before));
}

extern "C" fn dispatch_get_current_queue() -> usize {
    let current = CURRENT_QUEUE.with(|c| c.get());
    if current != 0 {
        current
    } else if crate::fw::runloop::is_main_thread() {
        main_queue()
    } else {
        dispatch_get_global_queue(0, 0)
    }
}

/// DISPATCH_QUEUE_SERIAL is no attribute; any attribute means concurrent.
pub(crate) extern "C" fn dispatch_queue_create(_label: usize, attr: usize) -> usize {
    let queue = leak([0usize; 16]);
    if attr == 0 {
        let state: &'static SerialQueue = Box::leak(Box::new(SerialQueue { jobs: Mutex::new(Jobs::default()) }));
        SERIAL_QUEUES.lock().unwrap().get_or_insert_with(Default::default).insert(queue, state);
    } else {
        SUSPENSIONS.lock().unwrap().get_or_insert_with(Default::default).concurrent.insert(queue, 0);
    }
    queue
}

fn mark_done(done: &(Mutex<bool>, Condvar)) {
    *done.0.lock().unwrap() = true;
    done.1.notify_all();
}

fn wait_for(done: &(Mutex<bool>, Condvar)) {
    let mut finished = done.0.lock().unwrap();
    while !*finished {
        finished = done.1.wait(finished).unwrap();
    }
}

fn enqueue(queue: usize, state: &'static SerialQueue, job: Job) {
    let mut jobs = state.jobs.lock().unwrap();
    jobs.waiting.push_back(job);
    start_worker(queue, state, jobs);
}

/// Starts the queue's worker unless one runs, the queue is suspended or empty.
fn start_worker(queue: usize, state: &'static SerialQueue, mut jobs: std::sync::MutexGuard<'_, Jobs>) {
    if jobs.running || jobs.suspended > 0 || jobs.waiting.is_empty() {
        return;
    }
    jobs.running = true;
    drop(jobs);
    std::thread::spawn(move || {
        CURRENT_RUNNER.with(|c| c.set(queue));
        loop {
            let job = {
                let mut jobs = state.jobs.lock().unwrap();
                let next = if jobs.suspended == 0 { jobs.waiting.pop_front() } else { None };
                match next {
                    Some(job) => job,
                    None => {
                        jobs.running = false;
                        return;
                    }
                }
            };
            if let Some(job) = try_hold(job, queue) {
                run(job);
            }
        }
    });
}

fn is_suspended(suspensions: &Suspensions, queue: usize) -> bool {
    match serial_queue(queue) {
        Some(state) => state.jobs.lock().unwrap().suspended > 0,
        None => suspensions.concurrent.get(&queue).is_some_and(|&count| count > 0),
    }
}

/// Holds a job (run by `runner`) if a queue on its way is suspended: the
/// queue it was sent to or any target after it, serial or concurrent. A
/// serial runner's own suspension is kept by its worker. Gives the job back
/// if it may run.
fn try_hold(job: Job, runner: usize) -> Option<Job> {
    if ANY_SUSPENDED.load(Ordering::SeqCst) == 0 {
        return Some(job);
    }
    let mut suspensions = SUSPENSIONS.lock().unwrap();
    try_hold_locked(suspensions.get_or_insert_with(Default::default), job, runner)
}

fn try_hold_locked(suspensions: &mut Suspensions, job: Job, runner: usize) -> Option<Job> {
    let mut at = job.sent_to;
    for _ in 0..32 {
        let own_worker_keeps_it = at == runner && serial_queue(at).is_some();
        if !own_worker_keeps_it && is_suspended(suspensions, at) {
            let held = suspensions.held.entry(at).or_default();
            let place = held.partition_point(|other| other.order <= job.order);
            held.insert(place, job);
            return None;
        }
        match TARGETS.lock().unwrap().as_ref().and_then(|t| t.get(&at)).copied() {
            Some(target) => at = target,
            None => break,
        }
    }
    Some(job)
}

/// dispatch_suspend on a queue the game made. Apple ignores it for the main
/// and global queues. False: not a queue the game made.
pub(crate) fn suspend_queue(queue: usize) -> bool {
    let mut suspensions = SUSPENSIONS.lock().unwrap();
    let suspensions = suspensions.get_or_insert_with(Default::default);
    let count = if let Some(state) = serial_queue(queue) {
        let mut jobs = state.jobs.lock().unwrap();
        jobs.suspended += 1;
        jobs.suspended
    } else {
        match suspensions.concurrent.get_mut(&queue) {
            Some(count) => {
                *count += 1;
                *count
            }
            None => return false,
        }
    };
    if count == 1 {
        ANY_SUSPENDED.fetch_add(1, Ordering::SeqCst);
    }
    true
}

/// The held blocks are sent on before the lock is let go, so blocks sent
/// after this can't pass them.
pub(crate) fn resume_queue(queue: usize) -> bool {
    let mut suspensions = SUSPENSIONS.lock().unwrap();
    let suspensions = suspensions.get_or_insert_with(Default::default);
    let still_suspended = if let Some(state) = serial_queue(queue) {
        let mut jobs = state.jobs.lock().unwrap();
        if jobs.suspended == 0 {
            crate::log!("dispatch_resume on a queue that wasn't suspended; ignored");
            return true;
        }
        jobs.suspended -= 1;
        jobs.suspended > 0
    } else {
        match suspensions.concurrent.get_mut(&queue) {
            None => return false,
            Some(0) => {
                crate::log!("dispatch_resume on a queue that wasn't suspended; ignored");
                return true;
            }
            Some(count) => {
                *count -= 1;
                *count > 0
            }
        }
    };
    if still_suspended {
        return true;
    }
    for job in suspensions.held.remove(&queue).unwrap_or_default() {
        submit_with(Some(&mut *suspensions), job);
    }
    if let Some(state) = serial_queue(queue) {
        start_worker(queue, state, state.jobs.lock().unwrap());
    }
    ANY_SUSPENDED.fetch_sub(1, Ordering::SeqCst);
    true
}

/// Sends a (copied) block to its queue; `done` is marked once it has run.
fn submit(job: Job) {
    submit_with(None, job);
}

fn submit_with(locked: Option<&mut Suspensions>, job: Job) {
    let queue = job.sent_to;
    let runner = runs_on(queue);
    let job = match locked {
        Some(suspensions) => try_hold_locked(suspensions, job, runner),
        None => try_hold(job, runner),
    };
    let Some(job) = job else { return };
    if queue == main_queue() && job.done.is_none() {
        crate::fw::runloop::post(crate::fw::runloop::Work::Block(job.block));
    } else if runner == main_queue() {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            if let Some(job) = try_hold(job, runner) {
                run(job);
            }
        })));
    } else if let Some(state) = serial_queue(runner) {
        enqueue(runner, state, job);
    } else {
        std::thread::spawn(move || {
            if let Some(job) = try_hold(job, runner) {
                CURRENT_QUEUE.with(|c| c.set(queue));
                let pool = crate::fw::counted::pool_mark();
                unsafe { call_block(job.block) };
                crate::fw::counted::pool_drain(pool);
                if let Some(done) = job.done {
                    mark_done(&done);
                }
            }
        });
    }
}

pub(crate) extern "C" fn dispatch_async(queue: usize, block: usize) {
    let copy = block_copy(block as *mut BlockHeader) as usize;
    submit(job(copy, queue, None));
}

/// Runs the block on the queue's turn and waits for it.
extern "C" fn dispatch_sync(queue: usize, block: usize) {
    let inline = || run_as(queue, block);
    let runner = runs_on(queue);
    let done: Done = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
    if runner == main_queue() {
        if crate::fw::runloop::is_main_thread() {
            return inline();
        }
        submit(job(block, queue, Some(done.clone())));
        wait_for(&done);
    } else if serial_queue(runner).is_some() {
        if CURRENT_RUNNER.with(|c| c.get()) == runner {
            // On Apple this waits forever; running it here keeps the game going.
            crate::log!("dispatch_sync onto the queue already running; ran it directly");
            return inline();
        }
        submit(job(block, queue, Some(done.clone())));
        wait_for(&done);
    } else {
        match try_hold(job(block, queue, Some(done.clone())), runner) {
            None => wait_for(&done),
            Some(_) => inline(),
        }
    }
}

extern "C" fn dispatch_after(when: u64, queue: usize, block: usize) {
    let copy = block_copy(block as *mut BlockHeader) as usize;
    std::thread::spawn(move || {
        let now = now_nanos();
        match deadline_nanos(when) {
            Some(at) if at > now => std::thread::sleep(std::time::Duration::from_nanos(at - now)),
            Some(_) => {}
            None => return,
        }
        dispatch_async(queue, copy);
    });
}

extern "C" fn dispatch_noop(_object: usize) {}

/// A null target means the default (a global queue): no target.
extern "C" fn dispatch_set_target_queue(object: usize, queue: usize) {
    if object == 0 || object == main_queue() || object == queue {
        return;
    }
    if crate::libsystem::timers::retarget(object, queue) {
        return;
    }
    let mut targets = TARGETS.lock().unwrap();
    let targets = targets.get_or_insert_with(Default::default);
    if queue == 0 {
        targets.remove(&object);
    } else {
        targets.insert(object, queue);
    }
}

struct Semaphore {
    count: Mutex<i64>,
    wake: Condvar,
}

extern "C" fn dispatch_semaphore_create(value: i64) -> usize {
    leak(Semaphore { count: Mutex::new(value), wake: Condvar::new() })
}

extern "C" fn dispatch_semaphore_signal(semaphore: usize) -> i64 {
    let semaphore = unsafe { &*(semaphore as *const Semaphore) };
    *semaphore.count.lock().unwrap() += 1;
    semaphore.wake.notify_one();
    0
}

extern "C" fn dispatch_semaphore_wait(semaphore: usize, timeout: u64) -> i64 {
    let semaphore = unsafe { &*(semaphore as *const Semaphore) };
    let mut count = semaphore.count.lock().unwrap();
    let deadline = deadline_nanos(timeout);
    while *count <= 0 {
        match deadline {
            None => count = semaphore.wake.wait(count).unwrap(),
            Some(at) => {
                let now = now_nanos();
                if now >= at {
                    return 1;
                }
                count = semaphore.wake.wait_timeout(count, std::time::Duration::from_nanos(at - now)).unwrap().0;
            }
        }
    }
    *count -= 1;
    0
}

extern "C" fn dispatch_time(when: u64, delta: i64) -> u64 {
    if when == !0 {
        return !0;
    }
    if (when as i64) < 0 {
        // A wall-clock time (negated nanoseconds since 1970).
        return wall_time((when as i64).wrapping_neg() as u64, delta);
    }
    let base = if when == 0 { now_nanos() } else { when };
    (base as i64 + delta) as u64
}

/// Apple's wall-clock dispatch time: the nanoseconds since 1970, negated.
pub(crate) fn wall_time(nanos: u64, delta: i64) -> u64 {
    let total = (nanos as i64).saturating_add(delta);
    if total <= 1 {
        // -1 and -0 mean "forever" and "now" (Apple's rule).
        return if delta >= 0 { !0 } else { (-2i64) as u64 };
    }
    total.wrapping_neg() as u64
}

// ----- dynamic loading -----

#[repr(C)]
struct DlInfo {
    file_name: usize,
    file_base: usize,
    symbol_name: usize,
    symbol_address: usize,
}

extern "C" fn ios_dladdr(address: usize, info: *mut DlInfo) -> i32 {
    let Some(loaded) = crate::loader::try_loaded() else { return 0 };
    if address < loaded.start || address >= loaded.end {
        return 0;
    }
    static PATH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let path = *PATH.get_or_init(|| crate::symbols::leak_cstr(&crate::fw::paths::apple_executable_path()));
    unsafe {
        *info = DlInfo { file_name: path, file_base: loaded.start, symbol_name: 0, symbol_address: 0 };
    }
    1
}

extern "C" fn ios_dlopen(path: *const libc::c_char, _mode: i32) -> usize {
    let path = unsafe { crate::objc::runtime::cstr(path as usize) };
    crate::log!("dlopen {path}: handing back our shared stand-in");
    1
}

extern "C" fn ios_dlsym(_handle: usize, name: *const libc::c_char) -> usize {
    let name = unsafe { crate::objc::runtime::cstr(name as usize) };
    match crate::symbols::lookup(&format!("_{name}")) {
        Some((address, _)) => address,
        None => {
            crate::log!("dlsym {name}: not available");
            0
        }
    }
}

extern "C" fn dyld_register_add_image(callback: usize) {
    let Some(loaded) = crate::loader::try_loaded() else { return };
    let f: extern "C" fn(usize, isize) = unsafe { std::mem::transmute(callback) };
    f(loaded.start, loaded.slide as isize);
}

extern "C" fn dyld_register_remove_image(_callback: usize) {}

extern "C" fn ns_version_of_link_time_library(name: *const libc::c_char) -> i32 {
    let name = unsafe { crate::objc::runtime::cstr(name as usize) };
    crate::log!("NSVersionOfLinkTimeLibrary {name}: answered -1");
    -1
}

pub fn register(r: &mut Registry) {
    r.function("mach_absolute_time", mach_absolute_time as usize);
    r.function("mach_timebase_info", mach_timebase_info as usize);
    r.function("mach_wait_until", mach_wait_until as usize);
    r.function("mach_host_self", mach_host_self as usize);
    r.function("mach_thread_self", mach_thread_self as usize);
    r.function("host_page_size", host_page_size as usize);
    r.function("host_statistics", host_statistics as usize);
    r.function("task_info", task_info as usize);
    r.function("vm_allocate", vm_allocate as usize);
    r.function("vm_deallocate", vm_deallocate as usize);
    r.function("vm_protect", vm_protect as usize);
    r.function("vm_read_overwrite", vm_read_overwrite as usize);
    r.function("thread_policy_set", thread_policy_set as usize);
    r.function("mach_port_deallocate", mach_port_deallocate as usize);
    r.function("mach_port_mod_refs", mach_port_mod_refs as usize);
    macro_rules! refused {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), $name as usize); )* };
    }
    refused!(
        mach_port_allocate, mach_port_insert_right, mach_port_move_member, mach_port_request_notification,
        mach_msg, task_get_exception_ports, task_swap_exception_ports, thread_get_exception_ports,
        thread_swap_exception_ports, task_threads, thread_info, exception_raise, exception_raise_state,
        exception_raise_state_identity, vm_map, mach_make_memory_entry_64
    );
    r.data("mach_task_self_", leak(0x103u32));
    r.data("vm_page_size", leak(16384usize));
    r.data("NDR_record", leak([0u8, 0, 0, 0, 1, 0, 0, 0]));
    r.function("_Block_copy", block_copy as usize);
    r.function("_Block_release", block_release as usize);
    r.function("_Block_object_assign", block_object_assign as usize);
    r.function("_Block_object_dispose", block_object_dispose as usize);
    r.function("dispatch_once", dispatch_once as usize);
    r.function("dispatch_once_f", dispatch_once_f as usize);
    r.function("dispatch_get_global_queue", dispatch_get_global_queue as usize);
    r.function("dispatch_get_current_queue", dispatch_get_current_queue as usize);
    r.function("dispatch_queue_create", dispatch_queue_create as usize);
    r.function("dispatch_async", dispatch_async as usize);
    r.function("dispatch_sync", dispatch_sync as usize);
    r.function("dispatch_after", dispatch_after as usize);
    r.function("dispatch_release", dispatch_noop as usize);
    r.function("dispatch_retain", dispatch_noop as usize);
    r.function("dispatch_set_target_queue", dispatch_set_target_queue as usize);
    r.function("dispatch_semaphore_create", dispatch_semaphore_create as usize);
    r.function("dispatch_semaphore_signal", dispatch_semaphore_signal as usize);
    r.function("dispatch_semaphore_wait", dispatch_semaphore_wait as usize);
    r.function("dispatch_time", dispatch_time as usize);
    r.data("_dispatch_main_q", main_queue());
    r.function("dladdr", ios_dladdr as usize);
    r.function("dlopen", ios_dlopen as usize);
    r.function("dlsym", ios_dlsym as usize);
    r.function("_dyld_register_func_for_add_image", dyld_register_add_image as usize);
    r.function("_dyld_register_func_for_remove_image", dyld_register_remove_image as usize);
    r.function("NSVersionOfLinkTimeLibrary", ns_version_of_link_time_library as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Giving back the send right mach_thread_self (or the task or host) handed out works,
    /// as on Apple, where each call adds a reference the caller drops later.
    #[test]
    fn thread_and_task_ports_can_be_let_go() {
        let port_function = |name: &str| {
            let mut r = crate::symbols::Registry::default();
            crate::libsystem::register(&mut r);
            r.get(name).unwrap().0
        };
        let deallocate: extern "C" fn(u32, u32) -> i32 = unsafe { std::mem::transmute(port_function("mach_port_deallocate")) };
        let mod_refs: extern "C" fn(u32, u32, u32, i32) -> i32 = unsafe { std::mem::transmute(port_function("mach_port_mod_refs")) };
        let task = 0x103;
        for port in [mach_thread_self(), mach_host_self(), task, 0, u32::MAX] {
            assert_eq!(deallocate(task, port), KERN_SUCCESS, "mach_port_deallocate({port:#x}) failed");
        }
        assert_eq!(mod_refs(task, mach_thread_self(), 0, -1), KERN_SUCCESS, "dropping a send right failed");
        assert_eq!(mod_refs(task, mach_thread_self(), 1, -1), 17, "a receive right that was never made was dropped");
    }

    /// A global block carrying a number and the queue it should report.
    #[repr(C)]
    struct TestBlock {
        header: BlockHeader,
        id: usize,
        queue: usize,
    }

    static INSIDE: AtomicUsize = AtomicUsize::new(0);
    static MOST_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    static WRONG_QUEUE: AtomicUsize = AtomicUsize::new(0);
    static ORDER: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    extern "C" fn record(block: usize) {
        let block = unsafe { &*(block as *const TestBlock) };
        let now = INSIDE.fetch_add(1, Ordering::SeqCst) + 1;
        MOST_INSIDE.fetch_max(now, Ordering::SeqCst);
        if dispatch_get_current_queue() != block.queue {
            WRONG_QUEUE.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        ORDER.lock().unwrap().push(block.id);
        INSIDE.fetch_sub(1, Ordering::SeqCst);
        FINISHED.fetch_add(1, Ordering::SeqCst);
    }

    fn block(id: usize, queue: usize) -> usize {
        let header = BlockHeader { isa: 0, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke: record as usize, descriptor: std::ptr::null() };
        Box::leak(Box::new(TestBlock { header, id, queue })) as *mut TestBlock as usize
    }

    #[test]
    fn queues_sharing_a_serial_target_take_turns() {
        let target = dispatch_queue_create(0, 0);
        let (a, b) = (dispatch_queue_create(0, 0), dispatch_queue_create(0, 1));
        dispatch_set_target_queue(a, target);
        dispatch_set_target_queue(b, target);
        for i in 0..30 {
            let queue = [a, b, target][i % 3];
            dispatch_async(queue, block(i, queue));
        }
        // A synchronous block waits its turn behind the others.
        dispatch_sync(a, block(100, a));
        assert_eq!(FINISHED.load(Ordering::SeqCst), 31);
        assert_eq!(MOST_INSIDE.load(Ordering::SeqCst), 1, "blocks overlapped");
        assert_eq!(WRONG_QUEUE.load(Ordering::SeqCst), 0, "dispatch_get_current_queue named the wrong queue");
        let order = ORDER.lock().unwrap().clone();
        let mut expected: Vec<usize> = (0..30).collect();
        expected.push(100);
        assert_eq!(order, expected, "blocks ran out of order");

        // A serial queue whose target is a suspended concurrent queue holds its
        // blocks, and runs them in sending order after resume.
        let concurrent = dispatch_queue_create(0, 1);
        let serial = dispatch_queue_create(0, 0);
        dispatch_set_target_queue(serial, concurrent);
        assert!(suspend_queue(concurrent));
        ORDER.lock().unwrap().clear();
        for i in 200..205 {
            dispatch_async(serial, block(i, serial));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(ORDER.lock().unwrap().is_empty(), "a block ran through a suspended concurrent queue");
        assert!(resume_queue(concurrent));
        dispatch_sync(serial, block(205, serial));
        assert_eq!(*ORDER.lock().unwrap(), (200..206).collect::<Vec<_>>(), "held blocks ran out of order");
        assert_eq!(ANY_SUSPENDED.load(Ordering::SeqCst), 0);
    }
}
