//! ARM (A32) → ARM64, using the same translator state and helpers as Thumb.
//! Every ARM instruction has a condition; a conditional one is wrapped in a
//! branch over it, like an instruction in a Thumb IT block.

use super::{cond, emit_exit, emit_exit_to, exit, host, offsets, shift, sign_extend, Asm, Flow, GuestMem, Reg, Result, Translator};
use super::{LR, MAX_INSTRUCTIONS, PC, SP, T0, T1, T2, T3, T4, T5, T6};

/// Translates the ARM block starting at `start` (4-byte aligned).
/// Returns the code and the guest address just past the last translated instruction.
pub fn translate(mem: GuestMem, start: u32) -> (Vec<u32>, u32) {
    let mut t = Translator { a: Asm::new(), mem, pc: start, size: 4, count: 0, in_it: false, thumb: false };
    let end;
    loop {
        if t.count >= MAX_INSTRUCTIONS {
            emit_exit(&mut t.a, t.pc, exit::JUMP);
            end = t.pc;
            break;
        }
        let checkpoint = t.a.checkpoint();
        let word = mem.read_u32(t.pc);
        match t.arm_instruction(word) {
            Ok(Flow::Next) => {
                t.pc += 4;
                t.count += 1;
            }
            Ok(Flow::End) => {
                end = t.pc + 4;
                break;
            }
            Err(()) => {
                t.a.rollback(checkpoint);
                emit_exit(&mut t.a, t.pc, exit::FALLBACK | (1 << 8));
                end = t.pc;
                break;
            }
        }
    }
    t.a.finish();
    (t.a.code, end)
}

/// The ARM data-processing opcodes in the Thumb-2 numbering `data_processing` uses.
/// Returns (opcode, rd, rn) with rd = PC for the compare forms and rn = PC for MOV/MVN.
fn data_opcode(op: u32, rd: u32, rn: u32) -> Option<(u32, u32, u32)> {
    Some(match op {
        0b0000 => (0b0000, rd, rn), // AND
        0b0001 => (0b0100, rd, rn), // EOR
        0b0010 => (0b1101, rd, rn), // SUB
        0b0011 => (0b1110, rd, rn), // RSB
        0b0100 => (0b1000, rd, rn), // ADD
        0b0101 => (0b1010, rd, rn), // ADC
        0b0110 => (0b1011, rd, rn), // SBC
        0b1000 => (0b0000, PC, rn), // TST
        0b1001 => (0b0100, PC, rn), // TEQ
        0b1010 => (0b1101, PC, rn), // CMP
        0b1011 => (0b1000, PC, rn), // CMN
        0b1100 if rn != PC => (0b0010, rd, rn), // ORR (rn = PC would read as MOV)
        0b1101 => (0b0010, rd, PC), // MOV
        0b1110 => (0b0001, rd, rn), // BIC
        0b1111 => (0b0011, rd, PC), // MVN
        _ => return None,           // RSC, ORR with the PC
    })
}

impl Translator {
    fn arm_instruction(&mut self, word: u32) -> Result<Flow> {
        let condition = word >> 28;
        if condition == 0xF {
            return self.arm_unconditional(word);
        }
        if (word >> 24) & 0xF == 0b1010 {
            // B<c>
            let target = self.pc_value().wrapping_add(sign_extend((word & 0xFF_FFFF) << 2, 26));
            return Ok(self.branch(target, condition, false));
        }
        if condition == cond::AL {
            return self.arm_body(word);
        }
        let skip = self.a.label();
        self.a.b_cond_to(cond::invert(condition), skip);
        let flow = self.arm_body(word)?;
        self.a.bind(skip);
        if flow == Flow::End {
            // The body branched when the condition held; otherwise continue after it.
            let next = self.next_pc();
            emit_exit(&mut self.a, next, exit::JUMP);
        }
        Ok(flow)
    }

