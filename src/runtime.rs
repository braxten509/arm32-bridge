//! The 32-bit CPUs (Unicorn) plus the bridge between guest code and host
//! functions.
//!
//! Every phone function the game imports gets an 8-byte ARM stand-in in the
//! stub area: `svc #index; bx lr`. When guest code calls it, the `svc` traps
//! into `on_interrupt`, which reads the guest's arguments (AAPCS softfp: r0-r3
//! then the stack), runs the Rust handler, and writes the result to r0/r1.
//! The `bx lr` then returns to the caller as if a real function had run.
//!
//! Threads: every host thread that runs guest code gets its own Unicorn
//! engine, all mapping the same guest memory. Only one of them runs guest
//! code at a time (the guest lock, like Python's GIL); a thread hands the
//! lock over whenever it blocks (mutex, condition variable, sleep, a call
//! into Java). Switching only ever happens inside a handler, never between
//! two guest instructions, so the guest's ldrex/strex atomics stay atomic.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use unicorn_engine::{ArmCpuModel, Arch, HookType, MemType, Mode, Prot, RegisterARM, Unicorn};

use crate::mem::{self, Bump, GuestHeap, GuestMem};

pub type Uc = Unicorn<'static, Thread>;

/// Register access for handlers, whichever engine is running the guest.
/// Registers 0-15 are r0-r15 (13 = sp, 14 = lr, 15 = pc).
pub trait GuestCpu {
    fn reg(&self, index: usize) -> u32;
    fn set_reg(&mut self, index: usize, value: u32);
    /// VFP/NEON register d0-d31.
    fn dreg(&self, index: usize) -> u64;
    fn set_dreg(&mut self, index: usize, value: u64);
    /// Ends the current guest run (after a fault or exit()).
    fn stop(&mut self);
}

const CORE_REGISTERS: [RegisterARM; 16] = [
    RegisterARM::R0,
    RegisterARM::R1,
    RegisterARM::R2,
    RegisterARM::R3,
    RegisterARM::R4,
    RegisterARM::R5,
    RegisterARM::R6,
    RegisterARM::R7,
    RegisterARM::R8,
    RegisterARM::R9,
    RegisterARM::R10,
    RegisterARM::R11,
    RegisterARM::R12,
    RegisterARM::SP,
    RegisterARM::LR,
    RegisterARM::PC,
];

const DOUBLE_REGISTERS: [RegisterARM; 32] = [
    RegisterARM::D0,
    RegisterARM::D1,
    RegisterARM::D2,
    RegisterARM::D3,
    RegisterARM::D4,
    RegisterARM::D5,
    RegisterARM::D6,
    RegisterARM::D7,
    RegisterARM::D8,
    RegisterARM::D9,
    RegisterARM::D10,
    RegisterARM::D11,
    RegisterARM::D12,
    RegisterARM::D13,
    RegisterARM::D14,
    RegisterARM::D15,
    RegisterARM::D16,
    RegisterARM::D17,
    RegisterARM::D18,
    RegisterARM::D19,
    RegisterARM::D20,
    RegisterARM::D21,
    RegisterARM::D22,
    RegisterARM::D23,
    RegisterARM::D24,
    RegisterARM::D25,
    RegisterARM::D26,
    RegisterARM::D27,
    RegisterARM::D28,
    RegisterARM::D29,
    RegisterARM::D30,
    RegisterARM::D31,
];

/// The translator's registers, when they are the live ones.
fn live_native(uc: &Uc) -> Option<&crate::dynarec::CpuState> {
    uc.get_data().native.as_ref().filter(|native| native.active).map(|native| &native.state)
}

fn live_native_mut(uc: &mut Uc) -> Option<&mut crate::dynarec::CpuState> {
    uc.get_data_mut().native.as_mut().filter(|native| native.active).map(|native| &mut native.state)
}

impl GuestCpu for Uc {
    fn reg(&self, index: usize) -> u32 {
        match live_native(self) {
            Some(state) => state.r[index],
            None => self.reg_read(CORE_REGISTERS[index]).unwrap() as u32,
        }
    }
    fn set_reg(&mut self, index: usize, value: u32) {
        match live_native_mut(self) {
            Some(state) => state.r[index] = value,
            None => self.reg_write(CORE_REGISTERS[index], value as u64).unwrap(),
        }
    }
    fn dreg(&self, index: usize) -> u64 {
        match live_native(self) {
            Some(state) => state.dreg(index),
            None => self.reg_read(DOUBLE_REGISTERS[index]).unwrap(),
        }
    }
    fn set_dreg(&mut self, index: usize, value: u64) {
        match live_native_mut(self) {
            Some(state) => state.set_dreg(index, value),
            None => self.reg_write(DOUBLE_REGISTERS[index], value).unwrap(),
        }
    }
    fn stop(&mut self) {
        // The translator checks `fault` / `exit_code` after every phone function.
        if live_native(self).is_none() {
            self.emu_stop().unwrap();
        }
    }
}

pub fn double_register(index: usize) -> RegisterARM {
    DOUBLE_REGISTERS[index]
}

