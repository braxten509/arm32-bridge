//! Game functions the bridge steps into. A wrapped function's first
//! instructions become a jump to a stand-in (a handler here), which can run
//! the original through a trampoline: the moved first instructions, then a
//! jump back past them. Done when the library holding the function loads,
//! before any of its code runs. Each wrap names its function by symbol, so
//! it only touches the game that has it.

use crate::runtime::{mem, Args, ArgSource, Handler, Ret, Uc};
use std::collections::HashMap;
use std::sync::Mutex;

struct Wrap {
    symbol: &'static str,
    /// Only in a library whose file name starts with this (None: any), for
    /// wraps that know one game's build.
    library: Option<&'static str>,
    handler: Handler,
}

const WRAPS: &[Wrap] = &[
    // The self-test's (tests/guest/selftest.cpp): 10 times the original's result.
    Wrap { symbol: "bridge_wrap_test_arm", library: None, handler: test_arm },
    Wrap { symbol: "bridge_wrap_test_thumb", library: None, handler: test_thumb },
    // Monster Life's Flash menus, blended between their steps (gameswf_blend.rs).
    Wrap { symbol: crate::gameswf_blend::ADVANCE, library: Some("libMonsterLife"), handler: crate::gameswf_blend::advance },
    Wrap { symbol: crate::gameswf_blend::DISPLAY, library: Some("libMonsterLife"), handler: crate::gameswf_blend::display },
    // Monster Life's menus measure the screen after its size is set (gameswf_menus.rs).
    Wrap { symbol: crate::gameswf_menus::RESIZE_MENUS, library: Some("libMonsterLife"), handler: crate::gameswf_menus::resize_menus },
    // ... and see the game's long-press notice where the finger is (gameswf_menus.rs).
    Wrap { symbol: crate::gameswf_menus::MENU_INPUT, library: Some("libMonsterLife"), handler: crate::gameswf_menus::menu_input },
    // ... and its currency store sells packs for Gold (monster_life_store.rs).
    Wrap { symbol: crate::monster_life_store::BUY_ITEM, library: Some("libMonsterLife"), handler: crate::monster_life_store::buy_item },
    // Rule the Kingdom, zoomed in 50% (rtk_zoom.rs).
    Wrap { symbol: crate::rtk_zoom::SETTERS[0], library: Some(crate::rtk_zoom::LIBRARY), handler: crate::rtk_zoom::set_world },
    Wrap { symbol: crate::rtk_zoom::SETTERS[1], library: Some(crate::rtk_zoom::LIBRARY), handler: crate::rtk_zoom::set_menus },
    Wrap { symbol: crate::rtk_zoom::SETTERS[2], library: Some(crate::rtk_zoom::LIBRARY), handler: crate::rtk_zoom::set_hover },
    Wrap { symbol: crate::rtk_zoom::SETTERS[3], library: Some(crate::rtk_zoom::LIBRARY), handler: crate::rtk_zoom::set_top_bar },
    // Rule the Kingdom's pictures, for the 2x picture pack (rtk_textures.rs).
    Wrap { symbol: crate::rtk_textures::GET_IMAGE_DATA, library: Some(crate::rtk_zoom::LIBRARY), handler: crate::rtk_textures::get_image_data },
];

fn applies(wrap: &Wrap, library: &str, symbol: &str) -> bool {
    wrap.symbol == symbol && wrap.library.is_none_or(|start| library.starts_with(start))
}

fn test_arm(uc: &mut Uc, args: &mut Args) -> Ret {
    test_times_ten(uc, args, "bridge_wrap_test_arm")
}

fn test_thumb(uc: &mut Uc, args: &mut Args) -> Ret {
    test_times_ten(uc, args, "bridge_wrap_test_thumb")
}

fn test_times_ten(uc: &mut Uc, args: &mut Args, symbol: &str) -> Ret {
    let (a, b) = (args.u32(), args.u32());
    match crate::runtime::call_guest(uc, original(symbol), &[a, b]) {
        Ok(result) => Ret::U32((result as u32).wrapping_mul(10)),
        Err(_) => Ret::U32(0),
    }
}

