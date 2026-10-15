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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
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
    /// Times called; only counted while `counting_calls()` (a shared counter
    /// would make every thread calling the same function wait on one cache line).
    pub calls: AtomicU64,
    used: AtomicBool,
    light: Light,
}

/// How a phone function may run when translated code calls it.
#[derive(Clone, Copy, PartialEq)]
enum Light {
    /// The full hand-off (`call_stub`).
    No,
    /// In place, without leaving guest code (`call_light`).
    Yes,
    /// pthread_mutex_lock: in place when the mutex is free, else the full
    /// hand-off (waiting must happen outside guest code).
    MutexLock,
}

/// Phone functions that never block, sleep, call guest code, change code or
/// jump. Games call them millions of times, so they skip the hand-off around
/// a call (register parking, two SeqCst state stores, timing): the thread
/// stays in guest code for the few microseconds they take, and a stopped
/// world or a signal is handled at the next block exit as usual.
const LIGHT: &[&str] = &[
    "memcpy", "__aeabi_memcpy", "__aeabi_memcpy4", "__aeabi_memcpy8", "memmove", "__aeabi_memmove", "__aeabi_memmove4",
    "__aeabi_memmove8", "__memcpy_chk", "__memmove_chk", "memset", "__memset_chk", "__aeabi_memset", "__aeabi_memset4",
    "__aeabi_memset8", "bzero", "__aeabi_memclr", "__aeabi_memclr4", "__aeabi_memclr8", "memcmp", "memchr", "memrchr",
    "strlen", "__strlen_chk", "strcmp", "strncmp", "strcasecmp", "strncasecmp", "strchr", "__strchr_chk", "strrchr",
    "__strrchr_chk", "strstr", "malloc", "calloc", "realloc", "free", "_ZdlPv", "memalign", "pthread_getspecific",
    "pthread_setspecific", "pthread_self", "pthread_mutex_trylock", "pthread_mutex_unlock", "clock_gettime",
];

fn light_kind(name: &str, handler: Option<Handler>) -> Light {
    match name {
        _ if handler.is_none() => Light::No,
        "pthread_mutex_lock" => Light::MutexLock,
        _ if LIGHT.contains(&name) => Light::Yes,
        _ => Light::No,
    }
}

/// Phone function stand-ins by stub slot (`index - FIRST_STUB`); made once, never freed.
fn stub_table() -> &'static [AtomicPtr<Stub>] {
    static TABLE: OnceLock<Box<[AtomicPtr<Stub>]>> = OnceLock::new();
    TABLE.get_or_init(|| (0..mem::STUB_SIZE / 8).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect())
}

/// The stand-in in stub slot `index`.
pub fn stub(index: u32) -> Option<&'static Stub> {
    let pointer = stub_table().get(index.checked_sub(FIRST_STUB)? as usize)?.load(Ordering::Acquire);
    unsafe { pointer.as_ref() }
}

/// Stub slots in the order the game first called them.
static FIRST_USE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn first_use() -> Vec<u32> {
    FIRST_USE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static COUNT_CALLS: AtomicBool = AtomicBool::new(false);

/// Counts calls per phone function (for the stats and the command-line report).
pub fn count_calls() {
    COUNT_CALLS.store(true, Ordering::Relaxed);
}

fn counting_calls() -> bool {
    COUNT_CALLS.load(Ordering::Relaxed) || stats_enabled()
}

static TRACE: AtomicBool = AtomicBool::new(false);
/// debug.arm32bridge.glcheck=1: every GL call is followed by glGetError, and errors are logged.
static GL_CHECK: AtomicBool = AtomicBool::new(false);

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

/// State shared by every guest thread (`shared()`; the guest heap has its own lock, `heap()`).
pub struct Shared {
    pub misc: Bump,
    /// Stub slot of each phone function made so far.
    stub_by_name: HashMap<String, u32>,
    /// Exported symbols of loaded guest libraries (name → guest address).
    pub guest_exports: HashMap<String, u32>,
    /// Names called that have no handler yet (they return 0).
    pub missing: Vec<String>,
    pub next_thread_id: u32,
    next_stack_top: u32,
    /// Finished threads' id, stack top, errno slot and TLS block, for reuse
    /// (ids stay small: locks keep the owner's id in 16 bits).
    free_threads: Vec<(u32, u32, u32, u32)>,
    pub state: crate::imports::State,
}

impl Shared {
    pub fn misc_alloc(&mut self, size: u32, align: u32) -> u32 {
        self.misc.alloc(size, align)
    }
    /// The guest copy of `text` stored as data import `key`, made on first
    /// use (one lookup and insert, so two threads get the same copy).
    pub fn cached_cstr(&mut self, key: String, text: &str) -> u32 {
        if let Some(&address) = self.state.data_imports.get(&key) {
            return address;
        }
        let address = self.misc_cstr(text);
        self.state.data_imports.insert(key, address);
        address
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
    /// Which guest thread (pthread_t) this is.
    pub identity: Arc<crate::imports::threads::Identity>,
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
    /// The last phone functions this thread called (for crash reports).
    pub recent: VecDeque<&'static str>,
    /// This host thread's place in stopping the world (parallel mode).
    pub runner: Arc<Runner>,
    /// The last code change (`note_code_change`) this thread's Unicorn has dropped.
    pub code_generation: u64,
    pub clean: Option<unicorn_engine::Context>,
    /// Where the thread goes on after the phone function or system call it is in
    /// (signals taken meanwhile resume there).
    pub resume_at: Option<u32>,
    /// Phone functions in progress (they can call back into game code), and the
    /// time spent in outermost ones, in `ticks`.
    pub phone_depth: u32,
    pub phone_ticks: u64,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
/// `thread_number()` of the thread holding SHARED (0 = none), to report a
/// thread locking it twice instead of hanging.
static SHARED_OWNER: AtomicU64 = AtomicU64::new(0);
static HEAP: Mutex<Option<GuestHeap>> = Mutex::new(None);
static MEMORY: OnceLock<GuestMem> = OnceLock::new();

pub fn mem() -> GuestMem {
    *MEMORY.get().expect("runtime::init not called")
}

/// A number for this host thread, never 0 (cheaper to compare than a ThreadId).
fn thread_number() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static NUMBER: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    NUMBER.with(|n| *n)
}

/// The shared state, locked until the result is dropped. Never hold it
/// across a call into guest code, Java, or anything else that may wait.
pub struct SharedGuard(MutexGuard<'static, Option<Shared>>);

impl std::ops::Deref for SharedGuard {
    type Target = Shared;
    fn deref(&self) -> &Shared {
        self.0.as_ref().expect("runtime::init not called")
    }
}

impl std::ops::DerefMut for SharedGuard {
    fn deref_mut(&mut self) -> &mut Shared {
        self.0.as_mut().expect("runtime::init not called")
    }
}

impl Drop for SharedGuard {
    fn drop(&mut self) {
        SHARED_OWNER.store(0, Ordering::Relaxed);
        count_lock(-1);
    }
}

thread_local! {
    /// Bridge locks (shared state, heap) this thread holds right now.
    static LOCKS_HELD: Cell<u32> = const { Cell::new(0) };
}

/// Whether bridge locks are counted (debug builds, ARM32_LOCKCHECK=1). The
/// count is a thread-local, which is slow on Android, so it is kept only then.
fn lock_checking() -> bool {
    static CHECKING: OnceLock<bool> = OnceLock::new();
    *CHECKING.get_or_init(|| cfg!(debug_assertions) || std::env::var("ARM32_LOCKCHECK").as_deref() == Ok("1"))
}

/// Adds `change` to this thread's count of bridge locks held (when checking).
fn count_lock(change: i32) {
    if lock_checking() {
        LOCKS_HELD.with(|held| held.set(held.get().wrapping_add_signed(change)));
    }
}

/// Debug builds and ARM32_LOCKCHECK=1: code that may block for long (or
/// stop every thread) must hold no bridge lock, or other threads hang behind it.
pub fn assert_no_bridge_locks(place: &str) {
    if lock_checking() && LOCKS_HELD.with(|held| held.get()) != 0 {
        crate::log::line(&format!("[bridge] lock check: {place} runs while this thread holds a bridge lock"));
        debug_assert!(false, "{place} runs while holding a bridge lock");
    }
}

pub fn shared() -> SharedGuard {
    let me = thread_number();
    let guard = match SHARED.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            assert!(SHARED_OWNER.load(Ordering::Relaxed) != me, "the shared state was locked twice by one thread");
            SHARED.lock().unwrap_or_else(|e| e.into_inner())
        }
    };
    SHARED_OWNER.store(me, Ordering::Relaxed);
    count_lock(1);
    SharedGuard(guard)
}

/// The guest heap (malloc and friends), locked until the result is dropped.
/// May be taken while holding `shared()`, never the other way round.
pub struct HeapGuard(MutexGuard<'static, Option<GuestHeap>>);

impl std::ops::Deref for HeapGuard {
    type Target = GuestHeap;
    fn deref(&self) -> &GuestHeap {
        self.0.as_ref().expect("runtime::init not called")
    }
}

impl std::ops::DerefMut for HeapGuard {
    fn deref_mut(&mut self) -> &mut GuestHeap {
        self.0.as_mut().expect("runtime::init not called")
    }
}

pub fn heap() -> HeapGuard {
    let guard = HEAP.lock().unwrap_or_else(|e| e.into_inner());
    count_lock(1);
    HeapGuard(guard)
}

impl Drop for HeapGuard {
    fn drop(&mut self) {
        count_lock(-1);
    }
}

/// Sets up guest memory and the shared state. Safe to call more than once,
/// also from several threads at once: the first does the work, the others
/// wait for it to finish.
pub fn init(trace: bool) {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| set_up(trace));
}

fn set_up(trace: bool) {
    let memory = GuestMem::reserve();
    memory.write_kernel_helpers();
    MEMORY.set(memory).ok();
    for depth in 0..MAX_DEPTH {
        memory.write_u32(trap_address(depth), UDF);
        memory.write_u32(trap_address(depth) + 4, UDF);
    }
    TRACE.store(trace, Ordering::Relaxed);
    GL_CHECK.store(crate::dynarec::property("debug.arm32bridge.glcheck").as_deref() == Some("1"), Ordering::Relaxed);
    *HEAP.lock().unwrap_or_else(|e| e.into_inner()) = Some(GuestHeap::new(memory));
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Shared {
        misc: Bump::new(mem::MISC_BASE, mem::MISC_END),
        stub_by_name: HashMap::new(),
        guest_exports: HashMap::new(),
        missing: Vec::new(),
        next_thread_id: 1,
        next_stack_top: mem::STACK_TOP,
        free_threads: Vec::new(),
        state: Default::default(),
    });
    start_engine_builder();
}

