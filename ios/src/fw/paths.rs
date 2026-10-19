//! Where Apple-side paths live on this device. The game sees the real
//! folders directly, so paths pass through unchanged, except for the
//! start-up command files of Unreal Engine 3 games (below).

use std::ffi::{CStr, CString};
use std::sync::Mutex;

pub unsafe fn host_path_c(path: *const libc::c_char) -> CString {
    if path.is_null() {
        return CString::default();
    }
    let original = CStr::from_ptr(path);
    if let Some(replacement) = original.to_str().ok().and_then(unreal_replacement) {
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

/// The command file the game's own command line names (its -exec=).
static GAME_COMMANDS: Mutex<Option<String>> = Mutex::new(None);

fn screen_hz() -> Option<u32> {
    std::env::var("IOS_SCREEN_HZ").ok()?.trim().parse::<f32>().ok().filter(|hz| *hz >= 30.0).map(|hz| hz.round() as u32)
}

fn unreal_replacement(path: &str) -> Option<CString> {
    if path.ends_with(COMMAND_LINE) {
        let hz = screen_hz()?;
        let original = std::fs::read(path).unwrap_or_default();
        let (line, game_commands) = with_our_commands(&String::from_utf8_lossy(&original));
        let first = GAME_COMMANDS.lock().unwrap().replace(game_commands.clone().unwrap_or_default()).is_none();
        if first {
            crate::log!("Unreal frame limit: raised to the screen's {hz} Hz (game's commands: {game_commands:?})");
        }
        return write_copy("UE3CommandLine.txt", line.as_bytes());
    }
    let (folder, name) = path.rsplit_once('/')?;
    if name != OUR_COMMANDS {
        return None;
    }
    let hz = screen_hz()?;
    let game_commands = GAME_COMMANDS.lock().unwrap().clone()?;
    let original = if game_commands.is_empty() { Vec::new() } else { std::fs::read(format!("{folder}/{game_commands}")).unwrap_or_default() };
    let line = format!("Set Engine MaxSmoothedFrameRate {}", hz + 2);
    static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log!("Unreal frame limit: the game reads its commands from {folder} with \"{line}\" added");
    }
    write_copy(OUR_COMMANDS, &append_line(original, &line))
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

/// Adds a line at the end, in the file's own text encoding (UE3 reads plain
/// 8-bit text or UTF-16 with a byte order mark).
fn append_line(mut text: Vec<u8>, line: &str) -> Vec<u8> {
    let utf16 = text.starts_with(&[0xff, 0xfe]);
    let added = format!("\r\n{line}\r\n");
    if utf16 {
        if text.len() % 2 == 1 {
            text.pop();
        }
        while text.len() >= 4 && text[text.len() - 2..] == [0, 0] {
            text.truncate(text.len() - 2);
        }
        text.extend(added.encode_utf16().flat_map(u16::to_le_bytes));
    } else {
        while text.last() == Some(&0) {
            text.pop();
        }
        text.extend_from_slice(added.as_bytes());
    }
    text
}

/// Writes a copy in the game's tmp folder and returns its path.
fn write_copy(name: &str, contents: &[u8]) -> Option<CString> {
    let folder = format!("{}/tmp/gamebridge", crate::fw::foundation::home());
    std::fs::create_dir_all(&folder).ok()?;
    let path = format!("{folder}/{name}");
    let partial = format!("{path}.{}", std::process::id());
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
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("A".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let expected: Vec<u8> = [0xff, 0xfe].into_iter().chain("A\r\nX\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(append_line(utf16, "X"), expected);
    }
}