    fn arm_unconditional(&mut self, word: u32) -> Result<Flow> {
        if word & 0x0C10_F000 == 0x0410_F000 {
            // PLD / PLDW / PLI: hints
            return Ok(Flow::Next);
        }
        if word & 0xFFFF_FF00 == 0xF57F_F000 && matches!((word >> 4) & 0xF, 4..=6) {
            // DSB / DMB / ISB: one guest thread runs at a time
            return Ok(Flow::Next);
        }
        if (word >> 25) & 7 == 0b101 {
            // BLX <imm>: call Thumb code
            let h = (word >> 24) & 1;
            let offset = sign_extend(((word & 0xFF_FFFF) << 2) | (h << 1), 26);
            self.a.mov_w_imm(LR, self.next_pc());
            return Ok(self.branch(self.pc_value().wrapping_add(offset), cond::AL, true));
        }
        Err(())
    }

    fn arm_body(&mut self, word: u32) -> Result<Flow> {
        match (word >> 25) & 7 {
            0b000 => {
                let low = (word >> 4) & 0xF;
                if low == 0b1001 {
                    if (word >> 24) & 1 == 0 {
                        self.arm_multiply(word)
                    } else {
                        self.arm_synchronization(word)
                    }
                } else if low & 0b1001 == 0b1001 {
                    self.arm_extra_load_store(word)
                } else if (word >> 23) & 3 == 0b10 && (word >> 20) & 1 == 0 {
                    self.arm_misc(word)
                } else {
                    self.arm_data(word)
                }
            }
            0b001 => {
                if (word >> 23) & 3 == 0b10 && (word >> 20) & 1 == 0 {
                    self.arm_move_wide_or_hint(word)
                } else {
                    self.arm_data(word)
                }
            }
            0b010 => self.arm_load_store(word),
            0b011 => {
                if (word >> 4) & 1 == 0 {
                    self.arm_load_store(word)
                } else {
                    self.arm_media(word)
                }
            }
            0b100 => self.arm_multiple(word),
            0b101 => {
                // BL (B is handled before the condition wrapper)
                let target = self.pc_value().wrapping_add(sign_extend((word & 0xFF_FFFF) << 2, 26));
                self.a.mov_w_imm(LR, self.next_pc());
                Ok(self.branch(target, cond::AL, false))
            }
            _ => self.arm_coprocessor(word),
        }
    }

    fn arm_data(&mut self, word: u32) -> Result<Flow> {
        let immediate = (word >> 25) & 1 == 1;
        let op = (word >> 21) & 0xF;
        let set_flags = (word >> 20) & 1 == 1;
        let (rn, rd) = ((word >> 16) & 0xF, (word >> 12) & 0xF);
        let compare = (0b1000..=0b1011).contains(&op);
        if rd == PC && set_flags && !compare {
            return Err(()); // exception return forms
        }
        let (op, rd, rn) = data_opcode(op, rd, rn).ok_or(())?;
        let logical = matches!(op, 0b0000..=0b0100);
        let want_carry = set_flags && logical;
        let (operand, carry): (Reg, Option<Reg>) = if immediate {
            let rotation = ((word >> 8) & 0xF) * 2;
            let value = (word & 0xFF).rotate_right(rotation);
            self.a.mov_w_imm(T2, value);
            if want_carry && rotation != 0 {
                self.a.mov_w_imm(T3, value >> 31);
                (T2, Some(T3))
            } else {
                (T2, None)
            }
        } else if (word >> 4) & 1 == 0 {
            let (rm, kind, amount) = (word & 0xF, (word >> 5) & 3, (word >> 7) & 0x1F);
            let m = self.read(rm, T0);
            if kind == shift::LSL && amount == 0 {
                (m, None)
            } else {
                let has_carry = self.shift_immediate(T2, m, kind, amount, want_carry);
                (T2, if has_carry && want_carry { Some(T3) } else { None })
            }
        } else {
            let (rm, rs) = (word & 0xF, (word >> 8) & 0xF);
            if rm == PC || rs == PC || rn == PC || rd == PC {
                return Err(());
            }
            self.shift_register(T2, rm, rs, (word >> 5) & 3, want_carry);
            if want_carry {
                // An amount of 0 keeps the current C.
                let shifted = self.a.label();
                self.a.cbz_w_to(T4, shifted);
                self.a.mrs_nzcv(T5);
                self.a.ubfx_w(T3, T5, 29, 1);
                self.a.bind(shifted);
                (T2, Some(T3))
            } else {
                (T2, None)
            }
        };
        self.data_processing(op, set_flags, rd, rn, operand, carry)
    }

