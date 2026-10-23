//! The built-in libz for 32-bit iPhone programs. It is Android's, built with
//! the standard ARM rules (r9 kept across calls), and keeps values in r9
//! across calls into the game's own callbacks: deflateInit2_ its memLevel
//! across zalloc, inflate's updatewindow the output end across zalloc,
//! inflateBack its output pointer across in() and out(). Apple's armv7 code
//! (iPhone OS 3+) may change r9 in any call. So the program's calls that hand
//! libz callbacks come here first, and each game callback is swapped for a
//! stand-in that calls it from the host: a nested guest call puts every
//! register back, r9 included. Android games keep the library as it is.

use std::collections::HashMap;
use std::sync::Mutex;

use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, call_guest, mem, stop_with_fault, ArgSource, Args, Ret, Uc};

/// The calls that take game callbacks: (name, argument count, callback
/// arguments). Each also takes the stream's zalloc/zfree (z_stream +0x20,
/// +0x24), which libz keeps calling through the stream until its End.
const WRAPPED: &[(&str, usize, &[usize])] = &[
    ("deflateInit_", 4, &[]),
    ("deflateInit2_", 8, &[]),
    ("inflateInit_", 3, &[]),
    ("inflateInit2_", 4, &[]),
    ("inflateBackInit_", 5, &[]),
    ("inflateBack", 5, &[1, 3]),
];

#[derive(Default)]
struct State {
    /// libz's code (start, end): its own default allocators.
    library: (u32, u32),
    /// The real functions, by name.
    real: HashMap<&'static str, u32>,
    /// Game callback -> its stand-in.
    stand_ins: HashMap<u32, u32>,
    /// Stand-in name -> game callback.
    callbacks: HashMap<&'static str, u32>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
}

pub fn register() {
    for &(name, ..) in WRAPPED {
        super::function(name, wrapped);
    }
}

/// Loads the built-in libz. Its exports bind the program's imports, except
/// the calls above, which bind to this module.
pub fn open(uc: &mut Uc) -> Result<(), String> {
    let index = arm32_bridge::elf::open_built_in(uc, "libz.so")?;
    let library = arm32_bridge::elf::with_loader(|loader| {
        let library = &loader.libraries[index];
        (library.base, library.base + library.size)
    });
    let mut real = HashMap::new();
    let mut shared = runtime::shared();
    for &(name, ..) in WRAPPED {
        real.insert(name, shared.guest_exports.remove(name).ok_or_else(|| format!("libz.so has no {name}"))?);
    }
    drop(shared);
    state(|state| {
        state.library = library;
        state.real = real;
    });
    Ok(())
}

fn wrapped(uc: &mut Uc, a: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let &(_, count, callbacks) = WRAPPED.iter().find(|(wrapped, ..)| *wrapped == name).expect("a wrapped libz call");
    let Some(real) = state(|state| state.real.get(name).copied()) else {
        stop_with_fault(uc, format!("{name}: libz.so is not loaded"));
        return Ret::Void;
    };
    let mut args: Vec<u32> = (0..count).map(|_| a.u32()).collect();
    let stream = args[0];
    if stream != 0 {
        for at in [stream + 0x20, stream + 0x24] {
            mem().write_u32(at, stand_in(mem().read_u32(at)));
        }
    }
    for &i in callbacks {
        args[i] = stand_in(args[i]);
    }
    match call_guest(uc, real, &args) {
        Ok(result) => Ret::U32(result as u32),
        Err(error) => {
            stop_with_fault(uc, error);
            Ret::Void
        }
    }
}

/// The stand-in for game callback `callback` (0, libz's own and stand-ins stay).
fn stand_in(callback: u32) -> u32 {
    state(|state| {
        let (start, end) = state.library;
        let own = (start..end).contains(&(callback & !1));
        if callback == 0 || own || state.stand_ins.values().any(|&stand_in| stand_in == callback) {
            return callback;
        }
        if let Some(&stand_in) = state.stand_ins.get(&callback) {
            return stand_in;
        }
        let name: &'static str = Box::leak(format!("zlib callback {callback:#010x}").into_boxed_str());
        ios32::register_function(name, call_callback);
        let stand_in = runtime::stub_for(name);
        state.stand_ins.insert(callback, stand_in);
        state.callbacks.insert(name, callback);
        stand_in
    })
}

/// Runs a game callback (zalloc, zfree, in, out: at most 3 arguments).
fn call_callback(uc: &mut Uc, a: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let callback = state(|state| state.callbacks.get(name).copied()).expect("a zlib callback stand-in");
    let args = [a.u32(), a.u32(), a.u32()];
    match call_guest(uc, callback, &args) {
        Ok(result) => Ret::U32(result as u32),
        Err(error) => {
            stop_with_fault(uc, error);
            Ret::Void
        }
    }
}