/// ARM32_STATS=1 (or the Android property debug.arm32bridge.stats=1): count
/// translator fallbacks and print them at the end (and every so often in an app).
pub fn stats_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ARM32_STATS").ok().or_else(|| crate::dynarec::property("debug.arm32bridge.stats")).as_deref() == Some("1")
    })
}

pub const SP: usize = 13;
pub const LR: usize = 14;
pub const PC: usize = 15;

/// What a handler hands back to the guest.
pub enum Ret {
    Void,
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    /// The handler already set the registers itself (for example longjmp).
    Jumped,
}

pub type Handler = fn(&mut Uc, &mut Args) -> Ret;

pub struct Stub {
    /// Made once per stub and never freed, so it can be passed around for free.
    pub name: &'static str,
    pub handler: Option<Handler>,
    pub calls: u64,
}

/// Return traps: one per nesting depth, so a host→guest call made from
/// inside a handler stops at its own return address.
const MAX_DEPTH: u32 = 64;
pub const FIRST_STUB: u32 = MAX_DEPTH;
const SVC: u32 = 0xEF00_0000;
const BX_LR: u32 = 0xE12F_FF1E;
const UDF: u32 = 0xE7F0_00F0;

pub const MAIN_STACK_SIZE: u32 = 8 << 20;
pub const THREAD_STACK_SIZE: u32 = 1 << 20;
/// Phone-function calls at least this slow (in ms) are logged as "[slow]".
const SLOW_CALL_MS: u64 = 5;

/// A cheap timestamp: the CPU's own counter on ARM64 (a clock call costs far
/// more, and every phone-function call is timed).
#[cfg(target_arch = "aarch64")]
fn ticks() -> u64 {
    let value: u64;
    unsafe { std::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack)) };
    value
}

#[cfg(target_arch = "aarch64")]
fn ticks_per_ms() -> u64 {
    let frequency: u64;
    unsafe { std::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency, options(nomem, nostack)) };
    (frequency / 1000).max(1)
}

#[cfg(not(target_arch = "aarch64"))]
fn ticks() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

#[cfg(not(target_arch = "aarch64"))]
fn ticks_per_ms() -> u64 {
    1_000_000
}

/// State shared by every guest thread (guarded by the guest lock).
pub struct Shared {
    pub heap: GuestHeap,
    pub misc: Bump,
    pub stubs: Vec<Stub>,
    pub stub_by_name: HashMap<String, u32>,
    /// Exported symbols of loaded guest libraries (name → guest address).
    pub guest_exports: HashMap<String, u32>,
    pub trace: bool,
    /// Order in which phone functions were first used.
    pub first_use: Vec<u32>,
    /// Names called that have no handler yet (they return 0).
    pub missing: Vec<String>,
    pub recent: VecDeque<&'static str>,
    pub next_thread_id: u32,
    next_stack_top: u32,
    pub state: crate::imports::State,
}

impl Shared {
    pub fn misc_alloc(&mut self, size: u32, align: u32) -> u32 {
        self.misc.alloc(size, align)
    }
    pub fn misc_cstr(&mut self, text: &str) -> u32 {
        let guest = self.misc.alloc(text.len() as u32 + 1, 1);
        mem().write_bytes(guest, text.as_bytes());
        mem().write_u8(guest + text.len() as u32, 0);
        guest
    }
    fn alloc_stack(&mut self, size: u32) -> u32 {
        let top = self.next_stack_top;
        assert!(top - size >= mem::STACK_BASE, "out of guest stack space");
        self.next_stack_top = top - size - mem::PAGE;
        top
    }
}

/// Per-thread state: the data attached to each Unicorn engine.
pub struct Thread {
    pub id: u32,
    pub depth: u32,
    pub fault: Option<String>,
    pub exit_code: Option<i32>,
    pub errno_slot: u32,
    pub tls_block: u32,
    pub stack_top: u32,
    /// Name of the phone function being handled right now.
    pub current_call: &'static str,
    /// Translator state, once this thread has run translated code.
    pub native: Option<Box<crate::dynarec::Native>>,
}

struct GuardedShared(RefCell<Option<Shared>>);
// Only touched while holding the guest lock.
unsafe impl Sync for GuardedShared {}

static SHARED: GuardedShared = GuardedShared(RefCell::new(None));
static MEMORY: OnceLock<GuestMem> = OnceLock::new();

pub fn mem() -> GuestMem {
    *MEMORY.get().expect("runtime::init not called")
}

/// The shared state. Never hold the result across a call into guest code.
pub fn shared() -> RefMut<'static, Shared> {
    debug_assert!(gil_held(), "shared state used without the guest lock");
    RefMut::map(SHARED.0.borrow_mut(), |s| s.as_mut().expect("runtime::init not called"))
}

