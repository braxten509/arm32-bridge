//! Loads a cocos2d-x game's sound effects ahead of time.
//!
//! cocos2d-x plays effects through Java's SoundPool and, the first time an
//! effect plays, decodes it on the spot. On current Android each decode goes
//! through the media service and they queue up, so first plays come out late
//! (the converter's Java fix 2, converter/src/dex.rs, removes the freeze that used to come with them).
//!
//! Nothing here is specific to one game. The bridge watches the game's Java
//! sound calls to learn which files are effects ("...Effect" methods) and which
//! are music ("...Music" methods), and saves that per game. Once it knows how
//! the game names its effects, it asks cocos2d-x to preload every other audio
//! file in the APK that isn't music, smallest first, one per frame, on the
//! game's own thread (cocos2d-x's sound class is not thread-safe).

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jni_sys::*;

const HELPER_CLASS: &CStr = c"org/cocos2dx/lib/Cocos2dxHelper";
const AUDIO_EXTENSIONS: [&str; 9] = ["aac", "m4a", "ogg", "wav", "mp3", "3gp", "amr", "flac", "opus"];
/// Frames to wait after the first frame before preloading (lets the game set up its sound class).
const START_AFTER_FRAMES: u32 = 30;

#[derive(Default)]
struct Preloader {
    loaded_memory: bool,
    effects: BTreeSet<String>,
    music: BTreeSet<String>,
    queue: VecDeque<String>,
    /// Paths already asked for (by the game or by us).
    requested: HashSet<String>,
    swept: bool,
    frames: u32,
    disabled: bool,
    preload_method: Option<(jclass, jmethodID)>,
}

// The JNI class is a global reference, usable from any thread.
unsafe impl Send for Preloader {}

static PRELOADER: Mutex<Option<Preloader>> = Mutex::new(None);

macro_rules! jcall {
    ($env:expr, $function:ident $(, $arg:expr)*) => {
        unsafe { ((**$env).$function.unwrap())($env $(, $arg)*) }
    };
}

fn with<R>(work: impl FnOnce(&mut Preloader) -> R) -> R {
    let mut guard = PRELOADER.lock().unwrap_or_else(|e| e.into_inner());
    let preloader = guard.get_or_insert_with(Preloader::default);
    if !preloader.loaded_memory {
        preloader.loaded_memory = true;
        load_memory(preloader);
    }
    work(preloader)
}

/// Where this game's learned sound list lives (the app's private files dir).
fn memory_file() -> Option<PathBuf> {
    let command_line = std::fs::read("/proc/self/cmdline").ok()?;
    let package = String::from_utf8_lossy(command_line.split(|&b| b == 0).next()?).into_owned();
    let package = package.split(':').next()?.to_string();
    Some(PathBuf::from(format!("/data/data/{package}/files/arm32bridge-sounds.txt")))
}

fn load_memory(preloader: &mut Preloader) {
    let Some(path) = memory_file() else { return };
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        match line.split_once('\t') {
            Some(("effect", sound)) => {
                preloader.effects.insert(sound.to_string());
            }
            Some(("music", sound)) => {
                preloader.music.insert(sound.to_string());
            }
            _ => {}
        }
    }
    let effects: Vec<String> = preloader.effects.iter().cloned().collect();
    preloader.queue.extend(effects);
}

fn remember(kind: &str, sound: &str) {
    let Some(path) = memory_file() else { return };
    // A fresh game may not have its files folder yet.
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{kind}\t{sound}");
    }
}

