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
}

impl Asm {
    pub fn new() -> Asm {
        Asm::default()
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
}
