//! Rule the Kingdom, zoomed in 50% (owner's ask): the game picks its world
//! zoom and its menu sizes from the screen size at start (SDL_baseInit), each
//! through one setter. Every value they are given is made 1.5 times bigger,
//! so the game draws its own art bigger and stays sharp.

use crate::runtime::{ArgSource, Args, Ret, Uc};

/// How much bigger.
pub const ZOOM: f32 = 1.5;

/// The world's zoom, then the menus', the hover labels' and the top bar's
/// (libkingdom.so 5.11; each takes a float, in r0).
pub const SETTERS: [&str; 4] = [
    "_ZN6Render17SetBaseZoomFactorEf",
    "_ZN3GUI21SetHighDPIScaleFactorEf",
    "_ZN3GUI26SetHoverHighDPIScaleFactorEf",
    "_ZN3GUI24SetHudHighDPIScaleFactorEf",
];

/// The game's library (the converter keeps it as libkingdom-arm32.so).
pub const LIBRARY: &str = "libkingdom";

/// A setter's float (as its bits), zoomed.
pub fn zoomed(bits: u32) -> u32 {
    (f32::from_bits(bits) * ZOOM).to_bits()
}

/// The setters' stand-ins (wraps.rs): each runs the original with the zoomed value.
pub fn set_world(uc: &mut Uc, args: &mut Args) -> Ret {
    set(uc, args, SETTERS[0])
}

pub fn set_menus(uc: &mut Uc, args: &mut Args) -> Ret {
    set(uc, args, SETTERS[1])
}

pub fn set_hover(uc: &mut Uc, args: &mut Args) -> Ret {
    set(uc, args, SETTERS[2])
}

pub fn set_top_bar(uc: &mut Uc, args: &mut Args) -> Ret {
    set(uc, args, SETTERS[3])
}

fn set(uc: &mut Uc, args: &mut Args, symbol: &str) -> Ret {
    let value = zoomed(args.u32());
    if let Err(why) = crate::runtime::call_guest(uc, crate::wraps::original(symbol), &[value]) {
        crate::log::line(&format!("[zoom] {symbol}: {why}"));
    }
    Ret::Void
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four size setters are stepped into in Rule the Kingdom only, and
    /// each value they get is 1.5 times bigger.
    #[test]
    fn rule_the_kingdoms_sizes_are_one_and_a_half_times_bigger() {
        for symbol in SETTERS {
            assert!(crate::wraps::wanted("libkingdom-arm32.so", symbol), "{symbol}");
            assert!(!crate::wraps::wanted("libMonsterLife.so", symbol), "{symbol}");
        }
        for (given, set) in [(1.0f32, 1.5f32), (2.0, 3.0), (0.8, 1.2)] {
            assert_eq!(f32::from_bits(zoomed(given.to_bits())), set, "{given}");
        }
    }
}