/// The trampolines (bit 0 set for Thumb), by symbol.
static ORIGINALS: Mutex<Option<HashMap<&'static str, u32>>> = Mutex::new(None);

const STAND_IN_PREFIX: &str = "wrap:";

/// The handler for the stand-in named `name` ("wrap:<symbol>").
pub fn handler(name: &str) -> Option<Handler> {
    let symbol = name.strip_prefix(STAND_IN_PREFIX)?;
    WRAPS.iter().find(|wrap| wrap.symbol == symbol).map(|wrap| wrap.handler)
}

/// Where the original of a wrapped function runs (0: not wrapped).
pub fn original(symbol: &str) -> u32 {
    ORIGINALS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|map| map.get(symbol).copied()).unwrap_or(0)
}

/// Whether `library`'s function `symbol` is wrapped.
pub fn wanted(library: &str, symbol: &str) -> bool {
    WRAPS.iter().any(|wrap| applies(wrap, library, symbol))
}

/// Wraps `library`'s function at `entry` (bit 0 set: Thumb), `size` bytes long.
pub fn wrap(library: &str, symbol: &str, entry: u32, size: u32) {
    let Some(wrap) = WRAPS.iter().find(|wrap| applies(wrap, library, symbol)) else { return };
    // flashblend=0 leaves the game's Flash player untouched, to compare against.
    if [crate::gameswf_blend::ADVANCE, crate::gameswf_blend::DISPLAY].contains(&symbol) && !crate::gameswf_blend::enabled() {
        return;
    }
    let stand_in = crate::runtime::stub_for(&format!("{STAND_IN_PREFIX}{symbol}"));
    let made = if entry & 1 == 1 { wrap_thumb(entry & !1, size, stand_in) } else { wrap_arm(entry, size, stand_in) };
    match made {
        Ok(trampoline) => {
            ORIGINALS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(wrap.symbol, trampoline);
            crate::log::line(&format!("[bridge] stepping into {symbol} ({library})"));
        }
        Err(why) => crate::log::line(&format!("[bridge] can't step into {symbol}: {why}")),
    }
}

/// `ldr pc, [pc, #-4]`: jumps to the word after it.
const ARM_JUMP: u32 = 0xE51F_F004;
/// `mov r0, r0`.
const ARM_NOP: u32 = 0xE1A0_0000;
/// `ldr.w pc, [pc, #0]` as two halfwords: jumps to the word at the next 4-byte boundary after its own start + 4.
const THUMB_JUMP: [u16; 2] = [0xF8DF, 0xF000];
const THUMB_NOP: u16 = 0xBF00;

fn wrap_arm(entry: u32, size: u32, stand_in: u32) -> Result<u32, String> {
    if size < 8 {
        return Err(format!("only {size} bytes long"));
    }
    let memory = mem();
    let words = arm_trampoline([memory.read_u32(entry), memory.read_u32(entry + 4)], entry, |address| memory.read_u32(address))?;
    let trampoline = crate::runtime::heap().memalign(4, words.len() as u32 * 4);
    if trampoline == 0 {
        return Err("no memory for its trampoline".into());
    }
    for (i, word) in words.into_iter().enumerate() {
        memory.write_u32(trampoline + i as u32 * 4, word);
    }
    memory.write_u32(entry, ARM_JUMP);
    memory.write_u32(entry + 4, stand_in);
    Ok(trampoline)
}

