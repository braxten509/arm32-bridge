//! Advanced SIMD (NEON) → ARM64 Advanced SIMD, decoded in the ARM encoding
//! (the Thumb forms are converted by the caller).
//!
//! Guest d(n) is half of v(n/2) and guest q(n) is all of v(n). A Q operation
//! works on the host register directly; a D operation reads an odd d register
//! through a scratch register (64-bit host operations clear the upper half of
//! their destination) and inserts its result into the right half.
//!
//! ARMv7 NEON floating point always uses the "standard FPSCR value"
//! (flush-to-zero, default NaN, round to nearest), while ARM64 vector
//! instructions follow FPCR. `fp_mode(true)` puts FPCR in that mode before NEON
//! floating point and `fp_mode(false)` puts the guest's FPSCR back for VFP.

use crate::dynarec::emit::{simd, Reg};
use crate::dynarec::{FPSCR_CONTROL, FPSCR_STANDARD};
use super::{host, offsets, Flow, Result, Translator};
use super::{PC, SP, T0, T1};

// Scratch vector registers (never guest registers).
const V0: Reg = 16;
const V1: Reg = 17;
const V2: Reg = 18;

impl Translator {
    /// Makes host FPCR match NEON (`true`) or VFP (`false`) floating point.
    pub(super) fn fp_mode(&mut self, neon: bool) {
        if self.neon_fp != neon {
            self.set_fp_mode(neon);
        }
    }

    /// Sets host FPCR for NEON or VFP whatever the tracked mode is (after
    /// conditional code, when the mode depends on whether it ran).
    pub(super) fn set_fp_mode(&mut self, neon: bool) {
        if neon {
            self.a.mov_w_imm(T0, FPSCR_STANDARD);
        } else {
            self.a.ldr_w_off(T0, host::STATE, offsets::FPSCR);
            self.a.and_w_imm(T0, T0, FPSCR_CONTROL, T1);
        }
        self.a.msr_fpcr(T0);
        self.neon_fp = neon;
    }

    /// A host register holding guest d(n) in its low half, or guest q(n/2).
    fn vector_read(&mut self, q: bool, n: u32, scratch: Reg) -> Reg {
        if q || n % 2 == 0 {
            n / 2
        } else {
            self.a.dup_element(true, scratch, n / 2, 1);
            scratch
        }
    }

    /// Where to compute a result for guest register `d` (finish with `vector_write`).
    fn vector_target(q: bool, d: u32) -> Reg {
        if q {
            d / 2
        } else {
            V0
        }
    }

    fn vector_write(&mut self, q: bool, d: u32, value: Reg) {
        if q {
            if value != d / 2 {
                self.a.mov_vector(d / 2, value);
            }
        } else {
            self.a.ins_element(true, d / 2, d % 2, value, 0);
        }
    }

    /// Advanced SIMD data processing (ARM encoding 1111 001U ...).
    pub(super) fn neon_data(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        if bit(23) == 0 {
            self.neon_three_same(word)
        } else if bit(6) == 1 && bit(4) == 0 && (word >> 20) & 3 != 3 {
            self.neon_by_scalar(word)
        } else {
            Err(())
        }
    }

