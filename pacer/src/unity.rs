//! Unity's own frame cap: Application.targetFrameRate.
//!
//! On phones Unity runs at 30 fps unless the game sets targetFrameRate. Many
//! games never do, and Unity then strips the C# setter from the build, so it
//! can't be called. The engine still has its own setter: Unity's getter (whose
//! address the engine hands to the scripting runtime as the
//! `UnityEngine.Application::get_targetFrameRate` internal call) is
//!
//! ```text
//!     [b getter]
//!     adrp xN, page ; ldr w0, [xN, #offset] ; ret
//! ```
//!
//! and the engine's setter is the function right after it:
//!
//! ```text
//!     adrp xM, page ; str w0, [xM, #offset] ; b apply   (or ret)
//! ```
//!
//! where apply re-reads the value and updates Unity's frame pacing (seen in
//! Unity 2022.3). The setter is only used when every instruction matches, both
//! address the same value and all of it lies in libunity's code. It is called
//! on Unity's main thread: from the first scripting call (il2cpp_runtime_invoke
//! or mono_runtime_invoke) after the getter is known. After that the value is
//! checked once a second and set again if something else changed it.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};

static GETTER: AtomicUsize = AtomicUsize::new(0);
static SETTER: AtomicUsize = AtomicUsize::new(0);
/// The game's own registered setter (when the game uses it), wrapped by `game_set`.
static GAME_SETTER: AtomicUsize = AtomicUsize::new(0);
/// The value the game last asked for through its setter.
static GAME_ASKED: AtomicI32 = AtomicI32::new(i32::MIN);
static GAVE_UP: AtomicBool = AtomicBool::new(false);
/// Monotonic ns of the next check.
static NEXT_CHECK: AtomicU64 = AtomicU64::new(0);
/// Unity's main thread: the one that registers its internal calls (0 = not known yet).
static MAIN_THREAD: AtomicI32 = AtomicI32::new(0);

const GETTER_NAME: &str = "UnityEngine.Application::get_targetFrameRate";
const SETTER_NAME: &str = "UnityEngine.Application::set_targetFrameRate";

/// The engine registers an internal call: remember the getter, and wrap the
/// setter if the game kept it. Returns the function to register.
pub fn internal_call(name: &str, function: usize) -> usize {
    if name.starts_with(GETTER_NAME) {
        GETTER.store(function, Ordering::SeqCst);
        MAIN_THREAD.store(this_thread(), Ordering::SeqCst);
        crate::log(&format!("Unity: found {GETTER_NAME}"));
    } else if name.starts_with(SETTER_NAME) && crate::pacing::setting().fps.is_some() {
        GAME_SETTER.store(function, Ordering::SeqCst);
        crate::log(&format!("Unity: the game sets targetFrameRate itself; raising what it asks for"));
        return game_set as *const () as usize;
    }
    function
}

/// What Unity's frame rate should be when the game asks for `asked` (-1 = Unity's default, 30 on phones).
fn target(asked: i32) -> i32 {
    match crate::pacing::setting().fps {
        None => asked,
        Some(fps) => fps.round() as i32,
    }
}

extern "C" fn game_set(value: i32) {
    GAME_ASKED.store(value, Ordering::SeqCst);
    let raised = target(value);
    if raised != value {
        crate::log(&format!("Unity: game asked for targetFrameRate {value}, set {raised}"));
    }
    let setter: extern "C" fn(i32) = unsafe { std::mem::transmute(GAME_SETTER.load(Ordering::SeqCst)) };
    setter(raised);
}

fn this_thread() -> i32 {
    thread_local! {
        static ID: i32 = unsafe { libc::gettid() };
    }
    ID.with(|id| *id)
}

