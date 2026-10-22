//! 32-bit iPhone programs (Mach-O, ARMv7) on the translator: loads one
//! program into guest memory at the addresses guest32-image planned, applies
//! its rebases, points every import at a stand-in, builds the start state
//! dyld would leave, and runs it. This first step stops at the first import
//! call and names it; no iPhone functions are provided yet.
//!
//! Apple differs from Android in three places, set through
//! `runtime::PlatformHooks`:
//! - system calls: `svc #0x80` with the number in r12 (0 = indirect, number
//!   in r0), arguments r0-r6 then r8, and errors flagged by the carry flag
//!   (xnu libsyscall SYS.h and bsd/dev/arm/systemcalls.c), not Linux's r7
//!   and negative results;
//! - the thread pointer: Apple's libSystem reads TPIDRURO and masks the low 2
//!   bits (they may hold the CPU number) to find its thread-specific data.
//!   The runtime's per-thread block is 8-aligned, so the Android value is
//!   already a valid, masked Apple value; it is kept;
//! - no Unicorn: every instruction runs translated, or the run stops and says which.
//!
//! Imports: binds can't tell a function from a variable. Each import gets an
//! 8-byte cell in the stub area that is both: called, the dispatcher runs its
//! stand-in by address; read as data (`_environ`, `___stack_chk_guard`, a
//! class reference), it holds zeros, since on this platform no stub code is
//! written there. A program writing more than 8 bytes through one would
//! spill into its neighbour's cell (harmless to dispatch, but not real data).

use std::collections::HashMap;
use std::sync::Mutex;

use guest32_image::{Bind32, BindSource, DylibKind, Entry32, ExportKind, FixupKind, Image32, LibraryRef, LoadPlan, SliceChoice};

use crate::mem::{self, MemoryLayout, PAGE};
use crate::runtime::{self, Args, GuestCpu, Handler, PlatformHooks, Ret, Uc};

/// Where 32-bit iPhone programs keep the stand-ins and return traps: above
/// the runtime data, below the stack, far from any program's own addresses.
pub const STUB_BASE: u32 = 0xD000_0000;

pub static APPLE32: PlatformHooks = PlatformHooks {
    name: "iPhone (32-bit)",
    resolve_stub: resolve_stub,
    syscall: apple_syscall,
    thread_pointer: thread_pointer,
    unicorn_allowed: false,
};

/// libSystem masks the low 2 bits of TPIDRURO; the runtime's block is
/// 8-aligned, so this is the same value Android code gets.
fn thread_pointer(tls_block: u32) -> u32 {
    tls_block & !3
}

// ---- system calls ----

/// One Apple ARM system call as the registers describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleCall {
    /// A BSD system call (`SYS_*` number) with its argument words in order.
    Unix { number: u32, args: [u32; 7] },
    /// A Mach trap (negative r12, e.g. -31 mach_msg_trap).
    Mach(i32),
    /// A machine-dependent call (r12 = 0x80000000, selector in r3).
    Machine(u32),
}

/// Where an indirect call's arguments continue on the stack, past r1-r6.
const INDIRECT_STACK_ARGUMENTS: u32 = 28;

/// Reads the call from r0-r15 (index = register number). armv7 passes
/// arguments in r0-r6 (r8 is armv7k only); an indirect call (r12 = 0) has
/// its number in r0, arguments in r1-r6 and the rest on the stack from
/// sp + 28, read through `stack_word`.
pub fn decode_syscall(registers: &[u32; 16], stack_word: impl Fn(u32) -> u32) -> AppleCall {
    let number = registers[12];
    if number == 0x8000_0000 {
        return AppleCall::Machine(registers[3]);
    }
    if (number as i32) < 0 {
        return AppleCall::Mach(number as i32);
    }
    let mut args = [0u32; 7];
    if number == 0 {
        args[..6].copy_from_slice(&registers[1..7]);
        args[6] = stack_word(registers[13].wrapping_add(INDIRECT_STACK_ARGUMENTS));
        return AppleCall::Unix { number: registers[0], args };
    }
    args.copy_from_slice(&registers[..7]);
    AppleCall::Unix { number, args }
}

/// The host NZCV word (C = bit 29) with the carry set or clear.
pub fn with_carry(nzcv: u64, carry: bool) -> u64 {
    if carry {
        nzcv | 1 << 29
    } else {
        nzcv & !(1 << 29)
    }
}

/// How xnu returns from a system call: success puts the values in r0/r1 and
/// clears carry; failure puts the errno in r0, 0 in r1 and sets carry.
/// Returns (r0, r1, carry).
pub fn syscall_return(outcome: Result<(u32, u32), u32>) -> (u32, u32, bool) {
    match outcome {
        Ok((r0, r1)) => (r0, r1, false),
        Err(errno) => (errno, 0, true),
    }
}