    fn arm_move_wide_or_hint(&mut self, word: u32) -> Result<Flow> {
        let rd = (word >> 12) & 0xF;
        let imm16 = ((word >> 4) & 0xF000) | (word & 0xFFF);
        match (word >> 20) & 0xF {
            0b0000 if rd != PC => self.a.mov_w_imm(rd, imm16), // MOVW
            0b0100 if rd != PC => self.a.emit(0x72A0_0000 | (imm16 << 5) | rd), // MOVT: movk wd, #imm16, lsl 16
            0b0010 if word & 0x0FFF_FF00 == 0x0320_F000 && word & 0xFF <= 4 => {} // NOP/YIELD/WFE/WFI/SEV
            _ => return Err(()),
        }
        Ok(Flow::Next)
    }

    fn arm_multiply(&mut self, word: u32) -> Result<Flow> {
        let op = (word >> 21) & 7;
        let set_flags = (word >> 20) & 1 == 1;
        let (rd, ra, rm, rn) = ((word >> 16) & 0xF, (word >> 12) & 0xF, (word >> 8) & 0xF, word & 0xF);
        if rd == PC || rm == PC || rn == PC {
            return Err(());
        }
        match op {
            0b000 => self.a.mul_w(rd, rn, rm), // MUL (ra should be 0)
            0b001 => {
                if ra == PC {
                    return Err(());
                }
                self.a.madd_w(rd, rn, rm, ra); // MLA
            }
            0b011 if !set_flags => {
                if ra == PC {
                    return Err(());
                }
                self.a.msub_w(rd, rn, rm, ra); // MLS
            }
            0b100..=0b111 if !set_flags => {
                // UMULL / UMLAL / SMULL / SMLAL: RdHi = bits 19-16, RdLo = bits 15-12
                let (rd_hi, rd_lo) = (rd, ra);
                if rd_lo == PC || rd_lo == rd_hi {
                    return Err(());
                }
                self.multiply_long(op >= 0b110, op & 1 == 1, rd_lo, rd_hi, rn, rm);
                return Ok(Flow::Next);
            }
            _ => return Err(()),
        }
        if set_flags {
            self.set_nz(rd);
        }
        Ok(Flow::Next)
    }

    /// LDREX/STREX and their byte/halfword/doubleword forms. Only one guest
    /// thread runs at a time, so they are plain accesses and stores always succeed.
    fn arm_synchronization(&mut self, word: u32) -> Result<Flow> {
        let (rn, rd, rt) = ((word >> 16) & 0xF, (word >> 12) & 0xF, word & 0xF);
        if (word >> 23) & 1 == 0 || (word >> 8) & 0xF != 0xF || rn == PC {
            return Err(()); // SWP/SWPB
        }
        let load = (word >> 20) & 1 == 1;
        let kind = (word >> 21) & 3; // 0 word, 1 doubleword, 2 byte, 3 halfword
        if load {
            // LDREX*: Rt = bits 15-12
            let rt = rd;
            if rt == PC {
                return Err(());
            }
            match kind {
                0 => self.a.ldr_w_uxtw(rt, host::MEMORY, rn),
                1 => {
                    if rt % 2 == 1 || rt == LR {
                        return Err(());
                    }
                    self.a.add_x_uxtw(T1, host::MEMORY, rn);
                    self.a.ldp_w_off(rt, rt + 1, T1, 0);
                }
                2 => self.a.ldrb_w_uxtw(rt, host::MEMORY, rn),
                _ => self.a.ldrh_w_uxtw(rt, host::MEMORY, rn),
            }
        } else {
            // STREX* Rd, Rt, [Rn]
            if rd == PC || rt == PC {
                return Err(());
            }
            match kind {
                0 => self.a.str_w_uxtw(rt, host::MEMORY, rn),
                1 => {
                    if rt % 2 == 1 || rt == LR {
                        return Err(());
                    }
                    self.a.add_x_uxtw(T1, host::MEMORY, rn);
                    self.a.stp_w_off(rt, rt + 1, T1, 0);
                }
                2 => self.a.strb_w_uxtw(rt, host::MEMORY, rn),
                _ => self.a.strh_w_uxtw(rt, host::MEMORY, rn),
            }
            self.a.mov_w_imm(rd, 0);
        }
        Ok(Flow::Next)
    }

