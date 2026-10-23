//! 32-bit iPhone programs (Mach-O, ARMv7) on the translator: loads one
//! program into guest memory at the addresses guest32-image planned, applies
//! its rebases, points its imports at what the embedder registered (see
//! "imports" below), runs its start-up functions, builds the start state
//! dyld would leave, and runs it. A call to a function nothing provides
//! stops the program and names it.
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
//! Imports: binds can't tell a function from a variable, so what the
//! embedder registers decides (see "imports" below). Stand-ins hold no code
//! on this platform (the dispatcher runs them by address), so their bytes
//! read as zeros; an import nothing provides that is only reached through
//! pointers gets a row of eight stand-ins: 64 zero bytes to read and write,
//! and a named stop if it turns out to be a function and is called.

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
    packed_pairs: true,
    errno_from_host: darwin_errno,
    library_import: Some(library_import),
};

/// Darwin's errno number for a Linux one. 1-34 are the same except 11
/// (Linux EAGAIN, Darwin EDEADLK); the rest differ (sys/errno.h).
pub fn darwin_errno(linux: i32) -> i32 {
    match linux {
        11 => 35,   // EAGAIN / EWOULDBLOCK
        35 => 11,   // EDEADLK
        36 => 63,   // ENAMETOOLONG
        37 => 77,   // ENOLCK
        38 => 78,   // ENOSYS
        39 => 66,   // ENOTEMPTY
        40 => 62,   // ELOOP
        42 => 91,   // ENOMSG
        43 => 90,   // EIDRM
        61 => 96,   // ENODATA
        62 => 101,  // ETIME
        71 => 100,  // EPROTO
        74 => 94,   // EBADMSG
        75 => 84,   // EOVERFLOW
        84 => 92,   // EILSEQ
        88 => 38,   // ENOTSOCK
        89 => 39,   // EDESTADDRREQ
        90 => 40,   // EMSGSIZE
        91 => 41,   // EPROTOTYPE
        92 => 42,   // ENOPROTOOPT
        93 => 43,   // EPROTONOSUPPORT
        94 => 44,   // ESOCKTNOSUPPORT
        95 => 45,   // ENOTSUP / EOPNOTSUPP
        96 => 46,   // EPFNOSUPPORT
        97 => 47,   // EAFNOSUPPORT
        98 => 48,   // EADDRINUSE
        99 => 49,   // EADDRNOTAVAIL
        100 => 50,  // ENETDOWN
        101 => 51,  // ENETUNREACH
        102 => 52,  // ENETRESET
        103 => 53,  // ECONNABORTED
        104 => 54,  // ECONNRESET
        105 => 55,  // ENOBUFS
        106 => 56,  // EISCONN
        107 => 57,  // ENOTCONN
        108 => 58,  // ESHUTDOWN
        109 => 59,  // ETOOMANYREFS
        110 => 60,  // ETIMEDOUT
        111 => 61,  // ECONNREFUSED
        112 => 64,  // EHOSTDOWN
        113 => 65,  // EHOSTUNREACH
        114 => 37,  // EALREADY
        115 => 36,  // EINPROGRESS
        116 => 70,  // ESTALE
        122 => 69,  // EDQUOT
        125 => 89,  // ECANCELED
        130 => 105, // EOWNERDEAD
        131 => 104, // ENOTRECOVERABLE
        other => other,
    }
}

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
//
// The embedder (gamebridge-ios) registers what it provides before the
// program is loaded: C functions by name (`register_function`) and data by
// name (`register_data`). Names are C names: the Mach-O symbol without its
// one leading underscore (`_malloc` -> `malloc`, `__ZdlPv` -> `_ZdlPv`,
// `_OBJC_CLASS_$_NSObject` -> `OBJC_CLASS_$_NSObject`), which is also what
// 32-bit ELF guest libraries (libz, libc++) call them, so both share one
// stand-in per function.
//
// At load time each import goes to, in this order: a definition in the
// program itself, an export of a loaded guest library, registered data (its
// own guest memory, made once), a registered function (a stand-in), and
// otherwise: a name some lazy bind uses is a function no one provides yet
// (a stand-in that stops the program when called and names it); a name only
// bound through non-lazy pointers or data words is data no one provides yet
// (64 zero bytes of its own, listed).

/// Makes a data import's guest object once and returns its guest address.
pub type DataMaker = Box<dyn Fn() -> u32 + Send + Sync>;