/// Called before each scripting call. Plugins may call into scripts from
/// other threads; only Unity's main thread changes the frame rate (the engine's
/// apply step isn't meant for other threads), so checks never overlap.
pub fn on_scripting_call(resolve_icall: Option<extern "C" fn(*const libc::c_char) -> usize>) {
    if GAVE_UP.load(Ordering::Relaxed) || crate::pacing::setting().fps.is_none() {
        return;
    }
    let main = MAIN_THREAD.load(Ordering::SeqCst);
    if main == 0 {
        // The registration was missed: Unity names its main thread UnityMain.
        if crate::thread_name() != "UnityMain" {
            return;
        }
        MAIN_THREAD.store(this_thread(), Ordering::SeqCst);
    } else if main != this_thread() {
        return;
    }
    let now = monotonic_ns();
    if now < NEXT_CHECK.load(Ordering::Relaxed) {
        return;
    }
    NEXT_CHECK.store(now + 1_000_000_000, Ordering::Relaxed);
    let mut getter = GETTER.load(Ordering::SeqCst);
    if getter == 0 {
        // The registration was missed (the helper came late): ask IL2CPP for it.
        if let Some(resolve) = resolve_icall {
            getter = resolve(c"UnityEngine.Application::get_targetFrameRate()".as_ptr());
            if getter != 0 {
                GETTER.store(getter, Ordering::SeqCst);
                crate::log("Unity: found the targetFrameRate getter through il2cpp_resolve_icall");
            }
        }
        if getter == 0 {
            return;
        }
    }
    let get: extern "C" fn() -> i32 = unsafe { std::mem::transmute(getter) };
    let current = get();
    let asked = GAME_ASKED.load(Ordering::SeqCst);
    let wanted = target(if asked == i32::MIN { current } else { asked });
    if current == wanted {
        return;
    }
    // The game's own registered setter when it has one, else the engine's.
    let setter = match (GAME_SETTER.load(Ordering::SeqCst), SETTER.load(Ordering::SeqCst)) {
        (0, 0) => match find_setter(getter) {
            Ok(setter) => {
                SETTER.store(setter, Ordering::SeqCst);
                setter
            }
            Err(why) => {
                crate::log(&format!("Unity: targetFrameRate left at {current}: {why}"));
                GAVE_UP.store(true, Ordering::Relaxed);
                return;
            }
        },
        (0, setter) => setter,
        (game, _) => game,
    };
    let set: extern "C" fn(i32) = unsafe { std::mem::transmute(setter) };
    set(wanted);
    let after = get();
    crate::log(&format!("Unity: targetFrameRate {current} -> {after} (thread {})", crate::thread_name()));
    if after != wanted {
        crate::log("Unity: the engine didn't keep the new targetFrameRate; giving up");
        GAVE_UP.store(true, Ordering::Relaxed);
    }
}

fn monotonic_ns() -> u64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

/// The engine's setter next to `getter`, checked against libunity's code segment.
fn find_setter(getter: usize) -> Result<usize, String> {
    let images = crate::got::images();
    let image = images.iter().find(|image| image.contains(getter)).ok_or("getter is in no loaded library")?;
    let read = |address: usize| -> Option<u32> {
        image.holds_code(address, 4).then(|| unsafe { std::ptr::read_unaligned(address as *const u32) })
    };
    engine_setter(read, getter).map_err(|why| format!("{why} (getter at {:#x} in {})", getter - image.base, image.name))
}

const RET: u32 = 0xd65f_03c0;

fn branch_target(pc: usize, instruction: u32) -> Option<usize> {
    if instruction & 0xfc00_0000 != 0x1400_0000 {
        return None;
    }
    let offset = (((instruction & 0x03ff_ffff) << 6) as i32 >> 6) as isize * 4;
    Some(pc.wrapping_add_signed(offset))
}

/// `adrp xN, page`: (register, page address).
fn adrp(pc: usize, instruction: u32) -> Option<(u32, usize)> {
    if instruction & 0x9f00_0000 != 0x9000_0000 {
        return None;
    }
    let low = (instruction >> 29) & 3;
    let high = (instruction >> 5) & 0x7ffff;
    let immediate = ((((high << 2) | low) << 11) as i32 >> 11) as isize;
    Some((instruction & 31, (pc & !0xfff).wrapping_add_signed(immediate << 12)))
}

