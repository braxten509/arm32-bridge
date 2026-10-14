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
const V3: Reg = 19;

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
        self.a.vfp_restore = neon;
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
        } else if bit(4) == 1 && (word >> 19) & 7 == 0 && bit(7) == 0 {
            self.neon_modified_immediate(word)
        } else if bit(4) == 1 {
            self.neon_shift(word)
        } else if bit(6) == 0 && (word >> 20) & 3 != 3 {
            self.neon_three_different(word)
        } else if bit(6) == 1 && (word >> 20) & 3 != 3 {
            self.neon_by_scalar(word)
        } else if (word >> 20) & 3 == 3 && bit(24) == 0 {
            self.neon_extract(word)
        } else if (word >> 20) & 3 == 3 && bit(11) == 0 {
            self.neon_two_misc(word)
        } else if (word >> 20) & 3 == 3 && (word >> 8) & 0xF == 0b1100 && bit(7) == 0 {
            self.neon_duplicate(word)
        } else {
            Err(())
        }
    }

    /// VEXT: bytes from the pair Vm:Vn starting at byte imm4.
    fn neon_extract(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(6) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let n = (bit(7) << 4) | ((word >> 16) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        let bytes = (word >> 8) & 0xF;
        if (q && (d | n | m) & 1 == 1) || (!q && bytes >= 8) {
            return Err(());
        }
        let vn = self.vector_read(q, n, V1);
        let vm = self.vector_read(q, m, V2);
        let target = Self::vector_target(q, d);
        self.a.ext(q, target, vn, vm, bytes);
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// VDUP (scalar): one element copied to every lane.
    fn neon_duplicate(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(6) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        let imm4 = (word >> 16) & 0xF;
        let (size, index) = match imm4 {
            _ if imm4 & 1 == 1 => (0, imm4 >> 1),
            _ if imm4 & 3 == 2 => (1, imm4 >> 2),
            _ if imm4 & 7 == 4 => (2, imm4 >> 3),
            _ => return Err(()),
        };
        if q && d & 1 == 1 {
            return Err(());
        }
        let lane = (m % 2) * (8 >> size) + index;
        let target = Self::vector_target(q, d);
        self.a.dup_lanes(q, size, target, m / 2, lane);
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// One register and a modified immediate: VMOV, VMVN, VORR, VBIC (immediate).
    fn neon_modified_immediate(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(6) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        if q && d & 1 == 1 {
            return Err(());
        }
        let (op, cmode) = (bit(5), (word >> 8) & 0xF);
        let imm8 = (bit(24) << 7) | (((word >> 16) & 7) << 4) | (word & 0xF);
        let Some(value) = expand_simd_immediate(op, cmode, imm8) else {
            return Err(());
        };
        // cmode 0xx1 / 10x1 are VORR (op 0) and VBIC (op 1); the rest move.
        let combine = cmode & 1 == 1 && cmode < 0b1100;
        let value = if !combine && op == 1 && cmode != 0b1110 { !value } else { value };
        self.a.mov_x_imm(T0, value);
        if !combine {
            if q {
                self.a.ins_general(true, d / 2, 0, T0);
                self.a.ins_general(true, d / 2, 1, T0);
            } else {
                self.a.ins_general(true, d / 2, d % 2, T0);
            }
            return Ok(Flow::Next);
        }
        self.a.ins_general(true, V1, 0, T0);
        self.a.ins_general(true, V1, 1, T0);
        let vd = self.vector_read(q, d, V2);
        let target = Self::vector_target(q, d);
        self.a.simd3(if op == 0 { simd::ORR } else { simd::BIC }, q, 0, target, vd, V1);
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// Two registers and a shift amount: VSHR, VSRA, VRSHR, VRSRA, VSRI, VSHL, VSLI,
    /// VQSHL/VQSHLU, the narrowing shifts, VSHLL / VMOVL and VCVT (fixed point).
    fn neon_shift(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        let imm6 = (word >> 16) & 0x3F;
        let (kind, u, long, b) = ((word >> 8) & 0xF, bit(24), bit(7) == 1, bit(6) == 1);
        // ARM64 immh:immb is L:imm6 for every form: both store 2*esize - shift for right
        // shifts and esize + shift for left shifts (esize the narrow size when narrowing).
        let imm7 = (bit(7) << 6) | imm6;
        // U and the ARM64 opcode go in the instruction; U means the same in both.
        let op = |opcode: u32| (u << 29) | (opcode << 11);
        if kind <= 0b0111 {
            // ARM64 opcode is A << 1: SSHR/USHR, SSRA, SRSHR, SRSRA, SRI, SHL/SLI, SQSHLU, SQSHL/UQSHL.
            if (u == 0 && (kind == 0b0100 || kind == 0b0110)) || (b && (d | m) & 1 == 1) {
                return Err(()); // VSRI and VQSHLU are U = 1 only; odd Q registers
            }
            let accumulate = matches!(kind, 0b0001 | 0b0011 | 0b0100) || (kind == 0b0101 && u == 1);
            let vm = self.vector_read(b, m, V1);
            let target = Self::vector_target(b, d);
            if accumulate && !b {
                let vd = self.vector_read(false, d, V0);
                if vd != V0 {
                    self.a.mov_vector(V0, vd);
                }
            }
            self.a.shift_immediate(op(kind << 1), b, imm7, target, vm);
            self.vector_write(b, d, target);
            return Ok(Flow::Next);
        }
        if kind == 0b1000 || kind == 0b1001 {
            // Narrowing: SHRN/RSHRN (U 0) or SQSHRUN/SQRSHRUN (U 1); SQSHRN/UQSHRN, SQRSHRN/UQRSHRN.
            if long || m & 1 == 1 {
                return Err(());
            }
            self.a.shift_immediate(op(0b10000 | ((kind & 1) << 1) | b as u32), false, imm7, V0, m / 2);
            self.vector_write(false, d, V0);
            return Ok(Flow::Next);
        }
        if kind >= 0b1110 {
            // VCVT between F32 and fixed point (imm6 = 64 - fraction bits): SCVTF/UCVTF, FCVTZS/FCVTZU.
            if long || imm6 < 32 || (b && (d | m) & 1 == 1) {
                return Err(());
            }
            self.fp_mode(true);
            let vm = self.vector_read(b, m, V1);
            let target = Self::vector_target(b, d);
            self.a.shift_immediate(op(if kind == 0b1110 { 0b11100 } else { 0b11111 }), b, imm7, target, vm);
            self.vector_write(b, d, target);
            return Ok(Flow::Next);
        }
        if kind == 0b1010 && !b && !long {
            // VSHLL #shift (shift < element size) / VMOVL (shift 0): signedness is U.
            if d & 1 == 1 {
                return Err(());
            }
            let size = match imm6 {
                0b001000..=0b001111 => 0,
                0b010000..=0b011111 => 1,
                0b100000..=0b111111 => 2,
                _ => return Err(()),
            };
            let shift = imm6 - (8 << size);
            let vm = self.vector_read(false, m, V1);
            self.a.shift_left_long(bit(24) == 0, size, d / 2, vm, shift);
            return Ok(Flow::Next);
        }
        Err(())
    }

    /// Two registers, miscellaneous: the table entries that map to one ARM64 vector
    /// instruction (VREV, VPADDL/VPADAL, VCLS/VCLZ/VCNT, VMVN, VQABS/VQNEG, compares
    /// with zero, VABS/VNEG, VMOVN/VQMOVN/VQMOVUN, VRECPE/VRSQRTE, VCVT) and the permutes.
    fn neon_two_misc(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let q = bit(6) == 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        let (size, opc1, opc2) = ((word >> 18) & 3, (word >> 16) & 3, (word >> 7) & 0xF);
        if opc1 == 0b10 && matches!(opc2, 0b0100 | 0b0101) {
            return self.neon_narrow(word, d, m, size, opc2);
        }
        if q && (d | m) & 1 == 1 {
            return Err(());
        }
        if opc1 == 0b10 && opc2 <= 0b0011 {
            return self.neon_permute(q, size, opc2, d, m);
        }
        let int = |u: u32, opcode: u32| 0x0E20_0800 | (u << 29) | (opcode << 12);
        let float = |u: u32, opcode: u32| 0x0E20_0800 | (u << 29) | (1 << 23) | (opcode << 12);
        // (instruction, sized, reads Vd, floating point)
        let (op, sized, accumulate, fp) = match (opc1, opc2) {
            (0b00, 0b0000) if size < 3 => (simd::REV64, true, false, false),
            (0b00, 0b0001) if size < 2 => (simd::REV32, true, false, false),
            (0b00, 0b0010) if size == 0 => (simd::REV16, true, false, false),
            (0b00, 0b0100 | 0b0101) if size < 3 => (int(opc2 & 1, 0b00010), true, false, false), // VPADDL
            (0b00, 0b1000) if size < 3 => (int(0, 0b00100), true, false, false), // VCLS
            (0b00, 0b1001) if size < 3 => (int(1, 0b00100), true, false, false), // VCLZ
            (0b00, 0b1010) if size == 0 => (int(0, 0b00101), true, false, false), // VCNT
            (0b00, 0b1011) if size == 0 => (simd::NOT, false, false, false),
            (0b00, 0b1100 | 0b1101) if size < 3 => (int(opc2 & 1, 0b00110), true, true, false), // VPADAL
            (0b00, 0b1110 | 0b1111) if size < 3 => (int(opc2 & 1, 0b00111), true, false, false), // VQABS / VQNEG
            (0b01, 0b0000) if size < 3 => (int(0, 0b01000), true, false, false), // VCGT #0
            (0b01, 0b0001) if size < 3 => (int(1, 0b01000), true, false, false), // VCGE #0
            (0b01, 0b0010) if size < 3 => (int(0, 0b01001), true, false, false), // VCEQ #0
            (0b01, 0b0011) if size < 3 => (int(1, 0b01001), true, false, false), // VCLE #0
            (0b01, 0b0100) if size < 3 => (int(0, 0b01010), true, false, false), // VCLT #0
            (0b01, 0b0110) if size < 3 => (simd::ABS, true, false, false),
            (0b01, 0b0111) if size < 3 => (simd::NEG, true, false, false),
            (0b01, 0b1000) if size == 2 => (float(0, 0b01100), false, false, true), // VCGT.F32 #0
            (0b01, 0b1001) if size == 2 => (float(1, 0b01100), false, false, true), // VCGE.F32 #0
            (0b01, 0b1010) if size == 2 => (float(0, 0b01101), false, false, true), // VCEQ.F32 #0
            (0b01, 0b1011) if size == 2 => (float(1, 0b01101), false, false, true), // VCLE.F32 #0
            (0b01, 0b1100) if size == 2 => (float(0, 0b01110), false, false, true), // VCLT.F32 #0
            (0b01, 0b1110) if size == 2 => (simd::FABS, false, false, false),
            (0b01, 0b1111) if size == 2 => (simd::FNEG, false, false, false),
            (0b11, 0b1000) if size == 2 => (float(0, 0b11100), false, false, true), // VRECPE.U32
            (0b11, 0b1010) if size == 2 => (float(0, 0b11101), false, false, true), // VRECPE.F32
            (0b11, 0b1001) if size == 2 => (float(1, 0b11100), false, false, true), // VRSQRTE.U32
            (0b11, 0b1011) if size == 2 => (float(1, 0b11101), false, false, true), // VRSQRTE.F32
            // VCVT: 00 F32.S32, 01 F32.U32, 10 S32.F32, 11 U32.F32 (toward zero).
            (0b11, 0b1100..=0b1111) if size == 2 => {
                ([simd::SCVTF, simd::UCVTF, simd::FCVTZS, simd::FCVTZU][(opc2 & 3) as usize], false, false, true)
            }
            _ => return Err(()),
        };
        if fp {
            self.fp_mode(true);
        }
        let vm = self.vector_read(q, m, V1);
        let target = Self::vector_target(q, d);
        if accumulate && !q {
            let vd = self.vector_read(false, d, V0);
            if vd != V0 {
                self.a.mov_vector(V0, vd);
            }
        }
        self.a.simd2_sized(op, q, if sized { size } else { 0 }, target, vm);
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// VMOVN / VQMOVUN / VQMOVN: a Q register narrowed into a D register.
    fn neon_narrow(&mut self, word: u32, d: u32, m: u32, size: u32, opc2: u32) -> Result<Flow> {
        if m & 1 == 1 || size == 3 {
            return Err(());
        }
        let op_bit = (word >> 6) & 1;
        let op = match (opc2, op_bit) {
            (0b0100, 0) => 0x0E20_0800 | (0b10010 << 12),             // VMOVN: XTN
            (0b0100, _) => 0x0E20_0800 | (1 << 29) | (0b10010 << 12), // VQMOVUN: SQXTUN
            (_, 0) => 0x0E20_0800 | (0b10100 << 12),                  // VQMOVN.S: SQXTN
            _ => 0x0E20_0800 | (1 << 29) | (0b10100 << 12),           // VQMOVN.U: UQXTN
        };
        self.a.simd2_sized(op, false, size, V0, m / 2);
        self.vector_write(false, d, V0);
        Ok(Flow::Next)
    }

    /// VSWP / VTRN / VUZP / VZIP: both registers change.
    fn neon_permute(&mut self, q: bool, size: u32, kind: u32, d: u32, m: u32) -> Result<Flow> {
        if d == m || (kind != 0 && size == 3) || (kind == 0 && size != 0) || (!q && size == 2 && kind >= 2) {
            return Err(()); // same register (unpredictable) or a size with no such operation
        }
        let vd = self.vector_read(q, d, V1);
        let vm = self.vector_read(q, m, V2);
        if kind == 0 {
            // Copy both before writing either (a Q source is the guest register itself).
            self.a.mov_vector(V0, vm);
            self.a.mov_vector(V3, vd);
        } else {
            let (low, high) = match kind {
                1 => (simd::TRN1, simd::TRN2),
                2 => (simd::UZP1, simd::UZP2),
                _ => (simd::ZIP1, simd::ZIP2),
            };
            self.a.simd3(low, q, size, V0, vd, vm);
            self.a.simd3(high, q, size, V3, vd, vm);
        }
        self.vector_write(q, d, V0);
        self.vector_write(q, m, V3);
        Ok(Flow::Next)
    }

    /// Three registers of the same length: the whole integer, logic and F32 table
    /// that maps to an ARM64 vector instruction (operands and lanes match).
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
        let (u, b) = (bit(24), bit(4) == 1);
        let high = bit(21); // F32 forms: picks the variant; bit 20 must be clear
        enum Op {
            Plain(u32, u32),
            /// Shifts by register: VSHL Vd, Vm, Vn shifts Vm (ARM64 shifts the first operand).
            Swapped(u32, u32),
            /// Reads Vd too (VMLA, VABA, VBSL, VFMA, ...).
            Accumulate(u32, u32),
            /// VMLA/VMLS.F32: product rounded, then added to or subtracted from d.
            MultiplyAccumulate(u32),
            /// VRECPS / VRSQRTS: (2 - a*b) or (3 - a*b) / 2 with the product rounded first.
            Step(bool),
        }
        let int = |opcode: u32| 0x0E20_0400 | (u << 29) | (opcode << 11);
        let float = |u: u32, variant: u32, opcode: u32| 0x0E20_0400 | (u << 29) | (variant << 23) | (opcode << 11);
        let a = (word >> 8) & 0xF;
        let is_float = a >= 0b1100;
        if is_float && size & 1 != 0 {
            return Err(()); // F16 / reserved
        }
        let sixty_four_ok = matches!((a, b), (0b0000, true) | (0b0010, true) | (0b0100, _) | (0b0101, _) | (0b1000, false));
        if !is_float && a != 0b0001 && size == 3 && (!q || !sixty_four_ok) {
            return Err(()); // 64-bit lanes (not logic, where size picks the op): only add/sub, saturating add/sub and shifts, in Q
        }
        let pairwise = matches!((a, b, u), (0b1010, _, _) | (0b1011, true, 0)) || (a == 0b1101 && !b && u == 1 && high == 0) || (a == 0b1111 && !b && u == 1);
        if pairwise && q {
            return Err(()); // pairwise operations have no Q form
        }
        let op = match (a, b) {
            (0b0000, false) => Op::Plain(int(0b00000), size),          // VHADD
            (0b0000, true) => Op::Plain(int(0b00001), size),           // VQADD
            (0b0001, false) => Op::Plain(int(0b00010), size),          // VRHADD
            (0b0001, true) if u == 0 => Op::Plain([simd::AND, simd::BIC, simd::ORR, simd::ORN][size as usize], 0),
            (0b0001, true) if size == 0 => Op::Plain(simd::EOR, 0),
            (0b0001, true) => Op::Accumulate([0, 0x2E60_1C00, 0x2EA0_1C00, 0x2EE0_1C00][size as usize], 0), // VBSL / VBIT / VBIF
            (0b0010, false) => Op::Plain(int(0b00100), size),          // VHSUB
            (0b0010, true) => Op::Plain(int(0b00101), size),           // VQSUB
            (0b0011, false) => Op::Plain(int(0b00110), size),          // VCGT (CMGT / CMHI)
            (0b0011, true) => Op::Plain(int(0b00111), size),           // VCGE (CMGE / CMHS)
            (0b0100, false) => Op::Swapped(int(0b01000), size),        // VSHL
            (0b0100, true) => Op::Swapped(int(0b01001), size),         // VQSHL
            (0b0101, false) => Op::Swapped(int(0b01010), size),        // VRSHL
            (0b0101, true) => Op::Swapped(int(0b01011), size),         // VQRSHL
            (0b0110, false) => Op::Plain(int(0b01100), size),          // VMAX
            (0b0110, true) => Op::Plain(int(0b01101), size),           // VMIN
            (0b0111, false) => Op::Plain(int(0b01110), size),          // VABD
            (0b0111, true) => Op::Accumulate(int(0b01111), size),      // VABA
            (0b1000, false) => Op::Plain(int(0b10000), size),          // VADD / VSUB
            (0b1000, true) => Op::Plain(int(0b10001), size),           // VTST / VCEQ
            (0b1001, false) => Op::Accumulate(int(0b10010), size),     // VMLA / VMLS
            (0b1001, true) if u == 0 || size == 0 => Op::Plain(int(0b10011), size), // VMUL / VMUL.P8
            (0b1010, false) => Op::Plain(int(0b10100), size),          // VPMAX
            (0b1010, true) => Op::Plain(int(0b10101), size),           // VPMIN
            (0b1011, false) if size == 1 || size == 2 => Op::Plain(int(0b10110), size), // VQDMULH / VQRDMULH
            (0b1011, true) if u == 0 => Op::Plain(int(0b10111), size), // VPADD
            (0b1100, true) if u == 0 => Op::Accumulate(float(0, high, 0b11001), 0), // VFMA / VFMS
            (0b1101, false) if u == 0 => Op::Plain(float(0, high, 0b11010), 0),    // VADD / VSUB.F32
            (0b1101, false) => Op::Plain(float(1, high, 0b11010), 0),              // VPADD / VABD.F32
            (0b1101, true) if u == 0 => Op::MultiplyAccumulate(if high == 0 { simd::FADD } else { simd::FSUB }),
            (0b1101, true) if high == 0 => Op::Plain(simd::FMUL, 0),
            (0b1110, false) if u == 0 && high == 0 => Op::Plain(float(0, 0, 0b11100), 0), // VCEQ.F32
            (0b1110, false) if u == 1 => Op::Plain(float(1, high, 0b11100), 0),   // VCGE / VCGT.F32
            (0b1110, true) if u == 1 => Op::Plain(float(1, high, 0b11101), 0),    // VACGE / VACGT
            (0b1111, false) => Op::Plain(float(u, high, 0b11110), 0),             // VMAX/VMIN, VPMAX/VPMIN.F32
            (0b1111, true) if u == 0 => Op::Step(high == 1),
            _ => return Err(()),
        };
        if matches!(op, Op::Plain(_, _) | Op::Swapped(_, _) | Op::Accumulate(_, _)) && is_float {
            self.fp_mode(true);
        }
        if matches!(op, Op::MultiplyAccumulate(_) | Op::Step(_)) {
            self.fp_mode(true);
        }
        let vn = self.vector_read(q, n, V1);
        let vm = self.vector_read(q, m, V2);
        let target = Self::vector_target(q, d);
        match op {
            Op::Plain(op, size) => self.a.simd3(op, q, size, target, vn, vm),
            Op::Swapped(op, size) => self.a.simd3(op, q, size, target, vm, vn),
            Op::Accumulate(op, size) => {
                if !q {
                    let vd = self.vector_read(false, d, V0);
                    if vd != V0 {
                        self.a.mov_vector(V0, vd);
                    }
                }
                self.a.simd3(op, q, size, target, vn, vm);
            }
            Op::MultiplyAccumulate(op) => {
                self.a.simd3(simd::FMUL, q, 0, V1, vn, vm);
                let vd = self.vector_read(q, d, V2);
                self.a.simd3(op, q, 0, target, vd, V1);
            }
            Op::Step(square_root) => {
                // Unicorn and ARMv7 round the product first; ARM64's FRECPS/FRSQRTS are
                // fused. Use the two-rounding result unless it is NaN (inf * 0, where
                // the fused instruction gives the defined 2.0 / 1.5).
                self.a.simd3(simd::FMUL, q, 0, V0, vn, vm);
                self.a.fmov_vector_imm(q, V3, if square_root { 0x08 } else { 0x00 }); // 3.0 / 2.0
                self.a.simd3(simd::FSUB, q, 0, V0, V3, V0);
                if square_root {
                    self.a.fmov_vector_imm(q, V3, 0x60); // 0.5
                    self.a.simd3(simd::FMUL, q, 0, V0, V0, V3);
                }
                self.a.simd3(float(0, square_root as u32, 0b11111), q, 0, V3, vn, vm);
                self.a.simd3(float(0, 0, 0b11100), q, 0, V1, V0, V0); // not NaN: all ones
                self.a.simd3(0x2E60_1C00, q, 0, V1, V0, V3); // bsl
                self.vector_write(q, d, V1);
                return Ok(Flow::Next);
            }
        }
        self.vector_write(q, d, target);
        Ok(Flow::Next)
    }

    /// Three registers of different lengths: VADDL/VADDW, VSUBL/VSUBW, VADDHN/VRADDHN,
    /// VSUBHN/VRSUBHN, VABAL, VABDL, VMLAL/VMLSL, VQDMLAL/VQDMLSL, VMULL (and .P8), VQDMULL.
    /// ARM64's opcode and U are ARMv7's A and U; the 64-bit forms use the low halves.
    fn neon_three_different(&mut self, word: u32) -> Result<Flow> {
        let bit = |i: u32| (word >> i) & 1;
        let d = (bit(22) << 4) | ((word >> 12) & 0xF);
        let n = (bit(7) << 4) | ((word >> 16) & 0xF);
        let m = (bit(5) << 4) | (word & 0xF);
        let (size, kind, u) = ((word >> 20) & 3, (word >> 8) & 0xF, bit(24));
        // (Vd is Q, Vn is Q, Vm is Q)
        let (wide_d, wide_n, wide_m) = match kind {
            0b0001 | 0b0011 => (true, true, false),   // VADDW / VSUBW
            0b0100 | 0b0110 => (false, true, true),   // VADDHN / VSUBHN (and rounding)
            0b1001 | 0b1011 | 0b1101 if u == 1 || size == 0 => return Err(()), // VQDMLAL / VQDMLSL / VQDMULL
            0b1110 if u == 1 || size != 0 => return Err(()), // VMULL.P8 only (P64 is ARMv8 crypto)
            0b1111 => return Err(()),
            _ => (true, false, false),
        };
        if (wide_d && d & 1 == 1) || (wide_n && n & 1 == 1) || (wide_m && m & 1 == 1) {
            return Err(());
        }
        let vn = self.vector_read(wide_n, n, V1);
        let vm = self.vector_read(wide_m, m, V2);
        let target = Self::vector_target(wide_d, d);
        self.a.simd3(0x0E20_0000 | (u << 29) | (kind << 12), false, size, target, vn, vm);
        self.vector_write(wide_d, d, target);
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
    /// VLD1/VST1 of 1-4 registers, of one lane, and VLD1 to all lanes.
    pub(super) fn neon_load_store(&mut self, word: u32) -> Result<Flow> {
        let load = (word >> 21) & 1 == 1;
        let (rn, rm) = ((word >> 16) & 0xF, word & 0xF);
        let d = (((word >> 22) & 1) << 4) | ((word >> 12) & 0xF);
        if rn == PC {
            return Err(());
        }
        self.a.add_x_uxtw(T1, host::MEMORY, rn);
        let bytes = if (word >> 23) & 1 == 0 {
            // VLD2-VLD4 / VST2-VST4: (elements per structure, registers per element, spacing).
            let structures = match (word >> 8) & 0xF {
                0b1000 => Some((2, 1, 1)),
                0b1001 => Some((2, 1, 2)),
                0b0011 => Some((2, 2, 2)),
                0b0100 => Some((3, 1, 1)),
                0b0101 => Some((3, 1, 2)),
                0b0000 => Some((4, 1, 1)),
                0b0001 => Some((4, 1, 2)),
                _ => None,
            };
            if let Some((elements, registers, spacing)) = structures {
                let size = (word >> 6) & 3;
                let last = d + (elements - 1) * spacing + registers - 1;
                if size == 3 || last >= 32 {
                    return Err(());
                }
                // Element k of every structure goes to D[d + k*spacing (+1)], here to v(16+k).
                let guest = |k: u32, half: u32| d + k * spacing + half;
                if !load {
                    for k in 0..elements {
                        for half in 0..registers {
                            let r = guest(k, half);
                            self.a.ins_element(true, V0 + k, half, r / 2, r % 2);
                        }
                    }
                }
                self.a.structures(load, elements, registers == 2, size, V0, T1);
                if load {
                    for k in 0..elements {
                        for half in 0..registers {
                            let r = guest(k, half);
                            self.a.ins_element(true, r / 2, r % 2, V0 + k, half);
                        }
                    }
                }
                let bytes = 8 * elements * registers;
                match rm {
                    PC => {}
                    SP => self.a.add_w_imm(rn, rn, bytes),
                    _ => self.a.add_w(rn, rn, rm, crate::dynarec::emit::shift::LSL, 0),
                }
                return Ok(Flow::Next);
            }
            let count = match (word >> 8) & 0xF {
                0b0111 => 1,
                0b1010 => 2,
                0b0110 => 3,
                0b0010 => 4,
                _ => return Err(()),
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
        } else if (word >> 8) & 0xF == 0b1100 {
            // VLD1 one element to all lanes of one or two D registers.
            let (size, two) = ((word >> 6) & 3, (word >> 5) & 1 == 1);
            if !load || size == 3 || d + two as u32 >= 32 {
                return Err(());
            }
            self.a.ld1r(size, V0, T1);
            for r in d..=d + two as u32 {
                self.a.ins_element(true, r / 2, r % 2, V0, 0);
            }
            1 << size
        } else {
            // One lane of 1, 2 or 4 bytes; the index sits above the alignment bits.
            let size = (word >> 10) & 3;
            if (word >> 8) & 3 != 0 || size == 3 {
                return Err(());
            }
            let index_align = (word >> 4) & 0xF;
            let index = match size {
                0 if index_align & 1 == 0 => index_align >> 1,
                1 if index_align & 2 == 0 => index_align >> 2,
                2 if index_align & 4 == 0 => index_align >> 3,
                _ => return Err(()),
            };
            let lane = (d % 2) * (8 >> size) + index;
            self.a.lane_load_store(load, size, d / 2, lane, T1);
            1 << size
        };
        match rm {
            PC => {}
            SP => self.a.add_w_imm(rn, rn, bytes),
            _ => self.a.add_w(rn, rn, rm, crate::dynarec::emit::shift::LSL, 0),
        }
        Ok(Flow::Next)
    }
}

/// AdvSIMDExpandImm: the 64-bit pattern of a NEON modified immediate (None when undefined).
fn expand_simd_immediate(op: u32, cmode: u32, imm8: u32) -> Option<u64> {
    let imm8 = imm8 as u64;
    let twice = |word: u64| (word << 32) | word;
    let four = |half: u64| half * 0x0001_0001_0001_0001;
    Some(match cmode >> 1 {
        0b000 => twice(imm8),
        0b001 => twice(imm8 << 8),
        0b010 => twice(imm8 << 16),
        0b011 => twice(imm8 << 24),
        0b100 => four(imm8),
        0b101 => four(imm8 << 8),
        0b110 if cmode & 1 == 0 => twice((imm8 << 8) | 0xFF),
        0b110 => twice((imm8 << 16) | 0xFFFF),
        _ => match (cmode & 1, op) {
            (0, 0) => imm8 * 0x0101_0101_0101_0101,
            (0, _) => (0..8).filter(|i| imm8 >> i & 1 == 1).map(|i| 0xFFu64 << (8 * i)).sum(),
            (_, 0) => {
                let (a, b, rest) = (imm8 >> 7, (imm8 >> 6) & 1, imm8 & 0x3F);
                let exponent_high = if b == 1 { 0b011111 } else { 0b100000 };
                twice((a << 31) | (exponent_high << 25) | (rest << 19))
            }
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::Translator;
    use crate::dynarec::emit::Asm;
    use crate::mem::GuestMem;

    /// Shifts by immediate and three-different-length forms, translated; both sides
    /// checked against the NDK assembler (guest ARMv7, host ARM64).
    #[test]
    fn shift_and_long_translations() {
        let cases: &[(u32, &[u32])] = &[
            (0xF2A1_2054, &[0x4F21_0441]),                           // vshr.s32 q1, q2, #31
            (0xF2A0_3595, &[0x5E18_0451, 0x5F60_5630, 0x6E18_0601]), // vshl.i64 d3, d5, #32
            (0xF39D_4116, &[0x4EA2_1C50, 0x2F1D_1470, 0x6E08_0602]), // vsra.u16 d4, d6, #3
            (0xF3A0_0452, &[0x6F20_4420]),                           // vsri.32 q0, q1, #32
            (0xF3BF_45D6, &[0x6F7F_5462]),                           // vsli.64 q2, q3, #63
            (0xF38F_1612, &[0x2F0F_6430, 0x6E18_0600]),              // vqshlu.s8 d1, d2, #7
            (0xF385_0791, &[0x5E18_0411, 0x7F45_7630, 0x6E08_0600]), // vqshl.u64 d0, d1, #5
            (0xF280_2292, &[0x5F40_2430, 0x6E08_0601]),              // vrshr.s64 d2, d2, #64
            (0xF388_835A, &[0x6F08_34A4]),                           // vrsra.u8 q4, q5, #8
            (0xF288_1814, &[0x0F08_8450, 0x6E18_0600]),              // vshrn.i16 d1, q2, #8
            (0xF2A0_0852, &[0x0F20_8C30, 0x6E08_0600]),              // vrshrn.i64 d0, q1, #32
            (0xF390_3818, &[0x2F10_8490, 0x6E18_0601]),              // vqshrun.s32 d3, q4, #16
            (0xF38F_2850, &[0x2F0F_8C10, 0x6E08_0601]),              // vqrshrun.s16 d2, q0, #1
            (0xF39D_2910, &[0x2F1D_9410, 0x6E08_0601]),              // vqshrn.u32 d2, q0, #3
            (0xF2AC_595C, &[0x0F2C_9CD0, 0x6E18_0602]),              // vqrshrn.s64 d5, q6, #20
            (0xF2B0_0F52, &[0x4F30_FC20]),                           // vcvt.s32.f32 q0, q1, #16
            (0xF3BF_1E13, &[0x5E18_0431, 0x2F3F_E630, 0x6E18_0600]), // vcvt.f32.u32 d1, d3, #1
            (0xF2E5_0572, &[0x4F25_5528]),                           // vshl.i32 q8, q9, #5
            (0xF384_2805, &[0x5E18_0452, 0x2E32_8041]),              // vmlal.u8 q1, d4, d5
            (0xF282_0404, &[0x0E22_4030, 0x6E08_0600]),              // vaddhn.i16 d0, q1, q2
            (0xF292_0103, &[0x5E18_0432, 0x0E72_1020]),              // vaddw.s16 q0, q1, d3
            (0xF3A0_4201, &[0x5E18_0412, 0x2EB2_2002]),              // vsubl.u32 q2, d0, d1
            (0xF3A4_1406, &[0x2EA3_4050, 0x6E18_0600]),              // vraddhn.i64 d1, q2, q3
            (0xF287_6509, &[0x5E18_0471, 0x5E18_0492, 0x0E32_5223]), // vabal.s8 q3, d7, d9
            (0xF392_8703, &[0x5E18_0432, 0x2E72_7024]),              // vabdl.u16 q4, d2, d3
            (0xF2AA_AA0B, &[0x5E18_04B2, 0x0EB2_A0A5]),              // vmlsl.s32 q5, d10, d11
            (0xF291_C902, &[0x5E18_0411, 0x0E61_9226]),              // vqdmlal.s16 q6, d1, d2
            (0xF2A3_EB04, &[0x5E18_0431, 0x0EA2_B227]),              // vqdmlsl.s32 q7, d3, d4
            (0xF295_0D06, &[0x5E18_0451, 0x0E63_D220]),              // vqdmull.s16 q0, d5, d6
            (0xF281_0E02, &[0x5E18_0411, 0x0E21_E220]),              // vmull.p8 q0, d1, d2
            (0xF2EE_ECAF, &[0x5E18_05F2, 0x0EB2_C1EF]),              // vmull.s32 q15, d30, d31
            (0xF298_360A, &[0x0E65_6090, 0x6E18_0601]),              // vsubhn.i32 d3, q4, q5
            (0xF380_2602, &[0x2E21_6010, 0x6E08_0601]),              // vrsubhn.i16 d2, q0, q1
            (0xF382_2302, &[0x2E21_3021]),                           // vsubw.u8 q1, q1, d2
            (0xF280_0000, &[0x0E20_0000]),                           // vaddl.s8 q0, d0, d0
        ];
        // Undefined encodings stay with the fallback.
        let undefined = [
            0xF2A1_3054, // vshr q with an odd Vd
            0xF2A0_0452, // vsri with U = 0
            0xF288_1815, // vshrn from an odd Vm
            0xF288_1894, // vshrn with L = 1
            0xF2B0_0F52 & !(0x20 << 16), // vcvt with imm6 < 32
            0xF384_3805, // vmlal into an odd Vd
            0xF283_0404, // vaddhn from an odd Vn
            0xF285_0D06, // vqdmull.s8
            0xF391_C902, // vqdmlal with U = 1
            0xF381_0E02, // vmull.p8 with U = 1
            0xF2A1_0E02, // vmull.p64 (ARMv8 crypto)
        ];
        let mem = GuestMem::reserve();
        let translate = |word: u32| {
            let mut t = Translator { a: Asm::new(), mem, pc: 0, size: 4, count: 0, in_it: false, thumb: false, neon_fp: true };
            t.neon_data(word).map(|_| t.a.code)
        };
        for &(word, host) in cases {
            assert_eq!(translate(word), Ok(host.to_vec()), "{word:#010x}");
        }
        for word in undefined {
            assert!(translate(word).is_err(), "{word:#010x}");
        }
    }
}
