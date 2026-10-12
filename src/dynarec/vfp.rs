//! VFP (scalar floating point) → ARM64 scalar floating point. Thumb-2 and ARM
//! share these encodings (the Thumb form is the ARM form with condition 1110),
//! so one decoder serves both; the caller handles the condition.
//!
//! Guest s and d registers are lanes of v0-v15: s(n) = v(n/4).s[n%4] and
//! d(n) = v(n/2).d[n%2]. Host scalar instructions read lane 0 and clear the
//! rest of their destination, so a source in another lane is first copied to
//! a scratch register (v16-v18), and every result is made in v16 and then
//! inserted into its lane.
//!
//! FPSCR while translated code runs: the control bits are in host FPCR and the
//! cumulative exception flags in host FPSR (same bit positions; the enter and
//! exit glue moves them); the compare flags (NZCV) stay in `CpuState::fpscr`.

use crate::dynarec::emit::{fp, shift, Reg};
use super::{host, offsets, Flow, Result, Translator};
use super::{PC, T0, T1, T2};
use crate::dynarec::{FPSCR_CONTROL, FPSCR_CUMULATIVE};

// Scratch vector registers (never guest registers).
const V0: Reg = 16;
const V1: Reg = 17;
const V2: Reg = 18;

/// A guest VFP register.
#[derive(Clone, Copy)]
struct Fp {
    double: bool,
    n: u32,
}

impl Fp {
    /// From an encoding's 4-bit field and its extra bit (D, N or M).
    fn decode(double: bool, field: u32, extra: u32) -> Fp {
        Fp { double, n: if double { (extra << 4) | field } else { (field << 1) | extra } }
    }
    fn host(self) -> Reg {
        if self.double {
            self.n / 2
        } else {
            self.n / 4
        }
    }
    fn lane(self) -> u32 {
        if self.double {
            self.n % 2
        } else {
            self.n % 4
        }
    }
    fn next(self) -> Fp {
        Fp { double: self.double, n: self.n + 1 }
    }
}

impl Translator {
    /// A host register holding `r` in lane 0.
    fn fp_read(&mut self, r: Fp, scratch: Reg) -> Reg {
        if r.lane() == 0 {
            r.host()
        } else {
            self.a.dup_element(r.double, scratch, r.host(), r.lane());
            scratch
        }
    }

    /// Stores lane 0 of `value` into `r`.
    fn fp_write(&mut self, r: Fp, value: Reg) {
        self.a.ins_element(r.double, r.host(), r.lane(), value, 0);
    }

    /// A VFP instruction (ARM encoding, condition ignored).
    pub(super) fn vfp(&mut self, word: u32) -> Result<Flow> {
        if (word >> 9) & 7 != 0b101 {
            return Err(());
        }
        match (word >> 24) & 0xF {
            0b1110 if (word >> 4) & 1 == 0 => self.vfp_data(word),
            0b1110 => self.vfp_transfer(word),
            0b1100 | 0b1101 => self.vfp_load_store(word),
            _ => Err(()),
        }
    }

    fn vfp_data(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let double = bit(8) == 1;
        let d = Fp::decode(double, (word >> 12) & 0xF, bit(22));
        let n = Fp::decode(double, (word >> 16) & 0xF, bit(7));
        let m = Fp::decode(double, word & 0xF, bit(5));
        let negate = bit(6) == 1;
        let opc1 = (bit(23) << 2) | ((word >> 20) & 3);
        if opc1 == 0b111 {
            return self.vfp_other(word, double, d, m);
        }
        let vn = self.fp_read(n, V0);
        let vm = self.fp_read(m, V1);
        match opc1 {
            0b000 | 0b001 => {
                // VMLA, VMLS, VNMLS, VNMLA: (±d) + (±(n*m)), the product rounded first
                self.a.fp2(fp::FMUL, double, V0, vn, vm);
                if negate {
                    self.a.fp1(fp::FNEG, double, V0, V0);
                }
                let mut vd = self.fp_read(d, V1);
                if opc1 == 0b001 {
                    self.a.fp1(fp::FNEG, double, V1, vd);
                    vd = V1;
                }
                self.a.fp2(fp::FADD, double, V0, vd, V0);
            }
            0b010 => self.a.fp2(if negate { fp::FNMUL } else { fp::FMUL }, double, V0, vn, vm),
            0b011 => self.a.fp2(if negate { fp::FSUB } else { fp::FADD }, double, V0, vn, vm),
            0b100 if !negate => self.a.fp2(fp::FDIV, double, V0, vn, vm),
            0b101 | 0b110 => {
                // VFNMA/VFNMS: -d ∓ n*m; VFMA/VFMS: d ± n*m (fused)
                let vd = self.fp_read(d, V2);
                let op = match (opc1, negate) {
                    (0b101, true) => fp::FNMADD,
                    (0b101, false) => fp::FNMSUB,
                    (_, false) => fp::FMADD,
                    (_, true) => fp::FMSUB,
                };
                self.a.fp3(op, double, V0, vn, vm, vd);
            }
            _ => return Err(()),
        }
        self.fp_write(d, V0);
        Ok(Flow::Next)
    }