// ---- running guest threads at the same time ----

/// Whether guest threads run at the same time (translator mode). Unicorn-only
/// mode and the checker keep one thread at a time with the guest lock (GIL).
/// ARM32_PARALLEL=0 / debug.arm32bridge.parallel=0 turns it off for testing.
pub fn parallel() -> bool {
    static PARALLEL: OnceLock<bool> = OnceLock::new();
    *PARALLEL.get_or_init(|| {
        let setting = std::env::var("ARM32_PARALLEL").ok().or_else(|| crate::dynarec::property("debug.arm32bridge.parallel"));
        crate::dynarec::enabled() && !crate::dynarec::check::enabled() && setting.as_deref() != Some("0")
    })
}

/// Where a host thread that runs guest code is right now.
pub const IN_GUEST: u32 = 0;
/// In a phone function or other host code (or not running guest code at all).
pub const OUTSIDE: u32 = 1;
/// Asleep in a bridge wait (it wakes for signals itself).
pub const WAITING: u32 = 2;
/// Stopped at a safe point because the world is stopped.
pub const PARKED: u32 = 3;

/// One host thread that runs guest code (one per engine).
pub struct Runner {
    pub state: AtomicU32,
    /// Non-zero asks translated code to leave to the dispatcher at its next
    /// block exit (a signal came, or the world is being stopped).
    pub attention: AtomicU32,
    /// For a helper standing in for a stuck thread (signals.rs): its number, else 0.
    pub helper: u32,
    /// Where the thread stopped for its current phone function (parallel
    /// mode): r0-r14, then the address it goes on at. Written before the
    /// OUTSIDE store, so a helper that sees OUTSIDE sees them.
    pub parked: [AtomicU32; 16],
    pub parked_valid: AtomicBool,
    /// The thread's fixed slots, for a helper standing in for it.
    pub thread_id: u32,
    pub errno_slot: u32,
    pub tls_block: u32,
}

/// Every runner, for stopping the world.
static RUNNERS: Mutex<Vec<Arc<Runner>>> = Mutex::new(Vec::new());
/// Set while one thread has stopped every other one (`stop_world`).
static STOP: AtomicBool = AtomicBool::new(false);
static STOPPER: Mutex<()> = Mutex::new(());
/// Threads wait here for the world to start again.
static RELEASED: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

fn new_runner(thread_id: u32, errno_slot: u32, tls_block: u32) -> Arc<Runner> {
    let runner = make_runner(thread_id, errno_slot, tls_block);
    RUNNERS.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::clone(&runner));
    runner
}

fn make_runner(thread_id: u32, errno_slot: u32, tls_block: u32) -> Arc<Runner> {
    Arc::new(Runner {
        state: AtomicU32::new(OUTSIDE),
        attention: AtomicU32::new(0),
        helper: HELPER.with(|helper| helper.get()),
        parked: std::array::from_fn(|_| AtomicU32::new(0)),
        parked_valid: AtomicBool::new(false),
        thread_id,
        errno_slot,
        tls_block,
    })
}

