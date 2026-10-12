//! Thumb / Thumb-2 → ARM64. One block is a run of guest instructions ending
//! at a branch (or `MAX_INSTRUCTIONS`). An instruction the translator does not
//! handle ends the block with a fallback exit, so Unicorn runs it.
//!
//! Translated code may not disturb the guest flags (host NZCV) unless the
//! guest instruction sets them, so helper sequences branch with cbz/tbz
//! instead of comparing.

use super::emit::{cond, shift, Asm, Reg, ZR};
use super::{exit, host, offsets};
use crate::mem::GuestMem;

#[path = "arm.rs"]
mod arm;
#[path = "neon.rs"]
mod neon;
#[path = "vfp.rs"]
mod vfp;
pub use arm::translate as translate_arm;

const MAX_INSTRUCTIONS: u32 = 64;

// Scratch registers (never guest registers).
const T0: Reg = 16;
const T1: Reg = 17;
const T2: Reg = 24;
const T3: Reg = 25;
const T4: Reg = 26;
const T5: Reg = 27;
const T6: Reg = 28;

const PC: u32 = 15;
const LR: u32 = 14;
const SP: u32 = 13;

/// Ends a block: continue at `pc` (bit 0 = Thumb) for `reason`.
pub fn emit_exit(a: &mut Asm, pc: u32, reason: u32) {
    a.mov_w_imm(host::EXIT_PC, pc);
    a.mov_w_imm(host::EXIT_REASON, reason);
    a.br(host::EXIT_STUB);
}

/// Ends a block: continue at the address in `target` (bit 0 = Thumb).
fn emit_exit_to(a: &mut Asm, target: Reg) {
    a.mov_w(host::EXIT_PC, target);
    a.mov_w_imm(host::EXIT_REASON, exit::JUMP);
    a.br(host::EXIT_STUB);
}

#[derive(PartialEq)]
enum Flow {
    Next,
    /// The block ended (an unconditional branch was emitted).
    End,
}

type Result<T> = std::result::Result<T, ()>;

struct Translator {
    a: Asm,
    mem: GuestMem,
    /// Address of the instruction being translated.
    pc: u32,
    /// Its size in bytes (2 or 4).
    size: u32,
    /// Instructions translated so far.
    count: u32,
    /// Inside an IT block (16-bit instructions do not set flags there).
    in_it: bool,
    /// Thumb (true) or ARM (false) code.
    thumb: bool,
    /// Host FPCR is in NEON's standard mode (see neon.rs); blocks start in the guest's FPSCR mode.
    neon_fp: bool,
}

/// Translates the block starting at `start` (a Thumb address without bit 0).
/// Returns the code and the guest address just past the last translated instruction.
pub fn translate(mem: GuestMem, start: u32) -> (Vec<u32>, u32) {
    let mut t = Translator { a: Asm::new(), mem, pc: start, size: 2, count: 0, in_it: false, thumb: true, neon_fp: false };
    let end;
    loop {
        if t.count >= MAX_INSTRUCTIONS {
            emit_exit(&mut t.a, t.pc | 1, exit::JUMP);
            end = t.pc;
            break;
        }
        let checkpoint = t.a.checkpoint();
        let first = mem.read_u16(t.pc) as u32;
        if first & 0xFF00 == 0xBF00 && first & 0xF != 0 {
            // IT: translate the whole block or fall back for all of it.
            match t.it_block(first) {
                Ok(Flow::Next) => continue,
                Ok(Flow::End) => {
                    end = t.pc;
                    break;
                }
                Err(instructions) => {
                    t.a.rollback(checkpoint);
                    emit_exit(&mut t.a, t.pc | 1, exit::FALLBACK | ((instructions + 1) << 8));
                    end = t.pc;
                    break;
                }
            }
        }
        match t.instruction(cond::AL) {
            Ok(Flow::Next) => {
                t.pc += t.size;
                t.count += 1;
            }
            Ok(Flow::End) => {
                end = t.pc + t.size;
                break;
            }
            Err(()) => {
                t.a.rollback(checkpoint);
                emit_exit(&mut t.a, t.pc | 1, exit::FALLBACK | (1 << 8));
                end = t.pc;
                break;
            }
        }
    }
    t.a.finish();
    (t.a.code, end)
}

/// Decodes a modified immediate (ThumbExpandImm): the value and, for
/// rotated forms, the carry it produces.
fn expand_immediate(imm12: u32) -> (u32, Option<bool>) {
    let imm8 = imm12 & 0xFF;
    if imm12 >> 10 == 0 {
        let value = match (imm12 >> 8) & 3 {
            0 => imm8,
            1 => (imm8 << 16) | imm8,
            2 => (imm8 << 24) | (imm8 << 8),
            _ => imm8 * 0x0101_0101,
        };
        (value, None)
    } else {
        let value = (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7);
        (value, Some(value >> 31 == 1))
    }
}

fn sign_extend(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((value << shift) as i32) >> shift) as u32
}

impl Translator {
    fn next_pc(&self) -> u32 {
        self.pc + self.size
    }

    /// The value r15 reads as: this instruction + 4 (Thumb) or + 8 (ARM).
    fn pc_value(&self) -> u32 {
        self.pc + if self.thumb { 4 } else { 8 }
    }

    /// Bit 0 of an address in this instruction set.
    fn mode_bit(&self) -> u32 {
        self.thumb as u32
    }

    /// A host register holding guest register `r` (r15 is materialised in `scratch`).
    fn read(&mut self, r: u32, scratch: Reg) -> Reg {
        if r == PC {
            self.a.mov_w_imm(scratch, self.pc_value());
            scratch
        } else {
            r
        }
    }

    // ---- flags ----

    /// N and Z from `result`; C and V keep their values.
    fn set_nz(&mut self, result: Reg) {
        let a = &mut self.a;
        a.mrs_nzcv(T0);
        a.tst_w(result, result);
        a.mrs_nzcv(T1);
        a.lsr_w_imm(T0, T0, 28);
        a.bfi_w(T1, T0, 28, 2);
        a.msr_nzcv(T1);
    }

    /// N and Z from `result`, C from bit 0 of `carry`; V keeps its value.
    fn set_nz_carry(&mut self, result: Reg, carry: Reg) {
        let a = &mut self.a;
        a.mrs_nzcv(T0);
        a.tst_w(result, result);
        a.mrs_nzcv(T1);
        a.lsr_w_imm(T0, T0, 28);
        a.bfi_w(T1, T0, 28, 1);
        a.bfi_w(T1, carry, 29, 1);
        a.msr_nzcv(T1);
    }

    // ---- shifts (immediate amounts; `kind` 0-3 = LSL LSR ASR ROR, ROR #0 = RRX) ----

