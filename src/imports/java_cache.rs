//! Remembered answers for Java checks a game repeats nonstop.
//!
//! Some games ask Java the same question on every frame. Pirates calls
//! DeviceJava.HeadphonesDisconnected() (AudioManager.isWiredHeadsetOn, a call
//! into the phone's audio service) 60 times a second; it is usually instant,
//! but while the audio service is busy (music changing on a menu switch) one
//! answer took 0.89 s and froze the game.
//!
//! A static Java method with no arguments and a plain-value result (boolean,
//! number) that keeps giving the same answer at least 60 times within 3 s is
//! answered from memory from then on. A helper thread re-asks Java 4 times a
//! second, so a real change (headphones plugged in) shows within ~0.25 s. When
//! the game stops asking for 2 s, or the method throws, the helper stops and
//! the method is called normally again.
//!
//! ARM32_JAVA_CACHE=0 (or the property debug.arm32bridge.javacache=0) turns
//! this off.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use jni_sys::*;

use crate::runtime::Ret;

/// Same answers in a row before the method is answered from memory.
const SAME_ANSWERS: u32 = 60;
/// ... all within this long (so only checks asked about 20+ times a second).
const WINDOW: Duration = Duration::from_secs(3);
/// How often the helper re-asks Java.
const REFRESH: Duration = Duration::from_millis(250);
/// The helper stops after the game hasn't asked for this long.
const IDLE_STOP: Duration = Duration::from_secs(2);

/// Counts one method's identical answers in a row.
#[derive(Default)]
struct Watch {
    value: u64,
    same: u32,
    since: Option<Instant>,
}

impl Watch {
    /// Records an answer; true once the method should be answered from memory.
    fn step(&mut self, value: u64, now: Instant) -> bool {
        let fresh = match self.since {
            Some(since) => value != self.value || now.duration_since(since) > WINDOW,
            None => true,
        };
        if fresh {
            *self = Watch { value, same: 1, since: Some(now) };
        } else {
            self.same += 1;
        }
        self.same >= SAME_ANSWERS
    }
}

struct Remembered {
    value: AtomicU64,
    /// Nanoseconds since START when the game last asked.
    last_asked: AtomicU64,
    stopped: AtomicBool,
}

enum Entry {
    Watching(Watch),
    Remembered(Arc<Remembered>),
}

fn table() -> &'static Mutex<HashMap<usize, Entry>> {
    static TABLE: OnceLock<Mutex<HashMap<usize, Entry>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn nanos(now: Instant) -> u64 {
    now.duration_since(start()).as_nanos() as u64
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let setting = std::env::var("ARM32_JAVA_CACHE").ok().or_else(|| crate::dynarec::property("debug.arm32bridge.javacache"));
        setting.as_deref() != Some("0")
    })
}

/// Whether a call can be remembered: static, no arguments, a plain-value result.
pub fn eligible(is_static: bool, parameters: &[u8], return_type: u8) -> bool {
    is_static && parameters.is_empty() && b"ZBCSIJFD".contains(&return_type) && enabled()
}

/// The remembered answer for `method`, if it has one.
pub fn answer(method: usize, return_type: u8) -> Option<Ret> {
    let table = table().lock().unwrap();
    let Some(Entry::Remembered(entry)) = table.get(&method) else { return None };
    if entry.stopped.load(Ordering::Acquire) {
        return None;
    }
    entry.last_asked.store(nanos(Instant::now()), Ordering::Release);
    Some(from_bits(return_type, entry.value.load(Ordering::Acquire)))
}

/// Records a real answer (after the call, on the calling thread). `class` is the
/// class the game called the method on; `threw` whether the call left an exception.
pub fn observe(env: *mut JNIEnv, method: usize, class: jclass, return_type: u8, result: &Ret, threw: bool, name: &str) {
    let now = Instant::now();
    let mut table = table().lock().unwrap();
    let entry = table.entry(method).or_insert_with(|| Entry::Watching(Watch::default()));
    let Entry::Watching(watch) = entry else { return };
    if threw || class.is_null() {
        *watch = Watch::default();
        return;
    }
    let value = to_bits(result);
    if !watch.step(value, now) {
        return;
    }
    let global = unsafe { ((**env).NewGlobalRef.unwrap())(env, class) } as usize;
    if global == 0 {
        *watch = Watch::default();
        return;
    }
    let remembered = Arc::new(Remembered {
        value: AtomicU64::new(value),
        last_asked: AtomicU64::new(nanos(now)),
        stopped: AtomicBool::new(false),
    });
    *entry = Entry::Remembered(remembered.clone());
    crate::log::line(&format!("[bridge] remembering Java check {name}: answered from memory, re-asked 4 times a second"));
    let name = name.to_string();
    let spawned = std::thread::Builder::new()
        .name("java-check".into())
        .spawn(move || refresh(method, global, return_type, remembered, name));
    if spawned.is_err() {
        table.remove(&method);
    }
}

