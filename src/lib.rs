//! arm32-bridge: runs 32-bit ARM Android library code inside a 64-bit
//! process. Built as a command-line tool (src/main.rs) for testing, and as a
//! drop-in replacement for a game's native library inside its APK (app.rs).

pub mod dynarec;
pub mod elf;
pub mod imports;
pub mod log;
pub mod mem;
pub mod runtime;

#[cfg(target_os = "android")]
pub mod app;
#[cfg(target_os = "android")]
pub mod sound_preload;