const SYS_EXIT: u32 = 1;
const SYS_WRITE: u32 = 4;
const EBADF: u32 = 9;
const EFAULT: u32 = 14;

fn apple_syscall(uc: &mut Uc, resume: u32) {
    runtime::clear_exclusive(uc);
    let immediate = if resume & 1 == 1 {
        u32::from(runtime::mem().read_u16((resume & !1) - 2) & 0xFF)
    } else {
        runtime::mem().read_u32(resume - 4) & 0x00FF_FFFF
    };
    let registers: [u32; 16] = std::array::from_fn(|i| if i < 15 { uc.reg(i) } else { resume & !1 });
    if immediate != 0x80 {
        return stop(uc, format!("svc #{immediate:#x} at {:#010x} is not an Apple system call", resume & !1));
    }
    let layout = mem::layout();
    let memory = runtime::mem();
    let call = decode_syscall(&registers, |address| {
        if layout.mapped(address, u64::from(address) + 4) {
            memory.read_u32(address)
        } else {
            0
        }
    });
    let outcome = match call {
        AppleCall::Unix { number: SYS_EXIT, args } => {
            crate::log::line(&format!("[ios32] exit({}) system call", args[0] as i32));
            uc.get_data_mut().exit_code = Some(args[0] as i32);
            return;
        }
        AppleCall::Unix { number: SYS_WRITE, args } => write(args[0], args[1], args[2]),
        AppleCall::Unix { number, .. } => {
            return stop(uc, format!("Apple system call {number} at {:#010x} is not handled yet", resume & !1));
        }
        AppleCall::Mach(trap) => return stop(uc, format!("Mach trap {trap} at {:#010x} is not handled yet", resume & !1)),
        AppleCall::Machine(selector) => {
            return stop(uc, format!("machine-dependent call {selector} at {:#010x} is not handled yet", resume & !1));
        }
    };
    let (r0, r1, carry) = syscall_return(outcome);
    uc.set_reg(0, r0);
    uc.set_reg(1, r1);
    match uc.get_data_mut().native.as_mut().filter(|native| native.active) {
        Some(native) => native.state.nzcv = with_carry(native.state.nzcv, carry),
        None => stop(uc, "Apple system call outside translated code".into()),
    }
}

/// write(2) to the host's standard output or error; other files aren't open yet.
fn write(fd: u32, buffer: u32, length: u32) -> Result<(u32, u32), u32> {
    use std::io::Write;
    if fd != 1 && fd != 2 {
        return Err(EBADF);
    }
    if !mem::layout().mapped(buffer, u64::from(buffer) + u64::from(length)) {
        return Err(EFAULT);
    }
    let memory = runtime::mem();
    let bytes = memory.bytes(buffer, length as usize);
    let written = if fd == 1 {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes).and_then(|_| out.flush())
    } else {
        std::io::stderr().lock().write_all(bytes)
    };
    written.map(|_| (length, 0)).map_err(|_| EFAULT)
}

fn stop(uc: &mut Uc, message: String) {
    if uc.get_data().fault.is_none() {
        uc.get_data_mut().fault = Some(message);
    }
}

// ---- imports ----

/// The first import the program called: (name, r0-r3, return address).
static FIRST_IMPORT: Mutex<Option<(String, [u32; 4], u32)>> = Mutex::new(None);

