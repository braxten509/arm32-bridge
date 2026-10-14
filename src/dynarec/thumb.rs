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
pub use arm::{translate as translate_arm, translate_copy as translate_arm_copy};

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
    if reason == exit::JUMP && !a.no_link {
        // While another guest thread waits for its turn, leave through the
        // dispatcher instead of jumping straight on (runtime::PREEMPT).
        let unlinked = a.label();
        restore_vfp_mode(a);
        preempt_check(a, unlinked);
        // Becomes `b <block>` once the block at `pc` is translated (engine.rs).
        a.links.push((a.len(), pc));
        a.nop();
        a.bind(unlinked);
    }
    a.mov_w_imm(host::EXIT_PC, pc);
    a.mov_w_imm(host::EXIT_REASON, reason);
    a.br(host::EXIT_STUB);
}

/// Puts host FPCR back in the guest's VFP mode before a linked exit from NEON mode (uses T0, T1).
fn restore_vfp_mode(a: &mut Asm) {
    if a.vfp_restore {
        a.ldr_w_off(T0, host::STATE, offsets::FPSCR);
        a.and_w_imm(T0, T0, super::FPSCR_CONTROL, T1);
        a.msr_fpcr(T0);
    }
}

/// Branches to `to` when the shared preempt flag is set (uses T0; flags untouched).
fn preempt_check(a: &mut Asm, to: super::emit::Label) {
    a.ldr_x_off(T0, host::STATE, offsets::PREEMPT_FLAG);
    a.ldr_w_off(T0, T0, 0);
    a.cbnz_w_to(T0, to);
}

/// Ends a block: continue at the address in `target` (bit 0 = Thumb; never
/// T0-T2). With
/// linking on, the jump table (engine.rs) is tried first; a miss goes to the
/// dispatcher, which fills the table.
fn emit_exit_to(a: &mut Asm, target: Reg) {
    let table = super::engine::jump_table();
    if table != 0 && !a.no_link {
        let miss = a.label();
        restore_vfp_mode(a);
        preempt_check(a, miss);
        // One 64-bit entry: (code - table) << 32 | guest pc; the table is at the start of code memory.
        a.mov_x_imm(T0, table as u64);
        a.ubfx_w(T1, target, 1, super::engine::JUMP_TABLE_BITS);
        a.add_x_lsl(T1, T0, T1, 3);
        a.ldr_x_off(T1, T1, 0);
        a.eor_w(T2, T1, target, shift::LSL, 0); // no compare: the guest flags live in NZCV
        a.cbnz_w_to(T2, miss);
        a.add_x_asr(T0, T0, T1, 32);
        a.br(T0);
        a.bind(miss);
    }
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
    /// Instructions are read from this copy of guest memory from `copy_start`
    /// on (see `translate_copy`); `read_outside` notes a read past it.
    copy: Vec<u8>,
    copy_start: u32,
    read_outside: bool,
}

impl Translator {
    fn new(mem: GuestMem, start: u32, thumb: bool, copy: Vec<u8>) -> Translator {
        let size = if thumb { 2 } else { 4 };
        Translator { a: Asm::new(), mem, pc: start, size, count: 0, in_it: false, thumb, neon_fp: false, copy, copy_start: start, read_outside: false }
    }

    fn copied(&self, address: u32, length: usize) -> Option<&[u8]> {
        let offset = address.wrapping_sub(self.copy_start) as usize;
        self.copy.get(offset..offset.checked_add(length)?)
    }

    /// An instruction halfword or word (from the copy when it holds it).
    fn fetch_u16(&mut self, address: u32) -> u16 {
        match self.copied(address, 2) {
            Some(bytes) => u16::from_le_bytes([bytes[0], bytes[1]]),
            None => {
                self.read_outside = true;
                self.mem.read_u16(address)
            }
        }
    }

