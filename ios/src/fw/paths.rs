//! Where Apple-side paths live on this device. The game sees the real
//! folders directly, so paths pass through unchanged, except for the
//! start-up command files and packed settings of Unreal Engine 3 games
//! (below).

use std::ffi::{CStr, CString};
use std::collections::HashMap;
use std::sync::Mutex;

pub unsafe fn host_path_c(path: *const libc::c_char) -> CString {
    if path.is_null() {
        return CString::default();
    }
    let original = CStr::from_ptr(path);
    if let Some(replacement) = original.to_str().ok().and_then(|p| unreal_replacement(p).or_else(|| settings_replacement(p))) {
        return replacement;
    }
    original.to_owned()
}

pub fn apple_executable_path() -> String {
    crate::loader::try_loaded()
        .map(|l| l.path.clone())
        .unwrap_or_default()
}

// ----- Unreal Engine 3 frame limit -----
//
// UE3 games read their command line from CookedIPhone/UE3CommandLine.txt and
// run the console commands in the file its -exec= names. Their frame limit
// (Engine MaxSmoothedFrameRate, often 30 or 60) is set there or in their
// settings, so they never draw faster than the iPhones of their day showed.
// Here the game reads copies of those two files with one line added that
// sets the limit to the phone screen's top rate (IOS_SCREEN_HZ). The game's
// own files stay as they are.

const COMMAND_LINE: &str = "/CookedIPhone/UE3CommandLine.txt";
const OUR_COMMANDS: &str = "GameBridgeCommands.txt";

/// The command file the game's own command line names (its -exec=, with
/// `\\` turned into `/`); empty when it names none. Set when the game
/// reads its command line.
static GAME_COMMANDS: Mutex<Option<String>> = Mutex::new(None);

/// Copies made so far: the path the game asked for -> the copy's path, or
/// None to let it see the real file. Made once (the contents don't change
/// during a run), under the lock, so threads asking together get the same
/// finished file.
static COPIES: Mutex<Option<HashMap<String, Option<CString>>>> = Mutex::new(None);

fn screen_hz() -> Option<u32> {
    let hz = std::env::var("IOS_SCREEN_HZ").ok()?.trim().parse::<f32>().ok()?;
    (30.0..=1000.0).contains(&hz).then(|| hz.round() as u32)
}

fn unreal_replacement(path: &str) -> Option<CString> {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    if !path.ends_with(COMMAND_LINE) && name != OUR_COMMANDS {
        return None;
    }
    let hz = screen_hz()?;
    if name == OUR_COMMANDS && GAME_COMMANDS.lock().unwrap().is_none() {
        // Asked for before the command line was read: not ours yet.
        return None;
    }
    let mut copies = COPIES.lock().unwrap();
    let copies = copies.get_or_insert_with(HashMap::new);
    if let Some(made) = copies.get(path) {
        return made.clone();
    }
    let made = if name == OUR_COMMANDS { commands_copy(path, hz) } else { command_line_copy(path, hz) };
    copies.insert(path.to_string(), made.clone());
    made
}

// ----- Unreal Engine 3 texture detail -----
//
// The game's packed settings (CookedIPhone/Coalesced_*.bin) choose per
// iPhone model how many top picture levels each texture group drops; the
// model we say we are drops some. The game reads a copy with every texture
// group at full size (fw/coalesced.rs). Encrypted settings stay as they are.

fn settings_replacement(path: &str) -> Option<CString> {
    let (folder, name) = path.rsplit_once('/')?;
    if !folder.ends_with("/CookedIPhone") || !name.starts_with("Coalesced") || !name.ends_with(".bin") {
        return None;
    }
    // Test switch: IOS_FULL_SIZE_TEXTURES=0 lets the game read its own settings.
    if std::env::var("IOS_FULL_SIZE_TEXTURES").is_ok_and(|v| v == "0") {
        return None;
    }
    let mut copies = COPIES.lock().unwrap();
    let copies = copies.get_or_insert_with(HashMap::new);
    if let Some(made) = copies.get(path) {
        return made.clone();
    }
    let made = std::fs::read(path).ok().and_then(|bytes| {
        let (changed, entries) = crate::fw::coalesced::full_size_textures(&bytes)?;
        crate::log!("Unreal texture detail: {entries} texture group settings in {name} set to full size");
        write_copy(path, &changed)
    });
    if made.is_none() {
        crate::log!("Unreal texture detail: {name} left as it is (encrypted, or nothing to change)");
    }
    copies.insert(path.to_string(), made.clone());
    made
}

/// The game's command line with its -exec= pointing at our command file.
/// Only when the game has one: a path it merely tries stays missing.
fn command_line_copy(path: &str, hz: u32) -> Option<CString> {
    let original = std::fs::read(path).ok()?;
    let (encoding, text) = decode(&original);
    let (first, rest) = match text.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (text.as_str(), None),
    };
    let ending = if first.ends_with('\r') { "\r\n" } else { "\n" };
    let (first, game_commands) = with_our_commands(first);
    let line = match rest {
        Some(rest) => format!("{first}{ending}{rest}"),
        None => first,
    };
    let game_commands = game_commands.map(|name| name.replace('\\', "/")).unwrap_or_default();
    crate::log!("Unreal frame limit: set to the screen's {hz} Hz (the game's own commands: {game_commands:?})");
    *GAME_COMMANDS.lock().unwrap() = Some(game_commands);
    write_copy(path, &encode(encoding, &line))
}

