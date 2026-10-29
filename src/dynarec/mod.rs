//! ARM32 → ARM64 translator (see docs/translator.md).
//!
//! Guest code is translated block by block into ARM64 code that keeps the
//! guest registers in host registers (r0-r14 → w0-w14, NZCV → host NZCV,
//! q0-q15 → v0-v15). Anything not translated yet runs in this thread's
//! Unicorn engine for a few instructions ("fallback").

pub mod check;
pub mod emit;
pub mod send_cache;
pub mod thumb;

mod engine;
pub use engine::{enabled, fallback_count, invalidate, invalidate_library, print_stats, property, run, seed_from_unicorn, Native};

/// The CPSR bits MRS reads besides the flags (mode, A/I/F masks, E), as Unicorn
/// has them: translated MRS returns what Unicorn's did. Set when an engine is made.
pub static CPSR_FIXED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x10);

/// Guest register state while (or between) running translated code.
/// Offsets are used by the generated code; keep them in sync with `offsets`.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
pub struct CpuState {
    /// r0-r15 (r15 is only meaningful outside translated code).
    pub r: [u32; 16],
    /// Host NZCV format: N=bit 31, Z=30, C=29, V=28.
    pub nzcv: u64,
    /// Where translated code left off: guest pc, bit 0 = Thumb.
    pub exit_pc: u32,
    /// Why it left: low 8 bits = `exit::*`, the rest depends on the reason.
    pub exit_reason: u32,
    pub host_sp: u64,
    pub fpscr: u32,
    /// CPSR Q and GE bits.
    pub cpsr_other: u32,
    /// TPIDRURO (bionic's thread pointer).
    pub tls: u32,
    /// The host's FPCR while translated code runs (restored on exit).
    pub host_fpcr: u32,
    /// Address of the shared "leave translated code" flag (runtime::PREEMPT), read at linked exits.
    pub preempt_flag: u64,
    /// The exclusive monitor: address of the last LDREX (`NO_EXCLUSIVE` = none)
    /// and the value it read. STREX stores only if memory still holds that value.
    pub excl_addr: u32,
    pub _pad: u32,
    pub excl_value: u64,
    /// q0-q15 = d0-d31 (d(2n) is the low half of q(n)).
    pub q: [[u64; 2]; 16],
}

pub mod offsets {
    pub const R: u32 = 0;
    pub const NZCV: u32 = 64;
    pub const EXIT_PC: u32 = 72;
    pub const EXIT_REASON: u32 = 76;
    pub const HOST_SP: u32 = 80;
    pub const FPSCR: u32 = 88;
    pub const CPSR_OTHER: u32 = 92;
    pub const TLS: u32 = 96;
    pub const HOST_FPCR: u32 = 100;
    pub const PREEMPT_FLAG: u32 = 104;
    pub const EXCL_ADDR: u32 = 112;
    pub const EXCL_VALUE: u32 = 120;
    pub const Q: u32 = 128;
}

const _: () = {
    use std::mem::offset_of;
    assert!(offset_of!(CpuState, r) == offsets::R as usize);
    assert!(offset_of!(CpuState, nzcv) == offsets::NZCV as usize);
    assert!(offset_of!(CpuState, exit_pc) == offsets::EXIT_PC as usize);
    assert!(offset_of!(CpuState, exit_reason) == offsets::EXIT_REASON as usize);
    assert!(offset_of!(CpuState, host_sp) == offsets::HOST_SP as usize);
    assert!(offset_of!(CpuState, fpscr) == offsets::FPSCR as usize);
    assert!(offset_of!(CpuState, cpsr_other) == offsets::CPSR_OTHER as usize);
    assert!(offset_of!(CpuState, tls) == offsets::TLS as usize);
    assert!(offset_of!(CpuState, host_fpcr) == offsets::HOST_FPCR as usize);
    assert!(offset_of!(CpuState, preempt_flag) == offsets::PREEMPT_FLAG as usize);
    assert!(offset_of!(CpuState, excl_addr) == offsets::EXCL_ADDR as usize);
    assert!(offset_of!(CpuState, excl_value) == offsets::EXCL_VALUE as usize);
    assert!(offset_of!(CpuState, q) == offsets::Q as usize);
};

/// `CpuState::excl_addr` when no LDREX is outstanding.
pub const NO_EXCLUSIVE: u32 = u32::MAX;

impl CpuState {
    /// Forgets an outstanding LDREX, as the kernel does on every exception
    /// return (system calls, phone functions, signal handlers).
    pub fn clear_exclusive(&mut self) {
        self.excl_addr = NO_EXCLUSIVE;
    }
    pub fn dreg(&self, index: usize) -> u64 {
        self.q[index / 2][index % 2]
    }
    pub fn set_dreg(&mut self, index: usize, value: u64) {
        self.q[index / 2][index % 2] = value;
    }
}

/// FPSCR bits kept in host FPCR (AHP, DN, FZ, RMode) while translated code
/// runs; ARM64 FPCR has them at the same positions.
pub const FPSCR_CONTROL: u32 = 0x07C0_0000;
/// FPSCR cumulative exception flags (QC, IDC, IXC, UFC, OFC, DZC, IOC), kept in
/// host FPSR (same positions).
pub const FPSCR_CUMULATIVE: u32 = 0x0800_009F;
/// FPSCR bits a VMSR can set (no exception enables, no FZ16), as on Cortex-A15 and in Unicorn.
pub const FPSCR_WRITABLE: u32 = 0xFFF7_009F;
/// The "standard FPSCR value" ARMv7 NEON floating point uses: flush-to-zero, default NaN, round to nearest.
pub const FPSCR_STANDARD: u32 = 0x0300_0000;
/// FPSCR LEN and STRIDE: VFP short vectors, which the translator does not do.
pub const FPSCR_VECTOR: u32 = 0x0037_0000;

/// Exit reasons (low 8 bits of `exit_reason`).
pub mod exit {
    /// Continue at `exit_pc`.
    pub const JUMP: u32 = 0;
    /// Run `exit_reason >> 8` instructions at `exit_pc` in Unicorn.
    pub const FALLBACK: u32 = 1;
    pub const YIELD: u32 = 2;
    /// A Linux system call (`svc` outside the stub area): run it, continue at `exit_pc`.
    pub const SYSCALL: u32 = 3;
}

/// Host registers with a fixed job inside translated code.
pub mod host {
    use super::emit::Reg;
    pub const STATE: Reg = 19;
    pub const MEMORY: Reg = 20;
    pub const EXIT_PC: Reg = 21;
    pub const EXIT_REASON: Reg = 22;
    pub const EXIT_STUB: Reg = 23;
    /// Until a block exit writes EXIT_PC and EXIT_REASON, those registers hold
    /// the address of the flag linked exits check (`CpuState::preempt_flag`)
    /// and of the jump table (the enter glue loads both), so an exit reads
    /// the flag with one load and finds the table without building its address.
    pub const PREEMPT_FLAG: Reg = EXIT_PC;
    pub const JUMP_TABLE: Reg = EXIT_REASON;
}
