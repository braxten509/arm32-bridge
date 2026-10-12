//! ARM32 → ARM64 translator (see docs/translator.md).
//!
//! Guest code is translated block by block into ARM64 code that keeps the
//! guest registers in host registers (r0-r14 → w0-w14, NZCV → host NZCV,
//! q0-q15 → v0-v15). Anything not translated yet runs in this thread's
//! Unicorn engine for a few instructions ("fallback").

pub mod check;
pub mod emit;
pub mod thumb;

mod engine;
pub use engine::{enabled, print_stats, property, run, Native};

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
    pub _pad: [u32; 6],
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
    assert!(offset_of!(CpuState, q) == offsets::Q as usize);
};

impl CpuState {
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

/// Exit reasons (low 8 bits of `exit_reason`).
pub mod exit {
    /// Continue at `exit_pc`.
    pub const JUMP: u32 = 0;
    /// Run `exit_reason >> 8` instructions at `exit_pc` in Unicorn.
    pub const FALLBACK: u32 = 1;
}

/// Host registers with a fixed job inside translated code.
pub mod host {
    use super::emit::Reg;
    pub const STATE: Reg = 19;
    pub const MEMORY: Reg = 20;
    pub const EXIT_PC: Reg = 21;
    pub const EXIT_REASON: Reg = 22;
    pub const EXIT_STUB: Reg = 23;
}