    /// result = rm shifted; with `carry_out`, bit 0 of T3 = shifter carry (or
    /// returns false when the carry is the old C flag).
    fn shift_immediate(&mut self, result: Reg, rm: Reg, kind: u32, amount: u32, carry_out: bool) -> bool {
        let a = &mut self.a;
        match (kind, amount) {
            (shift::LSL, 0) => {
                a.mov_w(result, rm);
                false
            }
            (shift::LSL, n) => {
                if carry_out {
                    a.ubfx_w(T3, rm, 32 - n, 1);
                }
                a.lsl_w_imm(result, rm, n);
                true
            }
            (shift::LSR, n) => {
                let n = if n == 0 { 32 } else { n };
                if carry_out {
                    a.ubfx_w(T3, rm, n - 1, 1);
                }
                if n == 32 {
                    a.mov_w_imm(result, 0);
                } else {
                    a.lsr_w_imm(result, rm, n);
                }
                true
            }
            (shift::ASR, n) => {
                let n = if n == 0 { 32 } else { n };
                if carry_out {
                    a.ubfx_w(T3, rm, n.min(32) - 1, 1);
                }
                a.asr_w_imm(result, rm, n.min(31));
                true
            }
            (_, 0) => {
                // RRX: C:rm >> 1
                a.mrs_nzcv(T4);
                if carry_out {
                    a.and_w_imm(T3, rm, 1, T5);
                }
                a.lsr_w_imm(result, rm, 1);
                a.ubfx_w(T4, T4, 29, 1);
                a.bfi_w(result, T4, 31, 1);
                true
            }
            (_, n) => {
                a.ror_w_imm(result, rm, n);
                if carry_out {
                    a.lsr_w_imm(T3, result, 31);
                }
                true
            }
        }
    }

    /// result = rm shifted by the low byte of rs (register-controlled shift).
    /// With `carry_out`, T3 bit 0 = carry and T4 = 1 when the amount was 0.
    fn shift_register(&mut self, result: Reg, rm: Reg, rs: Reg, kind: u32, carry_out: bool) {
        let a = &mut self.a;
        a.and_w_imm(T2, rs, 0xFF, T5); // amount
        let done = a.label();
        match kind {
            shift::LSL | shift::LSR => {
                // 64-bit shift of the zero-extended value covers 0-63; 64+ gives 0.
                a.mov_w(T6, rm); // zero-extends into x
                if kind == shift::LSL {
                    a.lslv_x(T5, T6, T2);
                } else {
                    a.lsl_x_imm(T5, T6, 1); // keep the last bit shifted out at bit 0
                    a.lsrv_x(T5, T5, T2);
                }
                let big = a.label();
                a.lsr_w_imm(T4, T2, 6);
                a.cbnz_w_to(T4, big);
                if carry_out {
                    if kind == shift::LSL {
                        a.lsr_x_imm(T3, T5, 32);
                    } else {
                        a.mov_w(T3, T5);
                    }
                }
                if kind == shift::LSL {
                    a.mov_w(result, T5);
                } else {
                    a.lsr_x_imm(T5, T5, 1);
                    a.mov_w(result, T5);
                }
                a.b_to(done);
                a.bind(big);
                a.mov_w_imm(result, 0);
                if carry_out {
                    a.mov_w_imm(T3, 0);
                }
            }
            shift::ASR => {
                // Amounts of 32+ fill with the sign; carry = bit 31.
                a.sxtw_x(T6, rm);
                let small = a.label();
                a.lsr_w_imm(T4, T2, 5);
                a.cbz_w_to(T4, small);
                a.mov_w_imm(T2, 32);
                a.bind(small);
                if carry_out {
                    a.lsl_x_imm(T5, T6, 1);
                    a.emit(0x9AC0_2800 | (T2 << 16) | (T5 << 5) | T5); // asrv x
                    a.mov_w(T3, T5);
                }
                a.emit(0x9AC0_2800 | (T2 << 16) | (T6 << 5) | T6); // asrv x
                a.mov_w(result, T6);
            }
            _ => {
                if carry_out {
                    // carry = bit (amount-1) mod 32 of rm = bit 31 of the result
                    a.rorv_w(result, rm, T2);
                    a.lsr_w_imm(T3, result, 31);
                } else {
                    a.rorv_w(result, rm, T2);
                }
            }
        }
        a.bind(done);
        if carry_out {
            // T4 = 1 when the amount is 0 (carry unchanged).
            a.mov_w_imm(T4, 0);
            let nonzero = a.label();
            a.cbnz_w_to(T2, nonzero);
            a.mov_w_imm(T4, 1);
            a.bind(nonzero);
        }
    }

    // ---- data processing ----

    /// Data-processing ops shared by the 32-bit immediate and shifted-register
    /// forms. `operand` holds the second operand; `carry` says where the
    /// shifter carry is (None = keep C).
    #[allow(clippy::too_many_arguments)]
    fn data_processing(&mut self, op: u32, set_flags: bool, rd: u32, rn: u32, operand: Reg, carry: Option<Reg>) -> Result<Flow> {
        let n = if rn == PC { self.read(PC, T1) } else { rn };
        let d = if rd == PC { T6 } else { rd };
        let logical_flags = |t: &mut Translator, result: Reg| {
            if set_flags {
                match carry {
                    Some(c) => t.set_nz_carry(result, c),
                    None => t.set_nz(result),
                }
            }
        };
        match op {
            0b0000 => {
                // AND / TST
                if rd == PC && set_flags {
                    self.a.and_w(T6, n, operand, shift::LSL, 0);
                    logical_flags(self, T6);
                    return Ok(Flow::Next);
                }
                self.a.and_w(d, n, operand, shift::LSL, 0);
                logical_flags(self, d);
            }
            0b0001 => {
                self.a.bic_w(d, n, operand, shift::LSL, 0);
                logical_flags(self, d);
            }
            0b0010 => {
                // ORR / MOV
                if rn == PC {
                    self.a.mov_w(d, operand);
                } else {
                    self.a.orr_w(d, n, operand, shift::LSL, 0);
                }
                logical_flags(self, d);
            }
            0b0011 => {
                // ORN / MVN
                if rn == PC {
                    self.a.mvn_w(d, operand);
                } else {
                    self.a.orn_w(d, n, operand, shift::LSL, 0);
                }
                logical_flags(self, d);
            }
            0b0100 => {
                // EOR / TEQ
                if rd == PC && set_flags {
                    self.a.eor_w(T6, n, operand, shift::LSL, 0);
                    logical_flags(self, T6);
                    return Ok(Flow::Next);
                }
                self.a.eor_w(d, n, operand, shift::LSL, 0);
                logical_flags(self, d);
            }
            0b1000 => {
                // ADD / CMN
                if rd == PC && set_flags {
                    self.a.adds_w(ZR, n, operand, shift::LSL, 0);
                    return Ok(Flow::Next);
                }
                if set_flags {
                    self.a.adds_w(d, n, operand, shift::LSL, 0);
                } else {
                    self.a.add_w(d, n, operand, shift::LSL, 0);
                }
            }
            0b1010 => {
                if set_flags {
                    self.a.adcs_w(d, n, operand);
                } else {
                    self.a.adc_w(d, n, operand);
                }
            }
            0b1011 => {
                if set_flags {
                    self.a.sbcs_w(d, n, operand);
                } else {
                    self.a.sbc_w(d, n, operand);
                }
            }
            0b1101 => {
                // SUB / CMP
                if rd == PC && set_flags {
                    self.a.subs_w(ZR, n, operand, shift::LSL, 0);
                    return Ok(Flow::Next);
                }
                if set_flags {
                    self.a.subs_w(d, n, operand, shift::LSL, 0);
                } else {
                    self.a.sub_w(d, n, operand, shift::LSL, 0);
                }
            }
            0b1110 => {
                if set_flags {
                    self.a.subs_w(d, operand, n, shift::LSL, 0);
                } else {
                    self.a.sub_w(d, operand, n, shift::LSL, 0);
                }
            }
            _ => return Err(()),
        }
        if rd == PC {
            if self.thumb {
                return Err(()); // computed jumps with Thumb-2 data ops: rare, leave to Unicorn
            }
            // ARM: the result went to T6; writing the PC interworks like BX.
            emit_exit_to(&mut self.a, T6);
            return Ok(Flow::End);
        }
        Ok(Flow::Next)
    }

