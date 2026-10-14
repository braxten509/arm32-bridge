//! Running translated code: executable memory, the enter/exit glue, the
//! block cache and the dispatch loop with its Unicorn fallback.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use unicorn_engine::RegisterARM;

use super::emit::{cond, shift, Asm, SP};
use super::{check, exit, host, offsets, thumb, CpuState, FPSCR_CONTROL, FPSCR_CUMULATIVE, FPSCR_VECTOR};
use crate::mem;
use crate::runtime::{self, mem as guest_mem, GuestCpu, Uc, LR};

const CODE_SIZE: usize = 64 << 20;

/// Per-thread translator state, kept in the thread's `Thread` data.
pub struct Native {
    pub state: CpuState,
    /// True while `state` holds the live registers (translated code or a
    /// handler it called); false while Unicorn holds them.
    pub active: bool,
    /// The checker's Unicorn write hook is installed.
    pub check_hook: bool,
}

impl Native {
    /// `flag`: what linked exits check before jumping on (`runtime::attention_flag_address`).
    pub fn new(tls: u32, flag: u64) -> Native {
        let mut state = CpuState::default();
        state.tls = tls;
        state.preempt_flag = flag;
        state.clear_exclusive();
        Native { state, active: false, check_hook: false }
    }
}

/// Whether guest code should run on the translator: the default on ARM64.
/// ARM32_ENGINE=unicorn (or the Android property debug.arm32bridge.engine=unicorn)
/// runs everything in Unicorn instead; the checker always uses the translator.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if !cfg!(target_arch = "aarch64") {
            return false; // translated code is ARM64
        }
        if super::check::enabled() {
            return true;
        }
        let setting = std::env::var("ARM32_ENGINE").ok().or_else(|| property("debug.arm32bridge.engine"));
        setting.as_deref() != Some("unicorn")
    })
}

#[cfg(target_os = "android")]
pub fn property(name: &str) -> Option<String> {
    extern "C" {
        fn __system_property_get(name: *const libc::c_char, value: *mut libc::c_char) -> libc::c_int;
    }
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = [0 as libc::c_char; 92];
    let length = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    (length > 0).then(|| unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "android"))]
pub fn property(_name: &str) -> Option<String> {
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
        CODE_BASE.store(base as usize, Ordering::Relaxed);
        #[cfg(all(target_arch = "aarch64", target_os = "android"))]
        fault::install();
        // The jump table comes first: entries are reached from CODE_BASE.
        for index in 0..1usize << JUMP_TABLE_BITS {
            unsafe { (*(base as *const AtomicU64).add(index)).store(EMPTY_JUMP, Ordering::Relaxed) };
        }
        let mut memory = CodeMemory { base: base as *mut u32, used: TABLE_BYTES / 4, enter: 0, exit_stub: 0 };
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
    // Guest FPSCR: control bits into FPCR (only written when they differ from
    // the host's, which is slow), cumulative flags into FPSR.
    a.mrs_fpcr(16);
    a.str_w_off(16, host::STATE, offsets::HOST_FPCR);
    a.ldr_w_off(24, host::STATE, offsets::FPSCR);
    a.and_w_imm(25, 24, FPSCR_CONTROL, 26);
    a.and_w_imm(27, 16, FPSCR_CONTROL, 26);
    a.cmp_w(25, 27);
    let same = a.label();
    a.b_cond_to(cond::EQ, same);
    a.msr_fpcr(25);
    a.bind(same);
    a.and_w_imm(25, 24, FPSCR_CUMULATIVE, 26);
    a.msr_fpsr(25);
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
    a.finish();
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
    // FPSCR flags back from FPSR; the host's FPCR back if the guest's differed.
    a.mrs_fpsr(16);
    a.and_w_imm(16, 16, FPSCR_CUMULATIVE, 24);
    a.ldr_w_off(17, host::STATE, offsets::FPSCR);
    a.and_w_imm(17, 17, !FPSCR_CUMULATIVE, 24);
    a.orr_w(17, 17, 16, shift::LSL, 0);
    a.str_w_off(17, host::STATE, offsets::FPSCR);
    a.ldr_w_off(16, host::STATE, offsets::HOST_FPCR);
    a.mrs_fpcr(17);
    a.cmp_w(16, 17);
    let same = a.label();
    a.b_cond_to(cond::EQ, same);
    a.msr_fpcr(16);
    a.bind(same);
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
    a.finish();
    let _ = SP;
    a.code
}