    fn arm_misc(&mut self, word: u32) -> Result<Flow> {
        let rm = word & 0xF;
        if word & 0x0FFF_FFF0 == 0x012F_FF10 || word & 0x0FFF_FFF0 == 0x012F_FF30 {
            // BX / BLX Rm
            if rm == PC {
                return Err(());
            }
            self.a.mov_w(T6, rm);
            if (word >> 5) & 1 == 1 {
                self.a.mov_w_imm(LR, self.next_pc());
            }
            emit_exit_to(&mut self.a, T6);
            return Ok(Flow::End);
        }
        if word & 0x0FFF_0FF0 == 0x016F_0F10 {
            // CLZ
            let rd = (word >> 12) & 0xF;
            if rd == PC || rm == PC {
                return Err(());
            }
            self.a.clz_w(rd, rm);
            return Ok(Flow::Next);
        }
        Err(())
    }

    /// LDR/STR(B) with an immediate or (shifted) register offset.
    fn arm_load_store(&mut self, word: u32) -> Result<Flow> {
        let register = (word >> 25) & 1 == 1;
        let (index, up, byte, w, load) = ((word >> 24) & 1 == 1, (word >> 23) & 1 == 1, (word >> 22) & 1 == 1, (word >> 21) & 1 == 1, (word >> 20) & 1 == 1);
        let (rn, rt) = ((word >> 16) & 0xF, (word >> 12) & 0xF);
        let size = if byte { 1 } else { 4 };
        // Post-indexed forms always write back (P=0 W=1 is the unprivileged form: same in user code).
        let writeback = w || !index;
        if register {
            let (rm, kind, amount) = (word & 0xF, (word >> 5) & 3, (word >> 7) & 0x1F);
            return self.load_store_register(load, size, false, rt, rn, rm, kind, amount, up, index, writeback);
        }
        let imm = word & 0xFFF;
        let offset = if up { imm } else { imm.wrapping_neg() };
        self.load_store(load, size, false, rt, rn, offset, index, writeback)
    }

    /// LDRH/STRH/LDRSB/LDRSH/LDRD/STRD.
    fn arm_extra_load_store(&mut self, word: u32) -> Result<Flow> {
        let (index, up, immediate, w, l) = ((word >> 24) & 1 == 1, (word >> 23) & 1 == 1, (word >> 22) & 1 == 1, (word >> 21) & 1 == 1, (word >> 20) & 1 == 1);
        let (rn, rt) = ((word >> 16) & 0xF, (word >> 12) & 0xF);
        if !index && w {
            return Err(()); // unprivileged forms
        }
        let writeback = w || !index;
        let (load, size, signed) = match ((word >> 5) & 3, l) {
            (1, false) => (false, 2, false),
            (1, true) => (true, 2, false),
            (2, false) => return self.arm_dual(word, true),
            (2, true) => (true, 1, true),
            (3, false) => return self.arm_dual(word, false),
            (3, true) => (true, 2, true),
            _ => return Err(()),
        };
        if immediate {
            let imm = ((word >> 4) & 0xF0) | (word & 0xF);
            let offset = if up { imm } else { imm.wrapping_neg() };
            self.load_store(load, size, signed, rt, rn, offset, index, writeback)
        } else {
            self.load_store_register(load, size, signed, rt, rn, word & 0xF, shift::LSL, 0, up, index, writeback)
        }
    }