fn is_audio(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

fn without_assets_prefix(path: &str) -> &str {
    path.strip_prefix("assets/").unwrap_or(path)
}

/// Called for every Java method the game calls; learns its sound files.
pub fn observe_java_call(env: *mut JNIEnv, method: &str, signature: &str, arguments: &[jvalue]) {
    let is_effect = method.contains("Effect");
    let is_music = method.contains("Music");
    if !(is_effect || is_music) || !signature.starts_with("(Ljava/lang/String;") || arguments.is_empty() {
        return;
    }
    let string = unsafe { arguments[0].l };
    if string.is_null() {
        return;
    }
    let chars = jcall!(env, GetStringUTFChars, string, std::ptr::null_mut());
    if chars.is_null() {
        return;
    }
    let sound = unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned();
    jcall!(env, ReleaseStringUTFChars, string, chars);
    if !is_audio(&sound) {
        return;
    }
    with(|preloader| {
        preloader.requested.insert(sound.clone());
        let (set, kind) = if is_effect { (&mut preloader.effects, "effect") } else { (&mut preloader.music, "music") };
        if set.insert(sound.clone()) {
            remember(kind, &sound);
        }
    });
}

/// Every audio file in the APK's assets as (path inside assets/, size). Reads only the zip's file list.
fn apk_audio_assets(apk: &Path) -> Vec<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let read = || -> std::io::Result<Vec<(String, u64)>> {
        let mut file = std::fs::File::open(apk)?;
        let length = file.metadata()?.len();
        // End of central directory record (signature 0x06054b50) within the last 64 KiB + 22 bytes.
        let tail_length = length.min(65_557);
        let mut tail = vec![0u8; tail_length as usize];
        file.seek(SeekFrom::Start(length - tail_length))?;
        file.read_exact(&mut tail)?;
        let u32_in = |data: &[u8], at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let u16_in = |data: &[u8], at: usize| u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        let Some(end) = (0..tail.len().saturating_sub(21)).rev().find(|&i| u32_in(&tail, i) == 0x0605_4b50) else {
            return Ok(Vec::new());
        };
        let (count, size, offset) = (u16_in(&tail, end + 10), u32_in(&tail, end + 12), u32_in(&tail, end + 16));
        let mut directory = vec![0u8; size];
        file.seek(SeekFrom::Start(offset as u64))?;
        file.read_exact(&mut directory)?;
        let mut assets = Vec::new();
        let mut at = 0;
        for _ in 0..count {
            if at + 46 > directory.len() || u32_in(&directory, at) != 0x0201_4b50 {
                break;
            }
            let uncompressed = u32_in(&directory, at + 24) as u64;
            let name_length = u16_in(&directory, at + 28);
            let skip = u16_in(&directory, at + 30) + u16_in(&directory, at + 32);
            let name = String::from_utf8_lossy(&directory[at + 46..(at + 46 + name_length).min(directory.len())]).into_owned();
            if let Some(inside) = name.strip_prefix("assets/") {
                if is_audio(inside) {
                    assets.push((inside.to_string(), uncompressed));
                }
            }
            at += 46 + name_length + skip;
        }
        Ok(assets)
    };
    read().unwrap_or_default()
}

/// The installed APK (…/<package>-<id>/base.apk), found from this library's folder (…/lib/arm64).
fn installed_apk(library_directory: &Path) -> PathBuf {
    library_directory.parent().and_then(Path::parent).map(|app| app.join("base.apk")).unwrap_or_default()
}

/// Adds every audio asset that isn't known music, smallest first, written the way the game writes effect paths.
fn sweep(preloader: &mut Preloader, library_directory: &Path) {
    preloader.swept = true;
    let Some(example) = preloader.effects.iter().next() else { return };
    let with_prefix = example.starts_with("assets/");
    let music: HashSet<&str> = preloader.music.iter().map(|m| without_assets_prefix(m)).collect();
    let mut assets = apk_audio_assets(&installed_apk(library_directory));
    assets.sort_by_key(|(_, size)| *size);
    let count = assets.len();
    for (inside, _) in assets {
        if music.contains(inside.as_str()) {
            continue;
        }
        preloader.queue.push_back(if with_prefix { format!("assets/{inside}") } else { inside });
    }
    crate::log::line(&format!("[sound] preloading up to {} of {count} audio files in the background", preloader.queue.len()));
}

fn preload_method(env: *mut JNIEnv, preloader: &mut Preloader) -> Option<(jclass, jmethodID)> {
    if let Some(found) = preloader.preload_method {
        return Some(found);
    }
    let class = jcall!(env, FindClass, HELPER_CLASS.as_ptr());
    if class.is_null() {
        jcall!(env, ExceptionClear);
        return None;
    }
    let method = jcall!(env, GetStaticMethodID, class, c"preloadEffect".as_ptr(), c"(Ljava/lang/String;)V".as_ptr());
    if method.is_null() {
        jcall!(env, ExceptionClear);
        jcall!(env, DeleteLocalRef, class);
        return None;
    }
    let global = jcall!(env, NewGlobalRef, class) as jclass;
    jcall!(env, DeleteLocalRef, class);
    preloader.preload_method = Some((global, method));
    preloader.preload_method
}

/// Called on the game's render thread before each frame; preloads at most one effect.
pub fn on_frame(env: *mut JNIEnv, library_directory: &Path) {
    let next = with(|preloader| {
        if preloader.disabled {
            return None;
        }
        preloader.frames += 1;
        if preloader.frames < START_AFTER_FRAMES {
            return None;
        }
        if !preloader.swept && !preloader.effects.is_empty() {
            sweep(preloader, library_directory);
        }
        while let Some(sound) = preloader.queue.pop_front() {
            if preloader.requested.insert(sound.clone()) {
                let Some(method) = preload_method(env, preloader) else {
                    preloader.disabled = true;
                    return None;
                };
                return Some((sound, method));
            }
        }
        None
    });
    let Some((sound, (class, method))) = next else { return };
    let Ok(text) = CString::new(sound) else { return };
    let string = jcall!(env, NewStringUTF, text.as_ptr());
    let arguments = [jvalue { l: string }];
    jcall!(env, CallStaticVoidMethodA, class, method, arguments.as_ptr());
    if jcall!(env, ExceptionCheck) != 0 {
        jcall!(env, ExceptionClear);
        crate::log::line("[sound] cocos2d-x refused a preload; stopping background preloading");
        with(|preloader| preloader.disabled = true);
    }
    jcall!(env, DeleteLocalRef, string);
}
