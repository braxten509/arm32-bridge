//! The Objective-C boundary between 32-bit (armv7) game code on the
//! translator and the 64-bit iPhone layer (see guest32-design.md,
//! "Objective-C (the core)").
//!
//! - [`encoding`]: type encodings and the armv7 / AAPCS64 layouts;
//! - [`selectors`]: encodings of the framework selectors the games send.

pub mod encoding;
pub mod selectors;