// ---- block cache (shared by all threads) ----

/// A cheap hash for guest addresses (the default SipHash showed up in profiles).
#[derive(Default)]
struct PcHasher(u64);

impl Hasher for PcHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u32(byte as u32);
        }
    }
    fn write_u32(&mut self, value: u32) {
        let mixed = (self.0 ^ value as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = mixed ^ (mixed >> 32);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type PcMap<V> = HashMap<u32, V, BuildHasherDefault<PcHasher>>;

/// Jump table for computed jumps in translated code (`emit_exit_to`):
/// 2^JUMP_TABLE_BITS entries indexed by guest pc bits 1 and up.
pub(super) const JUMP_TABLE_BITS: u32 = 16;

/// Each entry is one 64-bit word, `(code - CODE_BASE) << 32 | guest pc`, so
/// a thread never reads one block's pc with another block's code.
const TABLE_BYTES: usize = 8 << JUMP_TABLE_BITS;
const EMPTY_JUMP: u64 = u32::MAX as u64;

/// Address of the jump table (the start of code memory), or 0 when blocks are not linked.
static JUMP_TABLE: AtomicUsize = AtomicUsize::new(0);

fn jump_entry(pc: u32) -> &'static AtomicU64 {
    let index = ((pc >> 1) & ((1 << JUMP_TABLE_BITS) - 1)) as usize;
    unsafe { &*(CODE_BASE.load(Ordering::Relaxed) as *const AtomicU64).add(index) }
}

fn set_jump(pc: u32, code: usize) {
    let offset = (code - CODE_BASE.load(Ordering::Relaxed)) as u64;
    let entry = jump_entry(pc);
    let wanted = offset << 32 | pc as u64;
    // Rewrite only on a change: every core reads this table, and a store
    // would take the cache line away from all of them.
    if entry.load(Ordering::Relaxed) != wanted {
        entry.store(wanted, Ordering::Release);
    }
}

/// The enter glue and exit stub (fixed once code memory exists).
static ENTER: AtomicUsize = AtomicUsize::new(0);
static EXIT_STUB: AtomicUsize = AtomicUsize::new(0);

/// The dispatcher's lock-free first look: the jump table entry for `pc`, if
/// it holds that pc (blocks are only dropped with every other thread stopped,
/// and their entries cleared then). The block end is not known here; it is
/// only needed by the checker, which turns linking (and this table) off.
fn table_block(pc: u32) -> Option<(usize, u32, usize, usize)> {
    if jump_table() == 0 {
        return None;
    }
    let entry = jump_entry(pc).load(Ordering::Acquire);
    if entry as u32 != pc {
        return None;
    }
    let code = CODE_BASE.load(Ordering::Relaxed) + (entry >> 32) as usize;
    Some((code, 0, ENTER.load(Ordering::Relaxed), EXIT_STUB.load(Ordering::Relaxed)))
}

pub(super) fn jump_table() -> usize {
    JUMP_TABLE.load(Ordering::Relaxed)
}

/// Whether translated blocks jump straight to each other (off under the
/// checker, which compares one block at a time, or with debug.arm32bridge.link=0).
fn linking() -> bool {
    static LINKING: OnceLock<bool> = OnceLock::new();
    *LINKING.get_or_init(|| {
        let setting = std::env::var("ARM32_LINK").ok().or_else(|| property("debug.arm32bridge.link"));
        !check::enabled() && setting.as_deref() != Some("0")
    })
}

struct Cache {
    memory: CodeMemory,
    /// Guest pc → (code address, guest address just past the block).
    blocks: PcMap<(usize, u32)>,
    /// (code address, guest pc) of every block in placement order (rising
    /// addresses), so the fault handler can name the block a host pc is in.
    starts: Vec<(usize, u32)>,
    /// Guest pc not translated yet → exits waiting to be linked to it.
    pending: PcMap<Vec<usize>>,
    /// Guest pc → exits linked straight to its block (undone if it's dropped).
    incoming: PcMap<Vec<usize>>,
    /// Blocks outside the libraries (code a game made while running), by
    /// (first guest address, pc) → guest address just past the block, so a
    /// cache flush can find the blocks it covers.
    made_blocks: std::collections::BTreeMap<(u32, u32), u32>,
    /// Fallback sites: guest pc → times run in Unicorn (ARM32_STATS=1 prints them).
    fallbacks: HashMap<u32, u64>,
    fallback_total: u64,
}

unsafe impl Sync for Cache {}

/// Readers look blocks up; a writer translates, links or drops them.
static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();

fn cache() -> &'static RwLock<Cache> {
    CACHE.get_or_init(|| {
        let memory = CodeMemory::new();
        ENTER.store(memory.enter, Ordering::Relaxed);
        EXIT_STUB.store(memory.exit_stub, Ordering::Relaxed);
        if linking() {
            JUMP_TABLE.store(CODE_BASE.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        RwLock::new(Cache {
            memory,
            blocks: PcMap::default(),
            pending: PcMap::default(),
            incoming: PcMap::default(),
            made_blocks: std::collections::BTreeMap::new(),
            starts: Vec::new(),
            fallbacks: HashMap::new(),
            fallback_total: 0,
        })
    })
}

fn read_cache() -> RwLockReadGuard<'static, Cache> {
    cache().read().unwrap_or_else(|e| e.into_inner())
}

fn write_cache() -> RwLockWriteGuard<'static, Cache> {
    cache().write().unwrap_or_else(|e| e.into_inner())
}