    pub(super) fn fetch_u32(&mut self, address: u32) -> u32 {
        match self.copied(address, 4) {
            Some(bytes) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            None => {
                self.read_outside = true;
                self.mem.read_u32(address)
            }
        }
    }
}

/// A translated block: code, the guest address just past its last
/// instruction, and its exits to fixed guest addresses (see `Asm::links`).
pub struct Block {
    pub code: Vec<u32>,
    pub end: u32,
    pub links: Vec<(usize, u32)>,
}

/// Translates the block starting at `start` (a Thumb address without bit 0).
pub fn translate(mem: GuestMem, start: u32) -> Block {
    translate_copy(mem, start, Vec::new()).0
}

/// Like `translate`, reading instructions from `copy` (guest bytes from
/// `start` on) instead of guest memory, so the block is made from exactly
/// those bytes even if another thread rewrites them meanwhile. Also returns
/// the copy and whether an instruction was read past its end.
pub fn translate_copy(mem: GuestMem, start: u32, copy: Vec<u8>) -> (Block, Vec<u8>, bool) {
    let mut t = Translator::new(mem, start, true, copy);
    if let Some(word) = fence_prefix() {
        t.a.emit(word);
    }
    let end;
    loop {
        if t.count >= MAX_INSTRUCTIONS {
            emit_exit(&mut t.a, t.pc | 1, exit::JUMP);
            end = t.pc;
            break;
        }
        let checkpoint = t.a.checkpoint();
        let first = t.fetch_u16(t.pc) as u32;
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
    (Block { code: t.a.code, end, links: t.a.links }, t.copy, t.read_outside)
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
                    // 0, but computed from rm: a constant would drop the data
                    // dependency native code keeps (a load addressed with it stays ordered).
                    // Two W shifts, so the upper half of the host register doesn't matter.
                    a.lsr_w_imm(result, rm, 31);
                    a.lsr_w_imm(result, result, 1);
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
        // T2 holds the amount until the end (the carry check needs it).
        assert!(result != T2, "shift result would overwrite the amount");
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
                // 0, but computed from the shifted value (keeps its data dependency).
                if carry_out {
                    a.eor_w(T3, T5, T5, shift::LSL, 0);
                }
                a.eor_w(result, T5, T5, shift::LSL, 0);
            }
            shift::ASR => {
                // Amounts of 32+ fill with the sign; carry = bit 31.
                a.sxtw_x(T6, rm);
                // asrv uses the amount mod 64; any amount from 32 up fills with
                // the sign. Amounts 64-255 become 61-63 by arithmetic, not a
                // branch or a flag test (the guest flags live in NZCV), so the
                // amount stays a data dependency: T2 |= (0 - (T2 >> 6)) & 63.
                a.lsr_w_imm(T4, T2, 6);
                a.sub_w(T4, ZR, T4, shift::LSL, 0);
                a.and_w_imm(T4, T4, 63, T5);
                a.orr_w(T2, T2, T4, shift::LSL, 0);
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
            if load && size == 4 && crate::mem::fixed(address) && crate::mem::fixed(address + 3) {
                // A read-only library segment never changes: load it at translation time.
                // Code the game made while running (a JIT, or code in a library's .data)
                // may patch its literals in place (a call target, say) and flush just
                // those 4 bytes, which need not touch this block's range, so there the
                // load stays a real load (below).
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
            if rt == PC {
                emit_exit_to(&mut self.a, T6);
                return Ok(Flow::End);
            }
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
        if count == 0 || rn == PC || (list & (1 << SP) != 0 && rn != SP && load) {
            return Err(());
        }
        if !load && list & (1 << PC) != 0 {
            // Only ARM code can store PC (Mono's trampolines push it); it reads as the instruction + 8.
            if self.thumb {
                return Err(());
            }
            let value = self.pc_value();
            self.a.mov_w_imm(T6, value);
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
                a.str_w_off(if r == PC { T6 } else { r }, T1, offset);
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
            let first_half = self.fetch_u16(self.pc) as u32;
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
        let first = self.fetch_u16(self.pc) as u32;
        if first >> 11 >= 0b11101 {
            self.size = 4;
            let second = self.fetch_u16(self.pc + 2) as u32;
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
                if condition == 0xF && !self.in_it {
                    // SVC: a Linux system call (the dispatcher runs it).
                    let next = self.next_pc() | 1;
                    emit_exit(&mut self.a, next, exit::SYSCALL);
                    return Ok(Flow::End);
                }
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
                if (h == 0xBF10 || h == 0xBF20) && !self.in_it {
                    let next = self.next_pc() | 1;
                    emit_exit(&mut self.a, next, exit::YIELD);
                    Ok(Flow::End)
                } else if h & 0xF == 0 {
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
            // One LDP even when rt is the base register (the address is in T1): an
            // 8-byte-aligned LDRD is one single-copy-atomic read on LPAE cores, and so is
            // this LDP on hosts with FEAT_LSE2 (every ARMv8.4+ core, the Pixel 10's too).
            // Two separate loads let another core's write land between them.
            self.a.ldp_w_off(rt, rt2, T1, 0);
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
        match op {
            // STREX Rd, Rt, [Rn, #imm8*4]
            0b00100 => self.store_exclusive(2, rd, rt, 0, rn, (h2 & 0xFF) * 4),
            // LDREX Rt, [Rn, #imm8*4]
            0b00101 => self.load_exclusive(2, rt, 0, rn, (h2 & 0xFF) * 4),
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
                    // LDREXB / LDREXH / LDREXD Rt, Rt2 (bits 11-8), [Rn]
                    4 => self.load_exclusive(0, rt, 0, rn, 0),
                    5 => self.load_exclusive(1, rt, 0, rn, 0),
                    7 => self.load_exclusive(3, rt, rd, rn, 0),
                    _ => Err(()),
                }
            }
            0b01100 => {
                // STREXB / STREXH / STREXD Rd (bits 3-0), Rt, Rt2 (bits 11-8), [Rn]
                let (rd, rt2) = (h2 & 0xF, (h2 >> 8) & 0xF);
                match (h2 >> 4) & 0xF {
                    4 => self.store_exclusive(0, rd, rt, 0, rn, 0),
                    5 => self.store_exclusive(1, rd, rt, 0, rn, 0),
                    7 => self.store_exclusive(3, rd, rt, rt2, rn, 0),
                    _ => Err(()),
                }
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
            // PKHBT / PKHTB (tb = bit 5)
            if set_flags || kind & 1 != 0 || rd == PC || rn == PC || rm == PC {
                return Err(());
            }
            self.pack_halfwords(rd, rn, rm, kind >> 1 == 1, amount);
            return Ok(Flow::Next);
        }
        if rm == PC || rd == PC && !(set_flags && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101)) {
            return Err(());
        }
        let logical = matches!(op, 0b0000..=0b0100);
        let carry = self.shift_immediate(T2, rm, kind, amount, set_flags && logical);
        let carry = if carry && set_flags && logical { Some(T3) } else { None };
        self.data_processing(op, set_flags, rd, rn, T2, carry)
    }

    /// PKHBT: rd = rn bottom | (rm << amount) top; PKHTB: rd = rn top | (rm asr amount) bottom
    /// (an amount of 0 means 32 for PKHTB).
    pub(super) fn pack_halfwords(&mut self, rd: u32, rn: u32, rm: u32, top_bottom: bool, amount: u32) {
        let a = &mut self.a;
        if top_bottom {
            a.asr_w_imm(T4, rm, if amount == 0 { 31 } else { amount });
            a.mov_w(T5, rn);
            a.bfi_w(T5, T4, 0, 16);
            a.mov_w(rd, T5);
        } else {
            a.lsl_w_imm(T4, rm, amount);
            a.bfi_w(T4, rn, 0, 16);
            a.mov_w(rd, T4);
        }
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
            0b10000 | 0b10010 | 0b11000 | 0b11010 => {
                // SSAT / USAT Rd, #sat, Rn{, LSL/ASR #imm} (sh = bit 1 of op); ASR #0 is the
                // halfword form SSAT16 / USAT16
                let signed = op & 0b01000 == 0;
                let amount = (((h2 >> 12) & 7) << 2) | ((h2 >> 6) & 3);
                let kind = if op & 0b10 != 0 { shift::ASR } else { shift::LSL };
                if kind == shift::ASR && amount == 0 {
                    let bits = (h2 & 0xF) + signed as u32;
                    return self.saturate_halves_instruction(rd, rn, signed, bits);
                }
                let bits = (h2 & 0x1F) + signed as u32;
                return self.saturate_instruction(rd, rn, signed, bits, kind, amount);
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
        if h1 == 0xF3BF && h2 & 0xFFF0 == 0x8F20 {
            self.clear_exclusive();
            return Ok(Flow::Next);
        }
        if h1 == 0xF3BF && h2 & 0xFF00 == 0x8F00 && matches!((h2 >> 4) & 0xF, 4..=6) {
            self.barrier((h2 >> 4) & 0xF);
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
            (0b101, 0) | (0b101, 1) => {
                // SMMUL / SMMLA (R: rounded)
                let accumulate = (ra != PC).then_some(ra);
                self.most_significant_multiply(rd, rn, rm, accumulate, op2 == 1);
            }
            (0b001, _) => {
                // SMUL<x><y> / SMLA<x><y>: N (bit 5) picks Rn's top half, M (bit 4) Rm's.
                let accumulate = (ra != PC).then_some(ra);
                self.halfword_multiply(rd, rn, op2 >> 1 == 1, rm, op2 & 1 == 1, accumulate);
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
            (0b110, 0b0110) => {
                // UMAAL
                if rd_lo == PC || rd_lo == rd_hi {
                    return Err(());
                }
                self.multiply_accumulate_accumulate(rd_lo, rd_hi, rn, rm);
                Ok(Flow::Next)
            }
            _ => Err(()),
        }
    }

    /// rd = (16-bit half of rn) * (16-bit half of rm) (+ ra, setting Q on signed overflow).
    /// Computed without touching the host flags, which hold the guest's.
    pub(super) fn halfword_multiply(&mut self, rd: u32, rn: u32, n_top: bool, rm: u32, m_top: bool, ra: Option<u32>) {
        let a = &mut self.a;
        for (target, source, top) in [(T4, rn, n_top), (T5, rm, m_top)] {
            if top {
                a.asr_w_imm(target, source, 16);
            } else {
                a.sxth_w(target, source);
            }
        }
        a.mul_w(T4, T4, T5);
        let Some(ra) = ra else {
            a.mov_w(rd, T4);
            return;
        };
        a.add_w(T5, T4, ra, shift::LSL, 0);
        // Overflow: both inputs differ in sign from the sum.
        a.eor_w(T6, T4, T5, shift::LSL, 0);
        a.eor_w(T0, ra, T5, shift::LSL, 0);
        a.and_w(T6, T6, T0, shift::LSL, 0);
        a.lsr_w_imm(T6, T6, 31);
        a.ldr_w_off(T0, host::STATE, offsets::CPSR_OTHER);
        a.orr_w(T0, T0, T6, shift::LSL, 27);
        a.str_w_off(T0, host::STATE, offsets::CPSR_OTHER);
        a.mov_w(rd, T5);
    }

    /// SSAT / USAT Rd, #bits, Rn{, LSL/ASR #amount} (ASR #0 = ASR #32): signed `bits` 1-32,
    /// unsigned 0-31.
    pub(super) fn saturate_instruction(&mut self, rd: u32, rn: u32, signed: bool, bits: u32, kind: u32, amount: u32) -> Result<Flow> {
        if rd == PC || rn == PC {
            return Err(());
        }
        let value = if kind == shift::LSL && amount == 0 {
            rn
        } else {
            self.shift_immediate(T2, rn, kind, amount, false);
            T2
        };
        self.saturate(rd, value, signed, bits);
        Ok(Flow::Next)
    }

    /// SSAT16 / USAT16 Rd, #bits, Rn: each signed halfword saturated on its own.
    pub(super) fn saturate_halves_instruction(&mut self, rd: u32, rn: u32, signed: bool, bits: u32) -> Result<Flow> {
        if rd == PC || rn == PC {
            return Err(());
        }
        self.a.sxth_w(T2, rn);
        self.a.asr_w_imm(T3, rn, 16);
        self.saturate(T2, T2, signed, bits);
        self.saturate(T3, T3, signed, bits);
        self.a.bfi_w(T2, T3, 16, 16);
        self.a.mov_w(rd, T2);
        Ok(Flow::Next)
    }

    /// rd = value clamped to a signed (`bits` 1-32) or unsigned (0-31) range, setting Q
    /// when it was clamped. Branches with cbz, so the guest flags stay put (uses T0, T4, T6).
    fn saturate(&mut self, rd: Reg, value: Reg, signed: bool, bits: u32) {
        let a = &mut self.a;
        if signed && bits == 32 {
            a.mov_w(rd, value);
            return;
        }
        // T4 = 0 when the value is in range.
        if signed {
            a.sbfx_w(T4, value, 0, bits);
            a.eor_w(T4, T4, value, shift::LSL, 0);
        } else {
            a.lsr_w_imm(T4, value, bits);
        }
        let done = a.label();
        a.mov_w(rd, value);
        a.cbz_w_to(T4, done);
        // Out of range: the bound on the value's side (value asr 31 is 0 or -1).
        let max = if signed { (1u32 << (bits - 1)) - 1 } else { (1u32 << bits) - 1 };
        let bound = if max == 0 {
            ZR
        } else {
            a.mov_w_imm(T6, max);
            T6
        };
        if signed {
            a.eor_w(rd, bound, value, shift::ASR, 31); // max, or !max = min
        } else {
            a.bic_w(rd, bound, value, shift::ASR, 31); // max, or 0
        }
        a.ldr_w_off(T0, host::STATE, offsets::CPSR_OTHER);
        a.orr_w_imm(T0, T0, 1 << 27, T6);
        a.str_w_off(T0, host::STATE, offsets::CPSR_OTHER);
        a.bind(done);
    }

    fn multiply_accumulate_accumulate(&mut self, rd_lo: u32, rd_hi: u32, rn: u32, rm: u32) {
        self.a.mov_w(T4, rd_lo);
        self.a.add_x_uxtw(T4, T4, rd_hi);
        self.a.umaddl(T4, rn, rm, T4);
        self.a.mov_w(rd_lo, T4);
        self.a.lsr_x_imm(T4, T4, 32);
        self.a.mov_w(rd_hi, T4);
    }

    fn most_significant_multiply(&mut self, rd: u32, rn: u32, rm: u32, ra: Option<u32>, round: bool) {
        self.a.smaddl(T4, rn, rm, ZR);
        if let Some(ra) = ra {
            self.a.mov_w(T5, ra);
            self.a.add_x_lsl(T4, T4, T5, 32);
        }
        if round {
            self.a.mov_x_imm(T5, 0x8000_0000);
            self.a.add_x(T4, T4, T5);
        }
        self.a.lsr_x_imm(T4, T4, 32);
        self.a.mov_w(rd, T4);
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
        if let Some(kind) = cp15_barrier((h1 << 16) | h2) {
            self.barrier(kind);
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

/// One extra host instruction at the start of every translated block.
/// Default with parallel guest threads: `dmb ishst`. Card Wars (Mono) corrupts
/// its allocator memory about once in eight launches on two or more cores
/// without it and not once in 48 launches with it (9/70 without); the
/// exact guest code that needs it is not found yet (DEBUGGING.md, "OPEN: root
/// cause behind the light barrier"). Costs about 5% of own-code time.
/// debug.arm32bridge.fence / ARM32_FENCE (testing): "off" nothing, "1" dmb ish,
/// "ld" dmb ishld, "st" dmb ishst, "nop" a nop (same size, no ordering).
pub(super) fn fence_prefix() -> Option<u32> {
    static FENCE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *FENCE.get_or_init(|| {
        let setting = std::env::var("ARM32_FENCE").ok().or_else(|| super::property("debug.arm32bridge.fence"));
        let prefix = match setting.as_deref().filter(|value| !value.is_empty()) {
            Some("off") => None,
            Some("1") => Some(0xD503_3BBF),
            Some("ld") => Some(0xD503_39BF),
            Some("st") => Some(0xD503_3ABF),
            Some("nop") => Some(0xD503_201F),
            _ if crate::runtime::parallel() => Some(0xD503_3ABF),
            _ => None,
        };
        crate::log::line(&format!("[bridge] block start prefix: {}", match prefix {
            Some(0xD503_3BBF) => "dmb ish",
            Some(0xD503_39BF) => "dmb ishld",
            Some(0xD503_3ABF) => "dmb ishst",
            Some(_) => "nop",
            None => "none",
        }));
        prefix
    })
}

/// The ARMv6-style CP15 barriers, still used by code built for old CPUs:
/// MCR p15, 0, Rt, c7, c10, 4 (DSB), c7, c10, 5 (DMB), c7, c5, 4 (ISB).
/// Returns the `barrier` kind. The same encoding in ARM (condition AL) and Thumb.
fn cp15_barrier(word: u32) -> Option<u32> {
    match word & 0xFFFF_0FFF {
        0xEE07_0F9A => Some(4),
        0xEE07_0FBA => Some(5),
        0xEE07_0F95 => Some(6),
        _ => None,
    }
}

/// Whether the host has ARMv8.1 atomics (CASAL/SWPAL). Without them (or with
/// ARM32_LSE=0 / debug.arm32bridge.lse=0) exclusives use LDAXR/STLXR loops.
/// ARM32_FORCE_FALLBACK_SYNC=1 / debug.arm32bridge.fallback_sync=1 (testing):
/// exclusives and SWP always run in Unicorn, so its atomic path is exercised.
pub fn force_sync_fallback() -> bool {
    static FORCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE.get_or_init(|| {
        let setting = std::env::var("ARM32_FORCE_FALLBACK_SYNC").ok().or_else(|| super::property("debug.arm32bridge.fallback_sync"));
        setting.as_deref() == Some("1")
    })
}

fn use_lse() -> bool {
    static LSE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LSE.get_or_init(|| {
        #[cfg(target_arch = "aarch64")]
        let present = std::arch::is_aarch64_feature_detected!("lse");
        #[cfg(not(target_arch = "aarch64"))]
        let present = false;
        let setting = std::env::var("ARM32_LSE").ok().or_else(|| super::property("debug.arm32bridge.lse"));
        present && setting.as_deref() != Some("0")
    })
}

/// Exclusive loads and stores, emulated the way QEMU does it: LDREX records
/// the address and the value it read (`CpuState::excl_*`); STREX stores with a
/// compare-and-swap against that value, so it fails if another thread changed
/// the memory in between. (A change and a change back is not noticed, unlike
/// a real exclusive monitor.) `size`: 0 byte, 1 halfword, 2 word, 3 doubleword.
impl Translator {
    /// T0 = guest address `rn + offset`; an unaligned address leaves through a
    /// fallback (Unicorn raises the alignment fault real hardware raises).
    fn exclusive_address(&mut self, size: u32, rn: Reg, offset: u32) {
        if offset == 0 {
            self.a.mov_w(T0, rn);
        } else {
            self.a.add_w_any(T0, rn, offset, T1);
        }
        if size > 0 {
            let aligned = self.a.label();
            self.a.and_w_imm(T4, T0, (1 << size) - 1, T5);
            self.a.cbz_w_to(T4, aligned);
            emit_exit(&mut self.a, self.pc | self.thumb as u32, exit::FALLBACK | (1 << 8));
            self.a.bind(aligned);
        }
    }

    /// LDREX{B,H,D}: `rt2` is only used for doublewords.
    fn load_exclusive(&mut self, size: u32, rt: Reg, rt2: Reg, rn: Reg, offset: u32) -> Result<Flow> {
        if self.in_it || rn == PC || rt == PC || (size == 3 && (rt2 == PC || rt2 == rt)) || force_sync_fallback() {
            return Err(());
        }
        self.exclusive_address(size, rn, offset);
        self.a.str_w_off(T0, host::STATE, offsets::EXCL_ADDR);
        match size {
            0 => self.a.ldrb_w_uxtw(rt, host::MEMORY, T0),
            1 => self.a.ldrh_w_uxtw(rt, host::MEMORY, T0),
            2 => self.a.ldr_w_uxtw(rt, host::MEMORY, T0),
            _ => {
                // One 64-bit load, so the pair is read in one piece.
                self.a.ldr_x_uxtw(T2, host::MEMORY, T0);
                self.a.str_x_off(T2, host::STATE, offsets::EXCL_VALUE);
                self.a.mov_w(rt, T2);
                self.a.lsr_x_imm(T2, T2, 32);
                self.a.mov_w(rt2, T2);
                return Ok(Flow::Next);
            }
        }
        // A w register load clears the upper half, so x(rt) is the value.
        self.a.str_x_off(rt, host::STATE, offsets::EXCL_VALUE);
        Ok(Flow::Next)
    }

    /// STREX{B,H,D} Rd, Rt(, Rt2), [Rn, #offset]: Rd = 0 if stored, 1 if not.
    fn store_exclusive(&mut self, size: u32, rd: Reg, rt: Reg, rt2: Reg, rn: Reg, offset: u32) -> Result<Flow> {
        if self.in_it || rn == PC || rt == PC || rd == PC || rd == rn || rd == rt || force_sync_fallback() {
            return Err(());
        }
        if size == 3 && (rt2 == PC || rd == rt2) {
            return Err(());
        }
        self.exclusive_address(size, rn, offset);
        let (fail, done) = (self.a.label(), self.a.label());
        // The monitor is cleared whether or not the store happens.
        self.a.ldr_w_off(T1, host::STATE, offsets::EXCL_ADDR);
        self.a.eor_w(T1, T1, T0, shift::LSL, 0);
        self.a.mov_w_imm(T2, super::NO_EXCLUSIVE);
        self.a.str_w_off(T2, host::STATE, offsets::EXCL_ADDR);
        self.a.cbnz_w_to(T1, fail);
        self.a.ldr_x_off(T1, host::STATE, offsets::EXCL_VALUE);
        self.a.add_x_uxtw(T2, host::MEMORY, T0);
        let new = if size == 3 {
            self.a.mov_w(T5, rt);
            self.a.orr_x(T5, T5, rt2, shift::LSL, 32);
            T5
        } else {
            rt
        };
        if use_lse() {
            self.a.mov_x(T3, T1);
            self.a.casal(size, T1, new, T2);
            self.a.eor_x(T1, T1, T3);
            self.a.cbnz_x_to(T1, fail);
        } else {
            let (retry, differs) = (self.a.label(), self.a.label());
            self.a.bind(retry);
            self.a.ldaxr(size, T3, T2);
            self.a.eor_x(T4, T3, T1);
            self.a.cbnz_x_to(T4, differs);
            self.a.stlxr(size, T4, new, T2);
            self.a.cbnz_w_to(T4, retry);
            self.a.mov_w_imm(rd, 0);
            self.a.b_to(done);
            self.a.bind(differs);
            self.a.clrex();
            self.a.bind(fail);
            self.a.mov_w_imm(rd, 1);
            self.a.bind(done);
            return Ok(Flow::Next);
        }
        self.a.mov_w_imm(rd, 0);
        self.a.b_to(done);
        self.a.bind(fail);
        self.a.mov_w_imm(rd, 1);
        self.a.bind(done);
        Ok(Flow::Next)
    }

    /// CLREX.
    fn clear_exclusive(&mut self) {
        self.a.mov_w_imm(T0, super::NO_EXCLUSIVE);
        self.a.str_w_off(T0, host::STATE, offsets::EXCL_ADDR);
    }

    /// SWP / SWPB Rt, Rt2, [Rn]: Rt = old [Rn], [Rn] = Rt2, in one atomic step.
    fn swap(&mut self, size: u32, rt: Reg, rt2: Reg, rn: Reg) -> Result<Flow> {
        if rn == PC || rt == PC || rt2 == PC || rn == rt || rn == rt2 || force_sync_fallback() {
            return Err(());
        }
        self.exclusive_address(size, rn, 0);
        self.a.add_x_uxtw(T2, host::MEMORY, T0);
        if use_lse() {
            self.a.swpal(size, rt2, T3, T2);
        } else {
            let retry = self.a.label();
            self.a.bind(retry);
            self.a.ldaxr(size, T3, T2);
            self.a.stlxr(size, T4, rt2, T2);
            self.a.cbnz_w_to(T4, retry);
        }
        self.a.mov_w(rt, T3);
        Ok(Flow::Next)
    }

    /// DSB (`kind` 4), DMB (5) or ISB (6): the host's inner-shareable barrier
    /// of the same kind; every option (ST, LD, OSH...) is treated as the full one.
    fn barrier(&mut self, kind: u32) {
        match kind {
            4 => self.a.dsb_ish(),
            5 => self.a.dmb_ish(),
            _ => self.a.isb(),
        }
    }
}

#[cfg(test)]
mod copy_tests {
    use crate::mem::{GuestMem, HEAP_BASE};

    fn bytes(words: &[u32], size: usize) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()[..size].to_vec()).collect()
    }

    /// A block is made from the copy it's given, not from guest memory (another
    /// thread may rewrite that meanwhile); a read past a short copy is noted.
    #[test]
    fn translates_from_the_copy() {
        let mem = GuestMem::reserve();
        let at = HEAP_BASE;
        let arm = |value: u32| bytes(&[0xE3A0_0000 | value, 0xE12F_FF1E], 4); // mov r0, #value; bx lr
        let thumb = |value: u32| bytes(&[0x2000 | value, 0x4770], 2); // movs r0, #value; bx lr
        let wide = |value: u32| bytes(&[0xF240, value, 0x4770], 2); // movw r0, #value (two halves); bx lr
        let it = |value: u32| bytes(&[0x4280, 0xBF08, 0x2000 | value, 0x4770], 2); // cmp r0, r0; it eq; moveq r0, #value; bx lr
        let cases = [(false, arm(1), arm(2)), (true, thumb(1), thumb(2)), (true, wide(1), wide(2)), (true, it(1), it(2))];
        for (thumb_code, one, two) in cases {
            let translate = |copy: Vec<u8>| if thumb_code { super::translate_copy(mem, at, copy) } else { super::translate_arm_copy(mem, at, copy) };
            mem.write_bytes(at, &two);
            let (from_memory, _, outside) = translate(Vec::new());
            assert!(outside);
            mem.write_bytes(at, &one);
            let (from_copy, copy, outside) = translate(two.clone());
            assert_eq!(from_copy.code, from_memory.code, "{two:02x?}");
            assert_eq!(from_copy.end, at + two.len() as u32);
            assert!(!outside);
            assert_eq!(copy, two);
            // Cut inside the block: in its first instruction (ARM, movw between its
            // two halves) or its second.
            let (_, _, outside) = translate(two[..3].to_vec());
            assert!(outside);
        }
    }
}
