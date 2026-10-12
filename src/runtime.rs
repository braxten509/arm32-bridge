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
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use unicorn_engine::{ArmCpuModel, Arch, HookType, MemType, Mode, Prot, RegisterARM, Unicorn};

use crate::mem::{self, Bump, GuestHeap, GuestMem};

pub type Uc = Unicorn<'static, Thread>;

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
    pub name: String,
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

const MAIN_STACK_SIZE: u32 = 8 << 20;
pub const THREAD_STACK_SIZE: u32 = 1 << 20;

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
    pub recent: VecDeque<String>,
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
    pub current_call: String,
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
static WAKE: Condvar = Condvar::new();

thread_local! {
    static GIL_GUARD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    static ENGINE: Cell<*mut Uc> = const { Cell::new(std::ptr::null_mut()) };
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
    let guard = GIL.lock().unwrap_or_else(|e| e.into_inner());
    GIL_GUARD.with(|g| *g.borrow_mut() = Some(guard));
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
    let guard = GIL.lock().unwrap_or_else(|e| e.into_inner());
    GIL_GUARD.with(|g| *g.borrow_mut() = Some(guard));
    result
}

/// Gives up the guest lock until another thread calls `wake_guests` (or the
/// timeout passes). Callers re-check their condition in a loop.
pub fn wait_for_guests(timeout: Option<Duration>) {
    let guard = GIL_GUARD.with(|g| g.borrow_mut().take()).expect("waiting without the guest lock");
    let guard = match timeout {
        Some(timeout) => WAKE.wait_timeout(guard, timeout).unwrap_or_else(|e| e.into_inner()).0,
        None => WAKE.wait(guard).unwrap_or_else(|e| e.into_inner()),
    };
    GIL_GUARD.with(|g| *g.borrow_mut() = Some(guard));
}

pub fn wake_guests() {
    WAKE.notify_all();
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
        current_call: String::new(),
    };
    let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::ARM, thread).expect("start Unicorn");
    uc.ctl_set_cpu_model(ArmCpuModel::CORTEX_A15 as i32).expect("pick Cortex-A15 (ARMv7 + NEON)");
    for (start, end) in mem::MAPPED_REGIONS {
        unsafe {
            uc.mem_map_ptr(start as u64, (end - start) as u64, Prot::ALL, memory.host(start).cast())
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
    shared.stubs.push(Stub { name: name.to_string(), handler: crate::imports::handler_for(name), calls: 0 });
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

fn on_interrupt(uc: &mut Unicorn<'_, Thread>, interrupt: u32) {
    let uc = as_static(uc);
    let pc = uc.pc_read().unwrap() as u32;
    // EXCP_SWI is 2; the PC already points past the svc.
    let svc_address = pc.wrapping_sub(4);
    let in_stub_area = svc_address >= mem::STUB_BASE && svc_address < mem::STUB_BASE + mem::STUB_SIZE;
    if interrupt != 2 || !in_stub_area {
        let lr = uc.reg_read(RegisterARM::LR).unwrap();
        stop_with_fault(uc, format!("CPU exception {interrupt} at pc {pc:#010x} (lr {lr:#010x})"));
        return;
    }
    let index = mem().read_u32(svc_address) & 0x00FF_FFFF;
    let (handler, name, trace) = {
        let mut shared = shared();
        let stub = &mut shared.stubs[(index - FIRST_STUB) as usize];
        stub.calls += 1;
        let first = stub.calls == 1;
        let name = stub.name.clone();
        let handler = stub.handler;
        if first {
            shared.first_use.push(index);
        }
        if shared.recent.len() == 32 {
            shared.recent.pop_front();
        }
        shared.recent.push_back(name.clone());
        (handler, name, shared.trace)
    };
    uc.get_data_mut().current_call = name.clone();

    let mut args = Args::from_registers(uc);
    if trace {
        let r = args.registers;
        eprintln!("[call t{}] {name}({:#x}, {:#x}, {:#x}, {:#x})", uc.get_data().id, r[0], r[1], r[2], r[3]);
    }
    let result = match handler {
        Some(handler) => handler(uc, &mut args),
        None => {
            let mut shared = shared();
            if !shared.missing.contains(&name) {
                shared.missing.push(name.clone());
                crate::log::line(&format!("[bridge] no handler yet for {name}; returning 0"));
            }
            Ret::U32(0)
        }
    };
    match result {
        Ret::Void | Ret::Jumped => {}
        Ret::U32(value) => uc.reg_write(RegisterARM::R0, value as u64).unwrap(),
        Ret::U64(value) => {
            uc.reg_write(RegisterARM::R0, value & 0xFFFF_FFFF).unwrap();
            uc.reg_write(RegisterARM::R1, value >> 32).unwrap();
        }
        Ret::F32(value) => uc.reg_write(RegisterARM::R0, value.to_bits() as u64).unwrap(),
        Ret::F64(value) => {
            let bits = value.to_bits();
            uc.reg_write(RegisterARM::R0, bits & 0xFFFF_FFFF).unwrap();
            uc.reg_write(RegisterARM::R1, bits >> 32).unwrap();
        }
    }
    if uc.get_data().fault.is_some() || uc.get_data().exit_code.is_some() {
        uc.emu_stop().unwrap();
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
    uc.emu_stop().unwrap();
}

/// Reads arguments the way an ARM32 softfp caller passes them.
pub struct Args {
    pub registers: [u32; 4],
    next_register: usize,
    stack: u32,
}

impl Args {
    pub fn from_registers(uc: &Uc) -> Args {
        let r = |reg| uc.reg_read(reg).unwrap() as u32;
        Args {
            registers: [r(RegisterARM::R0), r(RegisterARM::R1), r(RegisterARM::R2), r(RegisterARM::R3)],
            next_register: 0,
            stack: r(RegisterARM::SP),
        }
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
    let saved = if depth > 0 {
        let mut context = uc.context_alloc().map_err(|e| format!("{e:?}"))?;
        uc.context_save(&mut context).map_err(|e| format!("{e:?}"))?;
        Some(context)
    } else {
        None
    };

    // Lay the arguments out: registers r0-r3, then the stack.
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
