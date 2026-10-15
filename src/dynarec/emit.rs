//! ARM64 instruction encoder. Registers are numbers 0-31 (31 is wzr/xzr, or
//! sp where the instruction takes sp). `w` methods work on 32-bit registers,
//! `x` methods on 64-bit ones.

pub type Reg = u32;
pub const ZR: Reg = 31;
pub const SP: Reg = 31;

/// A64 condition codes; ARM32 uses the same numbers.
pub mod cond {
    pub const EQ: u32 = 0;
    pub const NE: u32 = 1;
    pub const CS: u32 = 2;
    pub const CC: u32 = 3;
    pub const MI: u32 = 4;
    pub const PL: u32 = 5;
    pub const VS: u32 = 6;
    pub const VC: u32 = 7;
    pub const HI: u32 = 8;
    pub const LS: u32 = 9;
    pub const GE: u32 = 10;
    pub const LT: u32 = 11;
    pub const GT: u32 = 12;
    pub const LE: u32 = 13;
    pub const AL: u32 = 14;

    pub fn invert(condition: u32) -> u32 {
        condition ^ 1
    }
}

/// Shift kinds for shifted-register operands (same numbers as ARM32).
pub mod shift {
    pub const LSL: u32 = 0;
    pub const LSR: u32 = 1;
    pub const ASR: u32 = 2;
    pub const ROR: u32 = 3;
}

/// Scalar floating-point opcodes for `fp1`, `fp2` and `fp3` (single; `double` sets the type).
pub mod fp {
    pub const FMUL: u32 = 0x1E20_0800;
    pub const FDIV: u32 = 0x1E20_1800;
    pub const FADD: u32 = 0x1E20_2800;
    pub const FSUB: u32 = 0x1E20_3800;
    pub const FNMUL: u32 = 0x1E20_8800;
    pub const FMADD: u32 = 0x1F00_0000;
    pub const FMSUB: u32 = 0x1F00_8000;
    pub const FNMADD: u32 = 0x1F20_0000;
    pub const FNMSUB: u32 = 0x1F20_8000;
    pub const FABS: u32 = 0x1E20_C000;
    pub const FNEG: u32 = 0x1E21_4000;
    pub const FSQRT: u32 = 0x1E21_C000;
    /// fcvt sd, dn (source type double)
    pub const FCVT_TO_SINGLE: u32 = 0x1E22_4000;
    /// fcvt dd, sn (source type single)
    pub const FCVT_TO_DOUBLE: u32 = 0x1E22_C000;
}

/// Three-register vector opcodes for `simd3` (element size in bits 22-23 for ADD/SUB).
pub mod simd {
    pub const ADD: u32 = 0x0E20_8400;
    pub const SUB: u32 = 0x2E20_8400;
    pub const AND: u32 = 0x0E20_1C00;
    pub const BIC: u32 = 0x0E60_1C00;
    pub const ORR: u32 = 0x0EA0_1C00;
    pub const ORN: u32 = 0x0EE0_1C00;
    pub const EOR: u32 = 0x2E20_1C00;
    pub const FADD: u32 = 0x0E20_D400;
    pub const FSUB: u32 = 0x0EA0_D400;
    pub const FMUL: u32 = 0x2E20_DC00;
    pub const FMAX: u32 = 0x0E20_F400;
    pub const FMIN: u32 = 0x0EA0_F400;
    // Two-register (one source) single-precision vector operations.
    pub const SCVTF: u32 = 0x0E21_D800;
    pub const UCVTF: u32 = 0x2E21_D800;
    pub const FCVTZS: u32 = 0x0EA1_B800;
    pub const FCVTZU: u32 = 0x2EA1_B800;
    // One-source operations that take an element size at bits 22-23.
    pub const REV64: u32 = 0x0E20_0800;
    pub const REV32: u32 = 0x2E20_0800;
    pub const REV16: u32 = 0x0E20_1800;
    pub const NOT: u32 = 0x2E20_5800;
    pub const NEG: u32 = 0x2E20_B800;
    pub const ABS: u32 = 0x0E20_B800;
    pub const FNEG: u32 = 0x2EA0_F800;
    pub const FABS: u32 = 0x0EA0_F800;
    // Permutes (three-register form, element size at bits 22-23).
    pub const TRN1: u32 = 0x0E00_2800;
    pub const TRN2: u32 = 0x0E00_6800;
    pub const ZIP1: u32 = 0x0E00_3800;
    pub const ZIP2: u32 = 0x0E00_7800;
    pub const UZP1: u32 = 0x0E00_1800;
    pub const UZP2: u32 = 0x0E00_5800;
}

#[derive(Clone, Copy, Debug)]
pub struct Label(usize);

enum Fixup {
    /// b / bl: imm26 at bits 0-25.
    Branch26,
    /// b.cond / cbz / cbnz: imm19 at bits 5-23.
    Branch19,
    /// tbz / tbnz: imm14 at bits 5-18.
    Branch14,
}

#[derive(Default)]
pub struct Asm {
    pub code: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, Fixup)>,
    /// Exits to a fixed guest pc: (word index of a `nop` to patch into `b <block>`, guest pc).
    pub links: Vec<(usize, u32)>,
    /// Exits may not link to other blocks or use the jump table (host FPCR is not in the guest's mode).
    pub no_link: bool,
    /// Host FPCR is in NEON mode: linked exits put the guest's VFP mode back first
    /// (the next block expects it).
    pub vfp_restore: bool,
}