    /// LDRD/STRD Rt, Rt+1.
    fn arm_dual(&mut self, word: u32, load: bool) -> Result<Flow> {
        let (index, up, immediate, w) = ((word >> 24) & 1 == 1, (word >> 23) & 1 == 1, (word >> 22) & 1 == 1, (word >> 21) & 1 == 1);
        let (rn, rt) = ((word >> 16) & 0xF, (word >> 12) & 0xF);
        let writeback = w || !index;
        if rt % 2 == 1 || rt == LR || (rn == PC && writeback) {
            return Err(());
        }
        let rt2 = rt + 1;
        if load && writeback && (rt == rn || rt2 == rn) {
            return Err(());
        }
        // T2 = the offset
        if immediate {
            let imm = ((word >> 4) & 0xF0) | (word & 0xF);
            self.a.mov_w_imm(T2, if up { imm } else { imm.wrapping_neg() });
        } else {
            let rm = word & 0xF;
            if rm == PC || (load && (rm == rt || rm == rt2)) {
                return Err(());
            }
            if up {
                self.a.mov_w(T2, rm);
            } else {
                self.a.sub_w(T2, super::ZR, rm, shift::LSL, 0);
            }
        }
        let base = self.read(rn, T1);
        if index {
            self.a.add_w(T0, base, T2, shift::LSL, 0);
        } else {
            self.a.mov_w(T0, base);
        }
        self.a.add_x_uxtw(T1, host::MEMORY, T0);
        if load {
            if rt == rn {
                self.a.ldr_w_off(T3, T1, 0);
                self.a.ldr_w_off(rt2, T1, 4);
                self.a.mov_w(rt, T3);
            } else {
                self.a.ldp_w_off(rt, rt2, T1, 0);
            }
        } else {
            self.a.stp_w_off(rt, rt2, T1, 0);
        }
        if writeback {
            self.a.add_w(rn, rn, T2, shift::LSL, 0);
        }
        Ok(Flow::Next)
    }

    /// A single load/store whose offset is register `rm` shifted by an immediate.
    #[allow(clippy::too_many_arguments)]
    fn load_store_register(&mut self, load: bool, size: u32, signed: bool, rt: u32, rn: u32, rm: u32, kind: u32, amount: u32, up: bool, index: bool, writeback: bool) -> Result<Flow> {
        if rm == PC || (writeback && (rn == PC || rn == rm)) || (!load && rt == PC) || (load && writeback && rt == rn) {
            return Err(());
        }
        // T2 = the shifted offset (a copy, so a load into rm cannot change the write-back).
        let offset = if kind == shift::LSL && amount == 0 && !writeback {
            rm
        } else {
            self.shift_immediate(T2, rm, kind, amount, false);
            T2
        };
        let base = self.read(rn, T1);
        let address = if index {
            if up {
                self.a.add_w(T0, base, offset, shift::LSL, 0);
            } else {
                self.a.sub_w(T0, base, offset, shift::LSL, 0);
            }
            T0
        } else {
            base
        };
        let target = if rt == PC { T6 } else { rt };
        self.access(load, size, signed, target, address);
        if writeback {
            if index {
                self.a.mov_w(rn, T0);
            } else if up {
                self.a.add_w(rn, rn, offset, shift::LSL, 0);
            } else {
                self.a.sub_w(rn, rn, offset, shift::LSL, 0);
            }
        }
        if rt == PC {
            if size != 4 {
                return Err(());
            }
            emit_exit_to(&mut self.a, T6);
            return Ok(Flow::End);
        }
        Ok(Flow::Next)
    }