fn forget_runner(runner: &Arc<Runner>) {
    RUNNERS.lock().unwrap_or_else(|e| e.into_inner()).retain(|other| !Arc::ptr_eq(other, runner));
}

/// This thread leaves guest code (into a phone function or host code).
pub fn leave_guest(uc: &Uc) {
    if parallel() {
        uc.get_data().runner.state.store(OUTSIDE, Ordering::SeqCst);
    }
}

/// Records where this thread stops for a phone function (it goes on at
/// `resume`), so a helper can run signal handlers for it meanwhile.
fn park_registers(uc: &Uc, resume: u32) {
    if !parallel() {
        return;
    }
    let runner = &uc.get_data().runner;
    for register in 0..15 {
        runner.parked[register].store(uc.reg(register), Ordering::Relaxed);
    }
    runner.parked[15].store(resume, Ordering::Relaxed);
    runner.parked_valid.store(true, Ordering::Relaxed);
}

/// Whether another runner stands in for this thread's identity right now
/// (a helper treats its own borrow as clear).
fn borrowed_from(runner: &Runner, identity: &crate::imports::threads::Identity) -> bool {
    let borrower = identity.borrower.load(Ordering::SeqCst);
    borrower != 0 && borrower != runner.helper
}

/// This thread goes (back) into guest code, waiting first while the world is
/// stopped or a helper runs signal handlers for it. The state store and the
/// STOP / borrower loads pair with `stop_world`'s STOP store and the helper
/// service's borrower store, each followed by a state load (all SeqCst), so
/// one side always sees the other.
pub fn enter_guest(uc: &Uc) {
    if !parallel() {
        return;
    }
    let (runner, identity) = (&uc.get_data().runner, &uc.get_data().identity);
    loop {
        runner.state.store(IN_GUEST, Ordering::SeqCst);
        let stop = STOP.load(Ordering::SeqCst);
        if !stop && !borrowed_from(runner, identity) {
            return;
        }
        runner.state.store(if stop { PARKED } else { OUTSIDE }, Ordering::SeqCst);
        let (lock, released) = &RELEASED;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        while STOP.load(Ordering::SeqCst) || borrowed_from(runner, identity) {
            guard = released.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Ends (or undoes) a helper's borrow of `identity` and wakes its thread if
/// it waits to go back into guest code.
pub fn clear_borrower(identity: &crate::imports::threads::Identity) {
    let (lock, released) = &RELEASED;
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    identity.borrower.store(0, Ordering::SeqCst);
    released.notify_all();
}

/// Waits until no helper stands in for `identity` (a finishing thread).
pub fn wait_for_borrow_end(identity: &crate::imports::threads::Identity) {
    let started = Instant::now();
    let mut warned = false;
    let (lock, released) = &RELEASED;
    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    while identity.borrower.load(Ordering::SeqCst) != 0 {
        guard = released.wait_timeout(guard, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        if !warned && started.elapsed() > Duration::from_secs(5) {
            warned = true;
            crate::log::line(&format!(
                "[bridge] thread {:#x} is finishing but a signal handler still runs for it after 5 s",
                identity.handle
            ));
        }
    }
}

/// A helper (signals.rs) runs this thread's pending signal handlers as if
/// they came while `target` sat in its phone function: on the target's
/// identity and slots, with its parked registers, below its parked stack.
pub fn deliver_for(identity: &Arc<crate::imports::threads::Identity>, target: &Runner) {
    crate::imports::threads::adopt_identity(Some(Arc::clone(identity)));
    let uc = engine();
    {
        let flag = attention_flag_address(uc);
        let thread = uc.get_data_mut();
        thread.identity = Arc::clone(identity);
        thread.id = target.thread_id;
        thread.errno_slot = target.errno_slot;
        thread.tls_block = target.tls_block;
        thread.depth = 1;
        let native = thread.native.get_or_insert_with(|| Box::new(crate::dynarec::Native::new(target.tls_block, flag)));
        native.state.tls = target.tls_block;
        for register in 0..15 {
            native.state.r[register] = target.parked[register].load(Ordering::Relaxed);
        }
        native.state.clear_exclusive();
        native.active = true;
    }
    let _ = uc.reg_write(RegisterARM::C13_C0_3, target.tls_block as u64);
    // Signals sent meanwhile poke this helper too.
    *identity.helper_runner.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&uc.get_data().runner));
    let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(target.parked[15].load(Ordering::Relaxed)));
    enter_guest(uc);
    crate::imports::signals::deliver_pending_on(uc);
    leave_guest(uc);
    uc.get_data_mut().resume_at = resume_before;
    let thread = uc.get_data_mut();
    thread.depth = 0;
    thread.fault = None;
    thread.exit_code = None;
    if let Some(native) = thread.native.as_mut() {
        native.active = false;
    }
    crate::imports::threads::adopt_identity(None);
}

/// Stops every other thread that runs guest code at a safe point (a block
/// exit or outside guest code), runs `work`, and lets them go again. For rare
/// changes no guest code may see half done (dropping translated code). The
/// caller holds no bridge lock; another thread stopping the world first is
/// waited for (this thread counts as stopped meanwhile).
pub fn stop_world<R>(uc: &Uc, work: impl FnOnce() -> R) -> R {
    if !parallel() {
        return work();
    }
    assert_no_bridge_locks("stopping every thread");
    let me = &uc.get_data().runner;
    let was = me.state.swap(PARKED, Ordering::SeqCst);
    let stopper = STOPPER.lock().unwrap_or_else(|e| e.into_inner());
    STOP.store(true, Ordering::SeqCst);
    let runners: Vec<Arc<Runner>> = RUNNERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for runner in &runners {
        runner.attention.store(1, Ordering::SeqCst);
    }
    let started = Instant::now();
    let mut warned = false;
    for runner in runners.iter().filter(|runner| !Arc::ptr_eq(runner, me)) {
        while runner.state.load(Ordering::SeqCst) == IN_GUEST {
            std::thread::yield_now();
            if !warned && started.elapsed() > Duration::from_secs(2) {
                warned = true;
                crate::log::line("[bridge] stopping the world: a thread has not reached a safe point after 2 s");
            }
        }
    }
    STOPS.fetch_add(1, Ordering::Relaxed);
    let result = work();
    {
        let (lock, released) = &RELEASED;
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        STOP.store(false, Ordering::SeqCst);
        released.notify_all();
    }
    drop(stopper);
    me.state.store(was, Ordering::SeqCst);
    if was == IN_GUEST {
        enter_guest(uc);
    }
    result
}

/// Times the world was stopped (shown with the stats).
pub static STOPS: AtomicU64 = AtomicU64::new(0);

/// The flag translated code checks at every linked exit: this thread's
/// attention flag in parallel mode, else the shared time-slice flag.
pub fn attention_flag_address(uc: &Uc) -> u64 {
    if parallel() {
        uc.get_data().runner.attention.as_ptr() as u64
    } else {
        preempt_flag_address()
    }
}

/// The dispatcher between blocks (game code goes on at `resume`): stop for a
/// stopped world or a signal (parallel mode), or let a waiting thread have
/// the guest lock after a full time slice.
pub fn check_in(uc: &mut Uc, resume: u32) {
    if parallel() {
        if uc.get_data().runner.attention.load(Ordering::Relaxed) != 0 {
            safe_point(uc, resume);
        }
    } else if preempt_requested() {
        share_gil(uc, resume);
    }
}

/// The dispatcher found its attention flag set: clear it, wait out a stopped
/// world, and run handlers for signals that came (resuming at `resume`).
pub fn safe_point(uc: &mut Uc, resume: u32) {
    let runner = Arc::clone(&uc.get_data().runner);
    runner.attention.store(0, Ordering::SeqCst);
    std::sync::atomic::fence(Ordering::SeqCst);
    if STOP.load(Ordering::SeqCst) {
        runner.state.store(PARKED, Ordering::SeqCst);
        enter_guest(uc);
        // Stopped like an interrupt: the kernel's return to user space forgets
        // an outstanding LDREX, so a STREX after the pause fails and retries
        // (other threads may have changed the word and changed it back).
        clear_exclusive(uc);
    }
    let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(resume));
    crate::imports::signals::deliver_pending_on(uc);
    uc.get_data_mut().resume_at = resume_before;
}