/// Sets up guest memory and the shared state. Safe to call more than once.
pub fn init(trace: bool) {
    if MEMORY.get().is_some() {
        return;
    }
    let memory = GuestMem::reserve();
    memory.write_kernel_helpers();
    MEMORY.set(memory).ok();
    for depth in 0..MAX_DEPTH {
        memory.write_u32(trap_address(depth), UDF);
        memory.write_u32(trap_address(depth) + 4, UDF);
    }
    let _lock = hold_gil();
    *SHARED.0.borrow_mut() = Some(Shared {
        heap: GuestHeap::new(memory),
        misc: Bump::new(mem::MISC_BASE, mem::MISC_END),
        stubs: Vec::new(),
        stub_by_name: HashMap::new(),
        guest_exports: HashMap::new(),
        trace,
        first_use: Vec::new(),
        missing: Vec::new(),
        recent: Default::default(),
        next_thread_id: 1,
        next_stack_top: mem::STACK_TOP,
        state: Default::default(),
    });
}

// ---- the guest lock ----

static GIL: Mutex<()> = Mutex::new(());

thread_local! {
    static GIL_GUARD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    static ENGINE: Cell<*mut Uc> = const { Cell::new(std::ptr::null_mut()) };
    /// Time this thread spent inside phone functions (outermost calls only), in `ticks`.
    static PHONE_TICKS: Cell<u64> = const { Cell::new(0) };
    static PHONE_DEPTH: Cell<u32> = const { Cell::new(0) };
    static RESUME_AT: Cell<Option<u32>> = const { Cell::new(None) };
    static HELD_SINCE: Cell<u64> = const { Cell::new(0) };
    static LOCK_WAIT_TICKS: Cell<u64> = const { Cell::new(0) };
}

pub fn lock_wait_time() -> std::time::Duration {
    std::time::Duration::from_nanos(LOCK_WAIT_TICKS.with(|t| t.get()) * 1_000_000 / ticks_per_ms())
}

static LOCKS_TAKEN: AtomicU64 = AtomicU64::new(0);
static WANTING: AtomicU32 = AtomicU32::new(0);
const TIME_SLICE_MS: u64 = 2;
/// When the current holder took the guest lock (`ticks`), for the preempt ticker.
static HELD_SINCE_ANY: AtomicU64 = AtomicU64::new(0);
/// Set while a guest thread has waited a whole time slice for the lock: translated
/// code then leaves at its next block exit and the dispatcher hands the lock over
/// (a loop that never calls a phone function would otherwise never let go).
static PREEMPT: AtomicU32 = AtomicU32::new(0);

pub fn preempt_flag_address() -> u64 {
    PREEMPT.as_ptr() as u64
}

pub fn preempt_requested() -> bool {
    PREEMPT.load(Ordering::Relaxed) != 0
}

/// Started the first time a thread has to wait for the lock: sets PREEMPT when
/// the holder has had the lock for a whole time slice while others wait.
fn start_preempt_ticker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("arm32-preempt".into()).spawn(|| loop {
            if WANTING.load(Ordering::Relaxed) == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let held = ticks().wrapping_sub(HELD_SINCE_ANY.load(Ordering::Relaxed));
            if held >= TIME_SLICE_MS * ticks_per_ms() {
                PREEMPT.store(1, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(1));
        });
    });
}

fn take_gil() {
    let guard = match GIL.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            start_preempt_ticker();
            let started = ticks();
            WANTING.fetch_add(1, Ordering::Relaxed);
            let guard = GIL.lock().unwrap_or_else(|e| e.into_inner());
            WANTING.fetch_sub(1, Ordering::Relaxed);
            LOCK_WAIT_TICKS.with(|t| t.set(t.get() + ticks().wrapping_sub(started)));
            guard
        }
    };
    LOCKS_TAKEN.fetch_add(1, Ordering::Relaxed);
    let now = ticks();
    HELD_SINCE.with(|since| since.set(now));
    HELD_SINCE_ANY.store(now, Ordering::Relaxed);
    PREEMPT.store(0, Ordering::Relaxed);
    GIL_GUARD.with(|g| *g.borrow_mut() = Some(guard));
}

pub fn resume_point() -> Option<u32> {
    RESUME_AT.with(|r| r.get())
}

fn hand_over_gil() -> bool {
    if WANTING.load(Ordering::Relaxed) == 0 {
        PREEMPT.store(0, Ordering::Relaxed);
        return false;
    }
    let Some(guard) = GIL_GUARD.with(|g| g.borrow_mut().take()) else { return false };
    let taken = LOCKS_TAKEN.load(Ordering::Relaxed);
    drop(guard);
    let started = Instant::now();
    while LOCKS_TAKEN.load(Ordering::Relaxed) == taken && started.elapsed() < Duration::from_micros(100) {
        std::thread::yield_now();
    }
    take_gil();
    true
}

pub fn share_gil(resume: u32) {
    if hand_over_gil() {
        RESUME_AT.with(|r| r.set(Some(resume)));
        crate::imports::signals::deliver_pending();
        RESUME_AT.with(|r| r.set(None));
    }
}

fn share_gil_after_time_slice() {
    if WANTING.load(Ordering::Relaxed) != 0
        && ticks().wrapping_sub(HELD_SINCE.with(|since| since.get())) >= TIME_SLICE_MS * ticks_per_ms()
    {
        hand_over_gil();
    }
}