/// Helper thread: re-asks Java until the game stops asking or the method throws.
fn refresh(method: usize, class: usize, return_type: u8, entry: Arc<Remembered>, name: String) {
    let env = super::jni_host::env();
    let reason = loop {
        std::thread::sleep(REFRESH);
        let idle = nanos(Instant::now()).saturating_sub(entry.last_asked.load(Ordering::Acquire));
        if idle > IDLE_STOP.as_nanos() as u64 {
            break "the game stopped asking";
        }
        let Some(value) = call(env, class as jclass, method as jmethodID, return_type) else {
            break "it threw an exception";
        };
        entry.value.store(value, Ordering::Release);
    };
    entry.stopped.store(true, Ordering::Release);
    table().lock().unwrap().remove(&method);
    crate::log::line(&format!("[bridge] stopped remembering Java check {name}: {reason}"));
    unsafe { ((**env).DeleteGlobalRef.unwrap())(env, class as jobject) };
    super::jni_host::detach_current_thread();
}

/// Calls a static no-argument method; None if it threw (the exception is cleared).
fn call(env: *mut JNIEnv, class: jclass, id: jmethodID, return_type: u8) -> Option<u64> {
    let none = [jvalue { j: 0 }];
    let arguments = none.as_ptr();
    let value = unsafe {
        let e = &**env;
        match return_type {
            b'Z' => (e.CallStaticBooleanMethodA.unwrap())(env, class, id, arguments) as u64,
            b'B' => (e.CallStaticByteMethodA.unwrap())(env, class, id, arguments) as u8 as u64,
            b'C' => (e.CallStaticCharMethodA.unwrap())(env, class, id, arguments) as u64,
            b'S' => (e.CallStaticShortMethodA.unwrap())(env, class, id, arguments) as u16 as u64,
            b'I' => (e.CallStaticIntMethodA.unwrap())(env, class, id, arguments) as u32 as u64,
            b'J' => (e.CallStaticLongMethodA.unwrap())(env, class, id, arguments) as u64,
            b'F' => (e.CallStaticFloatMethodA.unwrap())(env, class, id, arguments).to_bits() as u64,
            _ => (e.CallStaticDoubleMethodA.unwrap())(env, class, id, arguments).to_bits(),
        }
    };
    if unsafe { ((**env).ExceptionCheck.unwrap())(env) } != 0 {
        unsafe { ((**env).ExceptionClear.unwrap())(env) };
        return None;
    }
    Some(value)
}

fn to_bits(result: &Ret) -> u64 {
    match *result {
        Ret::U32(value) => value as u64,
        Ret::U64(value) => value,
        Ret::F32(value) => value.to_bits() as u64,
        Ret::F64(value) => value.to_bits(),
        _ => 0,
    }
}

fn from_bits(return_type: u8, bits: u64) -> Ret {
    match return_type {
        b'J' => Ret::U64(bits),
        b'F' => Ret::F32(f32::from_bits(bits as u32)),
        b'D' => Ret::F64(f64::from_bits(bits)),
        _ => Ret::U32(bits as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_after_sixty_same_answers_within_the_window() {
        let mut watch = Watch::default();
        let start = Instant::now();
        for i in 0..59 {
            assert!(!watch.step(1, start + Duration::from_millis(16 * i)));
        }
        assert!(watch.step(1, start + Duration::from_millis(16 * 59)));
    }

    #[test]
    fn a_different_answer_starts_over() {
        let mut watch = Watch::default();
        let start = Instant::now();
        for i in 0..59 {
            watch.step(0, start + Duration::from_millis(i));
        }
        assert!(!watch.step(1, start + Duration::from_millis(60)));
        for i in 0..58 {
            assert!(!watch.step(1, start + Duration::from_millis(61 + i)));
        }
        assert!(watch.step(1, start + Duration::from_millis(200)));
    }

    #[test]
    fn slow_polling_is_never_remembered() {
        // Asked once a second: never 60 times within 3 s.
        let mut watch = Watch::default();
        let start = Instant::now();
        for i in 0..200 {
            assert!(!watch.step(1, start + Duration::from_secs(i)));
        }
    }

    #[test]
    fn only_static_no_argument_plain_values() {
        assert!(eligible(true, &[], b'Z'));
        assert!(eligible(true, &[], b'D'));
        assert!(!eligible(false, &[], b'Z'));
        assert!(!eligible(true, &[b'I'], b'Z'));
        assert!(!eligible(true, &[], b'V'));
        assert!(!eligible(true, &[], b'L'));
    }

    #[test]
    fn bits_round_trip() {
        for (kind, ret) in [(b'Z', Ret::U32(1)), (b'J', Ret::U64(1 << 40)), (b'F', Ret::F32(1.5)), (b'D', Ret::F64(-2.25))] {
            let back = from_bits(kind, to_bits(&ret));
            assert_eq!(to_bits(&back), to_bits(&ret));
        }
    }
}
