//! 32-bit iPhone games (armv7 only, no arm64 part): their code runs on the
//! arm32-bridge translator inside this process, while this layer keeps the
//! window, GL surface, run loop, display link, touches and sound exactly as
//! for 64-bit games. `run` loads the program with `arm32_bridge::ios32`,
//! points its imports at what is registered here, runs its start-up
//! functions and then its entry point.
//!
//! # API for the other 32-bit helpers (Objective-C, C++, GL/AL/CG/CF, ...)
//!
//! Guest code sees 32-bit addresses: guest address `g` is host memory
//! `mem().host(g)` (one 4 GiB reservation). Anything handed to the guest
//! must live in guest memory.
//!
//! **Register before the program is loaded** (from `register_all`, which
//! `run` calls first; add your module's `register()` there):
//! - [`function`]`(name, handler)`: a C function. `name` is the C name: the
//!   Mach-O symbol without its one leading underscore (`_objc_msgSend` ->
//!   `objc_msgSend`, `__ZdlPv` -> `_ZdlPv`). A handler is
//!   `fn(&mut Uc, &mut Args) -> Ret`; write it with
//!   `arm32_bridge::handler!(|uc, a| ...)` (gives `Option<Handler>`) or a
//!   plain `fn`. Read arguments in order with `a.u32()`, `a.i32()`,
//!   `a.f32()` (floats come in core registers: armv7 iPhone code is
//!   soft-float at calls), `a.u64()`/`a.f64()` (Apple's packed rule is
//!   applied: no even-register pair, may straddle r3 and the stack). Return
//!   `Ret::U32`, `Ret::F32`, `Ret::U64`/`Ret::F64` (r0:r1), `Ret::Void`.
//!   Structs bigger than 4 bytes are returned through a hidden pointer that
//!   arrives as the first argument (stret). Variadic: keep reading `a`.
//!   `uc.get_data().current_call` is the C name being handled.
//! - [`data`]`(name, make)`: a data import (constants, class structures,
//!   `__NSConcrete*Block`, CF/NS string constants). `make` runs once, on the
//!   first bind, and returns the guest address of the object it built; binds
//!   point there (plus their addend). Re-registering a name replaces it
//!   (e.g. the Objective-C helper replacing the default block classes).
//!   Helpers: [`data_words`], [`data_bytes`], [`data_pointer_to_cstring`].
//!
//! Names nothing registers: a function the program calls stops it with
//! "the program called NAME, which nothing provides yet" (IOS_KEEP_GOING=1
//! returns 0 instead and keeps a list); data only reached through pointers
//! gets 64 zero bytes and is listed (IOS_LIST_MISSING=1 prints both lists).
//!
//! **Memory and strings** (`arm32_bridge::runtime::mem()`, a `GuestMem`):
//! `read_u8/u16/u32/u64`, `write_*`, `bytes(g, len)`, `write_bytes`,
//! `string(g)` (a guest C string), `host(g)` (guest -> host pointer),
//! `guest(p)` (host pointer inside guest memory -> guest address).
//! - [`alloc`] / [`free`]: the guest's malloc heap (what the game's
//!   `malloc`/`free` use). [`alloc_permanent`]: never freed (tables, classes).
//! - [`cstring`]: a heap copy of a Rust string (the guest frees it, or you do);
//!   [`cstring_permanent`]: a copy kept forever, one per distinct text and key.
//!
//! **Calling guest code** from a handler or the host:
//! [`call`]`(uc, address, &[words])` (bit 0 of `address` = Thumb) returns
//! r0 | r1 << 32; `arm32_bridge::runtime::call_guest_with(uc, address,
//! &[Arg::Word(..), Arg::Pair(..)])` for 64-bit arguments (packed rule
//! applied). From a host thread that isn't running guest code (run loop,
//! display link, audio callbacks): `let _gil = runtime::hold_gil(); let uc =
//! runtime::engine();` first (each host thread gets its own engine).
//!
//! **Stopping**: `arm32_bridge::runtime::stop_with_fault(uc, message)`.
//!
//! Guest ELF libraries: the C++ runtime (libguest32cxx.so, guest32-cxx/) is
//! built for Apple's ABI, so all its imports go through this table like the
//! program's; the built-in libz is bionic's, so its C library imports use
//! Android's handlers (bionic layouts and errno numbers). Their exports bind
//! directly to the program's imports of the same name.