/// The game's command file with our frame limit added. The game may look
/// in more than one folder; only where its own file is (or anywhere, when
/// it has none) does it find the copy, so the others stay missing as before.
fn commands_copy(path: &str, hz: u32) -> Option<CString> {
    let game_commands = GAME_COMMANDS.lock().unwrap().clone()?;
    let folder = path.rsplit_once('/').map_or(".", |(folder, _)| folder);
    let original = if game_commands.is_empty() {
        Vec::new()
    } else {
        let own = if game_commands.starts_with('/') { game_commands.clone() } else { format!("{folder}/{game_commands}") };
        match std::fs::read(&own) {
            Ok(original) => original,
            Err(error) => {
                crate::log!("Unreal frame limit: the game's commands {own} can't be read ({error}); left alone there");
                return None;
            }
        }
    };
    let line = format!("Set Engine MaxSmoothedFrameRate {}", hz.saturating_add(2));
    crate::log!("Unreal frame limit: the game reads its commands from {folder} with \"{line}\" added");
    write_copy(path, &append_line(original, &line))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Encoding {
    Bytes,
    Utf16Le,
    Utf16Be,
}

/// Text as UE3 reads it: UTF-16 when it starts with a byte order mark,
/// else plain 8-bit text.
fn decode(bytes: &[u8]) -> (Encoding, String) {
    let units = |big: bool| -> Vec<u16> {
        bytes[2..].chunks_exact(2).map(|pair| if big { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from_le_bytes([pair[0], pair[1]]) }).collect()
    };
    match bytes {
        [0xff, 0xfe, ..] => (Encoding::Utf16Le, String::from_utf16_lossy(&units(false))),
        [0xfe, 0xff, ..] => (Encoding::Utf16Be, String::from_utf16_lossy(&units(true))),
        _ => (Encoding::Bytes, String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn encode(encoding: Encoding, text: &str) -> Vec<u8> {
    match encoding {
        Encoding::Bytes => text.as_bytes().to_vec(),
        Encoding::Utf16Le => [0xff, 0xfe].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        Encoding::Utf16Be => [0xfe, 0xff].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect(),
    }
}

/// The command line with its -exec= pointing at our command file, and the
/// file it named before (None when it had no -exec=).
fn with_our_commands(line: &str) -> (String, Option<String>) {
    let line = line.trim_end_matches(['\r', '\n', '\0']);
    let ours = format!("-exec=\"{OUR_COMMANDS}\"");
    let Some(start) = line.to_ascii_lowercase().find("-exec=") else {
        let separator = if line.trim().is_empty() { "" } else { " " };
        return (format!("{line}{separator}{ours}"), None);
    };
    let value = &line[start + "-exec=".len()..];
    let (name, length) = match value.strip_prefix('"') {
        Some(quoted) => {
            let end = quoted.find('"').unwrap_or(quoted.len());
            (&quoted[..end], (end + 2).min(value.len()))
        }
        None => {
            let end = value.find(char::is_whitespace).unwrap_or(value.len());
            (&value[..end], end)
        }
    };
    let end = start + "-exec=".len() + length;
    (format!("{}{ours}{}", &line[..start], &line[end..]), Some(name.to_string()))
}

/// Adds a line at the end, in the file's own text encoding.
fn append_line(text: Vec<u8>, line: &str) -> Vec<u8> {
    let (encoding, text) = decode(&text);
    let text = text.trim_end_matches('\0');
    encode(encoding, &format!("{text}\r\n{line}\r\n"))
}

/// Writes a copy in the game's tmp folder and returns its path (one file
/// per path the game asked for, named after it).
fn write_copy(asked: &str, contents: &[u8]) -> Option<CString> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    asked.hash(&mut hasher);
    let name = asked.rsplit_once('/').map_or(asked, |(_, name)| name);
    let folder = format!("{}/tmp/gamebridge", crate::fw::foundation::home());
    std::fs::create_dir_all(&folder).ok()?;
    let path = format!("{folder}/{:016x}-{name}", hasher.finish());
    // Same copy as last run: kept as it is.
    if std::fs::read(&path).is_ok_and(|old| old == contents) {
        return CString::new(path).ok();
    }
    let partial = format!("{path}.partial");
    std::fs::write(&partial, contents).ok()?;
    std::fs::rename(&partial, &path).ok()?;
    CString::new(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_points_at_our_commands() {
        assert_eq!(
            with_our_commands("-exec=\"Commands.txt\""),
            ("-exec=\"GameBridgeCommands.txt\"".to_string(), Some("Commands.txt".to_string()))
        );
        assert_eq!(
            with_our_commands("-nosound -EXEC=Startup.txt -log\r\n"),
            ("-nosound -exec=\"GameBridgeCommands.txt\" -log".to_string(), Some("Startup.txt".to_string()))
        );
        assert_eq!(with_our_commands(""), ("-exec=\"GameBridgeCommands.txt\"".to_string(), None));
        assert_eq!(with_our_commands("-nosound"), ("-nosound -exec=\"GameBridgeCommands.txt\"".to_string(), None));
    }

    #[test]
    fn line_added_in_the_file_encoding() {
        assert_eq!(append_line(b"Stat FPS".to_vec(), "X"), b"Stat FPS\r\nX\r\n".to_vec());
        assert_eq!(append_line(encode(Encoding::Utf16Le, "A"), "X"), encode(Encoding::Utf16Le, "A\r\nX\r\n"));
        assert_eq!(append_line(encode(Encoding::Utf16Be, "A\0"), "X"), encode(Encoding::Utf16Be, "A\r\nX\r\n"));
        assert_eq!(decode(&encode(Encoding::Utf16Be, "-exec=x")), (Encoding::Utf16Be, "-exec=x".to_string()));
    }
}