pub fn first_import() -> Option<(String, [u32; 4], u32)> {
    FIRST_IMPORT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Every import gets the trap for now: no iPhone function exists yet.
fn resolve_stub(_name: &str) -> Option<Handler> {
    Some(import_trap)
}

/// Records the call and stops the program there.
fn import_trap(uc: &mut Uc, args: &mut Args) -> Ret {
    let name = uc.get_data().current_call.to_string();
    let lr = uc.reg(runtime::LR);
    crate::log::line(&format!("[ios32] import call: {name} (lr {lr:#010x})"));
    FIRST_IMPORT.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert((name.clone(), args.registers, lr));
    stop(uc, format!("stopped at the first import call: {name}"));
    Ret::Void
}

// ---- loading ----

/// A program parsed and planned, before guest memory exists.
pub struct Program {
    pub file: Vec<u8>,
    pub image: Image32,
    pub plan: LoadPlan,
    pub layout: MemoryLayout,
}

/// What loading did, for the report.
#[derive(Default)]
pub struct Loaded {
    pub rebases: usize,
    pub binds_to_stubs: usize,
    pub binds_to_self: usize,
    /// Weak imports bound to 0.
    pub weak_missing: usize,
    pub stubs: usize,
}

pub fn slice_choice(arch: Option<&str>) -> Result<SliceChoice, String> {
    use guest32_image::ArmSubtype::*;
    let preference = match arch {
        None => return Ok(SliceChoice::default()),
        Some("armv7") => vec![V7],
        Some("armv7s") => vec![V7S],
        Some("armv6") => vec![V6],
        Some(other) => return Err(format!("unknown --arch {other} (armv7, armv7s or armv6)")),
    };
    Ok(SliceChoice { preference })
}

/// Reads and plans the program and works out the memory layout around it.
/// Nothing is mapped yet.
pub fn prepare(path: &std::path::Path, choice: &SliceChoice) -> Result<Program, String> {
    let file = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = Image32::parse_with(&file, choice).map_err(|e| e.to_string())?;
    // The program goes below the heap; everything else the runtime uses is above it.
    let window = guest32_image::AddressRange::new(u64::from(PAGE), u64::from(mem::HEAP_BASE));
    let planner = guest32_image::MemoryLayout {
        mapped: window,
        code: window,
        reserved: Vec::new(),
        page_size: PAGE,
        allow_slide: true,
    };
    let plan = guest32_image::plan_load(&image, &planner).map_err(|e| e.to_string())?;
    let layout = MemoryLayout::with_program(&program_regions(&image, &plan), STUB_BASE, mem::STUB_SIZE)?;
    Ok(Program { file, image, plan, layout })
}

/// The program's pages as layout regions: one per segment, merged where
/// segments share a page (code if either holds code).
fn program_regions(image: &Image32, plan: &LoadPlan) -> Vec<(u32, u32, bool)> {
    let mut pages: Vec<(u32, u32, bool)> = plan
        .segments
        .iter()
        .map(|s| (s.pages.start as u32, s.pages.end as u32, image.segments[s.segment].executable()))
        .collect();
    pages.sort_unstable();
    let mut merged: Vec<(u32, u32, bool)> = Vec::new();
    for (start, end, code) in pages {
        match merged.last_mut() {
            Some(last) if start < last.1 => {
                last.1 = last.1.max(end);
                last.2 |= code;
            }
            _ => merged.push((start, end, code)),
        }
    }
    merged
}

/// Copies the segments in, applies rebases, and points binds at their
/// targets. Needs runtime::init done with `program.layout`.
pub fn load(program: &Program) -> Result<Loaded, String> {
    let memory = runtime::mem();
    let slice = program.image.slice_bytes(&program.file).map_err(|e| e.to_string())?;
    for segment in &program.plan.segments {
        let start = segment.file_offset as usize;
        let bytes = slice
            .get(start..start + segment.file_size as usize)
            .ok_or_else(|| format!("segment {} lies outside the file", segment.name))?;
        // Fresh guest memory is zero, which covers each segment's zero-fill tail.
        memory.write_bytes(segment.start, bytes);
    }
    let slide = program.plan.slide;
    let exports = exported_definitions(program)?;
    let mut loaded = Loaded::default();
    for fixup in &program.plan.fixups {
        let at = fixup.guest_address;
        match fixup.action {
            guest32_image::FixupAction::Rebase(_) => {
                let value = memory.read_u32(at);
                // A pc-relative word to something outside the program moves the other way.
                let fixed = if fixup.kind == FixupKind::TextPcrel32 { value.wrapping_sub(slide) } else { value.wrapping_add(slide) };
                memory.write_u32(at, fixed);
                loaded.rebases += 1;
            }
            guest32_image::FixupAction::Bind(index) => {
                let bind = &program.image.binds[index];
                let weak_library = bind.library.and_then(|library| program.image.dylib(library)).is_some_and(|d| d.kind == DylibKind::Weak);
                let target = match bind_target(&exports, bind, weak_library) {
                    BindTarget::Program(address) => {
                        loaded.binds_to_self += 1;
                        address
                    }
                    BindTarget::Missing => {
                        loaded.weak_missing += 1;
                        0
                    }
                    BindTarget::StandIn => {
                        loaded.binds_to_stubs += 1;
                        runtime::stub_for(&bind.symbol)
                    }
                };
                let value = target.wrapping_add(bind.addend as u32);
                let word = if fixup.kind == FixupKind::TextPcrel32 { value.wrapping_sub(at.wrapping_add(4)) } else { value };
                memory.write_u32(at, word);
            }
        }
    }
    loaded.stubs = runtime::stubs().len();
    // Bytes of segments that can't be written never change: translated code may treat them as constants.
    for segment in &program.plan.segments {
        if !program.image.segments[segment.segment].writable() {
            mem::add_fixed(segment.start, segment.start + segment.size);
        }
    }
    Ok(loaded)
}

/// Where one bind points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindTarget {
    /// The program's own definition (slid, bit 0 = Thumb).
    Program(u32),
    /// A weak import nothing provides: dyld writes 0 (plus the addend).
    Missing,
    /// A stand-in in the stub area (a zeroed cell when read as data).
    StandIn,
}

