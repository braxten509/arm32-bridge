//! Game Bridge's iPhone layer: loads the 64-bit ARM code of an iPhone game
//! and stands in for the Apple system parts it calls.

// Stand-ins are registered by address, so functions are turned into numbers on purpose.
#![allow(unknown_lints, function_casts_as_integer)]

pub mod macho;

#[cfg(target_arch = "aarch64")]
pub mod output;
pub mod report;
#[cfg(target_arch = "aarch64")]
pub mod fw;
#[cfg(target_arch = "aarch64")]
pub mod libsystem;
#[cfg(target_arch = "aarch64")]
pub mod cxx;
#[cfg(target_arch = "aarch64")]
pub mod guest32;
#[cfg(target_arch = "aarch64")]
pub mod loader;
#[cfg(target_arch = "aarch64")]
pub mod objc;
#[cfg(target_arch = "aarch64")]
pub mod run;
#[cfg(target_arch = "aarch64")]
pub mod symbols;
#[cfg(target_arch = "aarch64")]
pub mod trap;
#[cfg(target_arch = "aarch64")]
pub mod varargs;
#[cfg(all(target_arch = "aarch64", target_os = "android"))]
pub mod android;