/// The trampoline for ARM instructions `moved` from `entry`: them, a jump
/// back past them, then the numbers moved loads of nearby numbers
/// (`ldr rN, [pc, #±n]`) read, which `read` gives by address; those loads
/// read their copy instead.
fn arm_trampoline(moved: [u32; 2], entry: u32, read: impl Fn(u32) -> u32) -> Result<Vec<u32>, String> {
    if let Some(words) = arm_address_trampoline(moved, entry, &read) {
        return Ok(words);
    }
    let mut words = vec![moved[0], moved[1], ARM_JUMP, entry + 8];
    for (i, &instruction) in moved.iter().enumerate() {
        let number_load = instruction & 0x0F7F_0000 == 0x051F_0000 && (instruction >> 12) & 0xF != 15 && instruction >> 28 != 0xF;
        if number_load {
            let offset = instruction & 0xFFF;
            let at = entry + i as u32 * 4 + 8;
            let at = if instruction & (1 << 23) != 0 { at + offset } else { at - offset };
            // The copy's distance from this load's new place (its pc reads 8 ahead).
            let distance = words.len() as u32 * 4 - (i as u32 * 4 + 8);
            words[i] = (instruction & 0xFFFF_F000) | (1 << 23) | distance;
            words.push(read(at));
        } else if arm_uses_pc(instruction) {
            return Err(format!("its first instructions use the pc ({instruction:#010x})"));
        }
    }
    Ok(words)
}

/// The trampoline for a position-independent address: `ldr rD, [pc, #±n]`
/// (a distance), then `add rD, pc, rD` (the address) or `ldr rD, [pc, rD]`
/// (the word there). The finished address is worked out now and loaded from
/// beside the trampoline; the second form then reads through it
/// (`ldr rD, [rD]`). None for anything else.
fn arm_address_trampoline(moved: [u32; 2], entry: u32, read: &impl Fn(u32) -> u32) -> Option<Vec<u32>> {
    let [load, second] = moved;
    // An unconditional word load from near the pc into rD (not the pc).
    if load & 0xFF7F_0000 != 0xE51F_0000 || (load >> 12) & 0xF == 15 {
        return None;
    }
    let register = (load >> 12) & 0xF;
    let then = match second {
        _ if second == 0xE08F_0000 | register << 12 | register => ARM_NOP,
        _ if second == 0xE79F_0000 | register << 12 | register => 0xE590_0000 | register << 16 | register << 12,
        _ => return None,
    };
    let offset = load & 0xFFF;
    let number_at = if load & (1 << 23) != 0 { entry + 8 + offset } else { entry + 8 - offset };
    // The second instruction's pc reads 8 ahead of it.
    let address = (entry + 4 + 8).wrapping_add(read(number_at));
    // The load's new place is the trampoline's start; the address sits 4 words on.
    Some(vec![(load & 0xFFFF_F000) | (1 << 23) | (4 * 4 - 8), then, ARM_JUMP, entry + 8, address])
}

/// Whether an ARM instruction reads, stores or writes the pc, or branches (so it can't run moved).
fn arm_uses_pc(instruction: u32) -> bool {
    let field = |shift: u32| (instruction >> shift) & 0xF;
    if instruction >> 28 == 0xF {
        return true; // unconditional space (blx, pld...): not moved
    }
    // movw/movt: bits 16-19 are part of the number.
    if instruction & 0x0FB0_0000 == 0x0300_0000 {
        return field(12) == 15;
    }
    match (instruction >> 25) & 7 {
        0b101 => true,                                                  // b, bl
        0b100 => field(16) == 15 || instruction & 0x8000 != 0,          // ldm/stm: base, or the pc in the list
        0b000 => field(16) == 15 || field(12) == 15 || field(0) == 15,  // data processing, bx, multiplies
        0b001 => field(16) == 15 || field(12) == 15,                    // data processing, immediate
        0b010 => field(16) == 15 || field(12) == 15,                    // loads and stores, immediate offset
        0b011 => field(16) == 15 || field(12) == 15 || field(0) == 15,  // loads and stores, register offset
        _ => field(16) == 15,                                           // coprocessor and VFP (vldr literal)
    }
}