/// Resolves one bind. Weak binds coalesce to the program's own exported
/// definition, as dyld picks the main program's first; binds naming this
/// image or a flat or weak lookup also look there. A weak import (its flag,
/// or a library loaded with LC_LOAD_WEAK_DYLIB) that nothing provides is
/// missing: no iPhone function exists yet, so every one is. Everything else
/// goes to a stand-in.
pub fn bind_target(exports: &HashMap<String, u32>, bind: &Bind32, weak_library: bool) -> BindTarget {
    let searches_self = !matches!((bind.source, bind.library), (BindSource::Normal | BindSource::Lazy, Some(LibraryRef::Dylib(_))));
    if searches_self {
        if let Some(&address) = exports.get(&bind.symbol) {
            return BindTarget::Program(address);
        }
    }
    if bind.weak_import || weak_library {
        return BindTarget::Missing;
    }
    BindTarget::StandIn
}

/// The program's exported definitions, slid, bit 0 set for Thumb code: the
/// export trie (all a stripped program keeps), plus external symbol-table
/// definitions it does not list.
fn exported_definitions(program: &Program) -> Result<HashMap<String, u32>, String> {
    const N_PEXT: u8 = 0x10;
    let image = &program.image;
    let slide = program.plan.slide;
    let mut exports: HashMap<String, u32> = image
        .symbols
        .iter()
        .filter(|s| s.is_section_defined() && s.is_external() && s.n_type & N_PEXT == 0)
        .map(|s| (s.name.clone(), s.value.wrapping_add(slide) | u32::from(s.is_thumb_definition())))
        .collect();
    let header = image.header_address().unwrap_or(0).wrapping_add(slide);
    for export in image.exports(&program.file).map_err(|e| e.to_string())? {
        let address = match (export.kind, export.offset, &export.reexport, export.resolver) {
            (ExportKind::Regular, Some(offset), None, None) => header.wrapping_add(offset),
            (ExportKind::Absolute, Some(value), None, None) => value,
            // Thread-local, re-exported and resolver exports need runtime support
            // that doesn't exist yet; binds to them go to stand-ins.
            _ => continue,
        };
        exports.insert(export.name, address);
    }
    Ok(exports)
}

// ---- starting ----

/// The words dyld leaves at sp for a program's own `start` (LC_UNIXTHREAD),
/// from sp upward: argc, argv[], NULL, envp[], NULL, apple[], NULL.
pub fn startup_words(argv: &[u32], envp: &[u32], apple: &[u32]) -> Vec<u32> {
    let mut words = vec![argv.len() as u32];
    for list in [argv, envp, apple] {
        words.extend_from_slice(list);
        words.push(0);
    }
    words
}

/// How the program was started and where.
pub struct Start {
    pub pc: u32,
    pub registers: [u32; 13],
    pub sp: u32,
    pub kind: &'static str,
}

/// Builds the start state dyld would leave. `argv0` is the program's path.
/// LC_MAIN: called like main(argc, argv, envp, apple), returning to the trap.
/// LC_UNIXTHREAD: the thread state's registers, argc at [sp], then argv,
/// envp and apple, jumping to pc in ARM or Thumb as the state says.
pub fn start_state(uc: &mut Uc, program: &Program, argv0: &str) -> Result<Start, String> {
    let entry = program.plan.entry.as_ref().ok_or("the program has no entry point")?;
    let (path, apple_text) = {
        let mut shared = runtime::shared();
        (shared.misc_cstr(argv0), shared.misc_cstr(&format!("executable_path={argv0}")))
    };
    let words = startup_words(&[path], &[], &[apple_text]);
    let stack_top = uc.get_data().stack_top;
    let block = (stack_top - 256 - words.len() as u32 * 4) & !15;
    let memory = runtime::mem();
    for (i, word) in words.iter().enumerate() {
        memory.write_u32(block + i as u32 * 4, *word);
    }
    let argv = block + 4;
    let envp = argv + 8; // argv[0], NULL
    let apple = envp + 4; // NULL
    match &entry.source {
        Entry32::Main { .. } => {
            let mut registers = [0u32; 13];
            registers[..4].copy_from_slice(&[1, argv, envp, apple]);
            Ok(Start { pc: entry.address, registers, sp: (block - 64) & !15, kind: "LC_MAIN" })
        }
        Entry32::UnixThread(state) => {
            if state.sp != 0 {
                crate::log::line(&format!("[ios32] thread state sp {:#x} replaced by the start stack", state.sp));
            }
            let pc = entry.address | u32::from(entry.thumb);
            Ok(Start { pc, registers: state.r, sp: block, kind: "LC_UNIXTHREAD" })
        }
    }
}