// ---- the guest lock (one thread at a time; not used in parallel mode) ----

static GIL: Mutex<()> = Mutex::new(());

thread_local! {
    static GIL_GUARD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    static ENGINE: Cell<*mut Uc> = const { Cell::new(std::ptr::null_mut()) };
    /// This host thread's helper number (signals.rs), set before its engine is made.
    static HELPER: Cell<u32> = const { Cell::new(0) };
    /// This host thread's runner, once it has an engine.
    static CURRENT_RUNNER: RefCell<Option<Arc<Runner>>> = const { RefCell::new(None) };
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

extern "C" {
    fn uc_emu_stop(engine: *mut std::ffi::c_void) -> i32;
}

/// The host thread that took the guest lock last (the holder while it is held).
static GIL_HOLDER: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

/// Each thread's innermost Unicorn run (its engine handle), while one is going.
/// Game code that spins without calling a phone function (waiting for another
/// thread to change a word) never reaches a point where it would hand over the
/// lock, so the ticker stops the holder's run instead (`call_guest_with`
/// resumes it after sharing the lock). Translated code checks PREEMPT itself.
static UNICORN_RUNS: Mutex<Vec<(std::thread::ThreadId, usize)>> = Mutex::new(Vec::new());

/// Marks this thread's run of `engine` (the previous one, when nested, is returned).
fn unicorn_run_started(engine: usize) -> Option<usize> {
    let me = std::thread::current().id();
    let mut runs = UNICORN_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    let previous = runs.iter().position(|(thread, _)| *thread == me).map(|index| runs.swap_remove(index).1);
    runs.push((me, engine));
    previous
}

fn unicorn_run_ended(previous: Option<usize>) {
    let me = std::thread::current().id();
    let mut runs = UNICORN_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    runs.retain(|(thread, _)| *thread != me);
    if let Some(engine) = previous {
        runs.push((me, engine));
    }
}

/// Stops the lock holder's Unicorn run, if it is in one (the ticker, after a full time slice).
fn stop_holder_run() {
    let holder = *GIL_HOLDER.lock().unwrap_or_else(|e| e.into_inner());
    let runs = UNICORN_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, engine)) = runs.iter().find(|(thread, _)| Some(*thread) == holder) {
        // Held under the list lock: the run can't end and its engine be freed meanwhile.
        unsafe { uc_emu_stop(engine as *mut std::ffi::c_void) };
    }
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
                if PREEMPT.swap(1, Ordering::Relaxed) == 0 {
                    stop_holder_run();
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        });
    });
}

fn take_gil() {
    if parallel() {
        return;
    }
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
    *GIL_HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
    let now = ticks();
    HELD_SINCE.with(|since| since.set(now));
    HELD_SINCE_ANY.store(now, Ordering::Relaxed);
    PREEMPT.store(0, Ordering::Relaxed);
    GIL_GUARD.with(|g| *g.borrow_mut() = Some(guard));
}

/// Marks this host thread as signal helper `number` (before its engine is made).
pub fn become_helper(number: u32) {
    HELPER.with(|helper| helper.set(number));
}

pub fn resume_point(uc: &Uc) -> Option<u32> {
    uc.get_data().resume_at
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

pub fn share_gil(uc: &mut Uc, resume: u32) {
    if parallel() {
        return;
    }
    if hand_over_gil() {
        let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(resume));
        crate::imports::signals::deliver_pending_on(uc);
        uc.get_data_mut().resume_at = resume_before;
    }
}

fn share_gil_after_time_slice() {
    if !parallel() && WANTING.load(Ordering::Relaxed) != 0
        && ticks().wrapping_sub(HELD_SINCE.with(|since| since.get())) >= TIME_SLICE_MS * ticks_per_ms()
    {
        hand_over_gil();
    }
}

/// Total time this thread has spent inside phone functions (a phone function
/// that calls back into game code counts that game code too).
pub fn phone_time(uc: &Uc) -> std::time::Duration {
    std::time::Duration::from_nanos(uc.get_data().phone_ticks * 1_000_000 / ticks_per_ms())
}

/// Whether this thread may touch guest state: always in parallel mode,
/// else while it holds the guest lock.
pub fn gil_held() -> bool {
    parallel() || GIL_GUARD.with(|g| g.borrow().is_some())
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
    if parallel() || gil_held() {
        return GilToken(false);
    }
    take_gil();
    GilToken(true)
}