#[derive(Default)]
struct Registry {
    functions: HashMap<String, Handler>,
    data: HashMap<String, std::sync::Arc<DataMaker>>,
    /// Data made so far: name -> guest address.
    made: HashMap<String, u32>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn registry<R>(work: impl FnOnce(&mut Registry) -> R) -> R {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    work(registry.get_or_insert_with(Registry::default))
}

/// The C name of a Mach-O symbol: without its one leading underscore.
pub fn c_name(symbol: &str) -> &str {
    symbol.strip_prefix('_').unwrap_or(symbol)
}

/// Provides C function `name` (a C name, see above). A later registration
/// of the same name replaces an earlier one; only stand-ins made after it see it.
pub fn register_function(name: &str, handler: Handler) {
    registry(|r| r.functions.insert(name.to_string(), handler));
}

/// Provides data import `name`: `make` builds its guest object (once, on
/// the first bind or `data_address` call) and returns its address.
pub fn register_data(name: &str, make: DataMaker) {
    registry(|r| r.data.insert(name.to_string(), std::sync::Arc::new(make)));
}

/// The registered handler for function `name`.
pub fn function(name: &str) -> Option<Handler> {
    registry(|r| r.functions.get(name).copied())
}

pub fn provides_function(name: &str) -> bool {
    registry(|r| r.functions.contains_key(name))
}

pub fn provides_data(name: &str) -> bool {
    registry(|r| r.data.contains_key(name))
}

/// The guest address of registered data `name`, made on first use. None:
/// nothing provides it.
pub fn data_address(name: &str) -> Option<u32> {
    let maker = registry(|r| match r.made.get(name) {
        Some(&address) => Err(address),
        None => Ok(r.data.get(name).cloned()),
    });
    let maker = match maker {
        Err(address) => return Some(address),
        Ok(maker) => maker?,
    };
    // Made outside the registry lock: a maker may ask for other data.
    let address = maker();
    Some(registry(|r| *r.made.entry(name.to_string()).or_insert(address)))
}

/// An import only reached through pointers that nothing provides: 64 zero
/// bytes of its own, made of stand-in slots, so if it is really a function
/// whose address was taken, calling it stops the program and names it. Listed.
pub fn unknown_data(name: &str) -> u32 {
    if let Some(&address) = runtime::shared().state.data_imports.get(name) {
        return address;
    }
    let address = runtime::stub_block(name, 8);
    let mut shared = runtime::shared();
    if !shared.state.data_imports.contains_key(name) {
        shared.state.unknown_data.push(name.to_string());
    }
    *shared.state.data_imports.entry(name.to_string()).or_insert(address)
}

static KEEP_GOING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Missing functions return 0 and the program goes on (for exploring), instead of stopping.
pub fn set_keep_going(keep_going: bool) {
    KEEP_GOING.store(keep_going, std::sync::atomic::Ordering::Relaxed);
}

/// Functions no one provides that the program called, in order: (name, r0-r3, return address).
static CALLED_MISSING: Mutex<Vec<(String, [u32; 4], u32)>> = Mutex::new(Vec::new());

/// The first function the program called that no one provides.
pub fn first_missing_call() -> Option<(String, [u32; 4], u32)> {
    CALLED_MISSING.lock().unwrap_or_else(|e| e.into_inner()).first().cloned()
}

/// Every function the program called that no one provides, in the order first called.
pub fn missing_calls() -> Vec<String> {
    CALLED_MISSING.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(name, _, _)| name.clone()).collect()
}

/// Stand-ins for ELF guest libraries' imports that use Android's handler
/// (those libraries are built against bionic) are named with this prefix.
const BIONIC_PREFIX: &str = "bionic:";

fn resolve_stub(name: &str) -> Option<Handler> {
    if let Some(bionic) = name.strip_prefix(BIONIC_PREFIX) {
        return Some(crate::imports::handler_for(bionic).unwrap_or(missing_function));
    }
    Some(registry(|r| r.functions.get(name).copied()).unwrap_or(missing_function))
}

/// A function no one provides: stops the program and names it (or, when
/// keeping going, returns 0).
fn missing_function(uc: &mut Uc, args: &mut Args) -> Ret {
    let name = uc.get_data().current_call.to_string();
    let lr = uc.reg(runtime::LR);
    let first_time = {
        let mut called = CALLED_MISSING.lock().unwrap_or_else(|e| e.into_inner());
        let first_time = !called.iter().any(|(called, _, _)| *called == name);
        if first_time {
            called.push((name.clone(), args.registers, lr));
        }
        first_time
    };
    if KEEP_GOING.load(std::sync::atomic::Ordering::Relaxed) {
        if first_time {
            crate::log::line(&format!("[ios32] {name} is provided by nothing yet; returning 0 (lr {lr:#010x})"));
        }
        return Ret::U32(0);
    }
    crate::log::line(&format!("[ios32] {name} is provided by nothing yet (called from {lr:#010x})"));
    stop(uc, format!("the program called {name}, which nothing provides yet (from {lr:#010x})"));
    Ret::Void
}