impl Asm {
    pub fn new() -> Asm {
        // Room for a typical block, so translating one rarely regrows these.
        Asm { code: Vec::with_capacity(512), labels: Vec::with_capacity(32), fixups: Vec::with_capacity(32), links: Vec::with_capacity(8), ..Asm::default() }
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn emit(&mut self, word: u32) {
        self.code.push(word);
    }

    /// Remembers the current position so a failed translation can be undone.
    pub fn checkpoint(&self) -> (usize, usize, usize) {
        (self.code.len(), self.labels.len(), self.fixups.len())
    }

    pub fn rollback(&mut self, checkpoint: (usize, usize, usize)) {
        self.code.truncate(checkpoint.0);
        self.labels.truncate(checkpoint.1);
        self.fixups.truncate(checkpoint.2);
        self.links.retain(|&(at, _)| at < checkpoint.0);
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, label: Label) {
        self.labels[label.0] = Some(self.code.len());
    }

    /// Resolves branches to labels. Panics if a label was never bound.
    pub fn finish(&mut self) {
        for (at, label, kind) in self.fixups.drain(..) {
            let target = self.labels[label.0].expect("branch to an unbound label");
            let delta = target as i64 - at as i64;
            let word = &mut self.code[at];
            match kind {
                Fixup::Branch26 => *word |= (delta as u32) & 0x03FF_FFFF,
                Fixup::Branch19 => *word |= ((delta as u32) & 0x7FFFF) << 5,
                Fixup::Branch14 => *word |= ((delta as u32) & 0x3FFF) << 5,
            }
        }
    }

    // ---- branches ----

    pub fn b_to(&mut self, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch26));
        self.emit(0x1400_0000);
    }

    pub fn b_cond_to(&mut self, condition: u32, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch19));
        self.emit(0x5400_0000 | condition);
    }

    pub fn cbz_w_to(&mut self, rt: Reg, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch19));
        self.emit(0x3400_0000 | rt);
    }

    pub fn cbnz_w_to(&mut self, rt: Reg, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch19));
        self.emit(0x3500_0000 | rt);
    }

    pub fn cbnz_x_to(&mut self, rt: Reg, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch19));
        self.emit(0xB500_0000 | rt);
    }

    /// tbz / tbnz on bit `bit` (0-31) of a w register.
    pub fn tbz_to(&mut self, rt: Reg, bit: u32, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch14));
        self.emit(0x3600_0000 | (bit << 19) | rt);
    }

    pub fn tbnz_to(&mut self, rt: Reg, bit: u32, label: Label) {
        self.fixups.push((self.code.len(), label, Fixup::Branch14));
        self.emit(0x3700_0000 | (bit << 19) | rt);
    }

    /// b with a word offset (for jumps to code outside this buffer, patched later).
    pub fn b_offset(&mut self, words: i64) {
        self.emit(0x1400_0000 | (words as u32 & 0x03FF_FFFF));
    }

    pub fn br(&mut self, rn: Reg) {
        self.emit(0xD61F_0000 | (rn << 5));
    }

    pub fn blr(&mut self, rn: Reg) {
        self.emit(0xD63F_0000 | (rn << 5));
    }

    pub fn ret(&mut self) {
        self.emit(0xD65F_03C0);
    }

    pub fn nop(&mut self) {
        self.emit(0xD503_201F);
    }

    // ---- atomics and barriers ----

    pub fn dmb_ish(&mut self) {
        self.emit(0xD503_3BBF);
    }
    pub fn dsb_ish(&mut self) {
        self.emit(0xD503_3B9F);
    }
    pub fn isb(&mut self) {
        self.emit(0xD503_3FDF);
    }
    pub fn clrex(&mut self) {
        self.emit(0xD503_3F5F);
    }
    /// SWPAL: rt gets the old [rn], [rn] gets rs (size as for `casal`).
    pub fn swpal(&mut self, size: u32, rs: Reg, rt: Reg, rn: Reg) {
        self.emit(0x38E0_8000 | (size << 30) | (rs << 16) | (rn << 5) | rt);
    }
    /// CASAL: compare [rn] with rs, store rt if equal; rs gets the old value.
    /// `size`: 0 byte, 1 halfword, 2 word, 3 doubleword (x registers).
    pub fn casal(&mut self, size: u32, rs: Reg, rt: Reg, rn: Reg) {
        self.emit(0x08E0_FC00 | (size << 30) | (rs << 16) | (rn << 5) | rt);
    }
    /// LDAXR{B,H} / LDAXR (size as for `casal`).
    pub fn ldaxr(&mut self, size: u32, rt: Reg, rn: Reg) {
        self.emit(0x085F_FC00 | (size << 30) | (rn << 5) | rt);
    }
    /// STLXR{B,H} / STLXR: ws = 0 on success (size as for `casal`).
    pub fn stlxr(&mut self, size: u32, ws: Reg, rt: Reg, rn: Reg) {
        self.emit(0x0800_FC00 | (size << 30) | (ws << 16) | (rn << 5) | rt);
    }
    /// x(rd) = x(rn) + (x(rm) >> amount), arithmetic shift.
    pub fn add_x_asr(&mut self, rd: Reg, rn: Reg, rm: Reg, amount: u32) {
        self.emit(0x8B80_0000 | (rm << 16) | (amount << 10) | (rn << 5) | rd);
    }

    // ---- constants ----

    /// Loads any 32-bit value into a w register (1-2 instructions).
    pub fn mov_w_imm(&mut self, rd: Reg, value: u32) {
        let (low, high) = (value & 0xFFFF, value >> 16);
        if high == 0 {
            self.emit(0x5280_0000 | (low << 5) | rd); // movz
        } else if low == 0 {
            self.emit(0x52A0_0000 | (high << 5) | rd); // movz lsl 16
        } else if high == 0xFFFF {
            self.emit(0x1280_0000 | ((!value & 0xFFFF) << 5) | rd); // movn
        } else if low == 0xFFFF {
            self.emit(0x12A0_0000 | ((!value >> 16) << 5) | rd); // movn lsl 16
        } else {
            self.emit(0x5280_0000 | (low << 5) | rd);
            self.emit(0x72A0_0000 | (high << 5) | rd); // movk lsl 16
        }
    }

    /// Loads any 64-bit value into an x register.
    pub fn mov_x_imm(&mut self, rd: Reg, value: u64) {
        self.emit(0xD280_0000 | (((value & 0xFFFF) as u32) << 5) | rd);
        for hw in 1..4u32 {
            let part = ((value >> (hw * 16)) & 0xFFFF) as u32;
            if part != 0 {
                self.emit(0xF280_0000 | (hw << 21) | (part << 5) | rd);
            }
        }
    }

    // ---- register moves ----

    pub fn mov_w(&mut self, rd: Reg, rm: Reg) {
        if rd != rm {
            self.emit(0x2A00_03E0 | (rm << 16) | rd); // orr rd, wzr, rm
        }
    }

    pub fn mov_x(&mut self, rd: Reg, rm: Reg) {
        if rd != rm {
            self.emit(0xAA00_03E0 | (rm << 16) | rd);
        }
    }

    /// mov xd, sp / mov sp, xn (add with immediate 0 uses sp).
    pub fn mov_x_from_sp(&mut self, rd: Reg) {
        self.emit(0x9100_0000 | (SP << 5) | rd);
    }

    pub fn mov_sp_from_x(&mut self, rn: Reg) {
        self.emit(0x9100_0000 | (rn << 5) | SP);
    }

    // ---- add / sub ----

    fn add_sub_imm(&mut self, base: u32, rd: Reg, rn: Reg, imm: u32) {
        debug_assert!(imm < 4096);
        self.emit(base | (imm << 10) | (rn << 5) | rd);
    }

    pub fn add_w_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0x1100_0000, rd, rn, imm);
    }
    pub fn adds_w_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0x3100_0000, rd, rn, imm);
    }
    pub fn sub_w_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0x5100_0000, rd, rn, imm);
    }
    pub fn subs_w_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0x7100_0000, rd, rn, imm);
    }
    pub fn add_x_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0x9100_0000, rd, rn, imm);
    }
    pub fn sub_x_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.add_sub_imm(0xD100_0000, rd, rn, imm);
    }

    fn add_sub_reg(&mut self, base: u32, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        debug_assert!(kind != shift::ROR && amount < 32);
        self.emit(base | (kind << 22) | (rm << 16) | (amount << 10) | (rn << 5) | rd);
    }

    pub fn add_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.add_sub_reg(0x0B00_0000, rd, rn, rm, kind, amount);
    }
    pub fn adds_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.add_sub_reg(0x2B00_0000, rd, rn, rm, kind, amount);
    }
    pub fn sub_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.add_sub_reg(0x4B00_0000, rd, rn, rm, kind, amount);
    }
    pub fn subs_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.add_sub_reg(0x6B00_0000, rd, rn, rm, kind, amount);
    }
    /// xd = xn + zero-extended wm (host address of a guest address).
    pub fn add_x_uxtw(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x8B20_0000 | (rm << 16) | (0b010 << 13) | (rn << 5) | rd);
    }
    /// add xd, xn, xm, lsl #amount
    pub fn add_x_lsl(&mut self, rd: Reg, rn: Reg, rm: Reg, amount: u32) {
        self.emit(0x8B00_0000 | (rm << 16) | (amount << 10) | (rn << 5) | rd);
    }
    pub fn add_x(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x8B00_0000 | (rm << 16) | (rn << 5) | rd);
    }

    pub fn adc_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1A00_0000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn adcs_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x3A00_0000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn sbc_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x5A00_0000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn sbcs_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x7A00_0000 | (rm << 16) | (rn << 5) | rd);
    }

    // ---- logical (immediate) ----

    /// Encodes a 32-bit bitmask immediate as (immr, imms), if it has the
    /// repeating rotated-ones pattern A64 logical instructions accept.
    pub fn logical_immediate(value: u32) -> Option<(u32, u32)> {
        if value == 0 || value == u32::MAX {
            return None;
        }
        let mut size = 32u32;
        while size > 2 {
            let half = size / 2;
            let mask = (1u32 << half) - 1;
            if value & mask != (value >> half) & mask {
                break;
            }
            size = half;
        }
        let mask = if size == 32 { u32::MAX } else { (1u32 << size) - 1 };
        let element = value & mask;
        let ones = element.count_ones();
        let pattern = if ones == 32 { u32::MAX } else { (1u32 << ones) - 1 };
        let rotate_right = |x: u32, r: u32| if r == 0 { x } else { ((x >> r) | (x << (size - r))) & mask };
        let immr = (0..size).find(|&r| rotate_right(pattern, r) == element)?;
        let imms = ((!(size - 1) << 1) & 0x3F) | (ones - 1);
        Some((immr, imms))
    }

    fn logical_imm(&mut self, base: u32, rd: Reg, rn: Reg, value: u32) -> bool {
        match Asm::logical_immediate(value) {
            Some((immr, imms)) => {
                self.emit(base | (immr << 16) | (imms << 10) | (rn << 5) | rd);
                true
            }
            None => false,
        }
    }

    /// and wd, wn, #value; uses `scratch` when the value is not encodable.
    pub fn and_w_imm(&mut self, rd: Reg, rn: Reg, value: u32, scratch: Reg) {
        if value == u32::MAX {
            self.mov_w(rd, rn);
        } else if value == 0 {
            self.mov_w_imm(rd, 0);
        } else if !self.logical_imm(0x1200_0000, rd, rn, value) {
            self.mov_w_imm(scratch, value);
            self.and_w(rd, rn, scratch, shift::LSL, 0);
        }
    }
    pub fn orr_w_imm(&mut self, rd: Reg, rn: Reg, value: u32, scratch: Reg) {
        if value == 0 {
            self.mov_w(rd, rn);
        } else if value == u32::MAX {
            self.mov_w_imm(rd, u32::MAX);
        } else if !self.logical_imm(0x3200_0000, rd, rn, value) {
            self.mov_w_imm(scratch, value);
            self.orr_w(rd, rn, scratch, shift::LSL, 0);
        }
    }
    pub fn eor_w_imm(&mut self, rd: Reg, rn: Reg, value: u32, scratch: Reg) {
        if value == 0 {
            self.mov_w(rd, rn);
        } else if value == u32::MAX {
            self.mvn_w(rd, rn);
        } else if !self.logical_imm(0x5200_0000, rd, rn, value) {
            self.mov_w_imm(scratch, value);
            self.eor_w(rd, rn, scratch, shift::LSL, 0);
        }
    }

    /// add wd, wn, #value for any 32-bit value (wraps); uses `scratch` if needed.
    pub fn add_w_any(&mut self, rd: Reg, rn: Reg, value: u32, scratch: Reg) {
        let negative = value.wrapping_neg();
        if value == 0 {
            self.mov_w(rd, rn);
        } else if value < 4096 {
            self.add_w_imm(rd, rn, value);
        } else if negative < 4096 {
            self.sub_w_imm(rd, rn, negative);
        } else if value & 0xFFF == 0 && value < (1 << 24) {
            self.emit(0x1140_0000 | ((value >> 12) << 10) | (rn << 5) | rd); // add lsl 12
        } else if negative & 0xFFF == 0 && negative < (1 << 24) {
            self.emit(0x5140_0000 | ((negative >> 12) << 10) | (rn << 5) | rd);
        } else {
            self.mov_w_imm(scratch, value);
            self.add_w(rd, rn, scratch, shift::LSL, 0);
        }
    }

    // ---- logical (shifted register) ----

    fn logical(&mut self, base: u32, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        debug_assert!(amount < 32);
        self.emit(base | (kind << 22) | (rm << 16) | (amount << 10) | (rn << 5) | rd);
    }

    pub fn and_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x0A00_0000, rd, rn, rm, kind, amount);
    }
    pub fn bic_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x0A20_0000, rd, rn, rm, kind, amount);
    }
    pub fn orr_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x2A00_0000, rd, rn, rm, kind, amount);
    }
    pub fn orn_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x2A20_0000, rd, rn, rm, kind, amount);
    }
    pub fn eor_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x4A00_0000, rd, rn, rm, kind, amount);
    }
    pub fn ands_w(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.logical(0x6A00_0000, rd, rn, rm, kind, amount);
    }
    pub fn and_x(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x8A00_0000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn eor_x(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0xCA00_0000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn orr_x(&mut self, rd: Reg, rn: Reg, rm: Reg, kind: u32, amount: u32) {
        self.emit(0xAA00_0000 | (kind << 22) | (rm << 16) | (amount << 10) | (rn << 5) | rd);
    }
    /// mvn wd, wm
    pub fn mvn_w(&mut self, rd: Reg, rm: Reg) {
        self.orn_w(rd, ZR, rm, shift::LSL, 0);
    }

    // ---- shifts and bitfields ----

    pub fn lslv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_2000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn lsrv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_2400 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn asrv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_2800 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn rorv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_2C00 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn lslv_x(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x9AC0_2000 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn lsrv_x(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x9AC0_2400 | (rm << 16) | (rn << 5) | rd);
    }

    pub fn ubfm_w(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.emit(0x5300_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd);
    }
    pub fn sbfm_w(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.emit(0x1300_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd);
    }
    pub fn bfm_w(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.emit(0x3300_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd);
    }
    pub fn ubfm_x(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.emit(0xD340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd);
    }
    pub fn sbfm_x(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.emit(0x9340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd);
    }

    pub fn lsl_w_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        let amount = amount & 31;
        self.ubfm_w(rd, rn, (32 - amount) & 31, 31 - amount);
    }
    pub fn lsr_w_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        self.ubfm_w(rd, rn, amount & 31, 31);
    }
    pub fn asr_w_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        self.sbfm_w(rd, rn, amount & 31, 31);
    }
    pub fn ror_w_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        self.emit(0x1380_0000 | (rn << 16) | ((amount & 31) << 10) | (rn << 5) | rd); // extr
    }
    pub fn lsr_x_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        self.ubfm_x(rd, rn, amount & 63, 63);
    }
    pub fn lsl_x_imm(&mut self, rd: Reg, rn: Reg, amount: u32) {
        let amount = amount & 63;
        self.ubfm_x(rd, rn, (64 - amount) & 63, 63 - amount);
    }
    pub fn ubfx_w(&mut self, rd: Reg, rn: Reg, lsb: u32, width: u32) {
        self.ubfm_w(rd, rn, lsb, lsb + width - 1);
    }
    pub fn sbfx_w(&mut self, rd: Reg, rn: Reg, lsb: u32, width: u32) {
        self.sbfm_w(rd, rn, lsb, lsb + width - 1);
    }
    /// Inserts the low `width` bits of rn into rd at `lsb`.
    pub fn bfi_w(&mut self, rd: Reg, rn: Reg, lsb: u32, width: u32) {
        self.bfm_w(rd, rn, (32 - lsb) & 31, width - 1);
    }
    pub fn uxtb_w(&mut self, rd: Reg, rn: Reg) {
        self.ubfm_w(rd, rn, 0, 7);
    }
    pub fn uxth_w(&mut self, rd: Reg, rn: Reg) {
        self.ubfm_w(rd, rn, 0, 15);
    }
    pub fn sxtb_w(&mut self, rd: Reg, rn: Reg) {
        self.sbfm_w(rd, rn, 0, 7);
    }
    pub fn sxth_w(&mut self, rd: Reg, rn: Reg) {
        self.sbfm_w(rd, rn, 0, 15);
    }
    /// sxtw xd, wn
    pub fn sxtw_x(&mut self, rd: Reg, rn: Reg) {
        self.sbfm_x(rd, rn, 0, 31);
    }

    // ---- multiply / divide / bit tricks ----

    pub fn madd_w(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(0x1B00_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
    }
    pub fn msub_w(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(0x1B00_8000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
    }
    pub fn mul_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.madd_w(rd, rn, rm, ZR);
    }
    /// xd = xa + sext(wn) * sext(wm)
    pub fn smaddl(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(0x9B20_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
    }
    /// xd = xa + zext(wn) * zext(wm)
    pub fn umaddl(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(0x9BA0_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
    }
    pub fn udiv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_0800 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn sdiv_w(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_0C00 | (rm << 16) | (rn << 5) | rd);
    }
    pub fn clz_w(&mut self, rd: Reg, rn: Reg) {
        self.emit(0x5AC0_1000 | (rn << 5) | rd);
    }
    pub fn rbit_w(&mut self, rd: Reg, rn: Reg) {
        self.emit(0x5AC0_0000 | (rn << 5) | rd);
    }
    pub fn rev_w(&mut self, rd: Reg, rn: Reg) {
        self.emit(0x5AC0_0800 | (rn << 5) | rd);
    }
    pub fn rev16_w(&mut self, rd: Reg, rn: Reg) {
        self.emit(0x5AC0_0400 | (rn << 5) | rd);
    }

    // ---- conditional select / compare ----

    pub fn csel_w(&mut self, rd: Reg, rn: Reg, rm: Reg, condition: u32) {
        self.emit(0x1A80_0000 | (rm << 16) | (condition << 12) | (rn << 5) | rd);
    }
    pub fn csinc_w(&mut self, rd: Reg, rn: Reg, rm: Reg, condition: u32) {
        self.emit(0x1A80_0400 | (rm << 16) | (condition << 12) | (rn << 5) | rd);
    }
    pub fn csinv_w(&mut self, rd: Reg, rn: Reg, rm: Reg, condition: u32) {
        self.emit(0x5A80_0000 | (rm << 16) | (condition << 12) | (rn << 5) | rd);
    }
    /// rd = condition ? 1 : 0
    pub fn cset_w(&mut self, rd: Reg, condition: u32) {
        self.csinc_w(rd, ZR, ZR, cond::invert(condition));
    }
    pub fn cmp_w(&mut self, rn: Reg, rm: Reg) {
        self.subs_w(ZR, rn, rm, shift::LSL, 0);
    }
    pub fn cmp_w_imm(&mut self, rn: Reg, imm: u32) {
        self.subs_w_imm(ZR, rn, imm);
    }
    pub fn tst_w(&mut self, rn: Reg, rm: Reg) {
        self.ands_w(ZR, rn, rm, shift::LSL, 0);
    }

    // ---- flags ----

    pub fn mrs_nzcv(&mut self, rt: Reg) {
        self.emit(0xD53B_4200 | rt);
    }
    pub fn msr_nzcv(&mut self, rt: Reg) {
        self.emit(0xD51B_4200 | rt);
    }

    /// mrs xt, fpcr / msr fpcr, xt / mrs xt, fpsr / msr fpsr, xt
    pub fn mrs_fpcr(&mut self, rt: Reg) {
        self.emit(0xD53B_4400 | rt);
    }
    pub fn msr_fpcr(&mut self, rt: Reg) {
        self.emit(0xD51B_4400 | rt);
    }
    pub fn mrs_fpsr(&mut self, rt: Reg) {
        self.emit(0xD53B_4420 | rt);
    }
    pub fn msr_fpsr(&mut self, rt: Reg) {
        self.emit(0xD51B_4420 | rt);
    }

    // ---- floating point (v registers; `double` picks .d/double or .s/single) ----

    fn element(double: bool, lane: u32) -> u32 {
        if double {
            (lane << 4) | 0b1000
        } else {
            (lane << 3) | 0b100
        }
    }
    fn fp_type(double: bool) -> u32 {
        (double as u32) << 22
    }
    /// dup sd/dd, vn.s/d[lane]: an element to lane 0 of a scalar.
    pub fn dup_element(&mut self, double: bool, rd: Reg, rn: Reg, lane: u32) {
        self.emit(0x5E00_0400 | (Self::element(double, lane) << 16) | (rn << 5) | rd);
    }
    /// ins vd.s/d[lane], vn.s/d[from]
    pub fn ins_element(&mut self, double: bool, rd: Reg, lane: u32, rn: Reg, from: u32) {
        let imm4 = if double { from << 3 } else { from << 2 };
        self.emit(0x6E00_0400 | (Self::element(double, lane) << 16) | (imm4 << 11) | (rn << 5) | rd);
    }
    /// ins vd.s[lane], wn / ins vd.d[lane], xn
    pub fn ins_general(&mut self, double: bool, rd: Reg, lane: u32, rn: Reg) {
        self.emit(0x4E00_1C00 | (Self::element(double, lane) << 16) | (rn << 5) | rd);
    }
    /// umov wd, vn.s[lane] / umov xd, vn.d[lane]
    pub fn umov(&mut self, double: bool, rd: Reg, rn: Reg, lane: u32) {
        self.emit(0x0E00_3C00 | ((double as u32) << 30) | (Self::element(double, lane) << 16) | (rn << 5) | rd);
    }
    /// Two-source scalar operation (`fp::FMUL` and so on).
    pub fn fp2(&mut self, op: u32, double: bool, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(op | Self::fp_type(double) | (rm << 16) | (rn << 5) | rd);
    }
    /// Three-source scalar operation (`fp::FMADD` and so on): rd = ra ± rn*rm.
    pub fn fp3(&mut self, op: u32, double: bool, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.emit(op | Self::fp_type(double) | (rm << 16) | (ra << 10) | (rn << 5) | rd);
    }
    /// One-source scalar operation (`fp::FABS` and so on); `double` is the source type.
    pub fn fp1(&mut self, op: u32, double: bool, rd: Reg, rn: Reg) {
        self.emit(op | Self::fp_type(double) | (rn << 5) | rd);
    }
    /// fcmp / fcmpe with a register or zero.
    pub fn fcmp(&mut self, double: bool, rn: Reg, rm: Reg, zero: bool, signaling: bool) {
        let opc = ((signaling as u32) << 4) | ((zero as u32) << 3);
        self.emit(0x1E20_2000 | Self::fp_type(double) | (rm << 16) | (rn << 5) | opc);
    }
    /// fmov sd/dd, #imm8 (VFPExpandImm encoding)
    pub fn fmov_imm(&mut self, double: bool, rd: Reg, imm8: u32) {
        self.emit(0x1E20_1000 | Self::fp_type(double) | (imm8 << 13) | rd);
    }
    /// scvtf / ucvtf sd/dd, wn
    pub fn int_to_float(&mut self, signed: bool, double: bool, rd: Reg, rn: Reg) {
        let op = if signed { 0x1E22_0000 } else { 0x1E23_0000 };
        self.emit(op | Self::fp_type(double) | (rn << 5) | rd);
    }
    /// scvtf / ucvtf sd/dd, wn, #fbits (1..=32): wn read as a fixed-point number
    pub fn fixed_to_float(&mut self, signed: bool, double: bool, rd: Reg, rn: Reg, fbits: u32) {
        let op = if signed { 0x1E02_0000 } else { 0x1E03_0000 };
        self.emit(op | Self::fp_type(double) | ((64 - fbits) << 10) | (rn << 5) | rd);
    }
    /// fcvtzs / fcvtzu wd, sn/dn, #fbits (1..=32): to fixed point, toward zero, saturating
    pub fn float_to_fixed(&mut self, signed: bool, double: bool, rd: Reg, rn: Reg, fbits: u32) {
        let op = if signed { 0x1E18_0000 } else { 0x1E19_0000 };
        self.emit(op | Self::fp_type(double) | ((64 - fbits) << 10) | (rn << 5) | rd);
    }
    /// fcvtzs / fcvtzu wd, sn/dn
    pub fn float_to_int(&mut self, signed: bool, double: bool, rd: Reg, rn: Reg) {
        let op = if signed { 0x1E38_0000 } else { 0x1E39_0000 };
        self.emit(op | Self::fp_type(double) | (rn << 5) | rd);
    }
    /// Three-register vector operation (`simd::ADD` and so on); `q` = 128 bits, `size` = element size code.
    /// 64-bit elements without `q` (no 1D vector form) use the scalar encoding (ADD, SQADD, USHL, ...).
    pub fn simd3(&mut self, op: u32, q: bool, size: u32, rd: Reg, rn: Reg, rm: Reg) {
        let form = if !q && size == 3 { 0x5000_0000 } else { (q as u32) << 30 };
        self.emit(op | form | (size << 22) | (rm << 16) | (rn << 5) | rd);
    }
    /// fmul vd.4s/2s, vn.4s/2s, vm.s[index]
    pub fn fmul_element(&mut self, q: bool, rd: Reg, rn: Reg, rm: Reg, index: u32) {
        self.emit(0x0F80_9000 | ((q as u32) << 30) | ((index & 1) << 21) | ((index >> 1) << 11) | (rm << 16) | (rn << 5) | rd);
    }
    /// mul / mla / mls vd.<T>, vn.<T>, vm.<Ts>[index] (`op` 0x0F00_8000 / 0x2F00_0000 / 0x2F00_4000);
    /// `size` 1 (16-bit lanes, vm below v16) or 2 (32-bit lanes).
    pub fn int_element(&mut self, op: u32, q: bool, size: u32, rd: Reg, rn: Reg, rm: Reg, index: u32) {
        let (h, l, m) = if size == 1 { (index >> 2, (index >> 1) & 1, index & 1) } else { (index >> 1, index & 1, 0) };
        self.emit(op | ((q as u32) << 30) | (size << 22) | (l << 21) | (m << 20) | (rm << 16) | (h << 11) | (rn << 5) | rd);
    }
    /// mov vd.16b, vn.16b
    pub fn mov_vector(&mut self, rd: Reg, rn: Reg) {
        self.simd3(simd::ORR, true, 0, rd, rn, rn);
    }
    /// ld1 {vt.s/d}[lane], [xn] (with `post`: [xn], #4/#8)
    pub fn ld1_lane(&mut self, double: bool, rt: Reg, lane: u32, rn: Reg, post: bool) {
        self.lane_access(0x0040_0000, double, rt, lane, rn, post);
    }
    /// st1 {vt.s/d}[lane], [xn] (with `post`: [xn], #4/#8)
    pub fn st1_lane(&mut self, double: bool, rt: Reg, lane: u32, rn: Reg, post: bool) {
        self.lane_access(0, double, rt, lane, rn, post);
    }
    fn lane_access(&mut self, load: u32, double: bool, rt: Reg, lane: u32, rn: Reg, post: bool) {
        self.lane_access_sized(load, if double { 3 } else { 2 }, rt, lane, rn, post);
    }
    /// ld1/st1 of one lane of 1 << `size` bytes (with `post`: [xn], #bytes).
    fn lane_access_sized(&mut self, load: u32, size: u32, rt: Reg, lane: u32, rn: Reg, post: bool) {
        let base = if post { 0x0D9F_0000 } else { 0x0D00_0000 };
        // Q:S:size hold the lane index above the element size bits.
        let (opcode, index_bits, low) = match size {
            0 => (0b000, lane, 0),
            1 => (0b010, lane << 1, 0),
            2 => (0b100, lane << 2, 0),
            _ => (0b100, lane << 3, 0b01),
        };
        let (q, s, size_field) = (index_bits >> 3, (index_bits >> 2) & 1, (index_bits & 3) | low);
        self.emit(base | load | (q << 30) | (opcode << 13) | (s << 12) | (size_field << 10) | (rn << 5) | rt);
    }
    /// ld1 / st1 {vt.b/h/s/d}[lane], [xn] for an element of 1 << `size` bytes.
    pub fn lane_load_store(&mut self, load: bool, size: u32, rt: Reg, lane: u32, rn: Reg) {
        self.lane_access_sized(if load { 0x0040_0000 } else { 0 }, size, rt, lane, rn, false);
    }
    /// ld1r {vt.8b/4h/2s}, [xn]: one element loaded into every lane of the low half.
    pub fn ld1r(&mut self, size: u32, rt: Reg, rn: Reg) {
        self.emit(0x0D40_C000 | (size << 10) | (rn << 5) | rt);
    }
    /// Element field (imm5) for an element of 1 << `size` bytes at `lane`.
    fn element_sized(size: u32, lane: u32) -> u32 {
        (lane << (size + 1)) | (1 << size)
    }
    /// ins vd.<size>[lane], wn/xn
    pub fn ins_general_sized(&mut self, size: u32, rd: Reg, lane: u32, rn: Reg) {
        self.emit(0x4E00_1C00 | (Self::element_sized(size, lane) << 16) | (rn << 5) | rd);
    }
    /// umov / smov wd, vn.<size>[lane] (smov only for bytes and halfwords).
    pub fn move_element_out(&mut self, signed: bool, size: u32, rd: Reg, rn: Reg, lane: u32) {
        let op = if signed { 0x0E00_2C00 } else { 0x0E00_3C00 };
        self.emit(op | (Self::element_sized(size, lane) << 16) | (rn << 5) | rd);
    }
    /// sshll / ushll vd.<2x>, vn.<x> (low half), #shift, for elements of 1 << `size` bytes.
    pub fn shift_left_long(&mut self, signed: bool, size: u32, rd: Reg, rn: Reg, shift: u32) {
        let op = if signed { 0x0F00_A400 } else { 0x2F00_A400 };
        self.emit(op | (((8 << size) + shift) << 16) | (rn << 5) | rd);
    }
    /// Shift by immediate (SSHR, SRI, SHRN, SQSHL, ...): `op` holds U and the opcode,
    /// `imm7` is immh:immb. 64-bit elements without `q` (no 1D vector form) use the scalar encoding.
    pub fn shift_immediate(&mut self, op: u32, q: bool, imm7: u32, rd: Reg, rn: Reg) {
        let base = if !q && imm7 >= 64 { 0x5F00_0400 } else { 0x0F00_0400 | ((q as u32) << 30) };
        self.emit(base | op | (imm7 << 16) | (rn << 5) | rd);
    }
    /// One-source vector operation (`simd::SCVTF` and so on).
    pub fn simd2(&mut self, op: u32, q: bool, rd: Reg, rn: Reg) {
        self.emit(op | ((q as u32) << 30) | (rn << 5) | rd);
    }
    /// One-source vector operation with an element size (`simd::REV64` and so on).
    pub fn simd2_sized(&mut self, op: u32, q: bool, size: u32, rd: Reg, rn: Reg) {
        self.emit(op | ((q as u32) << 30) | (size << 22) | (rn << 5) | rd);
    }
    /// fmov vd.2s/4s, #imm8 (VFPExpandImm) in every lane.
    pub fn fmov_vector_imm(&mut self, q: bool, rd: Reg, imm8: u32) {
        self.emit(0x0F00_F400 | ((q as u32) << 30) | ((imm8 >> 5) << 16) | ((imm8 & 0x1F) << 5) | rd);
    }
    /// dup vd.<T>, wn: a core register in every lane (1 << `size` bytes each).
    pub fn dup_general(&mut self, q: bool, size: u32, rd: Reg, rn: Reg) {
        self.emit(0x0E00_0C00 | ((q as u32) << 30) | ((1 << size) << 16) | (rn << 5) | rd);
    }
    /// ld2/ld3/ld4 or st2/st3/st4 {vt..vt+count-1}.<T>, [xn] (interleaved structures).
    pub fn structures(&mut self, load: bool, count: u32, q: bool, size: u32, rt: Reg, rn: Reg) {
        let opcode = match count {
            2 => 0b1000,
            3 => 0b0100,
            _ => 0b0000,
        };
        self.emit(0x0C00_0000 | ((q as u32) << 30) | ((load as u32) << 22) | (opcode << 12) | (size << 10) | (rn << 5) | rt);
    }
    /// ext vd, vn, vm, #bytes: bytes from the pair vm:vn starting at `bytes`.
    /// tbl / tbx vd.8b, {vn.16b - vn+count-1.16b}, vm.8b
    pub fn table_lookup(&mut self, extend: bool, rd: Reg, rn: Reg, count: u32, rm: Reg) {
        self.emit(0x0E00_0000 | (rm << 16) | ((count - 1) << 13) | ((extend as u32) << 12) | (rn << 5) | rd);
    }
    /// movi vd.8b/16b, #imm8
    pub fn movi_bytes(&mut self, q: bool, rd: Reg, imm8: u32) {
        self.emit(0x0F00_E400 | ((q as u32) << 30) | (((imm8 >> 5) & 7) << 16) | ((imm8 & 0x1F) << 5) | rd);
    }
    pub fn ext(&mut self, q: bool, rd: Reg, rn: Reg, rm: Reg, bytes: u32) {
        self.emit(0x2E00_0000 | ((q as u32) << 30) | (rm << 16) | (bytes << 11) | (rn << 5) | rd);
    }
    /// dup vd.<T>, vn.<size>[lane]
    pub fn dup_lanes(&mut self, q: bool, size: u32, rd: Reg, rn: Reg, lane: u32) {
        self.emit(0x0E00_0400 | ((q as u32) << 30) | (Self::element_sized(size, lane) << 16) | (rn << 5) | rd);
    }

    // ---- memory: guest accesses are [x20 (memory base), w(address), uxtw] ----

    fn mem_reg(&mut self, base: u32, rt: Reg, rn: Reg, rm: Reg) {
        // option = UXTW (010), S = 0
        self.emit(base | (rm << 16) | (0b010 << 13) | (rn << 5) | rt);
    }
    pub fn ldr_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0xB860_0800, rt, rn, rm);
    }
    pub fn str_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0xB820_0800, rt, rn, rm);
    }
    pub fn ldrb_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x3860_0800, rt, rn, rm);
    }
    pub fn strb_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x3820_0800, rt, rn, rm);
    }
    pub fn ldrh_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x7860_0800, rt, rn, rm);
    }
    pub fn strh_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x7820_0800, rt, rn, rm);
    }
    pub fn ldrsb_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x38E0_0800, rt, rn, rm);
    }
    pub fn ldrsh_w_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0x78E0_0800, rt, rn, rm);
    }
    pub fn ldr_x_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0xF860_0800, rt, rn, rm);
    }
    pub fn str_x_uxtw(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.mem_reg(0xF820_0800, rt, rn, rm);
    }
    /// ldr x, [xn, xm] (no extend: for host tables)
    pub fn ldr_x_reg(&mut self, rt: Reg, rn: Reg, rm: Reg) {
        self.emit(0xF860_6800 | (rm << 16) | (rn << 5) | rt);
    }

    // ---- memory: host structures at [xn, #offset] ----

    pub fn ldr_w_off(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 4096);
        self.emit(0xB940_0000 | ((offset / 4) << 10) | (rn << 5) | rt);
    }
    pub fn str_w_off(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 4096);
        self.emit(0xB900_0000 | ((offset / 4) << 10) | (rn << 5) | rt);
    }
    pub fn ldr_x_off(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 8 == 0 && offset / 8 < 4096);
        self.emit(0xF940_0000 | ((offset / 8) << 10) | (rn << 5) | rt);
    }
    pub fn str_x_off(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 8 == 0 && offset / 8 < 4096);
        self.emit(0xF900_0000 | ((offset / 8) << 10) | (rn << 5) | rt);
    }
    pub fn ldp_w_off(&mut self, rt: Reg, rt2: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 64);
        self.emit(0x2940_0000 | ((offset / 4) << 15) | (rt2 << 10) | (rn << 5) | rt);
    }
    pub fn stp_w_off(&mut self, rt: Reg, rt2: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 64);
        self.emit(0x2900_0000 | ((offset / 4) << 15) | (rt2 << 10) | (rn << 5) | rt);
    }
    pub fn ldp_q_off(&mut self, rt: Reg, rt2: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 16 == 0 && offset / 16 < 64);
        self.emit(0xAD40_0000 | ((offset / 16) << 15) | (rt2 << 10) | (rn << 5) | rt);
    }
    pub fn stp_q_off(&mut self, rt: Reg, rt2: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 16 == 0 && offset / 16 < 64);
        self.emit(0xAD00_0000 | ((offset / 16) << 15) | (rt2 << 10) | (rn << 5) | rt);
    }
    /// stp xt, xt2, [sp, #-16]!
    pub fn push_pair_x(&mut self, rt: Reg, rt2: Reg) {
        self.emit(0xA9BF_0000 | (rt2 << 10) | (SP << 5) | rt);
    }
    /// ldp xt, xt2, [sp], #16
    pub fn pop_pair_x(&mut self, rt: Reg, rt2: Reg) {
        self.emit(0xA8C1_0000 | (rt2 << 10) | (SP << 5) | rt);
    }
    /// stp dt, dt2, [sp, #-16]!
    pub fn push_pair_d(&mut self, rt: Reg, rt2: Reg) {
        self.emit(0x6DBF_0000 | (rt2 << 10) | (SP << 5) | rt);
    }
    /// ldp dt, dt2, [sp], #16
    pub fn pop_pair_d(&mut self, rt: Reg, rt2: Reg) {
        self.emit(0x6CC1_0000 | (rt2 << 10) | (SP << 5) | rt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodings checked against llvm-mc output.
    #[test]
    fn known_encodings() {
        let mut a = Asm::new();
        a.add_w(0, 1, 2, shift::LSL, 0); // add w0, w1, w2
        a.subs_w_imm(3, 4, 5); // subs w3, w4, #5
        a.ldr_w_uxtw(1, 20, 16); // ldr w1, [x20, w16, uxtw]
        a.lsl_w_imm(0, 1, 3); // lsl w0, w1, #3
        a.lsr_w_imm(0, 1, 3); // lsr w0, w1, #3
        a.mrs_nzcv(16); // mrs x16, nzcv
        a.cset_w(0, cond::EQ); // cset w0, eq
        a.ror_w_imm(2, 3, 8); // ror w2, w3, #8
        a.mov_w_imm(5, 0x1234_5678); // movz w5, #0x5678 ; movk w5, #0x1234, lsl #16
        a.and_w_imm(0, 1, 0xFF, 16); // and w0, w1, #0xff
        a.and_w_imm(2, 3, 0x3000_0000, 16); // and w2, w3, #0x30000000
        a.orr_w_imm(4, 5, 0x0F0F_0F0F, 16); // orr w4, w5, #0x0f0f0f0f
        a.eor_w_imm(6, 7, 0x8000_0000, 16); // eor w6, w7, #0x80000000
        a.add_x_uxtw(17, 20, 16); // add x17, x20, w16, uxtw
        assert_eq!(
            a.code,
            vec![
                0x0B02_0020,
                0x7100_1483,
                0xB870_4A81,
                0x531D_7020,
                0x5303_7C20,
                0xD53B_4210,
                0x1A9F_17E0,
                0x1383_2062,
                0x528A_CF05,
                0x72A2_4685,
                0x1200_1C20,
                0x1204_0462,
                0x3200_CCA4,
                0x5201_00E6,
                0x8B30_4291,
            ]
        );
    }

    /// Float encodings checked against the NDK assembler.
    #[test]
    fn float_encodings() {
        let mut a = Asm::new();
        a.mrs_fpcr(16);
        a.msr_fpcr(16);
        a.mrs_fpsr(17);
        a.msr_fpsr(17);
        a.dup_element(false, 16, 3, 2); // mov s16, v3.s[2]
        a.dup_element(true, 16, 3, 1); // mov d16, v3.d[1]
        a.ins_element(false, 5, 3, 16, 0); // mov v5.s[3], v16.s[0]
        a.ins_element(true, 5, 1, 17, 1); // mov v5.d[1], v17.d[1]
        a.ins_general(false, 5, 1, 7); // mov v5.s[1], w7
        a.ins_general(true, 5, 1, 16); // mov v5.d[1], x16
        a.umov(false, 3, 5, 3); // mov w3, v5.s[3]
        a.umov(true, 16, 5, 1); // mov x16, v5.d[1]
        a.fp2(fp::FMUL, false, 16, 1, 17);
        a.fp2(fp::FDIV, true, 16, 1, 17);
        a.fp2(fp::FADD, false, 16, 1, 17);
        a.fp2(fp::FSUB, true, 16, 1, 17);
        a.fp2(fp::FNMUL, false, 16, 1, 17);
        a.fp3(fp::FMADD, false, 16, 1, 17, 18);
        a.fp3(fp::FMSUB, true, 16, 1, 17, 18);
        a.fp3(fp::FNMADD, false, 16, 1, 17, 18);
        a.fp3(fp::FNMSUB, true, 16, 1, 17, 18);
        a.fp1(fp::FABS, false, 16, 17);
        a.fp1(fp::FNEG, true, 16, 17);
        a.fp1(fp::FSQRT, false, 16, 17);
        a.fp1(fp::FCVT_TO_SINGLE, true, 16, 17); // fcvt s16, d17
        a.fp1(fp::FCVT_TO_DOUBLE, false, 16, 17); // fcvt d16, s17
        a.fcmp(false, 16, 17, false, false);
        a.fcmp(true, 16, 0, true, false);
        a.fcmp(false, 16, 17, false, true);
        a.fcmp(true, 16, 0, true, true);
        a.fmov_imm(false, 16, 0x70); // fmov s16, #1.0
        a.fmov_imm(true, 16, 0xE0); // fmov d16, #-0.5
        a.int_to_float(true, false, 16, 16); // scvtf s16, w16
        a.int_to_float(false, true, 16, 16); // ucvtf d16, w16
        a.float_to_int(true, false, 16, 17); // fcvtzs w16, s17
        a.float_to_int(false, true, 16, 17); // fcvtzu w16, d17
        a.ld1_lane(false, 3, 3, 17, false); // ld1 {v3.s}[3], [x17]
        a.ld1_lane(true, 3, 1, 17, true); // ld1 {v3.d}[1], [x17], #8
        a.st1_lane(false, 3, 2, 17, true); // st1 {v3.s}[2], [x17], #4
        a.st1_lane(true, 3, 0, 17, false); // st1 {v3.d}[0], [x17]
        assert_eq!(
            a.code,
            vec![
                0xD53B_4410, 0xD51B_4410, 0xD53B_4431, 0xD51B_4431, 0x5E14_0470, 0x5E18_0470, 0x6E1C_0605, 0x6E18_4625,
                0x4E0C_1CE5, 0x4E18_1E05, 0x0E1C_3CA3, 0x4E18_3CB0, 0x1E31_0830, 0x1E71_1830, 0x1E31_2830, 0x1E71_3830,
                0x1E31_8830, 0x1F11_4830, 0x1F51_C830, 0x1F31_4830, 0x1F71_C830, 0x1E20_C230, 0x1E61_4230, 0x1E21_C230,
                0x1E62_4230, 0x1E22_C230, 0x1E31_2200, 0x1E60_2208, 0x1E31_2210, 0x1E60_2218, 0x1E2E_1010, 0x1E7C_1010,
                0x1E22_0210, 0x1E63_0210, 0x1E38_0230, 0x1E79_0230, 0x4D40_9223, 0x4DDF_8623, 0x4D9F_8223, 0x0D00_8623,
            ]
        );
    }

    /// Vector encodings checked against the NDK assembler.
    #[test]
    fn vector_encodings() {
        let mut a = Asm::new();
        a.simd3(simd::ADD, true, 2, 16, 1, 17); // add v16.4s
        a.simd3(simd::SUB, false, 0, 16, 1, 17); // sub v16.8b
        a.simd3(simd::ADD, true, 3, 16, 1, 17); // add v16.2d
        a.simd3(simd::AND, true, 0, 16, 1, 17);
        a.simd3(simd::BIC, false, 0, 16, 1, 17);
        a.simd3(simd::ORR, true, 0, 16, 1, 17);
        a.simd3(simd::ORN, false, 0, 16, 1, 17);
        a.simd3(simd::EOR, true, 0, 16, 1, 17);
        a.simd3(simd::FADD, true, 0, 16, 1, 17);
        a.simd3(simd::FSUB, false, 0, 16, 1, 17);
        a.simd3(simd::FMUL, true, 0, 16, 1, 17);
        a.fmul_element(true, 16, 1, 7, 3);
        a.fmul_element(false, 16, 1, 7, 1);
        a.fmul_element(true, 16, 1, 7, 2);
        assert_eq!(
            a.code,
            vec![
                0x4EB1_8430, 0x2E31_8430, 0x4EF1_8430, 0x4E31_1C30, 0x0E71_1C30, 0x4EB1_1C30, 0x0EF1_1C30, 0x6E31_1C30,
                0x4E31_D430, 0x0EB1_D430, 0x6E31_DC30, 0x4FA7_9830, 0x0FA7_9030, 0x4F87_9830,
            ]
        );
    }
}