    // ---- memory ----

    /// Loads (`size` 1/2/4, `signed`) or stores guest register `rt` at the guest address in `address`.
    fn access(&mut self, load: bool, size: u32, signed: bool, rt: Reg, address: Reg) {
        let a = &mut self.a;
        let base = host::MEMORY;
        match (load, size, signed) {
            (true, 4, _) => a.ldr_w_uxtw(rt, base, address),
            (true, 2, false) => a.ldrh_w_uxtw(rt, base, address),
            (true, 2, true) => a.ldrsh_w_uxtw(rt, base, address),
            (true, _, true) => a.ldrsb_w_uxtw(rt, base, address),
            (true, _, false) => a.ldrb_w_uxtw(rt, base, address),
            (false, 4, _) => a.str_w_uxtw(rt, base, address),
            (false, 2, _) => a.strh_w_uxtw(rt, base, address),
            (false, _, _) => a.strb_w_uxtw(rt, base, address),
        }
    }

    /// A single load/store with offset `offset` (signed), indexing `index`
    /// (true = use the offset address) and `writeback`.
    #[allow(clippy::too_many_arguments)]
    fn load_store(&mut self, load: bool, size: u32, signed: bool, rt: u32, rn: u32, offset: u32, index: bool, writeback: bool) -> Result<Flow> {
        if rn == PC && (writeback || !index) {
            return Err(());
        }
        if rn == PC {
            // Literal: the address is a constant (Align(PC, 4) + offset).
            let address = (self.pc_value() & !3).wrapping_add(offset);
            if load && size == 4 {
                // Literal pools live in code, which never changes: load at translation time.
                let value = self.mem.read_u32(address);
                if rt == PC {
                    self.a.mov_w_imm(T6, value);
                    emit_exit_to(&mut self.a, T6);
                    return Ok(Flow::End);
                }
                self.a.mov_w_imm(rt, value);
                return Ok(Flow::Next);
            }
            self.a.mov_w_imm(T0, address);
            if !load && rt == PC {
                return Err(());
            }
            let target = if rt == PC { T6 } else { rt };
            self.access(load, size, signed, target, T0);
            return Ok(Flow::Next);
        }
        let n = rn;
        let address = if index && offset != 0 {
            self.a.add_w_any(T0, n, offset, T1);
            T0
        } else {
            n
        };
        if !load && rt == PC {
            return Err(());
        }
        let target = if rt == PC { T6 } else { rt };
        if load && writeback && rt == rn {
            return Err(()); // unpredictable
        }
        self.access(load, size, signed, target, address);
        if writeback {
            self.a.add_w_any(n, n, offset, T1);
        }
        if rt == PC {
            if !load || size != 4 {
                return Err(());
            }
            emit_exit_to(&mut self.a, T6);
            return Ok(Flow::End);
        }
        Ok(Flow::Next)
    }

    /// LDM/STM: `list` of registers, increment-after (true) or decrement-before.
    fn load_store_multiple(&mut self, load: bool, rn: u32, list: u32, increment: bool, writeback: bool) -> Result<Flow> {
        self.load_store_multiple_at(load, rn, list, increment, !increment, writeback)
    }

    /// LDM/STM in any of the four modes: `up` = increment, `before` = adjust before each access.
    fn load_store_multiple_at(&mut self, load: bool, rn: u32, list: u32, up: bool, before: bool, writeback: bool) -> Result<Flow> {
        let count = list.count_ones();
        if count == 0 || rn == PC || (!load && list & (1 << PC) != 0) || (list & (1 << SP) != 0 && rn != SP && load) {
            return Err(());
        }
        let a = &mut self.a;
        // T0 = lowest address; T1 = host address of it.
        match (up, before) {
            (true, false) => a.mov_w(T0, rn),
            (true, true) => a.add_w_imm(T0, rn, 4),
            (false, true) => a.sub_w_imm(T0, rn, count * 4),
            (false, false) => a.sub_w_imm(T0, rn, count * 4 - 4),
        }
        a.add_x_uxtw(T1, host::MEMORY, T0);
        let mut offset = 0;
        for r in 0..16u32 {
            if list & (1 << r) == 0 {
                continue;
            }
            let target = if r == PC { T6 } else if r == rn && load { T5 } else { r };
            if load {
                a.ldr_w_off(target, T1, offset);
            } else {
                a.str_w_off(r, T1, offset);
            }
            offset += 4;
        }
        if writeback && !(load && list & (1 << rn) != 0) {
            if up {
                a.add_w_imm(rn, rn, count * 4);
            } else {
                a.sub_w_imm(rn, rn, count * 4);
            }
        }
        if load && list & (1 << rn) != 0 {
            a.mov_w(rn, T5);
        }
        if load && list & (1 << PC) != 0 {
            emit_exit_to(a, T6);
            return Ok(Flow::End);
        }
        Ok(Flow::Next)
    }

    // ---- branches ----

    fn branch(&mut self, target: u32, condition: u32, thumb: bool) -> Flow {
        let destination = if thumb { target | 1 } else { target & !3 };
        if condition == cond::AL {
            emit_exit(&mut self.a, destination, exit::JUMP);
            Flow::End
        } else {
            let skip = self.a.label();
            self.a.b_cond_to(cond::invert(condition), skip);
            emit_exit(&mut self.a, destination, exit::JUMP);
            self.a.bind(skip);
            // Not taken: the block ends here too, continuing after the branch.
            let next = self.next_pc() | self.mode_bit();
            emit_exit(&mut self.a, next, exit::JUMP);
            Flow::End
        }
    }

    // ---- IT blocks ----

    /// Translates `IT` and its instructions. Err(n) = fall back for the IT
    /// instruction and its n instructions.
    fn it_block(&mut self, it: u32) -> std::result::Result<Flow, u32> {
        let first = (it >> 4) & 0xF;
        let mask = it & 0xF;
        let length = 4 - mask.trailing_zeros();
        let mut conditions = vec![first];
        for i in 1..length {
            let bit = (mask >> (4 - i)) & 1;
            conditions.push((first & !1) | bit);
        }
        let it_pc = self.pc;
        let it_count = self.count;
        self.pc += 2;
        self.count += 1;
        self.in_it = true;
        let mut ended = false;
        for (i, &condition) in conditions.iter().enumerate() {
            let first_half = self.mem.read_u16(self.pc) as u32;
            if first_half & 0xFF00 == 0xBF00 && first_half & 0xF != 0 {
                break; // nested IT: invalid
            }
            let skip = self.a.label();
            if condition != cond::AL {
                self.a.b_cond_to(cond::invert(condition), skip);
            }
            let fp_mode = self.neon_fp;
            match self.instruction(cond::AL) {
                Ok(Flow::Next) => {}
                Ok(Flow::End) => {
                    if i + 1 != conditions.len() {
                        self.in_it = false;
                        self.pc = it_pc;
                        self.count = it_count;
                        return Err(length);
                    }
                    ended = true;
                }
                Err(()) => {
                    self.in_it = false;
                    self.pc = it_pc;
                    self.count = it_count;
                    return Err(length);
                }
            }
            self.a.bind(skip);
            if self.neon_fp != fp_mode {
                // The mode changed only if the instruction ran.
                let neon = self.neon_fp;
                self.set_fp_mode(neon);
            }
            self.pc += self.size;
            self.count += 1;
        }
        self.in_it = false;
        if ended {
            // The last instruction branched when its condition held; otherwise continue after it.
            emit_exit(&mut self.a, self.pc | 1, exit::JUMP);
            return Ok(Flow::End);
        }
        Ok(Flow::Next)
    }