/// Total time this thread has spent inside phone functions (a phone function
/// that calls back into game code counts that game code too).
pub fn phone_time() -> std::time::Duration {
    std::time::Duration::from_nanos(PHONE_TICKS.with(|t| t.get()) * 1_000_000 / ticks_per_ms())
}

pub fn gil_held() -> bool {
    GIL_GUARD.with(|g| g.borrow().is_some())
}

/// Holds the guest lock until dropped (does nothing if already held).
pub struct GilToken(bool);

impl Drop for GilToken {
    fn drop(&mut self) {
        if self.0 {
            GIL_GUARD.with(|g| g.borrow_mut().take());
        }
    }
}

pub fn hold_gil() -> GilToken {
    if gil_held() {
        return GilToken(false);
    }
    take_gil();
    GilToken(true)
}

/// Runs `work` with the guest lock released (for anything that may block).
pub fn without_gil<R>(work: impl FnOnce() -> R) -> R {
    let guard = GIL_GUARD.with(|g| g.borrow_mut().take());
    if guard.is_none() {
        return work();
    }
    drop(guard);
    let result = work();
    take_gil();
    crate::imports::signals::deliver_pending();
    result
}

struct Parker {
    woken: Mutex<bool>,
    ready: Condvar,
}

struct Waiter {
    thread: u32,
    key: u64,
    parker: Arc<Parker>,
}

static WAITERS: Mutex<Vec<Waiter>> = Mutex::new(Vec::new());

thread_local! {
    static PARKER: Arc<Parker> = Arc::new(Parker { woken: Mutex::new(false), ready: Condvar::new() });
}

/// Wait key for waits that any `wake_guests` ends.
pub const ANY_EVENT: u64 = u64::MAX;

/// Gives up the guest lock until another thread calls `wake_guests` (or the
/// timeout passes). Callers re-check their condition in a loop.
pub fn wait_for_guests(timeout: Option<Duration>) {
    wait_on(ANY_EVENT, timeout);
}

/// Gives up the guest lock until `wake_on(key)`, `wake_guests`, a signal for
/// this thread, or the timeout. Callers re-check their condition in a loop.
pub fn wait_on(key: u64, timeout: Option<Duration>) {
    // A signal sent before this thread started waiting runs instead of the wait.
    if crate::imports::signals::deliver_pending() {
        return;
    }
    let parker = PARKER.with(Arc::clone);
    *parker.woken.lock().unwrap_or_else(|e| e.into_inner()) = false;
    let thread = crate::imports::threads::current_handle();
    WAITERS.lock().unwrap_or_else(|e| e.into_inner()).push(Waiter { thread, key, parker: Arc::clone(&parker) });
    let guard = GIL_GUARD.with(|g| g.borrow_mut().take()).expect("waiting without the guest lock");
    drop(guard);
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut woken = parker.woken.lock().unwrap_or_else(|e| e.into_inner());
    while !*woken {
        woken = match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                parker.ready.wait_timeout(woken, deadline - now).unwrap_or_else(|e| e.into_inner()).0
            }
            None => parker.ready.wait(woken).unwrap_or_else(|e| e.into_inner()),
        };
    }
    drop(woken);
    WAITERS.lock().unwrap_or_else(|e| e.into_inner()).retain(|waiter| !Arc::ptr_eq(&waiter.parker, &parker));
    take_gil();
    // Signals sent to this thread while it waited run now.
    crate::imports::signals::deliver_pending();
}

fn wake_where(matches: impl Fn(&Waiter) -> bool) {
    let waiters = WAITERS.lock().unwrap_or_else(|e| e.into_inner());
    for waiter in waiters.iter().filter(|waiter| matches(waiter)) {
        *waiter.parker.woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
        waiter.parker.ready.notify_one();
    }
}

pub fn wake_guests() {
    wake_where(|_| true);
}

pub fn wake_on(key: u64) {
    wake_where(|waiter| waiter.key == key || waiter.key == ANY_EVENT);
}

pub fn wake_thread(thread: u32) {
    wake_where(|waiter| waiter.thread == thread);
}

// ---- engines ----

/// This thread's engine, created on first use. Needs the guest lock.
///
/// Re-entry on the same thread (guest → Java → native → guest) reuses the
/// same engine; Unicorn supports nested `emu_start` as long as it unwinds in
/// order, which a single thread's call stack guarantees.
pub fn engine() -> &'static mut Uc {
    assert!(gil_held(), "engine used without the guest lock");
    let existing = ENGINE.with(|e| e.get());
    if !existing.is_null() {
        return unsafe { &mut *existing };
    }
    let size = if shared().next_thread_id == 1 { MAIN_STACK_SIZE } else { THREAD_STACK_SIZE };
    let engine = Box::into_raw(Box::new(new_engine(size)));
    ENGINE.with(|e| e.set(engine));
    unsafe { &mut *engine }
}

/// Frees this thread's engine (guest threads call this when they finish).
pub fn drop_engine() {
    let existing = ENGINE.with(|e| e.replace(std::ptr::null_mut()));
    if !existing.is_null() {
        drop(unsafe { Box::from_raw(existing) });
    }
}