mod crypto;
pub mod fw;
mod libsystem;
mod locale;
pub mod objc;
#[cfg(test)]
pub(crate) mod tests;

use arm32_bridge::ios32;
use arm32_bridge::mem;
use arm32_bridge::runtime::{self, Handler, Uc};

pub use arm32_bridge::runtime::{mem as guest_memory, Arg, ArgSource, Args, Ret};

#[cfg(test)]
thread_local! {
    /// Every (kind, name) registered on this thread, for the duplicate test.
    static REGISTERED: std::cell::RefCell<Vec<(&'static str, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Registers C function `name` (see the module notes).
pub fn function(name: &str, handler: Handler) {
    #[cfg(test)]
    REGISTERED.with(|names| names.borrow_mut().push(("function", name.to_string())));
    ios32::register_function(name, handler);
}

/// Registers data import `name`: `make` returns the guest address of its object.
pub fn data(name: &str, make: impl Fn() -> u32 + Send + Sync + 'static) {
    #[cfg(test)]
    REGISTERED.with(|names| names.borrow_mut().push(("data", name.to_string())));
    ios32::register_data(name, Box::new(make));
}

/// Registers data import `name` as these 32-bit words.
pub fn data_words(name: &str, words: Vec<u32>) {
    data(name, move || {
        let address = alloc_permanent(words.len() as u32 * 4, 4);
        for (i, word) in words.iter().enumerate() {
            runtime::mem().write_u32(address + i as u32 * 4, *word);
        }
        address
    });
}

/// Registers data import `name` as these bytes (`align`-aligned).
pub fn data_bytes(name: &str, bytes: Vec<u8>, align: u32) {
    data(name, move || {
        let address = alloc_permanent(bytes.len().max(1) as u32, align);
        runtime::mem().write_bytes(address, &bytes);
        address
    });
}

/// Registers data import `name` as a `const char*` variable pointing at `text`.
pub fn data_pointer_to_cstring(name: &str, text: &'static str) {
    data(name, move || {
        let pointer = alloc_permanent(4, 4);
        runtime::mem().write_u32(pointer, cstring_permanent(text));
        pointer
    });
}

/// The guest address of registered data `name` (made on first use).
pub fn data_address(name: &str) -> Option<u32> {
    ios32::data_address(name)
}

/// Guest heap memory (the game's own malloc heap), 8-aligned.
pub fn alloc(size: u32) -> u32 {
    runtime::heap().malloc(size)
}

pub fn free(address: u32) {
    runtime::heap().free(address)
}

/// Guest memory that is never freed.
pub fn alloc_permanent(size: u32, align: u32) -> u32 {
    runtime::shared().misc_alloc(size, align)
}

/// A guest heap copy of `text` with its end mark.
pub fn cstring(text: &str) -> u32 {
    runtime::heap().strdup(text.as_bytes())
}

/// A permanent guest copy of `text` (the same copy every time for the same text).
pub fn cstring_permanent(text: &str) -> u32 {
    runtime::shared().cached_cstr(format!("guest32:{text}"), text)
}

/// Calls guest code at `address` with 32-bit arguments; r0 | r1 << 32.
pub fn call(uc: &mut Uc, address: u32, args: &[u32]) -> Result<u64, String> {
    runtime::call_guest(uc, address, args)
}

/// Every name the 32-bit layer provides, registered before loading.
pub fn register_all() {
    libsystem::register();
    fw::register();
    locale::register();
    crypto::register();
    objc::wire::register();
}

/// libc++, libc++abi and the SjLj unwinder for Apple's armv7 ABI
/// (guest32-cxx/README.md), built by build.rs.
const CXX_LIBRARY: &[u8] = include_bytes!(env!("GUEST32_CXX_LIBRARY"));

/// Loads the C++ runtime into guest memory and runs its constructors; its
/// exports then bind the program's C++ imports. Its own imports use this
/// layer's table (registered first).
pub fn open_cxx_library(uc: &mut Uc) -> Result<(), String> {
    arm32_bridge::elf::open_bytes(uc, "libguest32cxx.so", CXX_LIBRARY, true).map(|_| ())
}

/// The loaded program's header address and slide (dyld image functions).
static IMAGE: std::sync::OnceLock<(u32, u32, String)> = std::sync::OnceLock::new();

pub(crate) fn main_image() -> Option<&'static (u32, u32, String)> {
    IMAGE.get()
}

/// Whether a program file has no 64-bit ARM part but a 32-bit one this layer can run.
pub fn is_guest32_program(file: &[u8]) -> bool {
    crate::macho::thin_arm64(file).is_err() && guest32_image::Image32::parse(file).is_ok()
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0")
}

/// Runs a 32-bit game (its .app folder and program file) until its entry
/// point returns or it stops.
pub fn run(app_dir: &str, program_path: &str) -> Result<(), String> {
    crate::loader::set_guest32_app(program_path, app_dir);
    if !arm32_bridge::dynarec::enabled() {
        return Err("32-bit games need the translator (64-bit ARM, ARM32_ENGINE not set to unicorn)".into());
    }
    let program = ios32::prepare(std::path::Path::new(program_path), &guest32_image::SliceChoice::default())?;
    if program.image.encryption.as_ref().is_some_and(|e| e.id != 0) {
        return Err("this program is still encrypted by the App Store".into());
    }
    mem::set_layout(program.layout.clone())?;
    runtime::set_platform(&ios32::APPLE32)?;
    register_all();
    objc::wire::register_program(&program)?;
    ios32::set_keep_going(env_on("IOS_KEEP_GOING") || crate::trap::KEEP_GOING.load(std::sync::atomic::Ordering::Relaxed));
    // IOS_TRACE_CALLS=1 (test switch): logs every stand-in call (very slow).
    runtime::init(env_on("IOS_TRACE_CALLS"));
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    arm32_bridge::elf::open_built_in(uc, "libz.so")?;
    open_cxx_library(uc)?;
    objc::exceptions::setup(uc)?;
    let loaded = ios32::load(&program)?;
    objc::wire::install_program()?;
    let header = program.image.header_address().unwrap_or(0).wrapping_add(program.plan.slide);
    let _ = IMAGE.set((header, program.plan.slide, program_path.to_string()));
    crate::log!(
        "32-bit program: {:?} slice, {} rebases, {} binds to functions ({} stand-ins), {} to data, {} to guest libraries, {} inside, {} weak left 0",
        program.image.slice.subtype,
        loaded.rebases,
        loaded.binds_to_stubs,
        loaded.stubs,
        loaded.binds_to_data,
        loaded.binds_to_libraries,
        loaded.binds_to_self,
        loaded.weak_missing
    );
    crate::log!(
        "32-bit imports nothing provides yet: {} functions, {} data",
        loaded.missing_functions.len(),
        loaded.missing_data.len()
    );
    if env_on("IOS_LIST_MISSING") {
        crate::log!("missing functions: {}", loaded.missing_functions.join(" "));
        crate::log!("missing data: {}", loaded.missing_data.join(" "));
        crate::log!("weak imports left empty: {}", loaded.weak_left_empty.join(" "));
        crate::log!("C++ runtime imports nothing provides: {}", ios32::missing_library_imports().join(" "));
    }
    let start = ios32::start_state(uc, &program, program_path)?;
    crate::log!("32-bit start: {} at {:#010x} ({})", start.kind, start.pc & !1, if start.pc & 1 == 1 { "Thumb" } else { "ARM" });
    let result = objc::wire::run_loads().and_then(|_| ios32::run_initializers(uc, &program, &start)).and_then(|count| {
        crate::log!("32-bit start-up functions run: {count}");
        runtime::start_guest(uc, start.pc, &start.registers, start.sp)
    });
    let fallbacks = arm32_bridge::dynarec::fallback_count();
    let called = ios32::missing_calls();
    if !called.is_empty() {
        crate::log!("32-bit functions called that nothing provides yet: {}", called.join(" "));
    }
    crate::log!("translator fallbacks: {fallbacks}");
    if let Some(code) = uc.get_data().exit_code {
        crate::log!("32-bit program called exit({code})");
        return Ok(());
    }
    match result {
        Ok(value) => {
            crate::log!("32-bit program returned {}", value as u32 as i32);
            Ok(())
        }
        Err(error) => Err(error),
    }
}