/// Translated code is visible to this core; make every other core running
/// this process drop instructions it may have fetched early, before code
/// that jumps there is published (parallel mode only).
fn sync_other_cores() {
    if !runtime::parallel() {
        return;
    }
    const REGISTER_SYNC_CORE: libc::c_int = 64;
    const SYNC_CORE: libc::c_int = 32;
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let registered = *REGISTERED.get_or_init(|| {
        let ok = unsafe { libc::syscall(libc::SYS_membarrier, REGISTER_SYNC_CORE, 0, 0) } == 0;
        if !ok {
            crate::log::line("[jit] membarrier sync-core is not available; other cores may run stale code");
        }
        ok
    });
    if registered {
        unsafe { libc::syscall(libc::SYS_membarrier, SYNC_CORE, 0, 0) };
        MEMBARRIERS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Times other cores were synced after new code (shown with the stats).
static MEMBARRIERS: AtomicU64 = AtomicU64::new(0);

/// Start of translated code memory (for the fault handler).
static CODE_BASE: AtomicUsize = AtomicUsize::new(0);

/// Faults inside translated code (a bad guest memory access) are logged as a
/// guest fault with the guest block and registers; then the crash goes on to
/// the previous handler (Android's), as before.
#[cfg(all(target_arch = "aarch64", target_os = "android"))]
mod fault {
    use super::{Ordering, CACHE, CODE_BASE, CODE_SIZE};
    use std::sync::TryLockError;
    use std::sync::atomic::AtomicBool;

    /// The handlers that were there before, for SIGSEGV and SIGBUS.
    static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 2] = [std::mem::MaybeUninit::uninit(); 2];
    static REPORTED: AtomicBool = AtomicBool::new(false);

    pub fn install() {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fault as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut action.sa_mask);
            for (i, signal) in [libc::SIGSEGV, libc::SIGBUS].into_iter().enumerate() {
                let previous = (std::ptr::addr_of_mut!(PREVIOUS) as *mut libc::sigaction).add(i);
                libc::sigaction(signal, &action, previous);
            }
        }
    }

    extern "C" fn on_fault(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        let base = CODE_BASE.load(Ordering::Relaxed);
        let context = unsafe { &*(context as *const libc::ucontext_t) };
        let pc = context.uc_mcontext.pc as usize;
        if pc >= base && pc < base + CODE_SIZE && !REPORTED.swap(true, Ordering::Relaxed) {
            let registers = &context.uc_mcontext.regs;
            let host_address = unsafe { (*info).si_addr() } as usize;
            let guest_address = host_address.wrapping_sub(registers[20] as usize);
            let cache = CACHE.get().and_then(|cache| match cache.try_read() {
                Ok(cache) => Some(cache),
                Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            });
            let block = cache.and_then(|cache| {
                let index = cache.starts.partition_point(|&(start, _)| start <= pc);
                index.checked_sub(1).map(|i| cache.starts[i].1)
            });
            let mut text = format!(
                "[fault] bad guest memory access at {guest_address:#010x} in translated code (guest block {}, host pc {pc:#x}); registers:",
                block.map_or("unknown".to_string(), |pc| format!("{pc:#010x}"))
            );
            for (i, value) in registers.iter().take(15).enumerate() {
                text.push_str(&format!(" r{i}={:x}", *value as u32));
            }
            crate::log::line(&text);
        }
        // Hand the signal to the handler that was there before.
        unsafe {
            let slot = if signal == libc::SIGBUS { 1 } else { 0 };
            let previous = &*(std::ptr::addr_of!(PREVIOUS) as *const libc::sigaction).add(slot);
            if previous.sa_flags & libc::SA_SIGINFO != 0 && previous.sa_sigaction > 1 {
                let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) = std::mem::transmute(previous.sa_sigaction);
                handler(signal, info, context as *const _ as *mut libc::c_void);
            } else if previous.sa_sigaction > 1 {
                let handler: extern "C" fn(libc::c_int) = std::mem::transmute(previous.sa_sigaction);
                handler(signal);
            } else {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }
    }
}