fn new_engine(stack_size: u32) -> Uc {
    let memory = mem();
    let (id, stack_top, errno_slot, tls_block) = {
        let mut shared = shared();
        let id = shared.next_thread_id;
        shared.next_thread_id += 1;
        let stack_top = shared.alloc_stack(stack_size);
        let errno_slot = shared.misc_alloc(4, 4);
        let tls_block = shared.misc_alloc(0x100, 8);
        (id, stack_top, errno_slot, tls_block)
    };
    let thread = Thread {
        id,
        depth: 0,
        fault: None,
        exit_code: None,
        errno_slot,
        tls_block,
        stack_top,
        current_call: "",
        native: None,
    };
    let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::ARM, thread).expect("start Unicorn");
    uc.ctl_set_cpu_model(ArmCpuModel::CORTEX_A15 as i32).expect("pick Cortex-A15 (ARMv7 + NEON)");
    for (start, end, code) in mem::MAPPED_REGIONS {
        let protection = if code { Prot::ALL } else { Prot::READ | Prot::WRITE };
        unsafe {
            uc.mem_map_ptr(start as u64, (end - start) as u64, protection, memory.host(start).cast())
                .expect("map guest region");
        }
    }
    // Turn on the floating point / NEON unit: CPACR full access to cp10+cp11, FPEXC.EN.
    uc.reg_write(RegisterARM::C1_C0_2, 0x00F0_0000).unwrap();
    uc.reg_write(RegisterARM::FPEXC, 0x4000_0000).unwrap();
    // bionic reads the thread pointer from TPIDRURO.
    uc.reg_write(RegisterARM::C13_C0_3, tls_block as u64).unwrap();
    uc.add_intr_hook(on_interrupt).expect("add stub hook");
    uc.add_mem_hook(HookType::MEM_INVALID, 0, u64::MAX, on_bad_memory).expect("add fault hook");
    uc
}

fn trap_address(depth: u32) -> u32 {
    mem::STUB_BASE + depth * 8
}

/// The guest address of the stand-in for `name`, creating it on first use.
pub fn stub_for(name: &str) -> u32 {
    let mut shared = shared();
    if let Some(&index) = shared.stub_by_name.get(name) {
        return mem::STUB_BASE + index * 8;
    }
    let index = FIRST_STUB + shared.stubs.len() as u32;
    assert!(index * 8 < mem::STUB_SIZE, "too many stubs");
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    shared.stubs.push(Stub { name: leaked, handler: crate::imports::handler_for(name), calls: 0 });
    shared.stub_by_name.insert(name.to_string(), index);
    let address = mem::STUB_BASE + index * 8;
    mem().write_u32(address, SVC | index);
    mem().write_u32(address + 4, BX_LR);
    address
}

/// Hooks get the engine with a borrowed lifetime; ours is always `'static`.
fn as_static<'a, 'b>(uc: &'a mut Unicorn<'b, Thread>) -> &'a mut Uc {
    unsafe { std::mem::transmute::<&'a mut Unicorn<'b, Thread>, &'a mut Uc>(uc) }
}

const EXCP_YIELD: u32 = 0x10004;

fn on_interrupt(uc: &mut Unicorn<'_, Thread>, interrupt: u32) {
    let uc = as_static(uc);
    if interrupt == EXCP_YIELD {
        let thumb = (uc.reg_read(RegisterARM::CPSR).unwrap() as u32 >> 5) & 1;
        share_gil(uc.pc_read().unwrap() as u32 | thumb);
        return;
    }
    let pc = uc.pc_read().unwrap() as u32;
    // EXCP_SWI is 2; the PC already points past the svc.
    let svc_address = pc.wrapping_sub(4);
    let in_stub_area = svc_address >= mem::STUB_BASE && svc_address < mem::STUB_BASE + mem::STUB_SIZE;
    if interrupt == 2 && !in_stub_area {
        // A Linux system call made straight from game code: number in r7,
        // arguments in r0-r5, result (negative errno) back in r0.
        let number = uc.reg_read(RegisterARM::R7).unwrap() as u32;
        let registers = [RegisterARM::R0, RegisterARM::R1, RegisterARM::R2, RegisterARM::R3, RegisterARM::R4, RegisterARM::R5];
        let arguments = registers.map(|register| uc.reg_read(register).unwrap() as u32);
        let result = crate::imports::threads::linux_syscall(uc, number, arguments);
        uc.reg_write(RegisterARM::R0, result as u32 as u64).unwrap();
        if uc.get_data().fault.is_some() || uc.get_data().exit_code.is_some() {
            uc.stop();
        }
        return;
    }
    if interrupt != 2 {
        let lr = uc.reg_read(RegisterARM::LR).unwrap();
        stop_with_fault(uc, format!("CPU exception {interrupt} at pc {pc:#010x} (lr {lr:#010x})"));
        return;
    }
    let index = mem().read_u32(svc_address) & 0x00FF_FFFF;
    call_stub(uc, index);
    if uc.get_data().fault.is_some() || uc.get_data().exit_code.is_some() {
        uc.stop();
    }
}