    /// VMOV (immediate, register), VABS, VNEG, VSQRT, VCMP, VCVT.
    fn vfp_other(&mut self, word: u32, double: bool, d: Fp, m: Fp) -> Result<Flow> {
        let opc2 = (word >> 16) & 0xF;
        let op7 = (word >> 7) & 1 == 1;
        if (word >> 6) & 1 == 0 {
            // VMOV immediate: the same 8-bit float encoding as ARM64 FMOV
            let imm8 = ((word >> 12) & 0xF0) | (word & 0xF);
            self.a.fmov_imm(double, V0, imm8);
            self.fp_write(d, V0);
            return Ok(Flow::Next);
        }
        match (opc2, op7) {
            (0b0000, false) => {
                // VMOV register
                self.a.ins_element(double, d.host(), d.lane(), m.host(), m.lane());
                return Ok(Flow::Next);
            }
            (0b0000, true) | (0b0001, _) => {
                let op = match (opc2, op7) {
                    (0b0000, _) => fp::FABS,
                    (_, false) => fp::FNEG,
                    _ => fp::FSQRT,
                };
                let vm = self.fp_read(m, V1);
                self.a.fp1(op, double, V0, vm);
                self.fp_write(d, V0);
            }
            (0b0100 | 0b0101, _) => {
                // VCMP{E} with a register or zero: sets FPSCR.NZCV, keeps the guest flags
                let zero = opc2 & 1 == 1;
                let vd = self.fp_read(d, V0);
                let vm = if zero { 0 } else { self.fp_read(m, V1) };
                self.a.mrs_nzcv(T0);
                self.a.fcmp(double, vd, vm, zero, op7);
                self.a.mrs_nzcv(T1);
                self.a.msr_nzcv(T0);
                self.a.ldr_w_off(T2, host::STATE, offsets::FPSCR);
                self.a.lsr_w_imm(T1, T1, 28);
                self.a.bfi_w(T2, T1, 28, 4);
                self.a.str_w_off(T2, host::STATE, offsets::FPSCR);
            }
            (0b0111, true) => {
                // VCVT between double and single
                let d = Fp::decode(!double, (word >> 12) & 0xF, (word >> 22) & 1);
                let vm = self.fp_read(m, V1);
                self.a.fp1(if double { fp::FCVT_TO_SINGLE } else { fp::FCVT_TO_DOUBLE }, double, V0, vm);
                self.fp_write(d, V0);
            }
            (0b1000, _) => {
                // VCVT integer (in a single register) → float; op7 = signed
                let m = Fp::decode(false, word & 0xF, (word >> 5) & 1);
                self.a.umov(false, T0, m.host(), m.lane());
                self.a.int_to_float(op7, double, V0, T0);
                self.fp_write(d, V0);
            }
            (0b1100 | 0b1101, true) => {
                // VCVT float → integer, rounding toward zero; opc2 bit 0 = signed
                let d = Fp::decode(false, (word >> 12) & 0xF, (word >> 22) & 1);
                let vm = self.fp_read(m, V1);
                self.a.float_to_int(opc2 & 1 == 1, double, T0, vm);
                self.a.ins_general(false, d.host(), d.lane(), T0);
            }
            _ => return Err(()), // VCVTR, half precision, fixed point
        }
        Ok(Flow::Next)
    }

    /// Moves between core registers and VFP registers or FPSCR.
    fn vfp_transfer(&mut self, word: u32) -> Result<Flow> {
        let rt = (word >> 12) & 0xF;
        let to_core = (word >> 20) & 1 == 1;
        if (word >> 8) & 1 == 0 {
            match (word >> 21) & 7 {
                0b000 if word & 0x6F == 0 => {
                    // VMOV Sn <-> Rt
                    if rt == PC {
                        return Err(());
                    }
                    let n = Fp::decode(false, (word >> 16) & 0xF, (word >> 7) & 1);
                    if to_core {
                        self.a.umov(false, rt, n.host(), n.lane());
                    } else {
                        self.a.ins_general(false, n.host(), n.lane(), rt);
                    }
                    Ok(Flow::Next)
                }
                0b111 if (word >> 16) & 0xF == 1 && word & 0xEF == 0 => {
                    if to_core {
                        self.vmrs(rt);
                    } else {
                        // VMSR FPSCR, Rt
                        if rt == PC {
                            return Err(());
                        }
                        self.a.str_w_off(rt, host::STATE, offsets::FPSCR);
                        self.a.and_w_imm(T0, rt, FPSCR_CUMULATIVE, T1);
                        self.a.msr_fpsr(T0);
                        self.a.and_w_imm(T0, rt, FPSCR_CONTROL, T1);
                        self.a.msr_fpcr(T0);
                    }
                    Ok(Flow::Next)
                }
                _ => Err(()),
            }
        } else {
            // VMOV.32 Dd[x], Rt / VMOV.32 Rt, Dn[x] (8- and 16-bit lanes are left to Unicorn)
            if (word >> 22) & 3 != 0 || (word >> 5) & 3 != 0 || word & 0xF != 0 || rt == PC {
                return Err(());
            }
            let d = Fp::decode(true, (word >> 16) & 0xF, (word >> 7) & 1);
            let lane = d.lane() * 2 + ((word >> 21) & 1);
            if to_core {
                self.a.umov(false, rt, d.host(), lane);
            } else {
                self.a.ins_general(false, d.host(), lane, rt);
            }
            Ok(Flow::Next)
        }
    }