/// Longest guest span one block covers (blocks stop well before this).
const MAX_BLOCK_SPAN: u32 = 4096;

/// Drops translations of guest code in [start, end) that the game made while
/// running (it rewrote that memory and flushed the CPU cache). Exits linked
/// to dropped blocks go back to the dispatcher and relink on retranslation.
/// Other threads are stopped first (outside translated code), since the
/// dropped code may be running on them.
pub fn invalidate(uc: &Uc, start: u32, end: u32) {
    drop_blocks(uc, |cache| {
        cache
            .made_blocks
            .range((start.saturating_sub(MAX_BLOCK_SPAN), 0)..(end, 0))
            .filter(|&(&(first, _), &block_end)| block_end > start && first < end)
            .map(|(&(_, pc), _)| pc)
            .collect()
    });
}

/// How far a folded literal can lie from its block, with room to spare: an
/// offset up to 4095 from PC (the instruction + 8 in ARM) and a 4-byte word.
const LITERAL_REACH: u32 = 8192;

/// A read-only library segment in [start, end) was made writable (`mem::unfix`
/// has already run): drops every block that may hold its bytes, its own code
/// and blocks nearby that folded a literal from it. Retranslated blocks there
/// are watched like code the game made.
pub fn invalidate_library(uc: &Uc, start: u32, end: u32) {
    let (low, high) = (start.saturating_sub(LITERAL_REACH), end.saturating_add(LITERAL_REACH));
    drop_blocks(uc, |cache| {
        cache
            .blocks
            .iter()
            .filter(|&(&pc, &(_, block_end))| block_end > low && (pc & !1) < high)
            .map(|(&pc, _)| pc)
            .collect()
    });
}

fn drop_blocks(uc: &Uc, covered: impl Fn(&Cache) -> Vec<u32>) {
    if CACHE.get().is_none() || covered(&read_cache()).is_empty() {
        return;
    }
    runtime::stop_world(uc, || {
        let mut cache = write_cache();
        let dropped = covered(&cache);
        DROPPED.fetch_add(dropped.len() as u64, std::sync::atomic::Ordering::Relaxed);
        for pc in dropped {
            cache.made_blocks.remove(&(pc & !1, pc));
            cache.blocks.remove(&pc);
            for site in cache.incoming.remove(&pc).unwrap_or_default() {
                unpatch_link(site);
                cache.pending.entry(pc).or_default().push(site);
            }
            let entry = jump_entry(pc);
            if entry.load(Ordering::Relaxed) as u32 == pc {
                entry.store(EMPTY_JUMP, Ordering::Relaxed);
            }
        }
        sync_other_cores();
    });
}