    /// Three registers of the same length.
    fn neon_three_same(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(6) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let n = (bit(7) << 4) | ((word >> 16) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        if q && (d | n | m) & 1 == 1 {
            return Err(()); // undefined
        }
        let size = (word >> 20) & 3;
        let (u, b) = (bit(24) == 1, bit(4) == 1);
        enum Op {
            Plain(u32, u32),
            /// VMLA/VMLS.F32: product rounded, then added to or subtracted from d.
            MultiplyAccumulate(u32),
        }
        let op = match ((word >> 8) & 0xF, u, b) {
            (0b1000, _, _) if size == 3 && !q => return Err(()), // 64-bit lanes in a D register: no vector form
            (0b1000, false, false) => Op::Plain(simd::ADD, size),
            (0b1000, true, false) => Op::Plain(simd::SUB, size),
            (0b0001, false, true) => Op::Plain([simd::AND, simd::BIC, simd::ORR, simd::ORN][size as usize], 0),
            (0b0001, true, true) if size == 0 => Op::Plain(simd::EOR, 0),
            (0b1101, false, false) if size & 1 == 0 => Op::Plain(if size == 0 { simd::FADD } else { simd::FSUB }, 0),
            (0b1101, false, true) if size & 1 == 0 => Op::MultiplyAccumulate(if size == 0 { simd::FADD } else { simd::FSUB }),
            (0b1101, true, true) if size == 0 => Op::Plain(simd::FMUL, 0),
            _ => return Err(()),
        };
        if (word >> 8) & 0xF == 0b1101 {
            self.fp_mode(true);
        }
        let vn = self.vector_read(q, n, V1);
        let vm = self.vector_read(q, m, V2);
        let target = Self::vector_target(q, d);
        match op {
            Op::Plain(op, size) => self.a.simd3(op, q, size, target, vn, vm),
            Op::MultiplyAccumulate(op) => {
                self.a.simd3(simd::FMUL, q, 0, V1, vn, vm);
                let vd = self.vector_read(q, d, V2);
                self.a.simd3(op, q, 0, target, vd, V1);
            }
        }
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// Two registers and a scalar: VMUL, VMLA, VMLS (.F32) by an element.
    fn neon_by_scalar(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(24) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let n = (bit(7) << 4) | ((word >> 16) & 0xF);
        if (word >> 20) & 3 != 2 || (q && (d | n) & 1 == 1) {
            return Err(()); // only 32-bit elements
        }
        // 32-bit elements: the scalar is d(Vm)[M].
        let m = word & 0xF;
        let element = (m % 2) * 2 + bit(5);
        let accumulate = match (word >> 8) & 0xF {
            0b1001 => None,
            0b0001 => Some(simd::FADD),
            0b0101 => Some(simd::FSUB),
            _ => return Err(()),
        };
        self.fp_mode(true);
        let vn = self.vector_read(q, n, V1);
        let target = Self::vector_target(q, d);
        match accumulate {
            None => self.a.fmul_element(q, target, vn, m / 2, element),
            Some(op) => {
                self.a.fmul_element(q, V1, vn, m / 2, element);
                let vd = self.vector_read(q, d, V2);
                self.a.simd3(op, q, 0, target, vd, V1);
            }
        }
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// Advanced SIMD element and structure loads and stores (ARM 1111 0100 xxx0):
    /// VLD1/VST1 of 1-4 registers and of one 32-bit lane.
    pub(super) fn neon_load_store(&mut self, word: u32) -> Result<Flow> {
        let load = (word >> 21) & 1 == 1;
        let (rn, rm) = ((word >> 16) & 0xF, word & 0xF);
        let d = (((word >> 22) & 1) << 4) | ((word >> 12) & 0xF);
        if rn == PC {
            return Err(());
        }
        self.a.add_x_uxtw(T1, host::MEMORY, rn);
        let bytes = if (word >> 23) & 1 == 0 {
            let count = match (word >> 8) & 0xF {
                0b0111 => 1,
                0b1010 => 2,
                0b0110 => 3,
                0b0010 => 4,
                _ => return Err(()), // VLD2-VLD4 and friends
            };
            if d + count > 32 {
                return Err(());
            }
            for r in d..d + count {
                if load {
                    self.a.ld1_lane(true, r / 2, r % 2, T1, true);
                } else {
                    self.a.st1_lane(true, r / 2, r % 2, T1, true);
                }
            }
            count * 8
        } else {
            // One lane: 32-bit elements only (index = bit 7).
            if (word >> 8) & 0xF != 0b1000 || (word >> 6) & 1 != 0 {
                return Err(());
            }
            let lane = (d % 2) * 2 + ((word >> 7) & 1);
            if load {
                self.a.ld1_lane(false, d / 2, lane, T1, false);
            } else {
                self.a.st1_lane(false, d / 2, lane, T1, false);
            }
            4
        };
        match rm {
            PC => {}
            SP => self.a.add_w_imm(rn, rn, bytes),
            _ => self.a.add_w(rn, rn, rm, crate::dynarec::emit::shift::LSL, 0),
        }
        Ok(Flow::Next)
    }
}