/// A Linux system call from translated game code: number in r7, arguments in
/// r0-r5, result (negative errno) back in r0; game code goes on at `resume`.
pub fn native_syscall(uc: &mut Uc, resume: u32) {
    let number = uc.reg(7);
    let arguments = [0, 1, 2, 3, 4, 5].map(|index| uc.reg(index));
    RESUME_AT.with(|r| r.set(Some(resume)));
    let result = crate::imports::threads::linux_syscall(uc, number, arguments);
    RESUME_AT.with(|r| r.set(None));
    uc.set_reg(0, result as u32);
}

/// Runs the handler for phone function `index` with the current registers.
fn sampling() -> bool {
    static SAMPLING: OnceLock<bool> = OnceLock::new();
    *SAMPLING.get_or_init(|| crate::dynarec::property("debug.arm32bridge.sample").is_some_and(|value| value == "1" || value == "2"))
}

fn sample_caller(uc: &mut Uc, name: &str) {
    thread_local! {
        static LAST: Cell<u64> = const { Cell::new(0) };
    }
    let now = ticks();
    if now.wrapping_sub(LAST.with(|last| last.get())) < 500 * ticks_per_ms() {
        return;
    }
    LAST.with(|last| last.set(now));
    let sp = uc.reg(13);
    let memory = mem();
    let words = (uc.get_data().stack_top.saturating_sub(sp) / 4).min(1024);
    let stack: Vec<u32> = (0..words).map(|i| memory.read_u32(sp + i * 4)).collect();
    let callers: Vec<String> = stack
        .iter()
        .filter(|&&word| (mem::LIB_BASE..mem::LIB_END).contains(&word) && mem::executable(word & !1))
        .take(16)
        .map(|word| format!("{word:#x}"))
        .collect();
    let pmip = shared().guest_exports.get("mono_pmip").copied();
    let mut managed = Vec::new();
    let resolve = crate::dynarec::property("debug.arm32bridge.sample").as_deref() == Some("2");
    if let (Some(pmip), true) = (pmip, resolve) {
        for &word in stack.iter().filter(|&&word| (mem::HEAP_BASE..mem::HEAP_END).contains(&word)) {
            if managed.len() >= 12 {
                break;
            }
            if let Ok(text) = call_guest(uc, pmip, &[word]) {
                let text = text as u32;
                if text != 0 {
                    managed.push(memory.string(text));
                }
            }
        }
    }
    crate::log::line(&format!(
        "[sample t{}] {name} lr={:#x} r0={:#x} sp={sp:#x} stack: {} managed: {}",
        uc.get_data().id,
        uc.reg(14),
        uc.reg(0),
        callers.join(" "),
        managed.join(" | ")
    ));
}

/// Logs the most-called phone functions so far (with the translator stats).
pub fn log_top_calls(count: usize) {
    let mut calls: Vec<(u64, &'static str)> = shared().stubs.iter().map(|stub| (stub.calls, stub.name)).filter(|c| c.0 > 0).collect();
    calls.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let text: Vec<String> = calls.iter().take(count).map(|(calls, name)| format!("{name} {calls}")).collect();
    crate::log::line(&format!("[calls] {}", text.join(", ")));
}

pub fn call_stub(uc: &mut Uc, index: u32) {
    let (handler, name, trace) = {
        let mut shared = shared();
        let stub = &mut shared.stubs[(index - FIRST_STUB) as usize];
        stub.calls += 1;
        let first = stub.calls == 1;
        let name = stub.name;
        let handler = stub.handler;
        if first {
            shared.first_use.push(index);
        }
        if shared.recent.len() == 32 {
            shared.recent.pop_front();
        }
        shared.recent.push_back(name);
        (handler, name, shared.trace)
    };
    if sampling() {
        sample_caller(uc, name);
    }
    share_gil_after_time_slice();
    uc.get_data_mut().current_call = name;

    let mut args = Args::from_registers(uc);
    if trace {
        let r = args.registers;
        crate::log::line(&format!("[call t{}] {name}({:#x}, {:#x}, {:#x}, {:#x})", uc.get_data().id, r[0], r[1], r[2], r[3]));
    }
    let started = ticks();
    let result = match handler {
        Some(handler) => {
            PHONE_DEPTH.with(|depth| depth.set(depth.get() + 1));
            let result = handler(uc, &mut args);
            // Calls this slow show up as stutters at 120 Hz (8.3 ms per frame).
            let spent = ticks().wrapping_sub(started);
            if PHONE_DEPTH.with(|depth| {
                depth.set(depth.get() - 1);
                depth.get() == 0
            }) {
                PHONE_TICKS.with(|total| total.set(total.get() + spent));
            }
            if spent >= SLOW_CALL_MS * ticks_per_ms() {
                let java = if name.starts_with("jni:Call") { crate::imports::jni_host::last_method() } else { String::new() };
                crate::log::line(&format!(
                    "[slow] {name} {java} took {:.1} ms (thread {})",
                    spent as f64 / ticks_per_ms() as f64,
                    uc.get_data().id
                ));
            }
            result
        }
        None => {
            let mut shared = shared();
            if !shared.missing.iter().any(|missing| missing == name) {
                shared.missing.push(name.to_string());
                crate::log::line(&format!("[bridge] no handler yet for {name}; returning 0"));
            }
            Ret::U32(0)
        }
    };
    let jumped = matches!(result, Ret::Jumped);
    write_result(uc, result);
    if !jumped {
        crate::imports::signals::deliver_pending();
    }
}

/// Puts a handler's result where a softfp caller expects it (r0, or r0:r1).
pub fn write_result(cpu: &mut impl GuestCpu, result: Ret) {
    match result {
        Ret::Void | Ret::Jumped => {}
        Ret::U32(value) => cpu.set_reg(0, value),
        Ret::F32(value) => cpu.set_reg(0, value.to_bits()),
        Ret::U64(value) => {
            cpu.set_reg(0, value as u32);
            cpu.set_reg(1, (value >> 32) as u32);
        }
        Ret::F64(value) => {
            let bits = value.to_bits();
            cpu.set_reg(0, bits as u32);
            cpu.set_reg(1, (bits >> 32) as u32);
        }
    }
}

fn on_bad_memory(uc: &mut Unicorn<'_, Thread>, kind: MemType, address: u64, size: usize, _value: i64) -> bool {
    let uc = as_static(uc);
    let pc = uc.pc_read().unwrap_or(0);
    let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0);
    let message = format!("bad memory access ({kind:?}, {size} bytes at {address:#010x}) at pc {pc:#010x} (lr {lr:#010x})");
    if uc.get_data().fault.is_none() {
        uc.get_data_mut().fault = Some(message);
    }
    false
}