/// Puts the `nop` back at a linked exit, so it leaves through the dispatcher.
fn unpatch_link(site: usize) {
    unsafe {
        std::ptr::write_volatile(site as *mut u32, 0xD503_201F);
        __clear_cache(site as *mut libc::c_char, (site + 4) as *mut libc::c_char);
    }
}

/// Turns the `nop` at `site` into `b target`.
fn patch_link(site: usize, target: usize) {
    let offset = (target as i64 - site as i64) / 4;
    let word = 0x1400_0000 | (offset as u32 & 0x03FF_FFFF);
    unsafe {
        std::ptr::write_volatile(site as *mut u32, word);
        __clear_cache(site as *mut libc::c_char, (site + 4) as *mut libc::c_char);
    }
}

/// ARM32_BLOCKMAP=<file> (or the Android property debug.arm32bridge.blockmap=<file>,
/// for testing): appends "time host-address bytes guest-pc" (hex, time in ns of
/// CLOCK_MONOTONIC) for every translated block, so profiles of translated code
/// can be named by game address (scripts in arm32-bridge-games/analysis).
fn block_map() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static FILE: OnceLock<Option<std::sync::Mutex<std::fs::File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let path = std::env::var("ARM32_BLOCKMAP").ok().or_else(|| property("debug.arm32bridge.blockmap"))?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        crate::log::line(&format!("[bridge] block map to {path}: {}", if file.is_ok() { "on" } else { "could not open" }));
        file.ok().map(std::sync::Mutex::new)
    })
    .as_ref()
}

fn note_block(address: usize, bytes: usize, pc: u32) {
    use std::io::Write;
    let Some(file) = block_map() else { return };
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let time = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64;
    let _ = writeln!(file.lock().unwrap_or_else(|e| e.into_inner()), "{time} {address:x} {bytes:x} {pc:x}");
}

/// The translated block for a guest pc (bit 0 = Thumb), translating it on
/// first use: (code, block end, enter glue, exit stub).
fn block_for(pc: u32) -> (usize, u32, usize, usize) {
    let found = |cache: &Cache| {
        let &(code, end) = cache.blocks.get(&pc)?;
        if linking() {
            // A jump-table miss: the entry may belong to another pc with the same index.
            set_jump(pc, code);
        }
        Some((code, end, cache.memory.enter, cache.memory.exit_stub))
    };
    if let Some(block) = found(&read_cache()) {
        return block;
    }
    let mut guard = write_cache();
    if let Some(block) = found(&guard) {
        return block; // another thread translated it meanwhile
    }
    {
        let cache = &mut *guard;
        let (enter, exit_stub) = (cache.memory.enter, cache.memory.exit_stub);
        let block = if pc & 1 == 1 { thumb::translate(guest_mem(), pc & !1) } else { thumb::translate_arm(guest_mem(), pc) };
        let address = cache.memory.place(&block.code);
        note_block(address, block.code.len() * 4, pc);
        cache.blocks.insert(pc, (address, block.end));
        cache.starts.push((address, pc));
        // Watched unless all of it is fixed: a block may run on into a page
        // the game made writable (`mem::unfix`).
        if !mem::fixed_range(pc & !1, block.end) {
            cache.made_blocks.insert((pc & !1, pc), block.end);
        }
        if linking() {
            for &(at, target) in &block.links {
                let site = address + at * 4;
                if let Some(&(code, _)) = cache.blocks.get(&target) {
                    patch_link(site, code);
                    cache.incoming.entry(target).or_default().push(site);
                } else if !(mem::STUB_BASE..mem::STUB_BASE + mem::STUB_SIZE).contains(&(target & !1)) {
                    cache.pending.entry(target).or_default().push(site);
                }
            }
            // Nothing jumps to the new code yet; other cores sync before anything does.
            sync_other_cores();
            let waiting = cache.pending.remove(&pc).unwrap_or_default();
            for &site in &waiting {
                patch_link(site, address);
            }
            if !waiting.is_empty() {
                cache.incoming.entry(pc).or_default().extend(waiting);
            }
            set_jump(pc, address);
        } else {
            sync_other_cores();
        }
        (address, block.end, enter, exit_stub)
    }
}