/// Imports of Apple-ABI guest libraries that nothing provided at load time.
static LIBRARY_MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Imports of Apple-ABI guest libraries (libc++) that nothing provides, in load order.
pub fn missing_library_imports() -> Vec<String> {
    LIBRARY_MISSING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// For 32-bit ELF guest libraries loaded into an iPhone program.
///
/// Built for Apple's ABI (`apple`: the C++ runtime, compiled with Darwin
/// headers): every name goes to what the embedder registered, exactly like
/// the program's own imports; nothing registered: weak ones are 0, the rest
/// listed (`missing_library_imports`) and stop the program if called.
///
/// Built against bionic (the built-in libz): their C library calls keep
/// bionic's layouts (open flags, off_t, struct stat, errno numbers), so names
/// Android's handlers know go there (own stand-ins, "bionic:name") and data
/// Android knows comes from Android's table; other names as above.
fn library_import(name: &str, object: bool, weak: bool, apple: bool) -> u32 {
    if !apple {
        if crate::imports::known_data_import(name) {
            return crate::imports::data_import(name);
        }
        if !object && crate::imports::handler_for(name).is_some() {
            return runtime::stub_for(&format!("{BIONIC_PREFIX}{name}"));
        }
    }
    if let Some(address) = data_address(name) {
        return address;
    }
    if provides_function(name) {
        return runtime::stub_for(name);
    }
    if weak {
        return 0;
    }
    if apple {
        let mut missing = LIBRARY_MISSING.lock().unwrap_or_else(|e| e.into_inner());
        if !missing.iter().any(|known| known == name) {
            missing.push(name.to_string());
        }
    }
    if object {
        return unknown_data(name);
    }
    runtime::stub_for(name)
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
    /// Binds to exports of loaded guest libraries (libz, libc++).
    pub binds_to_libraries: usize,
    /// Binds to registered or unknown data.
    pub binds_to_data: usize,
    /// Weak imports bound to 0.
    pub weak_missing: usize,
    pub stubs: usize,
    /// Imported functions nothing provides (C names, sorted).
    pub missing_functions: Vec<String>,
    /// Imported data nothing provides (given 64 zero bytes each; C names, sorted).
    pub missing_data: Vec<String>,
    /// Weak imports left 0.
    pub weak_left_empty: Vec<String>,
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
    let lazily_bound: std::collections::HashSet<&str> =
        program.image.binds.iter().filter(|bind| bind.source == BindSource::Lazy).map(|bind| c_name(&bind.symbol)).collect();
    let mut loaded = Loaded::default();
    let mut missing_functions = std::collections::BTreeSet::new();
    let mut missing_data = std::collections::BTreeSet::new();
    let mut weak_left_empty = std::collections::BTreeSet::new();
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
                let name = c_name(&bind.symbol);
                let library_export = runtime::shared().guest_exports.get(name).copied();
                let provided = Provided {
                    library: library_export,
                    data: provides_data(name),
                    function: provides_function(name),
                    called_lazily: lazily_bound.contains(name),
                };
                let target = match bind_target(&exports, bind, weak_library, &provided) {
                    BindTarget::Program(address) => {
                        loaded.binds_to_self += 1;
                        address
                    }
                    BindTarget::Library(address) => {
                        loaded.binds_to_libraries += 1;
                        address
                    }
                    BindTarget::Missing => {
                        loaded.weak_missing += 1;
                        weak_left_empty.insert(name.to_string());
                        0
                    }
                    BindTarget::Data => {
                        loaded.binds_to_data += 1;
                        data_address(name).ok_or_else(|| format!("data import {name} was registered but made nothing"))?
                    }
                    BindTarget::UnknownData => {
                        loaded.binds_to_data += 1;
                        missing_data.insert(name.to_string());
                        unknown_data(name)
                    }
                    BindTarget::StandIn => {
                        loaded.binds_to_stubs += 1;
                        if !provided.function {
                            missing_functions.insert(name.to_string());
                        }
                        runtime::stub_for(name)
                    }
                };
                let value = target.wrapping_add(bind.addend as u32);
                let word = if fixup.kind == FixupKind::TextPcrel32 { value.wrapping_sub(at.wrapping_add(4)) } else { value };
                memory.write_u32(at, word);
            }
        }
    }
    loaded.stubs = runtime::stubs().len();
    loaded.missing_functions = missing_functions.into_iter().collect();
    loaded.missing_data = missing_data.into_iter().collect();
    loaded.weak_left_empty = weak_left_empty.into_iter().collect();
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
    /// An export of a loaded guest library.
    Library(u32),
    /// Registered data (`register_data`).
    Data,
    /// Data nothing provides: 64 zero bytes of its own.
    UnknownData,
    /// A weak import nothing provides: dyld writes 0 (plus the addend).
    Missing,
    /// A function's stand-in in the stub area (registered, or one that
    /// stops the program when called).
    StandIn,
}