pub fn stop_with_fault(uc: &mut Uc, message: String) {
    if uc.get_data().fault.is_none() {
        uc.get_data_mut().fault = Some(message);
    }
    uc.stop();
}

/// Reads arguments the way an ARM32 softfp caller passes them.
pub struct Args {
    pub registers: [u32; 4],
    next_register: usize,
    stack: u32,
}

impl Args {
    pub fn from_registers(cpu: &impl GuestCpu) -> Args {
        Args { registers: [cpu.reg(0), cpu.reg(1), cpu.reg(2), cpu.reg(3)], next_register: 0, stack: cpu.reg(SP) }
    }
}

/// A source of 32-bit-ABI arguments: live registers, or a guest `va_list`.
pub trait ArgSource {
    fn u32(&mut self) -> u32;
    fn u64(&mut self) -> u64;
    fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    fn i64(&mut self) -> i64 {
        self.u64() as i64
    }
    fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }
    fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }
}

impl ArgSource for Args {
    fn u32(&mut self) -> u32 {
        if self.next_register < 4 {
            self.next_register += 1;
            return self.registers[self.next_register - 1];
        }
        let value = mem().read_u32(self.stack);
        self.stack += 4;
        value
    }
    fn u64(&mut self) -> u64 {
        // 64-bit values use an even register pair, or an 8-aligned stack slot.
        if self.next_register < 4 {
            self.next_register = (self.next_register + 1) & !1;
        }
        if self.next_register < 4 {
            let low = self.registers[self.next_register] as u64;
            let high = self.registers[self.next_register + 1] as u64;
            self.next_register += 2;
            return low | (high << 32);
        }
        self.next_register = 4;
        self.stack = (self.stack + 7) & !7;
        let value = mem().read_u64(self.stack);
        self.stack += 8;
        value
    }
}

/// A guest `va_list` (on ARM32 it is just a pointer into the argument area).
pub struct VaList {
    pub pointer: u32,
}

impl ArgSource for VaList {
    fn u32(&mut self) -> u32 {
        let value = mem().read_u32(self.pointer);
        self.pointer += 4;
        value
    }
    fn u64(&mut self) -> u64 {
        self.pointer = (self.pointer + 7) & !7;
        let value = mem().read_u64(self.pointer);
        self.pointer += 8;
        value
    }
}

/// One argument for a host→guest call.
#[derive(Clone, Copy)]
pub enum Arg {
    /// int, pointer, float bits (softfp), bool...
    Word(u32),
    /// long long or double bits: an even register pair or 8-aligned stack slot.
    Pair(u64),
}

/// Calls guest code at `address` (bit 0 set = Thumb) with 32-bit arguments.
pub fn call_guest(uc: &mut Uc, address: u32, args: &[u32]) -> Result<u64, String> {
    let args: Vec<Arg> = args.iter().map(|&a| Arg::Word(a)).collect();
    call_guest_with(uc, address, &args)
}