/// Runs `work` with the guest lock released (for anything that may block).
pub fn without_gil<R>(work: impl FnOnce() -> R) -> R {
    assert_no_bridge_locks("a blocking phone call");
    if parallel() {
        let result = work();
        crate::imports::signals::deliver_pending();
        return result;
    }
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

/// One thread's sleeping place. Only its own thread waits on it, so a late
/// notify from an earlier wait is just a spurious wake-up (waits re-check).
struct Parker {
    lock: Mutex<()>,
    ready: Condvar,
}

/// One wait: who ended it, and whether its thread went to the kernel to sleep.
struct Slot {
    /// Claimed exactly once (a waker, the timeout or a signal).
    outcome: std::sync::atomic::AtomicU8,
    sleeping: std::sync::atomic::AtomicBool,
}

impl Slot {
    /// After claiming the outcome: notify the thread only if it sleeps in the
    /// kernel. Both sides use SeqCst (waker: claim, then read `sleeping`;
    /// waiter: set `sleeping`, then read the outcome), so at least one of them
    /// sees the other and the wake-up is never lost.
    fn wake(&self, parker: &Parker) {
        if self.sleeping.load(Ordering::SeqCst) {
            drop(parker.lock.lock().unwrap_or_else(|e| e.into_inner()));
            parker.ready.notify_one();
        }
    }
}

/// How long a waiting thread watches for a wake-up before sleeping in the
/// kernel. A wake-up in that window is a memory write: neither side makes a
/// system call, and the waker doesn't pay for the kernel waking a sleeping
/// thread (10.5% of Card Wars' main thread, DEBUGGING.md "Thread wake-ups").
/// debug.arm32bridge.spin=<microseconds> changes it (0 = sleep at once).
fn spin_time() -> Duration {
    static SPIN: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *SPIN.get_or_init(|| {
        let micros = crate::dynarec::property("debug.arm32bridge.spin").and_then(|value| value.trim().parse().ok()).unwrap_or(50);
        Duration::from_micros(micros)
    })
}

/// How a wait ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    /// `wake_on` / `wake_on_count` picked this waiter.
    Woken,
    TimedOut,
    /// The condition no longer held when checked, so it never slept.
    NotWaiting,
    /// A signal came for this thread (its handler has run when this returns).
    Interrupted,
}

const UNCLAIMED: u8 = 0;
const WOKEN: u8 = 1;
const TIMED_OUT: u8 = 2;
const INTERRUPTED: u8 = 3;

struct Waiter {
    thread: u32,
    key: u64,
    parker: Arc<Parker>,
    /// Its outcome is claimed under WAITERS.
    slot: Arc<Slot>,
}

/// Threads asleep in a bridge wait, oldest first.
static WAITERS: Mutex<Vec<Waiter>> = Mutex::new(Vec::new());

thread_local! {
    static PARKER: Arc<Parker> = Arc::new(Parker { lock: Mutex::new(()), ready: Condvar::new() });
}

fn waiters() -> MutexGuard<'static, Vec<Waiter>> {
    WAITERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether this thread is inside guest code with a signal it could take now.
fn signal_ready() -> bool {
    if !gil_held() {
        return false;
    }
    let uc = engine();
    let me = &uc.get_data().identity;
    uc.get_data().depth > 0 && me.pending.load(Ordering::SeqCst) & !me.blocked.load(Ordering::SeqCst) != 0
}

