//! Running translated code: executable memory, the enter/exit glue, the
//! block cache and the dispatch loop with its Unicorn fallback.

use std::collections::HashMap;
use std::sync::OnceLock;

use unicorn_engine::RegisterARM;

use super::emit::{Asm, SP};
use super::{exit, host, offsets, thumb, CpuState};
use crate::mem;
use crate::runtime::{self, mem as guest_mem, GuestCpu, Uc, LR};

const CODE_SIZE: usize = 64 << 20;

/// Per-thread translator state, kept in the thread's `Thread` data.
pub struct Native {
    pub state: CpuState,
    /// True while `state` holds the live registers (translated code or a
    /// handler it called); false while Unicorn holds them.
    pub active: bool,
}

impl Native {
    pub fn new(tls: u32) -> Native {
        let mut state = CpuState::default();
        state.tls = tls;
        Native { state, active: false }
    }
}

/// Whether guest code should run on the translator (ARM32_ENGINE=jit, or the
/// Android property debug.arm32bridge.engine=jit).
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if !cfg!(target_arch = "aarch64") {
            return false; // translated code is ARM64
        }
        if let Ok(value) = std::env::var("ARM32_ENGINE") {
            return value == "jit";
        }
        property("debug.arm32bridge.engine").as_deref() == Some("jit")
    })
}

#[cfg(target_os = "android")]
fn property(name: &str) -> Option<String> {
    extern "C" {
        fn __system_property_get(name: *const libc::c_char, value: *mut libc::c_char) -> libc::c_int;
    }
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = [0 as libc::c_char; 92];
    let length = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    (length > 0).then(|| unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "android"))]
fn property(_name: &str) -> Option<String> {
    None
}

// ---- executable memory ----

struct CodeMemory {
    base: *mut u32,
    used: usize,
    /// fn(state, memory base, code) -> exit reason
    enter: usize,
    exit_stub: usize,
}

unsafe impl Send for CodeMemory {}

extern "C" {
    fn __clear_cache(begin: *mut libc::c_char, end: *mut libc::c_char);
}

impl CodeMemory {
    fn new() -> CodeMemory {
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CODE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "could not map memory for translated code");
        let mut memory = CodeMemory { base: base as *mut u32, used: 0, enter: 0, exit_stub: 0 };
        memory.enter = memory.place(&enter_code());
        memory.exit_stub = memory.place(&exit_code());
        memory
    }

    /// Copies code in and returns its address.
    fn place(&mut self, code: &[u32]) -> usize {
        assert!((self.used + code.len()) * 4 <= CODE_SIZE, "translated code memory is full");
        let at = unsafe { self.base.add(self.used) };
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), at, code.len());
            __clear_cache(at as *mut libc::c_char, at.add(code.len()) as *mut libc::c_char);
        }
        self.used += code.len();
        at as usize
    }
}

/// Saves the host's callee-saved registers, loads the guest state and jumps to the block (x2).
fn enter_code() -> Vec<u32> {
    let mut a = Asm::new();
    a.push_pair_x(29, 30);
    a.push_pair_x(19, 20);
    a.push_pair_x(21, 22);
    a.push_pair_x(23, 24);
    a.push_pair_x(25, 26);
    a.push_pair_x(27, 28);
    a.push_pair_d(8, 9);
    a.push_pair_d(10, 11);
    a.push_pair_d(12, 13);
    a.push_pair_d(14, 15);
    a.mov_x(host::STATE, 0);
    a.mov_x(host::MEMORY, 1);
    a.mov_x(17, 2);
    a.mov_x(host::EXIT_STUB, 3);
    a.mov_x_from_sp(16);
    a.str_x_off(16, host::STATE, offsets::HOST_SP);
    a.ldr_x_off(16, host::STATE, offsets::NZCV);
    a.msr_nzcv(16);
    for pair in 0..8 {
        a.ldp_q_off(pair * 2, pair * 2 + 1, host::STATE, offsets::Q + pair * 32);
    }
    for pair in 0..7 {
        a.ldp_w_off(pair * 2, pair * 2 + 1, host::STATE, offsets::R + pair * 8);
    }
    a.ldr_w_off(14, host::STATE, offsets::R + 14 * 4);
    a.br(17);
    a.code
}