fn wrap_thumb(entry: u32, size: u32, stand_in: u32) -> Result<u32, String> {
    // The jump's word must sit on a 4-byte boundary: a nop first when the entry doesn't.
    let patch: Vec<u16> = if entry % 4 == 0 {
        vec![THUMB_JUMP[0], THUMB_JUMP[1], stand_in as u16, (stand_in >> 16) as u16]
    } else {
        vec![THUMB_NOP, THUMB_JUMP[0], THUMB_JUMP[1], stand_in as u16, (stand_in >> 16) as u16]
    };
    let patch_bytes = patch.len() as u32 * 2;
    let memory = mem();
    // Whole instructions covering the patch.
    let mut moved: Vec<u16> = Vec::new();
    while (moved.len() as u32) * 2 < patch_bytes {
        let at = entry + moved.len() as u32 * 2;
        let first = memory.read_u16(at);
        let instruction = if thumb_wide(first) { vec![first, memory.read_u16(at + 2)] } else { vec![first] };
        if thumb_uses_pc(&instruction) {
            return Err(format!("its first instructions use the pc ({:04x?})", instruction));
        }
        moved.extend(instruction);
    }
    let moved_bytes = moved.len() as u32 * 2;
    if size < moved_bytes {
        return Err(format!("only {size} bytes long"));
    }
    let trampoline = crate::runtime::heap().memalign(4, moved_bytes + 12);
    if trampoline == 0 {
        return Err("no memory for its trampoline".into());
    }
    let back = entry + moved_bytes;
    let mut code = moved;
    if (trampoline + code.len() as u32 * 2) % 4 != 0 {
        code.push(THUMB_NOP);
    }
    code.extend([THUMB_JUMP[0], THUMB_JUMP[1], (back | 1) as u16, ((back | 1) >> 16) as u16]);
    for (i, half) in code.iter().enumerate() {
        memory.write_u16(trampoline + i as u32 * 2, *half);
    }
    for (i, half) in patch.iter().enumerate() {
        memory.write_u16(entry + i as u32 * 2, *half);
    }
    Ok(trampoline | 1)
}

/// A 32-bit Thumb instruction starts with 0b11101, 0b11110 or 0b11111.
fn thumb_wide(first: u16) -> bool {
    first >> 11 >= 0b11101
}