    /// VMRS Rt, FPSCR (Rt = PC: copy FPSCR.NZCV to the guest flags).
    fn vmrs(&mut self, rt: u32) {
        self.a.ldr_w_off(T0, host::STATE, offsets::FPSCR);
        if rt == PC {
            self.a.and_w_imm(T0, T0, 0xF000_0000, T1);
            self.a.msr_nzcv(T0);
            return;
        }
        self.a.and_w_imm(T0, T0, !FPSCR_CUMULATIVE, T1);
        self.a.mrs_fpsr(T1);
        self.a.and_w_imm(T1, T1, FPSCR_CUMULATIVE, T2);
        self.a.orr_w(rt, T0, T1, shift::LSL, 0);
    }

    /// VLDR, VSTR, VLDM, VSTM (VPUSH and VPOP are these) and two-register VMOV.
    fn vfp_load_store(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1 == 1;
        let (before, up, writeback, load, double) = (bit(24), bit(23), bit(21), bit(20), bit(8));
        let rn = (word >> 16) & 0xF;
        let imm8 = word & 0xFF;
        let first = Fp::decode(double, (word >> 12) & 0xF, (word >> 22) & 1);
        if !before && !up {
            return if bit(22) && !writeback { self.vfp_move_pair(word) } else { Err(()) };
        }
        if before && !writeback {
            // VLDR / VSTR
            let offset = if up { imm8 * 4 } else { (imm8 * 4).wrapping_neg() };
            if rn == PC {
                self.a.mov_w_imm(T0, (self.pc_value() & !3).wrapping_add(offset));
            } else {
                self.a.add_w_any(T0, rn, offset, T1);
            }
            self.a.add_x_uxtw(T1, host::MEMORY, T0);
            if load {
                self.a.ld1_lane(double, first.host(), first.lane(), T1, false);
            } else {
                self.a.st1_lane(double, first.host(), first.lane(), T1, false);
            }
            return Ok(Flow::Next);
        }
        if before && up {
            return Err(()); // undefined
        }
        // VLDM / VSTM: increment-after, or decrement-before with writeback
        let count = if double { imm8 / 2 } else { imm8 };
        if rn == PC || count == 0 || (double && count > 16) || first.n + count > 32 {
            return Err(());
        }
        let bytes = imm8 * 4;
        if up {
            self.a.mov_w(T0, rn);
        } else {
            self.a.sub_w_imm(T0, rn, bytes);
        }
        self.a.add_x_uxtw(T1, host::MEMORY, T0);
        let mut r = first;
        for _ in 0..count {
            if load {
                self.a.ld1_lane(double, r.host(), r.lane(), T1, true);
            } else {
                self.a.st1_lane(double, r.host(), r.lane(), T1, true);
            }
            r = r.next();
        }
        if writeback {
            if up {
                self.a.add_w_imm(rn, rn, bytes);
            } else {
                self.a.mov_w(rn, T0);
            }
        }
        Ok(Flow::Next)
    }

    /// VMOV between two core registers and two singles or one double.
    fn vfp_move_pair(&mut self, word: u32) -> Result<Flow> {
        let (rt, rt2) = ((word >> 12) & 0xF, (word >> 16) & 0xF);
        let to_core = (word >> 20) & 1 == 1;
        if (word >> 4) & 0b1101 != 0b0001 || rt == PC || rt2 == PC || (to_core && rt == rt2) {
            return Err(());
        }
        let double = (word >> 8) & 1 == 1;
        let m = Fp::decode(double, word & 0xF, (word >> 5) & 1);
        if double {
            if to_core {
                self.a.umov(true, T0, m.host(), m.lane());
                self.a.mov_w(rt, T0);
                self.a.lsr_x_imm(T0, T0, 32);
                self.a.mov_w(rt2, T0);
            } else {
                self.a.mov_w(T0, rt);
                self.a.orr_x(T0, T0, rt2, shift::LSL, 32);
                self.a.ins_general(true, m.host(), m.lane(), T0);
            }
        } else {
            if m.n == 31 {
                return Err(());
            }
            for (r, core) in [(m, rt), (m.next(), rt2)] {
                if to_core {
                    self.a.umov(false, core, r.host(), r.lane());
                } else {
                    self.a.ins_general(false, r.host(), r.lane(), core);
                }
            }
        }
        Ok(Flow::Next)
    }
}