/// What the bridge has for one imported name.
#[derive(Clone, Copy, Debug, Default)]
pub struct Provided {
    /// A loaded guest library exports it at this address.
    pub library: Option<u32>,
    /// Registered as data.
    pub data: bool,
    /// Registered as a function.
    pub function: bool,
    /// Some lazy bind uses the name: the program calls it, so it is a function.
    pub called_lazily: bool,
}

/// Resolves one bind. Weak binds coalesce to the program's own exported
/// definition, as dyld picks the main program's first; binds naming this
/// image or a flat or weak lookup also look there. Then a guest library's
/// export, registered data, a registered function. A weak import (its flag,
/// or a library loaded with LC_LOAD_WEAK_DYLIB) that nothing provides is
/// left 0. Otherwise a name the program calls lazily is a function nothing
/// provides yet (a stand-in that stops it), and a name only reached through
/// pointers or data words is data nothing provides yet.
pub fn bind_target(exports: &HashMap<String, u32>, bind: &Bind32, weak_library: bool, provided: &Provided) -> BindTarget {
    let searches_self = !matches!((bind.source, bind.library), (BindSource::Normal | BindSource::Lazy, Some(LibraryRef::Dylib(_))));
    if searches_self {
        if let Some(&address) = exports.get(&bind.symbol) {
            return BindTarget::Program(address);
        }
    }
    if let Some(address) = provided.library {
        return BindTarget::Library(address);
    }
    if provided.data {
        return BindTarget::Data;
    }
    if provided.function {
        return BindTarget::StandIn;
    }
    if bind.weak_import || weak_library {
        return BindTarget::Missing;
    }
    if provided.called_lazily {
        BindTarget::StandIn
    } else {
        BindTarget::UnknownData
    }
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
    /// The argv, envp and apple lists on the start stack (start-up functions get them too).
    pub argv: u32,
    pub envp: u32,
    pub apple: u32,
}

/// The program's start-up functions (`__mod_init_func` sections: C++ static
/// constructors, `__attribute__((constructor))`), in order. Read from guest
/// memory, so call after `load`.
pub fn initializers(program: &Program) -> Vec<u32> {
    const S_MOD_INIT_FUNC_POINTERS: u8 = 0x9;
    let memory = runtime::mem();
    let slide = program.plan.slide;
    program
        .image
        .segments
        .iter()
        .flat_map(|segment| &segment.sections)
        .filter(|section| section.section_type() == S_MOD_INIT_FUNC_POINTERS)
        .flat_map(|section| {
            let start = section.address.wrapping_add(slide);
            (0..section.size / 4).map(move |i| start + i * 4)
        })
        .map(|slot| memory.read_u32(slot))
        .filter(|&function| function != 0)
        .collect()
}

/// Runs the start-up functions as dyld does before the entry point:
/// f(argc, argv, envp, apple, &ProgramVars).
pub fn run_initializers(uc: &mut Uc, program: &Program, start: &Start) -> Result<usize, String> {
    let functions = initializers(program);
    if functions.is_empty() {
        return Ok(0);
    }
    // struct ProgramVars { mach_header*, int* NXArgcPtr, char*** NXArgvPtr, char*** environPtr, char** __prognamePtr }
    let header = program.image.header_address().unwrap_or(0).wrapping_add(program.plan.slide);
    let vars = {
        let mut shared = runtime::shared();
        let vars = shared.misc_alloc(5 * 4 + 4 * 4, 4);
        let words = [header, vars + 20, vars + 24, vars + 28, vars + 32];
        let memory = runtime::mem();
        for (i, word) in words.iter().enumerate() {
            memory.write_u32(vars + i as u32 * 4, *word);
        }
        let program_name = memory.read_u32(start.argv);
        for (i, word) in [1, start.argv, start.envp, program_name].iter().enumerate() {
            memory.write_u32(vars + 20 + i as u32 * 4, *word);
        }
        vars
    };
    for (i, &function) in functions.iter().enumerate() {
        runtime::call_guest(uc, function, &[1, start.argv, start.envp, start.apple, vars])
            .map_err(|error| format!("start-up function {} of {} ({function:#010x}) failed: {error}", i + 1, functions.len()))?;
    }
    Ok(functions.len())
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
            Ok(Start { pc: entry.address, registers, sp: (block - 64) & !15, kind: "LC_MAIN", argv, envp, apple })
        }
        Entry32::UnixThread(state) => {
            if state.sp != 0 {
                crate::log::line(&format!("[ios32] thread state sp {:#x} replaced by the start stack", state.sp));
            }
            let pc = entry.address | u32::from(entry.thumb);
            Ok(Start { pc, registers: state.r, sp: block, kind: "LC_UNIXTHREAD", argv, envp, apple })
        }
    }
}
