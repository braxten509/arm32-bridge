//! The checker (ARM32_CHECK=1, or the Android property debug.arm32bridge.check=1).
//!
//! Before a translated block runs, the same block runs in Unicorn one
//! instruction at a time from the same registers, stopping where the block
//! stops. Unicorn's memory writes are recorded and then undone. After the
//! translated block runs, registers, flags, float registers, the exit point
//! and the written memory must match; the first difference per block is logged.
//!
//! `1` checks each block's first 32 runs and then every 1024th run (fast
//! enough to play); `all` checks every run.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use unicorn_engine::unicorn_const::HookType;

use super::engine::{property, read_unicorn, write_unicorn};
use super::CpuState;
use crate::runtime::{mem as guest_mem, Uc};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Off,
    Sampled,
    All,
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let value = std::env::var("ARM32_CHECK").ok().or_else(|| property("debug.arm32bridge.check"));
        match value.as_deref() {
            Some("1") => Mode::Sampled,
            Some("all") => Mode::All,
            _ => Mode::Off,
        }
    })
}

pub fn enabled() -> bool {
    mode() != Mode::Off
}

/// Longest run of Unicorn steps for one block (a block has at most 64 instructions).
const MAX_STEPS: u32 = 256;
/// Blocks whose difference gets logged in full.
const MAX_REPORTS: usize = 30;

static CHECKS: AtomicU64 = AtomicU64::new(0);
static DIFFERENCES: AtomicU64 = AtomicU64::new(0);
/// Reference runs that stopped with an error (not compared).
static FAILED: AtomicU64 = AtomicU64::new(0);
/// Reference runs where Unicorn ran instructions outside the block (not compared).
/// Unicorn treats a Thumb IT block as one instruction and can miss a stop request
/// for a few instructions after one, so it sometimes runs past where the block ends.
static RAN_PAST: AtomicU64 = AtomicU64::new(0);
static REPORTED: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    /// Unicorn's writes during a reference run: address and the bytes before.
    static WRITES: RefCell<Vec<(u32, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    static RUNS: RefCell<HashMap<u32, u64>> = RefCell::new(HashMap::new());
    /// Every instruction Unicorn ran during a reference run (address, size), for the report.
    static EXECUTED: RefCell<Vec<(u32, u32)>> = const { RefCell::new(Vec::new()) };
}

/// What Unicorn did with the block.
pub struct Reference {
    start: CpuState,
    end_state: CpuState,
    exit_pc: u32,
    steps: u32,
    /// Address and the bytes Unicorn left there.
    writes: Vec<(u32, Vec<u8>)>,
    /// Instructions Unicorn ran, with the step each began (address, size, step).
    executed: Vec<(u32, u32, u32)>,
}

fn should_check(pc: u32) -> bool {
    if mode() == Mode::All {
        return true;
    }
    RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        let count = runs.entry(pc).or_default();
        *count += 1;
        *count <= 32 || *count % 1024 == 0
    })
}

fn install_hook(uc: &mut Uc) {
    let native = uc.get_data_mut().native.as_mut().unwrap();
    if native.check_hook {
        return;
    }
    native.check_hook = true;
    uc.add_mem_hook(HookType::MEM_WRITE, 0, u64::MAX, |_uc, _kind, address, size, _value| {
        if RECORDING.with(|recording| recording.get()) {
            let address = address as u32;
            if !guest_mem().host_or_null(address).is_null() {
                let before = guest_mem().bytes(address, size).to_vec();
                WRITES.with(|writes| writes.borrow_mut().push((address, before)));
            }
        }
        true
    })
    .expect("add checker write hook");
    uc.add_code_hook(1, 0, |_uc, address, size| {
        if RECORDING.with(|recording| recording.get()) {
            EXECUTED.with(|executed| executed.borrow_mut().push((address as u32, size)));
        }
    })
    .expect("add checker code hook");
    // Code Unicorn translated before the hook existed would not report its writes.
    uc.ctl_flush_tb().expect("flush Unicorn's translated code");
}