    fn arm_media(&mut self, word: u32) -> Result<Flow> {
        let (rd, rm) = ((word >> 12) & 0xF, word & 0xF);
        if word & 0x0FE0_0070 == 0x07E0_0050 || word & 0x0FE0_0070 == 0x07A0_0050 {
            // UBFX / SBFX Rd, Rn, #lsb, #width
            let (width, lsb) = (((word >> 16) & 0x1F) + 1, (word >> 7) & 0x1F);
            if rd == PC || rm == PC || lsb + width > 32 {
                return Err(());
            }
            if (word >> 22) & 1 == 1 {
                self.a.ubfx_w(rd, rm, lsb, width);
            } else {
                self.a.sbfx_w(rd, rm, lsb, width);
            }
            return Ok(Flow::Next);
        }
        if word & 0x0FE0_0070 == 0x07C0_0010 {
            // BFI / BFC (Rn = PC)
            let (msb, lsb) = ((word >> 16) & 0x1F, (word >> 7) & 0x1F);
            if rd == PC || msb < lsb {
                return Err(());
            }
            let source = if rm == PC { super::ZR } else { rm };
            self.a.bfi_w(rd, source, lsb, msb - lsb + 1);
            return Ok(Flow::Next);
        }
        if word & 0x0F80_03F0 == 0x0680_0070 {
            // SXTB/SXTH/UXTB/UXTH, with an add when Rn is not the PC
            let rn = (word >> 16) & 0xF;
            if rd == PC || rm == PC {
                return Err(());
            }
            let rotation = ((word >> 10) & 3) * 8;
            if rotation == 0 {
                self.a.mov_w(T2, rm);
            } else {
                self.a.ror_w_imm(T2, rm, rotation);
            }
            match (word >> 20) & 7 {
                0b010 => self.a.sxtb_w(T2, T2),
                0b011 => self.a.sxth_w(T2, T2),
                0b110 => self.a.uxtb_w(T2, T2),
                0b111 => self.a.uxth_w(T2, T2),
                _ => return Err(()), // the 16-bit pair forms
            }
            if rn == PC {
                self.a.mov_w(rd, T2);
            } else {
                self.a.add_w(rd, rn, T2, shift::LSL, 0);
            }
            return Ok(Flow::Next);
        }
        if rd != PC && rm != PC {
            match word & 0x0FFF_0FF0 {
                0x06BF_0F30 => self.a.rev_w(rd, rm),
                0x06BF_0FB0 => self.a.rev16_w(rd, rm),
                0x06FF_0F30 => self.a.rbit_w(rd, rm),
                0x06FF_0FB0 => {
                    self.a.rev16_w(rd, rm); // REVSH
                    self.a.sxth_w(rd, rd);
                }
                _ => return self.arm_divide(word),
            }
            return Ok(Flow::Next);
        }
        Err(())
    }

    /// SDIV / UDIV Rd, Rn, Rm (division by zero gives 0 on both).
    fn arm_divide(&mut self, word: u32) -> Result<Flow> {
        let (rd, rm, rn) = ((word >> 16) & 0xF, (word >> 8) & 0xF, word & 0xF);
        if rd == PC || rm == PC || rn == PC {
            return Err(());
        }
        match word & 0x0FF0_F0F0 {
            0x0710_F010 => self.a.sdiv_w(rd, rn, rm),
            0x0730_F010 => self.a.udiv_w(rd, rn, rm),
            _ => return Err(()),
        }
        Ok(Flow::Next)
    }

    /// LDM/STM (increment-after and decrement-before; push and pop are these).
    fn arm_multiple(&mut self, word: u32) -> Result<Flow> {
        let (before, up, user, writeback, load) = ((word >> 24) & 1 == 1, (word >> 23) & 1 == 1, (word >> 22) & 1 == 1, (word >> 21) & 1 == 1, (word >> 20) & 1 == 1);
        let (rn, list) = ((word >> 16) & 0xF, word & 0xFFFF);
        if user || (rn == SP && list & (1 << SP) != 0) {
            return Err(());
        }
        match (before, up) {
            (false, true) => self.load_store_multiple(load, rn, list, true, writeback),
            (true, false) => self.load_store_multiple(load, rn, list, false, writeback),
            _ => Err(()), // IB / DA: rare
        }
    }

    fn arm_coprocessor(&mut self, word: u32) -> Result<Flow> {
        if word & 0x0FFF_0FFF == 0x0E1D_0F70 {
            // MRC p15, 0, Rt, c13, c0, 3: TPIDRURO (bionic's thread pointer)
            let rt = (word >> 12) & 0xF;
            if rt == PC {
                return Err(());
            }
            self.a.ldr_w_off(rt, host::STATE, offsets::TLS);
            return Ok(Flow::Next);
        }
        Err(()) // VFP, NEON, SVC: Unicorn for now
    }
}