/// Calls guest code with arguments laid out by the ARM32 softfp rules.
/// Safe to use from inside a handler: the CPU state is saved and restored.
pub fn call_guest_with(uc: &mut Uc, address: u32, args: &[Arg]) -> Result<u64, String> {
    if let Some(fault) = &uc.get_data().fault {
        return Err(fault.clone());
    }
    let depth = uc.get_data().depth;
    if depth >= MAX_DEPTH {
        return Err("guest calls nested too deeply".into());
    }
    // The translator runs top-level calls and calls made from its own
    // handlers; calls made while Unicorn is running stay in Unicorn.
    if crate::dynarec::enabled() && (depth == 0 || live_native(uc).is_some()) {
        return call_guest_native(uc, address, args, depth);
    }
    let saved = if depth > 0 {
        let mut context = uc.context_alloc().map_err(|e| format!("{e:?}"))?;
        uc.context_save(&mut context).map_err(|e| format!("{e:?}"))?;
        Some(context)
    } else {
        None
    };

    let (registers, stack_words) = lay_out_arguments(args);
    let mut sp = if depth > 0 {
        (uc.reg_read(RegisterARM::SP).unwrap() as u32 - 256) & !7
    } else {
        uc.get_data().stack_top - 64
    };
    sp = (sp - stack_words.len() as u32 * 4) & !7;
    let memory = mem();
    for (i, value) in stack_words.iter().enumerate() {
        memory.write_u32(sp + i as u32 * 4, *value);
    }
    let register_ids = [RegisterARM::R0, RegisterARM::R1, RegisterARM::R2, RegisterARM::R3];
    for (i, value) in registers.iter().enumerate() {
        uc.reg_write(register_ids[i], *value as u64).unwrap();
    }
    let trap = trap_address(depth);
    uc.reg_write(RegisterARM::SP, sp as u64).unwrap();
    uc.reg_write(RegisterARM::LR, trap as u64).unwrap();

    uc.get_data_mut().depth = depth + 1;
    let result = uc.emu_start(address as u64, trap as u64, 0, 0);
    uc.get_data_mut().depth = depth;

    let pc = uc.pc_read().unwrap() as u32;
    let r0 = uc.reg_read(RegisterARM::R0).unwrap();
    let r1 = uc.reg_read(RegisterARM::R1).unwrap();
    if let Some(context) = saved {
        uc.context_restore(&context).map_err(|e| format!("{e:?}"))?;
    }
    if let Some(fault) = &uc.get_data().fault {
        return Err(fault.clone());
    }
    if let Some(code) = uc.get_data().exit_code {
        return Err(format!("guest called exit({code})"));
    }
    if let Err(error) = result {
        let message = format!("CPU stopped with {error:?} at pc {pc:#010x}");
        uc.get_data_mut().fault = Some(message.clone());
        return Err(message);
    }
    if pc != trap {
        return Err(format!("guest stopped at {pc:#010x} instead of returning"));
    }
    Ok(r0 | (r1 << 32))
}

/// The same call on the translator: state lives in the thread's `CpuState`.
fn call_guest_native(uc: &mut Uc, address: u32, args: &[Arg], depth: u32) -> Result<u64, String> {
    if uc.get_data().native.is_none() {
        let tls = uc.get_data().tls_block;
        uc.get_data_mut().native = Some(Box::new(crate::dynarec::Native::new(tls)));
    }
    let (registers, stack_words) = lay_out_arguments(args);
    let stack_top = uc.get_data().stack_top;
    let native = uc.get_data_mut().native.as_mut().unwrap();
    let was_active = native.active;
    let saved = native.state;
    let mut sp = if was_active { (native.state.r[SP] - 256) & !7 } else { stack_top - 64 };
    sp = (sp - stack_words.len() as u32 * 4) & !7;
    let memory = mem();
    for (i, value) in stack_words.iter().enumerate() {
        memory.write_u32(sp + i as u32 * 4, *value);
    }
    let trap = trap_address(depth);
    native.state.r[..4].copy_from_slice(&registers);
    native.state.r[SP] = sp;
    native.state.r[LR] = trap;
    native.active = true;

    uc.get_data_mut().depth = depth + 1;
    let result = crate::dynarec::run(uc, address, trap);
    uc.get_data_mut().depth = depth;

    let native = uc.get_data_mut().native.as_mut().unwrap();
    let (r0, r1) = (native.state.r[0] as u64, native.state.r[1] as u64);
    if was_active {
        native.state = saved;
    } else {
        native.active = false;
    }
    if let Err(error) = result {
        if uc.get_data().fault.is_none() {
            uc.get_data_mut().fault = Some(error.clone());
        }
        return Err(error);
    }
    Ok(r0 | (r1 << 32))
}

/// Registers r0-r3 and stack words for a softfp call.
fn lay_out_arguments(args: &[Arg]) -> ([u32; 4], Vec<u32>) {
    let mut registers = [0u32; 4];
    let mut next_register = 0usize;
    let mut stack_words: Vec<u32> = Vec::new();
    for arg in args {
        match *arg {
            Arg::Word(value) => {
                if next_register < 4 {
                    registers[next_register] = value;
                    next_register += 1;
                } else {
                    stack_words.push(value);
                }
            }
            Arg::Pair(value) => {
                next_register = (next_register + 1) & !1;
                if next_register < 4 {
                    registers[next_register] = value as u32;
                    registers[next_register + 1] = (value >> 32) as u32;
                    next_register += 2;
                } else {
                    next_register = 4;
                    if stack_words.len() % 2 == 1 {
                        stack_words.push(0);
                    }
                    stack_words.push(value as u32);
                    stack_words.push((value >> 32) as u32);
                }
            }
        }
    }

    (registers, stack_words)
}