    // ---- decoding ----

    fn instruction(&mut self, _condition: u32) -> Result<Flow> {
        let first = self.mem.read_u16(self.pc) as u32;
        if first >> 11 >= 0b11101 {
            self.size = 4;
            let second = self.mem.read_u16(self.pc + 2) as u32;
            self.thumb32(first, second)
        } else {
            self.size = 2;
            self.thumb16(first)
        }
    }

    fn thumb16(&mut self, h: u32) -> Result<Flow> {
        let set_flags = !self.in_it;
        let low3 = |shift: u32| (h >> shift) & 7;
        match h >> 10 {
            0b000000..=0b000101 => {
                // LSL/LSR/ASR #imm5
                let (kind, amount, rm, rd) = ((h >> 11) & 3, (h >> 6) & 0x1F, low3(3), low3(0));
                if kind == shift::LSL && amount == 0 {
                    self.a.mov_w(rd, rm);
                    if set_flags {
                        self.set_nz(rd);
                    }
                    return Ok(Flow::Next);
                }
                let carry = self.shift_immediate(rd, rm, kind, amount, set_flags);
                if set_flags {
                    if carry {
                        self.set_nz_carry(rd, T3);
                    } else {
                        self.set_nz(rd);
                    }
                }
                Ok(Flow::Next)
            }
            0b000110 | 0b000111 => {
                // ADD/SUB register or imm3
                let (immediate, subtract) = ((h >> 10) & 1 == 1, (h >> 9) & 1 == 1);
                let (field, rn, rd) = (low3(6), low3(3), low3(0));
                let a = &mut self.a;
                match (immediate, subtract, set_flags) {
                    (false, false, true) => a.adds_w(rd, rn, field, shift::LSL, 0),
                    (false, false, false) => a.add_w(rd, rn, field, shift::LSL, 0),
                    (false, true, true) => a.subs_w(rd, rn, field, shift::LSL, 0),
                    (false, true, false) => a.sub_w(rd, rn, field, shift::LSL, 0),
                    (true, false, true) => a.adds_w_imm(rd, rn, field),
                    (true, false, false) => a.add_w_imm(rd, rn, field),
                    (true, true, true) => a.subs_w_imm(rd, rn, field),
                    (true, true, false) => a.sub_w_imm(rd, rn, field),
                }
                Ok(Flow::Next)
            }
            0b001000..=0b001111 => {
                // MOV/CMP/ADD/SUB imm8
                let (op, rd, imm) = ((h >> 11) & 3, low3(8), h & 0xFF);
                match op {
                    0 => {
                        self.a.mov_w_imm(rd, imm);
                        if set_flags {
                            self.set_nz(rd);
                        }
                    }
                    1 => self.a.cmp_w_imm(rd, imm),
                    2 if set_flags => self.a.adds_w_imm(rd, rd, imm),
                    2 => self.a.add_w_imm(rd, rd, imm),
                    _ if set_flags => self.a.subs_w_imm(rd, rd, imm),
                    _ => self.a.sub_w_imm(rd, rd, imm),
                }
                Ok(Flow::Next)
            }
            0b010000 => self.thumb16_alu(h, set_flags),
            0b010001 => self.thumb16_special(h),
            0b010010 | 0b010011 => {
                // LDR literal
                let (rt, imm) = (low3(8), (h & 0xFF) * 4);
                self.load_store(true, 4, false, rt, PC, imm, true, false)
            }
            0b010100..=0b010111 => {
                // load/store register offset
                let (op, rm, rn, rt) = ((h >> 9) & 7, low3(6), low3(3), low3(0));
                self.a.add_w(T0, rn, rm, shift::LSL, 0);
                let (load, size, signed) = match op {
                    0 => (false, 4, false),
                    1 => (false, 2, false),
                    2 => (false, 1, false),
                    3 => (true, 1, true),
                    4 => (true, 4, false),
                    5 => (true, 2, false),
                    6 => (true, 1, false),
                    _ => (true, 2, true),
                };
                self.access(load, size, signed, rt, T0);
                Ok(Flow::Next)
            }
            0b011000..=0b011111 => {
                // STR/LDR(B) imm5
                let (byte, load, imm, rn, rt) = ((h >> 12) & 1 == 1, (h >> 11) & 1 == 1, (h >> 6) & 0x1F, low3(3), low3(0));
                let (size, offset) = if byte { (1, imm) } else { (4, imm * 4) };
                self.load_store(load, size, false, rt, rn, offset, true, false)
            }
            0b100000..=0b100011 => {
                // STRH/LDRH imm5
                let (load, imm, rn, rt) = ((h >> 11) & 1 == 1, (h >> 6) & 0x1F, low3(3), low3(0));
                self.load_store(load, 2, false, rt, rn, imm * 2, true, false)
            }
            0b100100..=0b100111 => {
                // STR/LDR SP-relative
                let (load, rt, imm) = ((h >> 11) & 1 == 1, low3(8), (h & 0xFF) * 4);
                self.load_store(load, 4, false, rt, SP, imm, true, false)
            }
            0b101000 | 0b101001 => {
                // ADR
                let (rd, imm) = (low3(8), (h & 0xFF) * 4);
                self.a.mov_w_imm(rd, (self.pc_value() & !3) + imm);
                Ok(Flow::Next)
            }
            0b101010 | 0b101011 => {
                // ADD Rd, SP, #imm
                let (rd, imm) = (low3(8), (h & 0xFF) * 4);
                self.a.add_w_imm(rd, SP, imm);
                Ok(Flow::Next)
            }
            0b101100..=0b101111 => self.thumb16_misc(h),
            0b110000 | 0b110001 => {
                // STM Rn!, list
                let (rn, list) = (low3(8), h & 0xFF);
                self.load_store_multiple(false, rn, list, true, true)
            }
            0b110010 | 0b110011 => {
                // LDM Rn(!), list (writeback unless Rn is in the list)
                let (rn, list) = (low3(8), h & 0xFF);
                self.load_store_multiple(true, rn, list, true, list & (1 << rn) == 0)
            }
            0b110100..=0b110111 => {
                // B<c> / UDF / SVC
                let condition = (h >> 8) & 0xF;
                if condition >= 0xE {
                    return Err(());
                }
                let target = self.pc_value().wrapping_add(sign_extend((h & 0xFF) << 1, 9));
                Ok(self.branch(target, condition, true))
            }
            0b111000 | 0b111001 => {
                let target = self.pc_value().wrapping_add(sign_extend((h & 0x7FF) << 1, 12));
                Ok(self.branch(target, cond::AL, true))
            }
            _ => Err(()),
        }
    }