/// `ldr w0, [xN, #offset]` (load = true) or `str w0, [xN, #offset]`: (base register, offset).
fn word_access(instruction: u32, load: bool) -> Option<(u32, usize)> {
    let opcode = if load { 0xb940_0000 } else { 0xb900_0000 };
    if instruction & 0xffc0_0000 != opcode || instruction & 31 != 0 {
        return None;
    }
    Some(((instruction >> 5) & 31, ((instruction >> 10) & 0xfff) as usize * 4))
}

/// The engine setter for the getter at `getter`, reading code through `read`.
pub fn engine_setter(read: impl Fn(usize) -> Option<u32>, getter: usize) -> Result<usize, &'static str> {
    let mut at = getter;
    // The registered function may be a jump to the real getter.
    for _ in 0..2 {
        match branch_target(at, read(at).ok_or("can't read the getter")?) {
            Some(target) => at = target,
            None => break,
        }
    }
    let (register, page) = adrp(at, read(at).ok_or("can't read the getter")?).ok_or("getter doesn't start with adrp")?;
    let (base, offset) = word_access(read(at + 4).ok_or("can't read the getter")?, true).ok_or("getter doesn't load w0")?;
    if base != register || read(at + 8) != Some(RET) {
        return Err("getter isn't adrp + ldr + ret");
    }
    let setter = at + 12;
    let (setter_register, setter_page) = adrp(setter, read(setter).ok_or("can't read past the getter")?).ok_or("no adrp after the getter")?;
    let (setter_base, setter_offset) =
        word_access(read(setter + 4).ok_or("can't read past the getter")?, false).ok_or("no str w0 after the getter")?;
    if setter_base != setter_register || setter_page + setter_offset != page + offset {
        return Err("the function after the getter writes a different value");
    }
    let last = read(setter + 8).ok_or("can't read past the getter")?;
    if last != RET && branch_target(setter + 8, last).is_none() {
        return Err("the function after the getter doesn't end in a branch or return");
    }
    Ok(setter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Taverns & Quests 3.0.10 (Unity 2022.3.62f3) libunity.so, as disassembled.
    fn taverns_and_quests(address: usize) -> Option<u32> {
        Some(match address {
            0x3b9154 => 0x1405_1727, // b 0x4fedf0
            0x4fedf0 => 0xb000_6708, // adrp x8, 0x11df000
            0x4fedf4 => 0xb94c_2d00, // ldr w0, [x8, #0xc2c]
            0x4fedf8 => RET,
            0x4fedfc => 0xb000_6708, // adrp x8, 0x11df000
            0x4fee00 => 0xb90c_2d00, // str w0, [x8, #0xc2c]
            0x4fee04 => 0x1405_ed41, // b 0x67a308
            _ => return None,
        })
    }

    #[test]
    fn decodes_the_instructions() {
        assert_eq!(branch_target(0x3b9154, 0x1405_1727), Some(0x4fedf0));
        assert_eq!(adrp(0x4fedf0, 0xb000_6708), Some((8, 0x11df000)));
        assert_eq!(word_access(0xb94c_2d00, true), Some((8, 0xc2c)));
        assert_eq!(word_access(0xb90c_2d00, false), Some((8, 0xc2c)));
        assert_eq!(branch_target(0x4fee04, 0x1405_ed41), Some(0x67a308));
    }

    #[test]
    fn finds_the_engine_setter() {
        assert_eq!(engine_setter(taverns_and_quests, 0x3b9154), Ok(0x4fedfc));
    }

    #[test]
    fn refuses_a_setter_for_another_value() {
        let other = |address| if address == 0x4fee00 { Some(0xb90c_3100) } else { taverns_and_quests(address) };
        assert!(engine_setter(other, 0x3b9154).is_err());
    }

    #[test]
    fn refuses_unknown_code() {
        let other = |address| if address == 0x4fedf4 { Some(0xd503_201f) } else { taverns_and_quests(address) };
        assert!(engine_setter(other, 0x3b9154).is_err());
    }
}