/// Sleeps until `wake_on(key)` picks this thread, the deadline passes or a
/// signal arrives, as long as `still_waiting()` holds. It is checked under
/// the waiters' lock and wakers change the guest's memory before taking that
/// lock, so a wake-up between the check and the sleep is never lost (the
/// futex rule). Signals that arrived are delivered before this returns.
pub fn wait_until(key: u64, deadline: Option<Instant>, still_waiting: impl FnOnce() -> bool) -> Wait {
    assert_no_bridge_locks("a wait");
    let parker = PARKER.with(Arc::clone);
    let slot = Arc::new(Slot { outcome: std::sync::atomic::AtomicU8::new(UNCLAIMED), sleeping: std::sync::atomic::AtomicBool::new(false) });
    {
        let mut list = waiters();
        if signal_ready() {
            drop(list);
            crate::imports::signals::deliver_pending();
            return Wait::Interrupted;
        }
        if !still_waiting() {
            return Wait::NotWaiting;
        }
        let thread = crate::imports::threads::current_handle();
        list.push(Waiter { thread, key, parker: Arc::clone(&parker), slot: Arc::clone(&slot) });
    }
    // Asleep without the guest lock (GIL mode), so the thread that wakes this one can run.
    let held = GIL_GUARD.with(|g| g.borrow_mut().take()).is_some();
    // A signal wakes this wait and is delivered by this thread (no helper needed).
    let runner = CURRENT_RUNNER.with(|current| current.borrow().clone());
    let before = runner.as_ref().map(|runner| runner.state.swap(WAITING, Ordering::SeqCst));
    let claimed = || slot.outcome.load(Ordering::SeqCst) != UNCLAIMED;
    // Watch for a quick wake-up first (see spin_time).
    let spin = spin_time();
    if !spin.is_zero() {
        let started = Instant::now();
        let until = deadline.map_or(started + spin, |deadline| deadline.min(started + spin));
        while !claimed() && Instant::now() < until {
            std::hint::spin_loop();
        }
    }
    if !claimed() {
        let mut guard = parker.lock.lock().unwrap_or_else(|e| e.into_inner());
        slot.sleeping.store(true, Ordering::SeqCst);
        while !claimed() {
            guard = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    parker.ready.wait_timeout(guard, deadline - now).unwrap_or_else(|e| e.into_inner()).0
                }
                None => parker.ready.wait(guard).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
    if !claimed() {
        // Timed out, unless a waker claims it first (then the wake counts).
        let mut list = waiters();
        if slot.outcome.compare_exchange(UNCLAIMED, TIMED_OUT, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            list.retain(|waiter| !Arc::ptr_eq(&waiter.slot, &slot));
        }
    }
    if let (Some(runner), Some(before)) = (runner, before) {
        runner.state.store(before, Ordering::SeqCst);
    }
    if held {
        take_gil();
    }
    crate::imports::signals::deliver_pending();
    match slot.outcome.load(Ordering::SeqCst) {
        WOKEN => Wait::Woken,
        INTERRUPTED => Wait::Interrupted,
        _ => Wait::TimedOut,
    }
}

/// Wakes up to `count` threads waiting on `key`, oldest first; returns how many.
/// The kernel wake-ups happen after the waiters' lock is let go, so other
/// threads starting or ending a wait don't queue behind them.
pub fn wake_on_count(key: u64, count: u32) -> u32 {
    let mut picked = Vec::new();
    {
        let mut list = waiters();
        list.retain(|waiter| {
            if picked.len() as u32 >= count || waiter.key != key {
                return true;
            }
            if waiter.slot.outcome.compare_exchange(UNCLAIMED, WOKEN, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                return false;
            }
            picked.push((Arc::clone(&waiter.slot), Arc::clone(&waiter.parker)));
            false
        });
    }
    for (slot, parker) in &picked {
        slot.wake(parker);
    }
    picked.len() as u32
}

/// Wakes every thread waiting on `key`.
pub fn wake_on(key: u64) {
    wake_on_count(key, u32::MAX);
}

/// Wakes guest thread `thread` from any bridge wait (a signal came for it).
pub fn wake_thread(thread: u32) {
    let mut picked = Vec::new();
    {
        let mut list = waiters();
        list.retain(|waiter| {
            if waiter.thread != thread {
                return true;
            }
            if waiter.slot.outcome.compare_exchange(UNCLAIMED, INTERRUPTED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                picked.push((Arc::clone(&waiter.slot), Arc::clone(&waiter.parker)));
            }
            false
        });
    }
    for (slot, parker) in &picked {
        slot.wake(parker);
    }
}

/// Waits on `key` until woken, a signal or the timeout (callers re-check
/// their own condition in a loop).
pub fn wait_on(key: u64, timeout: Option<Duration>) {
    wait_until(key, timeout.map(|timeout| Instant::now() + timeout), || true);
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
    let mut uc = take_engine();
    adopt_engine(&mut uc);
    let engine = Box::into_raw(uc);
    ENGINE.with(|e| e.set(engine));
    unsafe { &mut *engine }
}

/// Frees this thread's engine (guest threads call this when they finish).
pub fn drop_engine() {
    let existing = ENGINE.with(|e| e.replace(std::ptr::null_mut()));
    if existing.is_null() {
        return;
    }
    let mut engine = unsafe { Box::from_raw(existing) };
    CURRENT_RUNNER.with(|current| current.borrow_mut().take());
    // The thread has finished: nothing runs on its stack any more (a helper
    // standing in for it is done before this). Thread 1 has the main stack.
    let thread = engine.get_data();
    if thread.id != 1 && thread.stack_top != 0 {
        shared().free_threads.push((thread.id, thread.stack_top, thread.errno_slot, thread.tls_block));
    }
    {
        let thread = engine.get_data();
        forget_runner(&thread.runner);
        let mut runner = thread.identity.runner.lock().unwrap_or_else(|e| e.into_inner());
        if runner.as_ref().is_some_and(|runner| Arc::ptr_eq(runner, &thread.runner)) {
            *runner = None;
        }
    }
    let (pool, _) = &ENGINE_POOL;
    let mut engines = pool.lock().unwrap_or_else(|e| e.into_inner());
    if engines.len() < KEPT_ENGINES {
        let thread = engine.get_data_mut();
        let (code_generation, clean) = (thread.code_generation, thread.clean.take());
        *thread = idle_thread(code_generation, clean);
        engines.push(PooledEngine(Box::into_raw(engine)));
    }
}

/// An engine sits in the pool or in one thread's ENGINE slot, never both, so
/// only one host thread uses it at a time.
struct PooledEngine(*mut Uc);
unsafe impl Send for PooledEngine {}

static ENGINE_POOL: (Mutex<Vec<PooledEngine>>, Condvar) = (Mutex::new(Vec::new()), Condvar::new());
const SPARE_ENGINES: usize = 2;
const KEPT_ENGINES: usize = 4;

fn start_engine_builder() {
    let _ = std::thread::Builder::new().name("arm32-engines".into()).spawn(|| {
        let (pool, wanted) = &ENGINE_POOL;
        loop {
            {
                let mut engines = pool.lock().unwrap_or_else(|e| e.into_inner());
                while engines.len() >= SPARE_ENGINES {
                    engines = wanted.wait(engines).unwrap_or_else(|e| e.into_inner());
                }
            }
            let engine = Box::into_raw(Box::new(build_engine()));
            pool.lock().unwrap_or_else(|e| e.into_inner()).push(PooledEngine(engine));
        }
    });
}

fn take_engine() -> Box<Uc> {
    let (pool, wanted) = &ENGINE_POOL;
    let taken = pool.lock().unwrap_or_else(|e| e.into_inner()).pop();
    wanted.notify_one();
    match taken {
        Some(PooledEngine(engine)) => unsafe { Box::from_raw(engine) },
        None => Box::new(build_engine()),
    }
}

fn idle_thread(code_generation: u64, clean: Option<unicorn_engine::Context>) -> Thread {
    Thread {
        identity: Arc::new(crate::imports::threads::Identity::new(0)),
        id: 0,
        depth: 0,
        fault: None,
        exit_code: None,
        errno_slot: 0,
        tls_block: 0,
        stack_top: 0,
        current_call: "",
        native: None,
        recent: VecDeque::new(),
        runner: make_runner(0, 0, 0),
        code_generation,
        clean,
        resume_at: None,
        phone_depth: 0,
        phone_ticks: 0,
    }
}

fn build_engine() -> Uc {
    let memory = mem();
    let code_generation = CODE_GENERATION.load(Ordering::Acquire);
    let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::ARM, idle_thread(code_generation, None)).expect("start Unicorn");
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
    if parallel() {
        // Atomic STREX/SWP and real barriers in Unicorn too (local patch,
        // see vendor/unicorn-engine-sys/ARM32-BRIDGE.md).
        uc.reg_write(UC_ARM_REG_PARALLEL, 1).expect("Unicorn parallel mode");
    }
    uc.add_intr_hook(on_interrupt).expect("add stub hook");
    uc.add_mem_hook(HookType::MEM_INVALID, 0, u64::MAX, on_bad_memory).expect("add fault hook");
    let mut clean = uc.context_alloc().expect("Unicorn context");
    uc.context_save(&mut clean).expect("save Unicorn context");
    uc.get_data_mut().clean = Some(clean);
    uc
}

fn adopt_engine(uc: &mut Uc) {
    let (id, stack_top, errno_slot, tls_block) = {
        // The id and its stack are handed out together: thread 1 (the first
        // to run guest code) gets the big main-thread stack.
        let mut shared = shared();
        if let Some((id, stack_top, errno_slot, tls_block)) = shared.free_threads.pop() {
            unsafe { std::ptr::write_bytes(mem().host(tls_block), 0, 0x100) };
            mem().write_u32(errno_slot, 0);
            (id, stack_top, errno_slot, tls_block)
        } else {
            let id = shared.next_thread_id;
            shared.next_thread_id += 1;
            let stack_size = if id == 1 { MAIN_STACK_SIZE } else { THREAD_STACK_SIZE };
            let stack_top = shared.alloc_stack(stack_size);
            let errno_slot = shared.misc_alloc(4, 4);
            let tls_block = shared.misc_alloc(0x100, 8);
            (id, stack_top, errno_slot, tls_block)
        }
    };
    let (code_generation, clean) = {
        let old = uc.get_data_mut();
        (old.code_generation, old.clean.take())
    };
    if let Some(clean) = &clean {
        uc.context_restore(clean).expect("restore Unicorn context");
    }
    let thread = Thread {
        identity: crate::imports::threads::current_identity(),
        id,
        depth: 0,
        fault: None,
        exit_code: None,
        errno_slot,
        tls_block,
        stack_top,
        current_call: "",
        native: None,
        recent: VecDeque::with_capacity(32),
        runner: new_runner(id, errno_slot, tls_block),
        code_generation,
        clean,
        resume_at: None,
        phone_depth: 0,
        phone_ticks: 0,
    };
    // A helper's engine is made while it stands in for a thread: that
    // thread keeps its own runner.
    thread.identity.runner.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| Arc::clone(&thread.runner));
    let identity = &thread.identity;
    if identity.pending.load(Ordering::SeqCst) & !identity.blocked.load(Ordering::SeqCst) != 0 {
        thread.runner.attention.store(1, Ordering::SeqCst);
    }
    CURRENT_RUNNER.with(|current| *current.borrow_mut() = Some(Arc::clone(&thread.runner)));
    *uc.get_data_mut() = thread;
    // bionic reads the thread pointer from TPIDRURO.
    uc.reg_write(RegisterARM::C13_C0_3, tls_block as u64).unwrap();
    let _ = uc.reg_write(UC_ARM_REG_EXCLUSIVE_ADDR, u64::MAX);
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
    let index = FIRST_STUB + shared.stub_by_name.len() as u32;
    assert!(index * 8 < mem::STUB_SIZE, "too many stubs");
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    let handler = crate::imports::handler_for(name);
    let light = light_kind(name, handler);
    let stub = Stub { name: leaked, handler, calls: AtomicU64::new(0), used: AtomicBool::new(false), light };
    // Published before its code is written, so a thread calling the code finds it.
    stub_table()[(index - FIRST_STUB) as usize].store(Box::into_raw(Box::new(stub)), Ordering::Release);
    shared.stub_by_name.insert(name.to_string(), index);
    let address = mem::STUB_BASE + index * 8;
    mem().write_u32(address, SVC | index);
    mem().write_u32(address + 4, BX_LR);
    address
}