    fn thumb16_alu(&mut self, h: u32, set_flags: bool) -> Result<Flow> {
        let (op, rm, rdn) = ((h >> 6) & 0xF, (h >> 3) & 7, h & 7);
        match op {
            0x0 => {
                self.a.and_w(rdn, rdn, rm, shift::LSL, 0);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
            0x1 => {
                self.a.eor_w(rdn, rdn, rm, shift::LSL, 0);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
            0x2 | 0x3 | 0x4 | 0x7 => {
                let kind = match op {
                    0x2 => shift::LSL,
                    0x3 => shift::LSR,
                    0x4 => shift::ASR,
                    _ => shift::ROR,
                };
                self.shift_register(T6, rdn, rm, kind, set_flags);
                self.a.mov_w(rdn, T6);
                if set_flags {
                    // amount 0 keeps C: use the current C instead of the shifter's.
                    let keep = self.a.label();
                    let done = self.a.label();
                    self.a.cbnz_w_to(T4, keep);
                    self.set_nz_carry(rdn, T3);
                    self.a.b_to(done);
                    self.a.bind(keep);
                    self.set_nz(rdn);
                    self.a.bind(done);
                }
            }
            0x5 if set_flags => self.a.adcs_w(rdn, rdn, rm),
            0x5 => self.a.adc_w(rdn, rdn, rm),
            0x6 if set_flags => self.a.sbcs_w(rdn, rdn, rm),
            0x6 => self.a.sbc_w(rdn, rdn, rm),
            0x8 => {
                self.a.and_w(T6, rdn, rm, shift::LSL, 0);
                self.set_nz(T6);
            }
            0x9 if set_flags => self.a.subs_w(rdn, ZR, rm, shift::LSL, 0),
            0x9 => self.a.sub_w(rdn, ZR, rm, shift::LSL, 0),
            0xA => self.a.cmp_w(rdn, rm),
            0xB => self.a.adds_w(ZR, rdn, rm, shift::LSL, 0),
            0xC => {
                self.a.orr_w(rdn, rdn, rm, shift::LSL, 0);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
            0xD => {
                self.a.mul_w(rdn, rdn, rm);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
            0xE => {
                self.a.bic_w(rdn, rdn, rm, shift::LSL, 0);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
            _ => {
                self.a.mvn_w(rdn, rm);
                if set_flags {
                    self.set_nz(rdn);
                }
            }
        }
        Ok(Flow::Next)
    }

    fn thumb16_special(&mut self, h: u32) -> Result<Flow> {
        let op = (h >> 8) & 3;
        let rd = ((h >> 4) & 8) | (h & 7);
        let rm = (h >> 3) & 0xF;
        match op {
            0 => {
                // ADD Rd, Rm (no flags)
                let m = self.read(rm, T1);
                if rd == PC {
                    let pc = self.read(PC, T0);
                    self.a.add_w(T6, pc, m, shift::LSL, 0);
                    self.a.orr_w_imm(T6, T6, 1, T5);
                    emit_exit_to(&mut self.a, T6);
                    return Ok(Flow::End);
                }
                let d = self.read(rd, T0);
                self.a.add_w(rd, d, m, shift::LSL, 0);
                Ok(Flow::Next)
            }
            1 => {
                let n = self.read(rd, T0);
                let m = self.read(rm, T1);
                self.a.cmp_w(n, m);
                Ok(Flow::Next)
            }
            2 => {
                // MOV Rd, Rm (no flags)
                let m = self.read(rm, T1);
                if rd == PC {
                    self.a.orr_w_imm(T6, m, 1, T5);
                    emit_exit_to(&mut self.a, T6);
                    return Ok(Flow::End);
                }
                self.a.mov_w(rd, m);
                Ok(Flow::Next)
            }
            _ => {
                // BX / BLX Rm
                let link = (h >> 7) & 1 == 1;
                if rm == PC {
                    if link {
                        return Err(()); // unpredictable
                    }
                    // BX PC: switch to the ARM code right after this word-aligned pair
                    let target = self.pc_value() & !3;
                    emit_exit(&mut self.a, target, exit::JUMP);
                    return Ok(Flow::End);
                }
                self.a.mov_w(T6, rm);
                if link {
                    self.a.mov_w_imm(LR, self.next_pc() | 1);
                }
                emit_exit_to(&mut self.a, T6);
                Ok(Flow::End)
            }
        }
    }

    fn thumb16_misc(&mut self, h: u32) -> Result<Flow> {
        match (h >> 5) & 0x7F {
            0b0000000..=0b0000011 => {
                self.a.add_w_imm(SP, SP, (h & 0x7F) * 4);
                Ok(Flow::Next)
            }
            0b0000100..=0b0000111 => {
                self.a.sub_w_imm(SP, SP, (h & 0x7F) * 4);
                Ok(Flow::Next)
            }
            0b0001000..=0b0001111 | 0b0011000..=0b0011111 | 0b1001000..=0b1001111 | 0b1011000..=0b1011111 => {
                // CBZ / CBNZ
                let nonzero = (h >> 11) & 1 == 1;
                let rn = h & 7;
                let offset = (((h >> 9) & 1) << 6) | (((h >> 3) & 0x1F) << 1);
                let target = self.pc_value() + offset;
                let skip = self.a.label();
                if nonzero {
                    self.a.cbz_w_to(rn, skip);
                } else {
                    self.a.cbnz_w_to(rn, skip);
                }
                emit_exit(&mut self.a, target | 1, exit::JUMP);
                self.a.bind(skip);
                let next = self.next_pc();
            emit_exit(&mut self.a, next | 1, exit::JUMP);
                Ok(Flow::End)
            }
            0b0010000..=0b0010111 => {
                let (rm, rd) = ((h >> 3) & 7, h & 7);
                match (h >> 6) & 3 {
                    0 => self.a.sxth_w(rd, rm),
                    1 => self.a.sxtb_w(rd, rm),
                    2 => self.a.uxth_w(rd, rm),
                    _ => self.a.uxtb_w(rd, rm),
                }
                Ok(Flow::Next)
            }
            0b0100000..=0b0101111 => {
                // PUSH {list, lr?}
                let list = (h & 0xFF) | (((h >> 8) & 1) << LR);
                self.load_store_multiple(false, SP, list, false, true)
            }
            0b1010000..=0b1010011 | 0b1010110..=0b1010111 => {
                let (rm, rd) = ((h >> 3) & 7, h & 7);
                match (h >> 6) & 3 {
                    0 => self.a.rev_w(rd, rm),
                    1 => self.a.rev16_w(rd, rm),
                    3 => {
                        // REVSH
                        self.a.rev16_w(rd, rm);
                        self.a.sxth_w(rd, rd);
                    }
                    _ => return Err(()),
                }
                Ok(Flow::Next)
            }
            0b1100000..=0b1101111 => {
                // POP {list, pc?}
                let list = (h & 0xFF) | (((h >> 8) & 1) << PC);
                self.load_store_multiple(true, SP, list, true, true)
            }
            0b1111000..=0b1111111 => {
                // hints (IT is handled earlier): NOP, YIELD, WFE, WFI, SEV
                if h & 0xF == 0 {
                    Ok(Flow::Next)
                } else {
                    Err(())
                }
            }
            _ => Err(()),
        }
    }

    fn thumb32(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op1 = (h1 >> 11) & 3;
        let op2 = (h1 >> 4) & 0x7F;
        match op1 {
            0b01 => {
                if op2 & 0b1100100 == 0b0000000 {
                    self.load_store_multiple32(h1, h2)
                } else if op2 & 0b1100100 == 0b0000100 {
                    self.load_store_dual(h1, h2)
                } else if op2 & 0b1100000 == 0b0100000 {
                    self.data_shifted_register(h1, h2)
                } else {
                    self.coprocessor(h1, h2)
                }
            }
            0b10 => {
                if h2 & 0x8000 == 0 {
                    if op2 & 0b0100000 == 0 {
                        self.data_modified_immediate(h1, h2)
                    } else {
                        self.data_plain_immediate(h1, h2)
                    }
                } else {
                    self.branch_misc(h1, h2)
                }
            }
            _ => {
                if op2 & 0b1110001 == 0b0000000 {
                    self.store_single(h1, h2)
                } else if op2 & 0b1100111 == 0b0000001 || op2 & 0b1100111 == 0b0000011 || op2 & 0b1100111 == 0b0000101 {
                    self.load_single(h1, h2)
                } else if op2 & 0b1110000 == 0b0100000 {
                    self.data_register(h1, h2)
                } else if op2 & 0b1111000 == 0b0110000 {
                    self.multiply(h1, h2)
                } else if op2 & 0b1111000 == 0b0111000 {
                    self.long_multiply(h1, h2)
                } else if op2 & 0b1110001 == 0b0010000 {
                    // Advanced SIMD loads and stores: 1111 1001 is ARM's 1111 0100
                    self.neon_load_store(0xF400_0000 | ((h1 & 0xFF) << 16) | h2)
                } else if op2 & 0b1000000 == 0b1000000 {
                    self.coprocessor(h1, h2)
                } else {
                    Err(())
                }
            }
        }
    }

    fn load_store_multiple32(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op = (h1 >> 7) & 3;
        let (writeback, load, rn) = ((h1 >> 5) & 1 == 1, (h1 >> 4) & 1 == 1, h1 & 0xF);
        let list = h2 & 0xDFFF;
        match op {
            0b01 => self.load_store_multiple(load, rn, list, true, writeback),
            0b10 => self.load_store_multiple(load, rn, list, false, writeback),
            _ => Err(()),
        }
    }

    fn load_store_dual(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let (p, u, w, l, rn) = ((h1 >> 8) & 1, (h1 >> 7) & 1, (h1 >> 5) & 1, (h1 >> 4) & 1, h1 & 0xF);
        let (rt, rt2, imm8) = ((h2 >> 12) & 0xF, (h2 >> 8) & 0xF, h2 & 0xFF);
        if p == 0 && w == 0 {
            // exclusive accesses and table branches
            return self.exclusive_or_table(h1, h2);
        }
        if rt == PC || rt2 == PC || rn == PC && w == 1 {
            return Err(());
        }
        let offset = if u == 1 { imm8 * 4 } else { (imm8 * 4).wrapping_neg() };
        let base = if rn == PC {
            self.a.mov_w_imm(T0, (self.pc_value() & !3).wrapping_add(if p == 1 { offset } else { 0 }));
            T0
        } else if p == 1 {
            self.a.add_w_any(T0, rn, offset, T1);
            T0
        } else {
            rn
        };
        self.a.add_x_uxtw(T1, host::MEMORY, base);
        if l == 1 {
            if rt == rt2 || (w == 1 && (rt == rn || rt2 == rn)) {
                return Err(());
            }
            if rt == rn {
                self.a.ldr_w_off(T2, T1, 0);
                self.a.ldr_w_off(rt2, T1, 4);
                self.a.mov_w(rt, T2);
            } else {
                self.a.ldp_w_off(rt, rt2, T1, 0);
            }
        } else {
            self.a.stp_w_off(rt, rt2, T1, 0);
        }
        if w == 1 {
            self.a.add_w_any(rn, rn, offset, T2);
        }
        Ok(Flow::Next)
    }

    fn exclusive_or_table(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let rn = h1 & 0xF;
        let op = (h1 >> 4) & 0x1F; // bits 8:4 of h1 = 0 U 1 0 L with P=W=0
        let (rt, rd) = ((h2 >> 12) & 0xF, (h2 >> 8) & 0xF);
        // Only one guest thread runs at a time, so exclusives are plain accesses
        // and a store-exclusive always succeeds.
        match op {
            0b00100 => {
                // STREX Rd, Rt, [Rn, #imm8*4]
                if rn == PC {
                    return Err(());
                }
                let offset = (h2 & 0xFF) * 4;
                self.a.add_w_any(T0, rn, offset, T1);
                self.a.str_w_uxtw(rt, host::MEMORY, T0);
                self.a.mov_w_imm(rd, 0);
                Ok(Flow::Next)
            }
            0b00101 => {
                if rn == PC || rt == PC {
                    return Err(());
                }
                let offset = (h2 & 0xFF) * 4;
                self.a.add_w_any(T0, rn, offset, T1);
                self.a.ldr_w_uxtw(rt, host::MEMORY, T0);
                Ok(Flow::Next)
            }
            0b01101 => {
                let kind = (h2 >> 4) & 0xF;
                let rm = h2 & 0xF;
                match kind {
                    0 | 1 => {
                        // TBB / TBH [Rn, Rm(, lsl #1)]
                        let n = self.read(rn, T1);
                        if kind == 0 {
                            self.a.add_w(T0, n, rm, shift::LSL, 0);
                            self.a.ldrb_w_uxtw(T2, host::MEMORY, T0);
                        } else {
                            self.a.add_w(T0, n, rm, shift::LSL, 1);
                            self.a.ldrh_w_uxtw(T2, host::MEMORY, T0);
                        }
                        self.a.mov_w_imm(T6, self.pc_value() | 1);
                        self.a.add_w(T6, T6, T2, shift::LSL, 1);
                        emit_exit_to(&mut self.a, T6);
                        Ok(Flow::End)
                    }
                    4 | 5 | 7 => {
                        // LDREXB / LDREXH / LDREXD
                        if rn == PC {
                            return Err(());
                        }
                        match kind {
                            4 => self.a.ldrb_w_uxtw(rt, host::MEMORY, rn),
                            5 => self.a.ldrh_w_uxtw(rt, host::MEMORY, rn),
                            _ => {
                                self.a.add_x_uxtw(T1, host::MEMORY, rn);
                                self.a.ldp_w_off(rt, rd, T1, 0);
                            }
                        }
                        Ok(Flow::Next)
                    }
                    _ => Err(()),
                }
            }
            0b01100 => {
                let kind = (h2 >> 4) & 0xF;
                let rd = h2 & 0xF;
                let rt2 = (h2 >> 8) & 0xF;
                if rn == PC {
                    return Err(());
                }
                match kind {
                    4 => self.a.strb_w_uxtw(rt, host::MEMORY, rn),
                    5 => self.a.strh_w_uxtw(rt, host::MEMORY, rn),
                    7 => {
                        self.a.add_x_uxtw(T1, host::MEMORY, rn);
                        self.a.stp_w_off(rt, rt2, T1, 0);
                    }
                    _ => return Err(()),
                }
                self.a.mov_w_imm(rd, 0);
                Ok(Flow::Next)
            }
            _ => Err(()),
        }
    }

    fn data_shifted_register(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op = (h1 >> 5) & 0xF;
        let set_flags = (h1 >> 4) & 1 == 1;
        let (rn, rd, rm) = (h1 & 0xF, (h2 >> 8) & 0xF, h2 & 0xF);
        let amount = (((h2 >> 12) & 7) << 2) | ((h2 >> 6) & 3);
        let kind = (h2 >> 4) & 3;
        if op == 0b0110 {
            // PKHBT / PKHTB
            return Err(());
        }
        if rm == PC || rd == PC && !(set_flags && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101)) {
            return Err(());
        }
        let logical = matches!(op, 0b0000..=0b0100);
        let carry = self.shift_immediate(T2, rm, kind, amount, set_flags && logical);
        let carry = if carry && set_flags && logical { Some(T3) } else { None };
        self.data_processing(op, set_flags, rd, rn, T2, carry)
    }

    fn data_modified_immediate(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op = (h1 >> 5) & 0xF;
        let set_flags = (h1 >> 4) & 1 == 1;
        let (rn, rd) = (h1 & 0xF, (h2 >> 8) & 0xF);
        let imm12 = (((h1 >> 10) & 1) << 11) | (((h2 >> 12) & 7) << 8) | (h2 & 0xFF);
        let (value, carry) = expand_immediate(imm12);
        if rd == PC && !(set_flags && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101)) {
            return Err(());
        }
        self.a.mov_w_imm(T2, value);
        let logical = matches!(op, 0b0000..=0b0100);
        let carry = match carry {
            Some(c) if set_flags && logical => {
                self.a.mov_w_imm(T3, c as u32);
                Some(T3)
            }
            _ => None,
        };
        self.data_processing(op, set_flags, rd, rn, T2, carry)
    }

    fn data_plain_immediate(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op = (h1 >> 4) & 0x1F;
        let (rn, rd) = (h1 & 0xF, (h2 >> 8) & 0xF);
        let imm12 = (((h1 >> 10) & 1) << 11) | (((h2 >> 12) & 7) << 8) | (h2 & 0xFF);
        if rd == PC {
            return Err(());
        }
        match op {
            0b00000 => {
                // ADDW / ADR
                if rn == PC {
                    self.a.mov_w_imm(rd, (self.pc_value() & !3).wrapping_add(imm12));
                } else {
                    self.a.add_w_any(rd, rn, imm12, T1);
                }
            }
            0b01010 => {
                // SUBW / ADR (subtract)
                if rn == PC {
                    self.a.mov_w_imm(rd, (self.pc_value() & !3).wrapping_sub(imm12));
                } else {
                    self.a.add_w_any(rd, rn, imm12.wrapping_neg(), T1);
                }
            }
            0b00100 => {
                // MOVW
                let imm16 = ((h1 & 0xF) << 12) | imm12;
                self.a.mov_w_imm(rd, imm16);
            }
            0b01100 => {
                // MOVT
                let imm16 = ((h1 & 0xF) << 12) | imm12;
                self.a.emit(0x72A0_0000 | (imm16 << 5) | rd); // movk wd, #imm16, lsl 16
            }
            0b10100 | 0b11100 => {
                // SBFX / UBFX
                let lsb = (((h2 >> 12) & 7) << 2) | ((h2 >> 6) & 3);
                let width = (h2 & 0x1F) + 1;
                if rn == PC || lsb + width > 32 {
                    return Err(());
                }
                if op == 0b10100 {
                    self.a.sbfx_w(rd, rn, lsb, width);
                } else {
                    self.a.ubfx_w(rd, rn, lsb, width);
                }
            }
            0b10110 => {
                // BFI / BFC
                let lsb = (((h2 >> 12) & 7) << 2) | ((h2 >> 6) & 3);
                let msb = h2 & 0x1F;
                if msb < lsb {
                    return Err(());
                }
                let width = msb - lsb + 1;
                let source = if rn == PC { ZR } else { rn };
                self.a.bfi_w(rd, source, lsb, width);
            }
            _ => return Err(()),
        }
        Ok(Flow::Next)
    }

    fn branch_misc(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let s_bit = (h1 >> 10) & 1;
        let (j1, j2) = ((h2 >> 13) & 1, (h2 >> 11) & 1);
        // h2 bits 14 and 12 pick the form.
        match ((h2 >> 14) & 1, (h2 >> 12) & 1) {
            (0, 0) => {
                let condition = (h1 >> 6) & 0xF;
                if condition >= 0xE {
                    return self.misc_control(h1, h2);
                }
                let imm = (s_bit << 20) | (j2 << 19) | (j1 << 18) | ((h1 & 0x3F) << 12) | ((h2 & 0x7FF) << 1);
                let target = self.pc_value().wrapping_add(sign_extend(imm, 21));
                Ok(self.branch(target, condition, true))
            }
            (bit14, bit12) => {
                let i1 = (!(j1 ^ s_bit)) & 1;
                let i2 = (!(j2 ^ s_bit)) & 1;
                let imm = (s_bit << 24) | (i1 << 23) | (i2 << 22) | ((h1 & 0x3FF) << 12) | ((h2 & 0x7FF) << 1);
                let offset = sign_extend(imm, 25);
                match (bit14, bit12) {
                    (0, _) => Ok(self.branch(self.pc_value().wrapping_add(offset), cond::AL, true)),
                    (_, 1) => {
                        // BL
                        self.a.mov_w_imm(LR, self.next_pc() | 1);
                        Ok(self.branch(self.pc_value().wrapping_add(offset), cond::AL, true))
                    }
                    _ => {
                        // BLX to ARM code
                        self.a.mov_w_imm(LR, self.next_pc() | 1);
                        let target = (self.pc_value() & !3).wrapping_add(offset) & !3;
                        Ok(self.branch(target, cond::AL, false))
                    }
                }
            }
        }
    }

    fn misc_control(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        // Barriers (DMB/DSB/ISB) and NOP hints are no-ops here: one guest thread runs at a time.
        if h1 == 0xF3BF && h2 & 0xFF00 == 0x8F00 {
            return Ok(Flow::Next);
        }
        if h1 == 0xF3AF && h2 == 0x8000 {
            return Ok(Flow::Next);
        }
        Err(())
    }

    fn store_single(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let size = match (h1 >> 5) & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err(()),
        };
        self.single(false, size, false, h1, h2)
    }

    fn load_single(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let size = match (h1 >> 5) & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err(()),
        };
        let signed = (h1 >> 8) & 1 == 1;
        let rt = (h2 >> 12) & 0xF;
        if rt == PC && size != 4 {
            // PLD / PLI and unallocated hints
            return if size == 1 || size == 2 { Ok(Flow::Next) } else { Err(()) };
        }
        self.single(true, size, signed, h1, h2)
    }

    fn single(&mut self, load: bool, size: u32, signed: bool, h1: u32, h2: u32) -> Result<Flow> {
        let rn = h1 & 0xF;
        let rt = (h2 >> 12) & 0xF;
        if rn == PC {
            if !load {
                return Err(());
            }
            let up = (h1 >> 7) & 1 == 1;
            let imm = h2 & 0xFFF;
            let offset = if up { imm } else { imm.wrapping_neg() };
            return self.load_store(true, size, signed, rt, PC, offset, true, false);
        }
        if (h1 >> 7) & 1 == 1 {
            // imm12
            return self.load_store(load, size, signed, rt, rn, h2 & 0xFFF, true, false);
        }
        if (h2 >> 11) & 1 == 1 {
            // imm8 with P/U/W (P=0,W=0 is undefined; 1110 = unprivileged = plain)
            let (p, u, w) = ((h2 >> 10) & 1, (h2 >> 9) & 1, (h2 >> 8) & 1);
            if p == 0 && w == 0 {
                return Err(());
            }
            let imm = h2 & 0xFF;
            let offset = if u == 1 { imm } else { imm.wrapping_neg() };
            return self.load_store(load, size, signed, rt, rn, offset, p == 1, w == 1);
        }
        if (h2 >> 6) & 0x3F == 0 {
            // register offset: [Rn, Rm, LSL #imm2]
            let (rm, amount) = (h2 & 0xF, (h2 >> 4) & 3);
            if rm == PC || rm == SP {
                return Err(());
            }
            self.a.add_w(T0, rn, rm, shift::LSL, amount);
            if !load && rt == PC {
                return Err(());
            }
            let target = if rt == PC { T6 } else { rt };
            self.access(load, size, signed, target, T0);
            if rt == PC {
                if size != 4 {
                    return Err(());
                }
                emit_exit_to(&mut self.a, T6);
                return Ok(Flow::End);
            }
            return Ok(Flow::Next);
        }
        Err(())
    }

    fn data_register(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op1 = (h1 >> 4) & 0xF;
        let op2 = (h2 >> 4) & 0xF;
        let (rn, rd, rm) = (h1 & 0xF, (h2 >> 8) & 0xF, h2 & 0xF);
        if h2 & 0xF000 != 0xF000 || rd == PC || rm == PC {
            return Err(());
        }
        if op1 & 0b1000 == 0 && op2 == 0 {
            // LSL/LSR/ASR/ROR(S) Rd, Rn, Rm
            let kind = (op1 >> 1) & 3;
            let set_flags = op1 & 1 == 1;
            if rn == PC {
                return Err(());
            }
            self.shift_register(T6, rn, rm, kind, set_flags);
            self.a.mov_w(rd, T6);
            if set_flags {
                let keep = self.a.label();
                let done = self.a.label();
                self.a.cbnz_w_to(T4, keep);
                self.set_nz_carry(rd, T3);
                self.a.b_to(done);
                self.a.bind(keep);
                self.set_nz(rd);
                self.a.bind(done);
            }
            return Ok(Flow::Next);
        }
        if op1 & 0b1000 == 0 && op2 & 0b1000 == 0b1000 {
            // extends (with optional add), rotation 0/8/16/24
            let rotation = ((h2 >> 4) & 3) * 8;
            if rotation == 0 {
                self.a.mov_w(T2, rm);
            } else {
                self.a.ror_w_imm(T2, rm, rotation);
            }
            match op1 {
                0b0000 => self.a.sxth_w(T2, T2),
                0b0001 => self.a.uxth_w(T2, T2),
                0b0100 => self.a.sxtb_w(T2, T2),
                0b0101 => self.a.uxtb_w(T2, T2),
                _ => return Err(()),
            }
            if rn == PC {
                self.a.mov_w(rd, T2);
            } else {
                self.a.add_w(rd, rn, T2, shift::LSL, 0);
            }
            return Ok(Flow::Next);
        }
        if op1 & 0b1100 == 0b1000 && op2 & 0b1100 == 0b1000 {
            // misc: REV, REV16, RBIT, REVSH, CLZ (Rn must equal Rm)
            match (op1 & 3, op2 & 3) {
                (0b01, 0b00) => self.a.rev_w(rd, rm),
                (0b01, 0b01) => self.a.rev16_w(rd, rm),
                (0b01, 0b10) => self.a.rbit_w(rd, rm),
                (0b01, 0b11) => {
                    self.a.rev16_w(rd, rm);
                    self.a.sxth_w(rd, rd);
                }
                (0b11, 0b00) => self.a.clz_w(rd, rm),
                _ => return Err(()),
            }
            return Ok(Flow::Next);
        }
        Err(())
    }

    fn multiply(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op1 = (h1 >> 4) & 7;
        let op2 = (h2 >> 4) & 3;
        let (rn, ra, rd, rm) = (h1 & 0xF, (h2 >> 12) & 0xF, (h2 >> 8) & 0xF, h2 & 0xF);
        if (h2 >> 6) & 3 != 0 || rd == PC || rn == PC || rm == PC {
            return Err(());
        }
        match (op1, op2) {
            (0, 0) => {
                if ra == PC {
                    self.a.mul_w(rd, rn, rm);
                } else {
                    self.a.madd_w(rd, rn, rm, ra);
                }
            }
            (0, 1) => {
                if ra == PC {
                    return Err(());
                }
                self.a.msub_w(rd, rn, rm, ra);
            }
            _ => return Err(()),
        }
        Ok(Flow::Next)
    }

    fn long_multiply(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        let op1 = (h1 >> 4) & 7;
        let op2 = (h2 >> 4) & 0xF;
        let (rn, rd_lo, rd_hi, rm) = (h1 & 0xF, (h2 >> 12) & 0xF, (h2 >> 8) & 0xF, h2 & 0xF);
        if rn == PC || rm == PC || rd_hi == PC {
            return Err(());
        }
        match (op1, op2) {
            (0b001, 0b1111) | (0b011, 0b1111) => {
                // SDIV / UDIV Rd(=rd_hi), Rn, Rm (division by zero gives 0 on both)
                if op1 == 0b001 {
                    self.a.sdiv_w(rd_hi, rn, rm);
                } else {
                    self.a.udiv_w(rd_hi, rn, rm);
                }
                Ok(Flow::Next)
            }
            (0b000, 0) | (0b010, 0) | (0b100, 0) | (0b110, 0) => {
                // SMULL / UMULL / SMLAL / UMLAL
                if rd_lo == PC || rd_lo == rd_hi {
                    return Err(());
                }
                self.multiply_long(op1 == 0b000 || op1 == 0b100, op1 >= 0b100, rd_lo, rd_hi, rn, rm);
                Ok(Flow::Next)
            }
            _ => Err(()),
        }
    }

    /// rd_hi:rd_lo = rn * rm (+ rd_hi:rd_lo when accumulating), 64-bit.
    fn multiply_long(&mut self, signed: bool, accumulate: bool, rd_lo: u32, rd_hi: u32, rn: u32, rm: u32) {
        if accumulate {
            // T4 = rd_hi:rd_lo
            self.a.mov_w(T4, rd_lo);
            self.a.orr_x(T4, T4, rd_hi, shift::LSL, 32);
        } else {
            self.a.mov_w_imm(T4, 0);
        }
        if signed {
            self.a.smaddl(T4, rn, rm, T4);
        } else {
            self.a.umaddl(T4, rn, rm, T4);
        }
        self.a.mov_w(rd_lo, T4);
        self.a.lsr_x_imm(T4, T4, 32);
        self.a.mov_w(rd_hi, T4);
    }

    fn coprocessor(&mut self, h1: u32, h2: u32) -> Result<Flow> {
        // MRC p15, 0, Rt, c13, c0, 3: read TPIDRURO (bionic's thread pointer).
        if h1 == 0xEE1D && h2 & 0x0FFF == 0x0F70 {
            let rt = (h2 >> 12) & 0xF;
            if rt == PC {
                return Err(());
            }
            self.a.ldr_w_off(rt, host::STATE, offsets::TLS);
            return Ok(Flow::Next);
        }
        if h1 & 0xEF00 == 0xEF00 {
            // Advanced SIMD data processing: 111U 1111 is ARM's 1111 001U
            return self.neon_data(0xF200_0000 | (((h1 >> 12) & 1) << 24) | ((h1 & 0xFF) << 16) | h2);
        }
        if h1 & 0xFC00 == 0xEC00 {
            // VFP: the ARM encoding with condition 1110
            return self.vfp((h1 << 16) | h2);
        }
        Err(())
    }
}
