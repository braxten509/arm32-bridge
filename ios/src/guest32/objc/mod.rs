//! The Objective-C boundary between 32-bit (armv7) game code on the
//! translator and the 64-bit iPhone layer (see guest32-design.md,
//! "Objective-C (the core)").
//!
//! - [`encoding`]: type encodings and the armv7 / AAPCS64 layouts;
//! - [`selectors`]: encodings of the framework selectors the games send.

pub mod bridge;
#[cfg(test)]
mod bridge_tests;
pub mod classes;
#[cfg(test)]
mod classes_tests;
pub mod convert;
#[cfg(test)]
mod convert_tests;
pub mod encoding;
pub mod functions;
#[cfg(test)]
mod functions_tests;
pub mod mirror;
pub mod selectors;
#[cfg(test)]
mod testing;
