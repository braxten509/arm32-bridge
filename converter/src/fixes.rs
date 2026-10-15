//! Known game fixes: changes for particular games that the bridge can't make
//! for every game. Each one is tied to one exact 32-bit library by its SHA-256,
//! so another version of the game (with its code elsewhere) is never touched.
//!
//! - `patches`: (virtual address, original bytes, new bytes) in that library.
//!   The original bytes are checked before anything changes.
//! - `settings`: bridge settings the converted game carries in its settings
//!   file (SETTINGS_FILE, next to the bridge). The bridge reads them at start;
//!   the same setting made on the phone (`adb shell setprop`) wins.
//!
//! To add a game: find its fix, get the library's `sha256sum`, add an entry.

use sha2::{Digest, Sha256};

/// The settings file's name in the converted APK; the bridge reads it (src/app.rs).
pub const SETTINGS_FILE: &str = "libarm32_bridge_settings.so";

pub struct Fix {
    pub game: &'static str,
    pub library: &'static str,
    pub sha256: &'static str,
    /// What the patches do, for the converter's report.
    pub reason: &'static str,
    pub patches: &'static [(u32, &'static [u8], &'static [u8])],
    pub settings: &'static [(&'static str, &'static str)],
}

pub const FIXES: &[Fix] = &[Fix {
    game: "Assassin's Creed Pirates 2.9.1",
    library: "libSparkApp.so",
    sha256: "1dc7f264f7591193ddf8a234250f3919511f3dda16b5c383de9b66c17cc291a7",
    // lj_gc_step: lim = stepmul * 10 -> stepmul * 2.5 (`lsls r3,r3,#1` -> `lsrs r3,r3,#1`), so a Lua
    // clean-up step triggered on the frame thread does a quarter of the work and the game's own
    // clean-up thread does the rest (late frames 17% -> 5%).
    reason: "smaller Lua clean-up steps per frame (less stutter)",
    patches: &[(0x6bf40e, &[0x5b, 0x00], &[0x5b, 0x08])],
    // Its own limit is 30 fps; the bridge lifts limits by default, and 60 plays steadiest.
    settings: &[("debug.arm32bridge.unlock.com.ubisoft.assassin.pirates", "60")],
}];

/// What the fixes did: report lines (`fix ...` / `warn ...`) and the settings file's text (empty for none).
#[derive(Default)]
pub struct Applied {
    pub report: Vec<String>,
    pub settings: String,
}

/// Applies every fix whose library is among `libraries` (file name, bytes).
pub fn apply(libraries: &mut [(String, Vec<u8>)]) -> Applied {
    let mut applied = Applied::default();
    for (file, bytes) in libraries.iter_mut() {
        let Some(fix) = FIXES.iter().find(|fix| fix.library == file && hex(&Sha256::digest(&bytes[..])) == fix.sha256) else {
            continue;
        };
        match patch(bytes, fix.patches) {
            Ok(()) => applied.report.push(format!("fix {file}: {} ({})", fix.reason, fix.game)),
            Err(reason) => {
                applied.report.push(format!("warn {file}: known fix for {} not applied: {reason}", fix.game));
                continue;
            }
        }
        for (name, value) in fix.settings {
            applied.settings.push_str(&format!("{name}={value}\n"));
            applied.report.push(format!("fix game settings: {name}={value} ({})", fix.game));
        }
    }
    applied
}

/// Changes every patch's bytes, or none of them if any original bytes differ.
fn patch(library: &mut [u8], patches: &[(u32, &[u8], &[u8])]) -> Result<(), String> {
    let mut places = Vec::new();
    for &(address, old, new) in patches {
        let at = file_offset(library, address).ok_or_else(|| format!("{address:#x} is not in the library"))?;
        if library.get(at..at + old.len()) != Some(old) || old.len() != new.len() {
            return Err(format!("unexpected bytes at {address:#x}"));
        }
        places.push((at, new));
    }
    for (at, new) in places {
        library[at..at + new.len()].copy_from_slice(new);
    }
    Ok(())
}

/// Where a virtual address of a 32-bit ELF library lies in the file (from its loaded segments).
fn file_offset(elf: &[u8], address: u32) -> Option<usize> {
    let word = |at: usize| elf.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let half = |at: usize| elf.get(at..at + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));
    if elf.get(..5)? != b"\x7fELF\x01" {
        return None;
    }
    let (table, count) = (word(0x1c)? as usize, half(0x2c)? as usize);
    (0..count).find_map(|index| {
        let header = table + 32 * index;
        let (kind, offset, virtual_address, size) = (word(header)?, word(header + 4)?, word(header + 8)?, word(header + 16)?);
        (kind == 1 && (virtual_address..virtual_address.checked_add(size)?).contains(&address))
            .then(|| (address - virtual_address + offset) as usize)
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-bit ELF with one loaded segment: file 0x100.. at address 0x8100.
    fn library() -> Vec<u8> {
        let mut elf = vec![0u8; 0x200];
        elf[..5].copy_from_slice(b"\x7fELF\x01");
        elf[0x1c..0x20].copy_from_slice(&0x34u32.to_le_bytes());
        elf[0x2c..0x2e].copy_from_slice(&1u16.to_le_bytes());
        let header = [1u32, 0x100, 0x8100, 0x8100, 0x100, 0x100, 5, 0x1000];
        for (index, value) in header.iter().enumerate() {
            elf[0x34 + 4 * index..0x38 + 4 * index].copy_from_slice(&value.to_le_bytes());
        }
        elf[0x150..0x152].copy_from_slice(&[0x5b, 0x00]);
        elf
    }

    #[test]
    fn patches_at_the_address() {
        let mut elf = library();
        patch(&mut elf, &[(0x8150, &[0x5b, 0x00], &[0x5b, 0x08])]).unwrap();
        assert_eq!(&elf[0x150..0x152], &[0x5b, 0x08]);
    }

    #[test]
    fn leaves_other_bytes_alone() {
        let mut elf = library();
        let before = elf.clone();
        assert!(patch(&mut elf, &[(0x8150, &[0x5b, 0x00], &[0x5b, 0x08]), (0x8160, &[0x12], &[0x34])]).is_err());
        assert_eq!(elf, before);
        assert!(patch(&mut elf, &[(0x9000, &[0x00], &[0x01])]).is_err());
    }

    #[test]
    fn only_the_exact_library() {
        let mut libraries = vec![("libSparkApp.so".to_string(), library())];
        let applied = apply(&mut libraries);
        assert!(applied.report.is_empty() && applied.settings.is_empty());
        assert_eq!(libraries[0].1, library());
    }
}