/// Stores the guest state, restores the host registers and returns the exit reason (w22).
fn exit_code() -> Vec<u32> {
    let mut a = Asm::new();
    for pair in 0..7 {
        a.stp_w_off(pair * 2, pair * 2 + 1, host::STATE, offsets::R + pair * 8);
    }
    a.str_w_off(14, host::STATE, offsets::R + 14 * 4);
    a.mrs_nzcv(16);
    a.str_x_off(16, host::STATE, offsets::NZCV);
    for pair in 0..8 {
        a.stp_q_off(pair * 2, pair * 2 + 1, host::STATE, offsets::Q + pair * 32);
    }
    a.str_w_off(host::EXIT_PC, host::STATE, offsets::EXIT_PC);
    a.str_w_off(host::EXIT_REASON, host::STATE, offsets::EXIT_REASON);
    a.mov_w(0, host::EXIT_REASON);
    a.ldr_x_off(16, host::STATE, offsets::HOST_SP);
    a.mov_sp_from_x(16);
    a.pop_pair_d(14, 15);
    a.pop_pair_d(12, 13);
    a.pop_pair_d(10, 11);
    a.pop_pair_d(8, 9);
    a.pop_pair_x(27, 28);
    a.pop_pair_x(25, 26);
    a.pop_pair_x(23, 24);
    a.pop_pair_x(21, 22);
    a.pop_pair_x(19, 20);
    a.pop_pair_x(29, 30);
    a.ret();
    let _ = SP;
    a.code
}

// ---- block cache (shared by all threads; only used under the guest lock) ----

struct Cache {
    memory: CodeMemory,
    blocks: HashMap<u32, usize>,
    /// Fallback sites: guest pc → times run in Unicorn (ARM32_STATS=1 prints them).
    fallbacks: HashMap<u32, u64>,
}

struct GuardedCache(std::cell::RefCell<Option<Cache>>);
unsafe impl Sync for GuardedCache {}
static CACHE: GuardedCache = GuardedCache(std::cell::RefCell::new(None));

fn with_cache<R>(work: impl FnOnce(&mut Cache) -> R) -> R {
    debug_assert!(runtime::gil_held());
    let mut cache = CACHE.0.borrow_mut();
    let cache = cache.get_or_insert_with(|| Cache { memory: CodeMemory::new(), blocks: HashMap::new(), fallbacks: HashMap::new() });
    work(cache)
}

/// The translated block for a guest pc (bit 0 = Thumb), translating it on first use.
fn block_for(pc: u32) -> (usize, usize, usize) {
    with_cache(|cache| {
        let (enter, exit_stub) = (cache.memory.enter, cache.memory.exit_stub);
        if let Some(&code) = cache.blocks.get(&pc) {
            return (code, enter, exit_stub);
        }
        let code = if pc & 1 == 1 {
            thumb::translate(guest_mem(), pc & !1)
        } else {
            // ARM-mode code runs in Unicorn for now.
            let mut a = Asm::new();
            thumb::emit_exit(&mut a, pc, exit::FALLBACK | (16 << 8));
            a.code
        };
        let address = cache.memory.place(&code);
        cache.blocks.insert(pc, address);
        (address, enter, exit_stub)
    })
}

type EnterFn = unsafe extern "C" fn(*mut CpuState, *mut u8, usize, usize) -> u32;

/// Runs guest code from `pc` (bit 0 = Thumb) until it reaches `stop_at`.
pub fn run(uc: &mut Uc, pc: u32, stop_at: u32) -> Result<(), String> {
    let memory_base = guest_mem().host(0) as *mut u8;
    let mut pc = pc;
    loop {
        if pc & !1 == stop_at {
            return Ok(());
        }
        let address = pc & !1;
        if address >= mem::STUB_BASE && address < mem::STUB_BASE + mem::STUB_SIZE {
            // A phone function: `svc #index; bx lr`.
            let index = (address - mem::STUB_BASE) / 8;
            if index < runtime::FIRST_STUB {
                return Err(format!("guest returned to a stale return trap at {address:#010x}"));
            }
            runtime::call_stub(uc, index);
            let thread = uc.get_data();
            if let Some(fault) = &thread.fault {
                return Err(fault.clone());
            }
            if let Some(code) = thread.exit_code {
                return Err(format!("guest called exit({code})"));
            }
            pc = uc.reg(LR);
            continue;
        }
        let (code, enter, exit_stub) = block_for(pc);
        let state = &mut uc.get_data_mut().native.as_mut().expect("translator state").state as *mut CpuState;
        let enter: EnterFn = unsafe { std::mem::transmute(enter) };
        let reason = unsafe { enter(state, memory_base, code, exit_stub) };
        let state = unsafe { &mut *state };
        pc = state.exit_pc;
        match reason & 0xFF {
            exit::JUMP => {}
            exit::FALLBACK => {
                pc = fallback(uc, pc, (reason >> 8).max(1), stop_at)?;
            }
            other => return Err(format!("translated code left with unknown reason {other}")),
        }
    }
}

