//! Apple kernel (Mach) calls, dispatch queues, blocks and dynamic loading.

use crate::symbols::{leak, Registry};
use std::sync::{Condvar, Mutex};

const KERN_SUCCESS: i32 = 0;
const KERN_FAILURE: i32 = 5;

fn now_nanos() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
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

extern "C" fn vm_allocate(_task: u32, address: *mut usize, size: usize, flags: i32) -> i32 {
    unsafe {
        let fixed = flags & 1 == 0;
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
    unsafe { libc::munmap(address as *mut _, size) };
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
    mach_port_allocate, mach_port_deallocate, mach_port_insert_right, mach_port_mod_refs,
    mach_port_move_member, mach_port_request_notification, mach_msg, task_get_exception_ports,
    task_swap_exception_ports, thread_get_exception_ports, thread_swap_exception_ports, task_threads,
    thread_suspend, thread_resume, thread_get_state, thread_info, exception_raise,
    exception_raise_state, exception_raise_state_identity, vm_map, mach_make_memory_entry_64
);

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
            _ => *destination = object,
        }
    }
}

extern "C" fn block_object_dispose(_object: usize, _flags: i32) {}

pub unsafe fn call_block(block: usize) {
    let header = block as *const BlockHeader;
    let invoke: extern "C" fn(usize) = std::mem::transmute((*header).invoke);
    invoke(block);
}

// ----- dispatch -----

/// Work waiting for the main thread's run loop (step 2 runs it).
pub static MAIN_QUEUE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

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

extern "C" fn dispatch_get_global_queue(_priority: i64, _flags: u64) -> usize {
    *GLOBAL_QUEUE_OBJECT.get_or_init(|| leak([0usize; 16]))
}

extern "C" fn dispatch_get_current_queue() -> usize {
    main_queue()
}

extern "C" fn dispatch_queue_create(_label: usize, _attr: usize) -> usize {
    leak([0usize; 16])
}

extern "C" fn dispatch_async(queue: usize, block: usize) {
    let copy = block_copy(block as *mut BlockHeader) as usize;
    if queue == main_queue() {
        MAIN_QUEUE.lock().unwrap().push(copy);
    } else {
        std::thread::spawn(move || unsafe { call_block(copy) });
    }
}

extern "C" fn dispatch_sync(_queue: usize, block: usize) {
    unsafe { call_block(block) };
}

extern "C" fn dispatch_after(_when: u64, queue: usize, block: usize) {
    dispatch_async(queue, block);
}

extern "C" fn dispatch_noop(_object: usize) {}

extern "C" fn dispatch_set_target_queue(_object: usize, _queue: usize) {}

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
    let deadline = if timeout == !0 { None } else { Some(timeout) };
    while *count <= 0 {
        match deadline {
            None => count = semaphore.wake.wait(count).unwrap(),
            Some(0) => return 1,
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
    let base = if when == 0 { now_nanos() } else { when };
    (base as i64 + delta) as u64
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
    macro_rules! refused {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), $name as usize); )* };
    }
    refused!(
        mach_port_allocate, mach_port_deallocate, mach_port_insert_right, mach_port_mod_refs,
        mach_port_move_member, mach_port_request_notification, mach_msg, task_get_exception_ports,
        task_swap_exception_ports, thread_get_exception_ports, thread_swap_exception_ports, task_threads,
        thread_suspend, thread_resume, thread_get_state, thread_info, exception_raise,
        exception_raise_state, exception_raise_state_identity, vm_map, mach_make_memory_entry_64
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
