//! Apple's C library (libSystem) for the game.

pub mod io;
pub mod libc;
pub mod mach;
pub mod threads;

use crate::symbols::Registry;

pub fn register(r: &mut Registry) {
    libc::register(r);
    io::register(r);
    threads::register(r);
    mach::register(r);
}