const CORE: [RegisterARM; 15] = [
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
];

/// CPSR Q (bit 27) and GE (bits 16-19).
const CPSR_Q_GE: u32 = 0x080F_0000;

/// Runs `count` instructions at `pc` in Unicorn and returns where it stopped.
fn fallback(uc: &mut Uc, pc: u32, count: u32, stop_at: u32) -> Result<u32, String> {
    if crate::runtime::stats_enabled() {
        with_cache(|cache| *cache.fallbacks.entry(pc).or_default() += 1);
    }
    let state = uc.get_data().native.as_ref().unwrap().state;
    for (i, register) in CORE.iter().enumerate() {
        uc.reg_write(*register, state.r[i] as u64).unwrap();
    }
    // Only NZCV, Q and GE move between engines; Unicorn keeps its mode bits, and
    // the Thumb bit comes from bit 0 of the start address.
    let current = uc.reg_read(RegisterARM::CPSR).unwrap() as u32;
    let cpsr = (current & !(0xF000_0000 | CPSR_Q_GE)) | (state.nzcv as u32 & 0xF000_0000) | (state.cpsr_other & CPSR_Q_GE);
    uc.reg_write(RegisterARM::CPSR, cpsr as u64).unwrap();
    for i in 0..32 {
        uc.reg_write(runtime::double_register(i), state.dreg(i)).unwrap();
    }
    uc.reg_write(RegisterARM::FPSCR, state.fpscr as u64).unwrap();
    uc.reg_write(RegisterARM::C13_C0_3, state.tls as u64).unwrap();

    uc.get_data_mut().native.as_mut().unwrap().active = false;
    let result = uc.emu_start(pc as u64, stop_at as u64, 0, count as usize);
    uc.get_data_mut().native.as_mut().unwrap().active = true;

    let mut state = uc.get_data().native.as_ref().unwrap().state;
    for (i, register) in CORE.iter().enumerate() {
        state.r[i] = uc.reg_read(*register).unwrap() as u32;
    }
    let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap() as u32;
    state.nzcv = (cpsr & 0xF000_0000) as u64;
    state.cpsr_other = cpsr & CPSR_Q_GE;
    for i in 0..32 {
        state.set_dreg(i, uc.reg_read(runtime::double_register(i)).unwrap());
    }
    state.fpscr = uc.reg_read(RegisterARM::FPSCR).unwrap() as u32;
    let new_pc = uc.reg_read(RegisterARM::PC).unwrap() as u32 | ((cpsr >> 5) & 1);
    state.r[15] = new_pc & !1;
    uc.get_data_mut().native.as_mut().unwrap().state = state;

    let thread = uc.get_data();
    if let Some(fault) = &thread.fault {
        return Err(fault.clone());
    }
    if let Some(code) = thread.exit_code {
        return Err(format!("guest called exit({code})"));
    }
    if let Err(error) = result {
        return Err(format!("CPU stopped with {error:?} at pc {:#010x} (fallback from {pc:#010x})", new_pc & !1));
    }
    Ok(new_pc)
}

/// Prints the most-used fallback sites (ARM32_STATS=1).
pub fn print_stats() {
    with_cache(|cache| {
        let mut sites: Vec<(&u32, &u64)> = cache.fallbacks.iter().collect();
        sites.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = cache.fallbacks.values().sum();
        crate::log::line(&format!("[jit] {} blocks, {} fallbacks at {} sites", cache.blocks.len(), total, sites.len()));
        for (pc, count) in sites.iter().take(40) {
            let pc = **pc;
            let thumb = pc & 1 == 1;
            let address = pc & !1;
            let first = guest_mem().read_u16(address);
            let second = guest_mem().read_u16(address + 2);
            let text = if thumb { format!("{first:04x} {second:04x}") } else { format!("{:08x}", guest_mem().read_u32(address)) };
            crate::log::line(&format!("[jit]   {count:8} x {address:#010x} {} {text}", if thumb { "T" } else { "A" }));
        }
    });
}