/// Runs the block at `pc` (bit 0 = Thumb; instructions up to `end`) in Unicorn
/// and undoes its memory writes. None when the block cannot be checked.
pub fn reference(uc: &mut Uc, pc: u32, end: u32, stop_at: u32) -> Option<Reference> {
    let thumb = pc & 1;
    if end <= pc & !1 || !should_check(pc) {
        return None;
    }
    install_hook(uc);
    // Code another thread rewrote since this engine last ran (cacheflush).
    crate::runtime::sync_unicorn_code(uc);
    let start = uc.get_data().native.as_ref().unwrap().state;
    write_unicorn(uc, &start);
    let fault_before = uc.get_data_mut().fault.take();
    uc.get_data_mut().native.as_mut().unwrap().active = false;
    WRITES.with(|writes| writes.borrow_mut().clear());
    EXECUTED.with(|executed| executed.borrow_mut().clear());
    let mut executed = Vec::new();
    RECORDING.with(|recording| recording.set(true));

    let mut current = pc & !1;
    let mut steps = 0;
    let mut end_state = start;
    let mut failed = false;
    let exit_pc = loop {
        // A raw system call ends the translated block before it runs (the dispatcher
        // runs it afterwards): stop here too, and never make the call twice.
        let (is_svc, size) = if thumb == 1 {
            (guest_mem().read_u16(current) >> 8 == 0xDF, 2)
        } else {
            let word = guest_mem().read_u32(current);
            ((word >> 24) & 0xF == 0xF && word >> 28 != 0xF, 4)
        };
        if is_svc && !(crate::mem::STUB_BASE..crate::mem::STUB_BASE + crate::mem::STUB_SIZE).contains(&current) {
            break (current + size) | thumb;
        }
        let result = uc.emu_start((current | thumb) as u64, stop_at as u64, 0, 1);
        steps += 1;
        EXECUTED.with(|ran| executed.extend(ran.borrow_mut().drain(..).map(|(address, size)| (address, size, steps))));
        let next = read_unicorn(uc, &mut end_state);
        if result.is_err() || uc.get_data().fault.is_some() {
            if FAILED.fetch_add(1, Ordering::Relaxed) < 5 {
                crate::log::line(&format!(
                    "[check] Unicorn stopped in block {:#010x} at {current:#010x}: {result:?} {:?}",
                    pc & !1,
                    uc.get_data().fault
                ));
            }
            failed = true;
            break next;
        }
        let address = next & !1;
        if next & 1 != thumb || address <= current || address >= end || steps >= MAX_STEPS {
            break next;
        }
        current = address;
    };

    RECORDING.with(|recording| recording.set(false));
    let recorded = WRITES.with(|writes| std::mem::take(&mut *writes.borrow_mut()));
    let writes = recorded.iter().map(|(address, before)| (*address, guest_mem().bytes(*address, before.len()).to_vec())).collect();
    for (address, before) in recorded.iter().rev() {
        guest_mem().write_bytes(*address, before);
    }
    let thread = uc.get_data_mut();
    thread.fault = fault_before;
    thread.native.as_mut().unwrap().active = true;
    if failed {
        return None;
    }
    let block = (pc & !1)..end;
    if executed.iter().any(|(address, _, _)| !block.contains(address)) {
        RAN_PAST.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some(Reference { start, end_state, exit_pc, steps, writes, executed })
}

/// Compares the translated block's result with Unicorn's.
pub fn compare(uc: &mut Uc, pc: u32, end: u32, reference: &Reference) {
    if (CHECKS.fetch_add(1, Ordering::Relaxed) + 1) % (1 << 18) == 0 {
        print_summary(); // a running app never reaches the end-of-run print
    }
    let state = &uc.get_data().native.as_ref().unwrap().state;
    let expected = &reference.end_state;
    let mut differences = Vec::new();
    if state.exit_pc != reference.exit_pc {
        differences.push(format!("exit pc jit={:#010x} unicorn={:#010x}", state.exit_pc, reference.exit_pc));
    }
    for i in 0..15 {
        if state.r[i] != expected.r[i] {
            differences.push(format!("r{i} jit={:#010x} unicorn={:#010x}", state.r[i], expected.r[i]));
        }
    }
    let flags = |value: u64| value as u32 & 0xF000_0000;
    if flags(state.nzcv) != flags(expected.nzcv) {
        differences.push(format!("NZCV jit={:x} unicorn={:x}", flags(state.nzcv) >> 28, flags(expected.nzcv) >> 28));
    }
    if state.cpsr_other != expected.cpsr_other {
        differences.push(format!("Q/GE jit={:#x} unicorn={:#x}", state.cpsr_other, expected.cpsr_other));
    }
    for i in 0..32 {
        if state.dreg(i) != expected.dreg(i) {
            differences.push(format!("d{i} jit={:#018x} unicorn={:#018x}", state.dreg(i), expected.dreg(i)));
        }
    }
    if state.fpscr != expected.fpscr {
        differences.push(format!("FPSCR jit={:#x} unicorn={:#x}", state.fpscr, expected.fpscr));
    }
    for (address, bytes) in &reference.writes {
        let now = guest_mem().bytes(*address, bytes.len()).to_vec();
        if &now != bytes {
            differences.push(format!("memory {address:#010x} jit={now:02x?} unicorn={bytes:02x?}"));
        }
    }
    if differences.is_empty() {
        return;
    }
    DIFFERENCES.fetch_add(1, Ordering::Relaxed);
    let mut reported = REPORTED.lock().unwrap();
    let reported = reported.get_or_insert_with(HashSet::new);
    if reported.len() >= MAX_REPORTS || !reported.insert(pc) {
        return;
    }
    let start = &reference.start;
    let mut text = format!(
        "[check] difference in block {:#010x}..{end:#010x} ({} Unicorn steps, call depth {}):",
        pc & !1,
        reference.steps,
        uc.get_data().depth
    );
    for difference in &differences {
        text.push_str(&format!("\n[check]   {difference}"));
    }
    text.push_str("\n[check]   start:");
    for i in 0..15 {
        text.push_str(&format!(" r{i}={:x}", start.r[i]));
    }
    text.push_str(&format!(" NZCV={:x}", flags(start.nzcv) >> 28));
    text.push_str("\n[check]   Unicorn ran (step: address/size):");
    for (address, size, step) in reference.executed.iter().take(48) {
        text.push_str(&format!(" {step}:{address:x}/{size}"));
    }
    text.push_str("\n[check]   code:");
    let mut address = pc & !1;
    while address < end {
        text.push_str(&format!(" {:04x}", guest_mem().read_u16(address)));
        address += 2;
    }
    crate::log::line(&text);
}

pub fn print_summary() {
    crate::log::line(&format!(
        "[check] {} blocks checked, {} differences, not compared: {} (Unicorn stopped), {} (Unicorn ran past the block)",
        CHECKS.load(Ordering::Relaxed),
        DIFFERENCES.load(Ordering::Relaxed),
        FAILED.load(Ordering::Relaxed),
        RAN_PAST.load(Ordering::Relaxed)
    ));
}