type EnterFn = unsafe extern "C" fn(*mut CpuState, *mut u8, usize, usize) -> u32;

/// Runs guest code from `pc` (bit 0 = Thumb) until it reaches `stop_at`.
pub fn run(uc: &mut Uc, pc: u32, stop_at: u32) -> Result<(), String> {
    let memory_base = guest_mem().host(0) as *mut u8;
    let mut pc = pc;
    // The block that jumped to `pc` (for fault messages).
    let mut came_from = 0u32;
    loop {
        if pc & !1 == stop_at {
            return Ok(());
        }
        // Linked exits came here instead of jumping on: another thread has
        // waited a whole time slice, the world is stopping, or a signal came.
        runtime::check_in(uc, pc);
        let address = pc & !1;
        if address >= mem::STUB_BASE && address < mem::STUB_BASE + mem::STUB_SIZE {
            // A phone function: `svc #index; bx lr`.
            if address % 8 == 4 {
                // The `bx lr` half: a Unicorn fallback stopped right after the
                // `svc`, so the function already ran. Just return.
                pc = uc.reg(LR);
                continue;
            }
            let index = (address - mem::STUB_BASE) / 8;
            if index < runtime::FIRST_STUB {
                return Err(format!("guest returned to a stale return trap at {address:#010x}"));
            }
            if !runtime::call_light(uc, index) {
                runtime::call_stub(uc, index);
            }
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
        if uc.get_data().native.as_ref().unwrap().state.fpscr & FPSCR_VECTOR != 0 {
            // VFP short vectors (never used on Android): leave the code to Unicorn.
            pc = fallback(uc, pc, 1, stop_at)?;
            continue;
        }
        if !mem::executable(address) {
            let message = format!(
                "jumped to {pc:#010x}, which is not code, from the block at {came_from:#010x} (lr {:#010x}, sp {:#010x})",
                uc.reg(LR),
                uc.reg(13)
            );
            uc.get_data_mut().fault = Some(message.clone());
            return Err(message);
        }
        let (code, end, enter, exit_stub) = table_block(pc).unwrap_or_else(|| block_for(pc));
        let reference = if check::enabled() { check::reference(uc, pc, end, stop_at) } else { None };
        let state = &mut uc.get_data_mut().native.as_mut().expect("translator state").state as *mut CpuState;
        let enter: EnterFn = unsafe { std::mem::transmute(enter) };
        let reason = unsafe { enter(state, memory_base, code, exit_stub) };
        if let Some(reference) = reference {
            check::compare(uc, pc, end, &reference);
        }
        let state = unsafe { &mut *state };
        came_from = pc;
        pc = state.exit_pc;
        match reason & 0xFF {
            exit::JUMP => {}
            exit::FALLBACK => {
                pc = fallback(uc, pc, (reason >> 8).max(1), stop_at)?;
            }
            exit::YIELD => runtime::check_in(uc, pc),
            exit::SYSCALL => {
                runtime::native_syscall(uc, pc);
                let thread = uc.get_data();
                if let Some(fault) = &thread.fault {
                    return Err(fault.clone());
                }
                if let Some(code) = thread.exit_code {
                    return Err(format!("guest called exit({code})"));
                }
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
/// CPSR IT state (bits 10-15 and 25-26).
const CPSR_IT: u32 = 0x0600_FC00;

/// Times Unicorn still had IT state when guest state was copied in (ARM32_STATS / checker summary).
pub(super) static STALE_IT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Blocks dropped because the game rewrote their code (shown with the stats).
static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Copies guest state into Unicorn. Only NZCV, Q and GE move between engines;
/// Unicorn keeps its mode bits, and the Thumb bit comes from the start address.
/// The IT state is cleared: the guest is never inside an IT block here, since
/// translated code exits between instructions and Unicorn cannot stop inside one.
pub(super) fn write_unicorn(uc: &mut Uc, state: &CpuState) {
    let current = uc.reg_read(RegisterARM::CPSR).unwrap() as u32;
    if current & CPSR_IT != 0 {
        STALE_IT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let cpsr = (current & !(0xF000_0000 | CPSR_Q_GE | CPSR_IT)) | (state.nzcv as u32 & 0xF000_0000) | (state.cpsr_other & CPSR_Q_GE);
    let mut values = [0u64; TRANSFER_COUNT];
    for i in 0..15 {
        values[i] = state.r[i] as u64;
    }
    values[15] = cpsr as u64;
    for i in 0..32 {
        values[16 + i] = state.dreg(i);
    }
    values[48] = state.fpscr as u64;
    // The exclusive monitor, so an LDREX on one engine pairs with a STREX on the other.
    values[49] = if state.excl_addr == super::NO_EXCLUSIVE { u64::MAX } else { state.excl_addr as u64 };
    values[50] = state.excl_value;
    values[51] = state.tls as u64;
    let pointers: [*const std::ffi::c_void; TRANSFER_COUNT] = std::array::from_fn(|i| (&raw const values[i]).cast());
    let ids = transfer_ids();
    let error = unsafe { uc_reg_write_batch(uc.get_handle().cast(), ids.as_ptr(), pointers.as_ptr(), TRANSFER_COUNT as i32) };
    assert_eq!(error, 0, "Unicorn register write");
}

/// Unicorn's exclusive monitor (local patch, see vendor/unicorn-engine-sys/ARM32-BRIDGE.md).
const UC_ARM_REG_EXCLUSIVE_ADDR: i32 = 141;
const UC_ARM_REG_EXCLUSIVE_VAL: i32 = 142;

const TRANSFER_COUNT: usize = 52;

fn transfer_ids() -> &'static [i32; TRANSFER_COUNT] {
    static IDS: OnceLock<[i32; TRANSFER_COUNT]> = OnceLock::new();
    IDS.get_or_init(|| {
        let mut ids = [0i32; TRANSFER_COUNT];
        for (i, register) in CORE.iter().enumerate() {
            ids[i] = *register as i32;
        }
        ids[15] = RegisterARM::CPSR as i32;
        for i in 0..32 {
            ids[16 + i] = runtime::double_register(i) as i32;
        }
        ids[48] = RegisterARM::FPSCR as i32;
        ids[49] = UC_ARM_REG_EXCLUSIVE_ADDR;
        ids[50] = UC_ARM_REG_EXCLUSIVE_VAL;
        ids[51] = RegisterARM::C13_C0_3 as i32;
        ids
    })
}

fn read_ids() -> &'static [i32; TRANSFER_COUNT] {
    static IDS: OnceLock<[i32; TRANSFER_COUNT]> = OnceLock::new();
    IDS.get_or_init(|| {
        let mut ids = *transfer_ids();
        ids[51] = RegisterARM::PC as i32;
        ids
    })
}

extern "C" {
    fn uc_reg_write_batch(uc: *mut std::ffi::c_void, regs: *const i32, values: *const *const std::ffi::c_void, count: i32) -> i32;
    fn uc_reg_read_batch(uc: *mut std::ffi::c_void, regs: *const i32, values: *const *mut std::ffi::c_void, count: i32) -> i32;
}

/// Copies Unicorn's registers into `state` and returns its pc (bit 0 = Thumb).
pub(super) fn read_unicorn(uc: &mut Uc, state: &mut CpuState) -> u32 {
    let mut values = [0u64; TRANSFER_COUNT];
    let base = values.as_mut_ptr();
    let pointers: [*mut std::ffi::c_void; TRANSFER_COUNT] = std::array::from_fn(|i| unsafe { base.add(i) }.cast());
    let ids = read_ids();
    let error = unsafe { uc_reg_read_batch(uc.get_handle().cast(), ids.as_ptr(), pointers.as_ptr(), TRANSFER_COUNT as i32) };
    assert_eq!(error, 0, "Unicorn register read");
    for i in 0..15 {
        state.r[i] = values[i] as u32;
    }
    let cpsr = values[15] as u32;
    state.nzcv = (cpsr & 0xF000_0000) as u64;
    state.cpsr_other = cpsr & CPSR_Q_GE;
    for i in 0..32 {
        state.set_dreg(i, values[16 + i]);
    }
    state.fpscr = values[48] as u32;
    let address = values[49];
    state.excl_addr = if address > u32::MAX as u64 { super::NO_EXCLUSIVE } else { address as u32 };
    state.excl_value = values[50];
    let pc = values[51] as u32 | ((cpsr >> 5) & 1);
    state.r[15] = pc & !1;
    pc
}

/// Takes the guest registers from Unicorn into this thread's translator
/// state (a nested call while Unicorn runs the caller).
pub fn seed_from_unicorn(uc: &mut Uc) {
    let mut state = uc.get_data().native.as_ref().unwrap().state;
    read_unicorn(uc, &mut state);
    uc.get_data_mut().native.as_mut().unwrap().state = state;
}

/// Runs `count` instructions at `pc` in Unicorn and returns where it stopped.
fn fallback(uc: &mut Uc, pc: u32, count: u32, stop_at: u32) -> Result<u32, String> {
    if crate::runtime::stats_enabled() {
        let total = {
            let mut cache = write_cache();
            *cache.fallbacks.entry(pc).or_default() += 1;
            cache.fallback_total += 1;
            cache.fallback_total
        };
        static LAST_PRINT: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if total % 4096 == 0 && now >= LAST_PRINT.load(Ordering::Relaxed) + 10 {
            LAST_PRINT.store(now, Ordering::Relaxed);
            print_stats(); // a running app never reaches the end-of-run print
        }
    }
    let mut state = uc.get_data().native.as_ref().unwrap().state;
    write_unicorn(uc, &state);
    uc.get_data_mut().native.as_mut().unwrap().active = false;
    runtime::sync_unicorn_code(uc);
    let result = uc.emu_start(pc as u64, stop_at as u64, 0, count as usize);
    let new_pc = read_unicorn(uc, &mut state);
    let native = uc.get_data_mut().native.as_mut().unwrap();
    native.active = true;
    native.state = state;

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
    {
        let cache = read_cache();
        let mut sites: Vec<(&u32, &u64)> = cache.fallbacks.iter().collect();
        sites.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = cache.fallbacks.values().sum();
        crate::log::line(&format!(
            "[jit] {} blocks ({} dropped after code changes), {} fallbacks at {} sites, {} starts with leftover IT state",
            cache.blocks.len(),
            DROPPED.load(std::sync::atomic::Ordering::Relaxed),
            total,
            sites.len(),
            STALE_IT.load(std::sync::atomic::Ordering::Relaxed)
        ));
        for (pc, count) in sites.iter().take(40) {
            let pc = **pc;
            let thumb = pc & 1 == 1;
            let address = pc & !1;
            let first = guest_mem().read_u16(address);
            let second = guest_mem().read_u16(address + 2);
            let text = if thumb { format!("{first:04x} {second:04x}") } else { format!("{:08x}", guest_mem().read_u32(address)) };
            crate::log::line(&format!("[jit]   {count:8} x {address:#010x} {} {text}", if thumb { "T" } else { "A" }));
        }
    }
    if runtime::parallel() {
        crate::log::line(&format!(
            "[jit] parallel: {} core syncs after new code, {} stops of every thread",
            MEMBARRIERS.load(Ordering::Relaxed),
            runtime::STOPS.load(Ordering::Relaxed)
        ));
    }
    crate::runtime::log_top_calls(20);
    if check::enabled() {
        check::print_summary();
    }
}