/// Every phone function stand-in made so far, in slot order.
pub fn stubs() -> Vec<&'static Stub> {
    let count = shared().stub_by_name.len() as u32;
    (FIRST_STUB..FIRST_STUB + count).filter_map(stub).collect()
}

/// The last phone functions this thread called (for crash reports).
pub fn recent_calls() -> Vec<&'static str> {
    let engine = ENGINE.with(|e| e.get());
    if engine.is_null() {
        return Vec::new();
    }
    unsafe { &*engine }.get_data().recent.iter().copied().collect()
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
        let resume = uc.pc_read().unwrap() as u32 | thumb;
        share_gil(uc, resume);
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
        // As for a phone function: it may block (futex), so it waits outside guest code.
        clear_exclusive(uc);
        let thumb = (uc.reg_read(RegisterARM::CPSR).unwrap() as u32 >> 5) & 1;
        let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(pc | thumb));
        park_registers(uc, pc | thumb);
        leave_guest(uc);
        let result = crate::imports::threads::linux_syscall(uc, number, arguments);
        enter_guest(uc);
        uc.get_data_mut().resume_at = resume_before;
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
    clear_exclusive(uc);
    let number = uc.reg(7);
    let arguments = [0, 1, 2, 3, 4, 5].map(|index| uc.reg(index));
    let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(resume));
    park_registers(uc, resume);
    leave_guest(uc);
    let result = crate::imports::threads::linux_syscall(uc, number, arguments);
    enter_guest(uc);
    uc.get_data_mut().resume_at = resume_before;
    uc.set_reg(0, result as u32);
}

/// Forgets an outstanding LDREX, as the kernel does on every return from an
/// exception (system call, phone function, signal handler): a STREX after it fails.
pub fn clear_exclusive(uc: &mut Uc) {
    match uc.get_data_mut().native.as_mut() {
        Some(native) if native.active => native.state.clear_exclusive(),
        _ => {
            let _ = uc.reg_write(UC_ARM_REG_EXCLUSIVE_ADDR, u64::MAX);
        }
    }
}

/// Unicorn's exclusive monitor address (local patch, see vendor/unicorn-engine-sys/ARM32-BRIDGE.md).
const UC_ARM_REG_EXCLUSIVE_ADDR: i32 = 141;
/// Unicorn translates for several CPUs at once (CF_PARALLEL; local patch).
const UC_ARM_REG_PARALLEL: i32 = 143;

/// Code changes (cacheflush), numbered, so every thread's Unicorn drops its
/// stale translations before it runs again.
static CODE_GENERATION: AtomicU64 = AtomicU64::new(0);
static CODE_CHANGES: Mutex<VecDeque<(u64, u32, u32)>> = Mutex::new(VecDeque::new());
const KEPT_CODE_CHANGES: usize = 64;

pub fn note_code_change(start: u32, end: u32) {
    let mut changes = CODE_CHANGES.lock().unwrap_or_else(|e| e.into_inner());
    let generation = CODE_GENERATION.load(Ordering::Relaxed) + 1;
    if changes.len() == KEPT_CODE_CHANGES {
        changes.pop_front();
    }
    changes.push_back((generation, start, end));
    CODE_GENERATION.store(generation, Ordering::Release);
}