/// Whether a Thumb instruction reads the pc, branches, or opens an IT block (so it can't run moved).
fn thumb_uses_pc(instruction: &[u16]) -> bool {
    let first = instruction[0];
    if let [_, second] = instruction {
        if first >> 11 == 0b11110 && second & 0x8000 != 0 {
            return true; // branches and misc control
        }
        if first & 0xFFF0 == 0xE8D0 && second & 0xFFE0 == 0xF000 {
            return true; // tbb, tbh: branch tables after the instruction
        }
        if first >> 11 == 0b11110 {
            // Data processing with an immediate: only adr reads the pc (bits 0-3 are a number or "no register").
            return first & 0xFBFF == 0xF20F || first & 0xFBFF == 0xF2AF;
        }
        if first & 0xFE00 == 0xEA00 {
            return second & 0xF == 0xF; // shifted register: the pc as the shifted one (no Rn means mov/mvn)
        }
        return first & 0xF == 0xF; // anything else with the pc as its base register (literal loads...)
    }
    first >> 11 == 0b01001                                    // ldr literal
        || first >> 11 == 0b10100                             // adr
        || first >> 11 == 0b11100                             // b
        || (first >> 12 == 0b1101 && (first >> 9) & 7 != 0b111) // b<cond>
        || first & 0xF500 == 0xB100                           // cbz, cbnz
        || (first >> 8 == 0b1011_1111 && first & 0xF != 0)   // it
        || (first >> 10 == 0b010001 && ((first >> 3) & 0xF == 15 || ((first >> 4) & 8 | first & 7) == 15)) // hi-register ops, bx, blx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Instructions that use the pc (stored, as an address, or as a branch
    /// table) are not moved; common prologue instructions are.
    #[test]
    fn only_instructions_free_of_the_pc_are_moved() {
        // push {r4,pc}; ldr r0,[r1,pc]; b .; ldr r0,[pc,#4]
        for arm in [0xE92D_8010, 0xE791_000F, 0xEAFF_FFFE, 0xE59F_0004] {
            assert!(arm_uses_pc(arm), "{arm:#010x}");
        }
        // push {r4,lr}; vpush {d8}; mov r4,r0; sub sp,sp,#12; movw r0,#0xf000; str r0,[sp]
        for arm in [0xE92D_4010, 0xED2D_8B02, 0xE1A0_4000, 0xE24D_D00C, 0xE30F_0000, 0xE58D_0000] {
            assert!(!arm_uses_pc(arm), "{arm:#010x}");
        }
        // tbb [r0,r1]; tbh [r0,r1,lsl #1]; ldr r0,[pc]; bl; adr.w r0
        for thumb in [&[0xE8D0, 0xF001][..], &[0xE8D0, 0xF011], &[0x4800], &[0xF7FF, 0xFFFE], &[0xF20F, 0x0000]] {
            assert!(thumb_uses_pc(thumb), "{thumb:04x?}");
        }
        // push {r4,lr}; vpush {d8}; mov.w r0,#1; mov.w r4,r0; movw r0,#0xf000; sub sp,#8
        for thumb in [&[0xB510][..], &[0xED2D, 0x8B02], &[0xF04F, 0x0001], &[0xEA4F, 0x0400], &[0xF24F, 0x0000], &[0xB082]] {
            assert!(!thumb_uses_pc(thumb), "{thumb:04x?}");
        }
    }

    /// A moved ARM load of a number kept near the function reads the same
    /// number from beside the trampoline; other uses of the pc still refuse.
    #[test]
    fn moved_number_loads_read_the_same_number() {
        let numbers = |address: u32| match address {
            0x10F0 => 0xCAFE_BABE, // 0x1004 + 8 + 0xe4
            0x2000 => 0x1234_5678, // 0x2000 + 8 - 8
            _ => 0,
        };
        // push {r4-r10,lr}; ldr r4,[pc,#0xe4]
        let words = arm_trampoline([0xE92D_47F0, 0xE59F_40E4], 0x1000, numbers).unwrap();
        assert_eq!(words, [0xE92D_47F0, 0xE59F_4004, ARM_JUMP, 0x1008, 0xCAFE_BABE]);
        // ldr r0,[pc,#-8]; mov r4,r0
        let words = arm_trampoline([0xE51F_0008, 0xE1A0_4000], 0x2000, numbers).unwrap();
        assert_eq!(words, [0xE59F_0008, 0xE1A0_4000, ARM_JUMP, 0x2008, 0x1234_5678]);
        // ldr pc,[pc,#4]; ldrb r0,[pc,#4]
        for refused in [0xE59F_F004, 0xE5DF_0004] {
            assert!(arm_trampoline([refused, 0xE1A0_4000], 0x1000, numbers).is_err(), "{refused:#010x}");
        }
    }

    /// A position-independent address (`ldr r3,[pc,#n]` then `add r3,pc,r3`
    /// or `ldr r3,[pc,r3]`, as Rule the Kingdom's setters start) is worked
    /// out once: the trampoline loads the finished address (and reads
    /// through it for the second form).
    #[test]
    fn moved_position_independent_addresses_stay_the_same() {
        let numbers = |address: u32| if address == 0x1661A8 { 0x0049_8D94 } else { 0 };
        let address = 0x16619C + 8 + 0x0049_8D94;
        // ldr r3,[pc,#8]; add r3,pc,r3
        let words = arm_trampoline([0xE59F_3008, 0xE08F_3003], 0x166198, numbers).unwrap();
        assert_eq!(words, [0xE59F_3008, ARM_NOP, ARM_JUMP, 0x1661A0, address]);
        // ldr r3,[pc,#8]; ldr r3,[pc,r3]: the trampoline reads through the address (ldr r3,[r3])
        let words = arm_trampoline([0xE59F_3008, 0xE79F_3003], 0x166198, numbers).unwrap();
        assert_eq!(words, [0xE59F_3008, 0xE593_3000, ARM_JUMP, 0x1661A0, address]);
        // ... but not when the second adds the pc to another register than the one loaded
        assert!(arm_trampoline([0xE59F_3008, 0xE08F_2003], 0x166198, numbers).is_err());
    }
}