/// Drops Unicorn translations of code changed since this thread last looked
/// (everything, if it fell more than KEPT_CODE_CHANGES behind).
pub fn sync_unicorn_code(uc: &mut Uc) {
    let seen = uc.get_data().code_generation;
    if CODE_GENERATION.load(Ordering::Acquire) == seen {
        return;
    }
    let changes = CODE_CHANGES.lock().unwrap_or_else(|e| e.into_inner());
    let latest = CODE_GENERATION.load(Ordering::Relaxed);
    if changes.front().is_none_or(|&(oldest, _, _)| oldest > seen + 1) {
        let _ = uc.ctl_flush_tb();
    } else {
        for &(_, start, end) in changes.iter().filter(|&&(generation, _, _)| generation > seen) {
            let _ = uc.ctl_remove_cache(start as u64, end as u64);
        }
    }
    drop(changes);
    uc.get_data_mut().code_generation = latest;
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
    let mut calls: Vec<(u64, &'static str)> =
        stubs().iter().map(|stub| (stub.calls.load(Ordering::Relaxed), stub.name)).filter(|c| c.0 > 0).collect();
    calls.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let text: Vec<String> = calls.iter().take(count).map(|(calls, name)| format!("{name} {calls}")).collect();
    crate::log::line(&format!("[calls] {}", text.join(", ")));
}

/// Runs a light phone function (see `LIGHT`) in place for translated code.
/// False: take the full hand-off instead (first call, tracing, sampling, one
/// guest lock, or a mutex that is taken).
pub fn call_light(uc: &mut Uc, index: u32) -> bool {
    let Some(stub) = stub(index) else { return false };
    if stub.light == Light::No || !parallel() || !stub.used.load(Ordering::Relaxed) || TRACE.load(Ordering::Relaxed) || GL_CHECK.load(Ordering::Relaxed) || sampling() {
        return false;
    }
    let Some(handler) = stub.handler else { return false };
    let mut args = Args::from_registers(uc);
    let result = if stub.light == Light::MutexLock {
        match crate::imports::threads::lock_if_free(uc, args.registers[0]) {
            Some(result) => Ret::U32(result),
            None => return false,
        }
    } else {
        uc.get_data_mut().current_call = stub.name;
        handler(uc, &mut args)
    };
    clear_exclusive(uc);
    if counting_calls() {
        stub.calls.fetch_add(1, Ordering::Relaxed);
    }
    write_result(uc, result);
    true
}

pub fn call_stub(uc: &mut Uc, index: u32) {
    let Some(stub) = stub(index) else {
        stop_with_fault(uc, format!("called phone function slot {index}, which was never made"));
        return;
    };
    if counting_calls() {
        stub.calls.fetch_add(1, Ordering::Relaxed);
    }
    if !stub.used.load(Ordering::Relaxed) && !stub.used.swap(true, Ordering::Relaxed) {
        FIRST_USE.lock().unwrap_or_else(|e| e.into_inner()).push(index);
    }
    let (handler, name, trace) = (stub.handler, stub.name, TRACE.load(Ordering::Relaxed));
    let recent = &mut uc.get_data_mut().recent;
    if recent.len() == 32 {
        recent.pop_front();
    }
    recent.push_back(name);
    if sampling() {
        sample_caller(uc, name);
    }
    clear_exclusive(uc);
    share_gil_after_time_slice();
    uc.get_data_mut().current_call = name;

    let mut args = Args::from_registers(uc);
    if trace {
        let r = args.registers;
        crate::log::line(&format!("[call t{}] {name}({:#x}, {:#x}, {:#x}, {:#x})", uc.get_data().id, r[0], r[1], r[2], r[3]));
    }
    let started = ticks();
    let resume = uc.reg(LR);
    // Signals taken during or right after this call resume at its return address.
    let resume_before = std::mem::replace(&mut uc.get_data_mut().resume_at, Some(resume));
    park_registers(uc, resume);
    leave_guest(uc);
    let result = match handler {
        Some(handler) => {
            uc.get_data_mut().phone_depth += 1;
            let registers = args.registers;
            let result = handler(uc, &mut args);
            if GL_CHECK.load(Ordering::Relaxed) && name.starts_with("gl") && name != "glGetError" {
                crate::imports::gles::check_error(name, &registers);
            }
            // Calls this slow show up as stutters at 120 Hz (8.3 ms per frame).
            let spent = ticks().wrapping_sub(started);
            let thread = uc.get_data_mut();
            thread.phone_depth -= 1;
            if thread.phone_depth == 0 {
                thread.phone_ticks += spent;
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
    enter_guest(uc);
    let jumped = matches!(result, Ret::Jumped);
    write_result(uc, result);
    if !jumped && gil_held() {
        crate::imports::signals::deliver_pending_on(uc);
    }
    uc.get_data_mut().resume_at = resume_before;
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
    // In parallel mode every call runs there (G4): a Unicorn run could not
    // stop for a stopped world.
    if crate::dynarec::enabled() && (depth == 0 || live_native(uc).is_some() || parallel()) {
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
    sync_unicorn_code(uc);
    let mut start = address;
    let result = loop {
        let previous = unicorn_run_started(uc.get_handle() as usize);
        let result = uc.emu_start(start as u64, trap as u64, 0, 0);
        unicorn_run_ended(previous);
        // Stopped early with no fault or exit: the time slice ran out (every
        // other stop records one). Let the waiting threads in, then go on.
        let pc = uc.pc_read().unwrap() as u32;
        let (fault, exit) = (uc.get_data().fault.is_some(), uc.get_data().exit_code.is_some());
        if result.is_ok() && pc != trap && !fault && !exit {
            let thumb = (uc.reg_read(RegisterARM::CPSR).unwrap() as u32 >> 5) & 1;
            start = pc | thumb;
            share_gil(uc, start);
            continue;
        }
        break result;
    };
    uc.get_data_mut().depth = depth;

    let pc = uc.pc_read().unwrap() as u32;
    let r0 = uc.reg_read(RegisterARM::R0).unwrap();
    let r1 = uc.reg_read(RegisterARM::R1).unwrap();
    // For the crash report: where a bad jump came from (lr) and what it was handed.
    let registers = if result.is_err() {
        use RegisterARM::*;
        let names = [R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, SP, LR];
        names.iter().enumerate().map(|(i, &name)| format!("r{i}={:#x}", uc.reg_read(name).unwrap_or(0))).collect::<Vec<_>>().join(" ")
    } else {
        String::new()
    };
    if let Some(context) = saved {
        uc.context_restore(&context).map_err(|e| format!("{e:?}"))?;
        clear_exclusive(uc);
    }
    if let Some(fault) = &uc.get_data().fault {
        return Err(fault.clone());
    }
    if let Some(code) = uc.get_data().exit_code {
        return Err(format!("guest called exit({code})"));
    }
    if let Err(error) = result {
        let message = format!("CPU stopped with {error:?} at pc {pc:#010x}; {registers}");
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
        let flag = attention_flag_address(uc);
        uc.get_data_mut().native = Some(Box::new(crate::dynarec::Native::new(tls, flag)));
    }
    // Wait out a stopped world (or a helper's borrow) before touching the guest stack.
    let was_in_guest = parallel() && uc.get_data().runner.state.load(Ordering::SeqCst) == IN_GUEST;
    enter_guest(uc);
    // The phone function that made this call stays parked where it was.
    let outer_parked: Option<[u32; 16]> = (parallel() && depth > 0)
        .then(|| std::array::from_fn(|i| uc.get_data().runner.parked[i].load(Ordering::Relaxed)));
    // Called from a phone function that Unicorn ran (a fallback): keep
    // Unicorn's state for when this returns, and start from its registers.
    let unicorn_live = depth > 0 && !uc.get_data().native.as_ref().unwrap().active;
    let unicorn_saved = if unicorn_live {
        let saved = uc.context_alloc().and_then(|mut context| uc.context_save(&mut context).map(|_| context));
        match saved {
            Ok(context) => Some(context),
            Err(error) => {
                if !was_in_guest {
                    leave_guest(uc);
                }
                return Err(format!("{error:?}"));
            }
        }
    } else {
        None
    };
    if unicorn_live {
        crate::dynarec::seed_from_unicorn(uc);
    }
    let (registers, stack_words) = lay_out_arguments(args);
    let stack_top = uc.get_data().stack_top;
    let native = uc.get_data_mut().native.as_mut().unwrap();
    let was_active = native.active;
    let saved = native.state;
    let mut sp = if was_active || unicorn_live { (native.state.r[SP] - 256) & !7 } else { stack_top - 64 };
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
        native.state.clear_exclusive();
    } else {
        native.active = false;
    }
    if let Some(context) = unicorn_saved {
        let _ = uc.context_restore(&context);
        clear_exclusive(uc);
    }
    if parallel() {
        let runner = &uc.get_data().runner;
        match outer_parked {
            Some(parked) => {
                for (slot, value) in runner.parked.iter().zip(parked) {
                    slot.store(value, Ordering::Relaxed);
                }
            }
            None => runner.parked_valid.store(false, Ordering::Relaxed),
        }
    }
    if was_in_guest {
        enter_guest(uc);
    } else {
        leave_guest(uc);
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
