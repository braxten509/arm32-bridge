//! Unpacked textures, kept for next time. The graphics chip can't read
//! PVRTC, so those textures are unpacked to plain pixels here (pvrtc.rs);
//! the first time, the pixels are also saved in the game's Caches folder,
//! and after that (even in later runs) the saved copy is read back instead
//! of unpacking again. It works the same for every game: a texture is known
//! by its bytes, size and format. Over the size limit, the copies used
//! longest ago are dropped. Test switch: IOS_TEXTURE_CACHE=0 turns it off.

use std::io::{Read, Write};
use std::sync::Arc;

/// Saved copies of one game take at most this much space.
const LIMIT: u64 = 1 << 30;
/// Smaller textures unpack faster than they read back.
const KEEP_FROM: u32 = 128 * 128;
const MAGIC: &[u8; 4] = b"GBTX";
const HEADER: usize = 12;

/// A name for this texture: its bytes, size and format mixed together.
fn key(packed: &[u8], width: u32, height: u32, format: u32) -> u64 {
    let mix = |h: u64, word: u64| (h ^ word).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(31);
    let mut words = packed.chunks_exact(8);
    let mut h = mix(0x243F_6A88_85A3_08D3, packed.len() as u64);
    h = mix(h, (width as u64) << 32 | height as u64);
    h = mix(h, format as u64);
    for word in &mut words {
        h = mix(h, u64::from_le_bytes(word.try_into().unwrap()));
    }
    let mut tail = [0u8; 8];
    tail[..words.remainder().len()].copy_from_slice(words.remainder());
    let h = mix(h, u64::from_le_bytes(tail));
    h ^ h >> 29
}

fn read_copy(path: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    let length = width as usize * height as usize * 4;
    let mut file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() != (HEADER + length) as u64 {
        return None;
    }
    let mut header = [0u8; HEADER];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != MAGIC || header[4..8] != width.to_le_bytes() || header[8..] != height.to_le_bytes() {
        return None;
    }
    let mut pixels = Vec::with_capacity(length);
    file.take(length as u64).read_to_end(&mut pixels).ok()?;
    (pixels.len() == length).then_some(pixels)
}

/// Writes beside the copy's place, then moves it there: a run cut short
/// never leaves half a copy under the real name.
fn save_copy(dir: &str, path: &str, width: u32, height: u32, pixels: &[u8]) {
    static NUMBER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let number = NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let part = format!("{path}.{}-{number}.part", std::process::id());
    let written = std::fs::create_dir_all(dir).and_then(|_| {
        let mut file = std::fs::File::create(&part)?;
        file.write_all(MAGIC)?;
        file.write_all(&width.to_le_bytes())?;
        file.write_all(&height.to_le_bytes())?;
        file.write_all(pixels)
    });
    if written.and_then(|_| std::fs::rename(&part, path)).is_err() {
        let _ = std::fs::remove_file(&part);
    }
}

/// The texture's pixels: its saved copy in `dir`, or else `unpack`'s,
/// which are then saved (here, or on another thread when not `save_now`).
pub fn unpacked_in(dir: &str, packed: &[u8], width: u32, height: u32, format: u32, save_now: bool, unpack: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<Vec<u8>>> {
    let path = format!("{dir}/{:016x}-{width}x{height}.rgba", key(packed, width, height, format));
    if let Some(pixels) = AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|ahead| ahead.get(&path).cloned()) {
        return Some(pixels);
    }
    if let Some(pixels) = read_copy(&path, width, height) {
        // Just used: the last to go when trimming.
        if let Ok(file) = std::fs::File::options().append(true).open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Some(Arc::new(pixels));
    }
    let pixels = Arc::new(unpack()?);
    if pixels.len() == width as usize * height as usize * 4 {
        if save_now {
            save_copy(dir, &path, width, height, &pixels);
        } else {
            let (dir, pixels) = (dir.to_owned(), pixels.clone());
            std::thread::spawn(move || save_copy(&dir, &path, width, height, &pixels));
        }
    }
    Some(pixels)
}

/// Saved copies read into memory ahead, by file.
static AHEAD: std::sync::Mutex<Option<std::collections::HashMap<String, Arc<Vec<u8>>>>> = std::sync::Mutex::new(None);
/// At most this much of them.
const AHEAD_LIMIT: u64 = 256 << 20;

/// Reads the copies used most recently into memory, up to `limit` bytes
/// (at the game's start, on a thread of its own): a texture the game sends
/// later, mid-fight, then needs no disk read in the middle of a frame.
/// Returns how many are held.
pub fn read_ahead(dir: &str, limit: u64) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut copies: Vec<(std::time::SystemTime, u64, String)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "rgba"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.metadata().ok()?.len(), e.path().to_string_lossy().into_owned())))
        .collect();
    copies.sort_by(|a, b| b.0.cmp(&a.0));
    let mut total = 0;
    for (_, length, path) in copies {
        if total + length > limit {
            continue;
        }
        let Some(bytes) = std::fs::read(&path).ok().filter(|b| b.len() > HEADER && &b[..4] == MAGIC) else { continue };
        let (width, height) = (u32::from_le_bytes(bytes[4..8].try_into().unwrap()), u32::from_le_bytes(bytes[8..12].try_into().unwrap()));
        if bytes.len() != HEADER + width as usize * height as usize * 4 {
            continue;
        }
        total += length;
        AHEAD.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(path, Arc::new(bytes[HEADER..].to_vec()));
    }
    AHEAD.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |ahead| ahead.keys().filter(|k| k.starts_with(dir)).count())
}

/// Drops the copies used longest ago until the rest fit in `limit` bytes
/// (and any left half-written).
pub fn trim(dir: &str, limit: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut copies = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        match path.extension().and_then(|e| e.to_str()) {
            Some("rgba") => copies.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), path)),
            Some("part") => { let _ = std::fs::remove_file(&path); }
            _ => {}
        }
    }
    let mut total: u64 = copies.iter().map(|c| c.1).sum();
    copies.sort();
    for (_, length, path) in copies {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= length;
        }
    }
    total
}

/// The game's folder for saved copies; None when turned off.
fn folder() -> Option<&'static str> {
    static FOLDER: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FOLDER.get_or_init(|| {
        if std::env::var("IOS_TEXTURE_CACHE").is_ok_and(|v| v == "0") {
            crate::log!("graphics: unpacked textures are not kept (IOS_TEXTURE_CACHE=0)");
            return None;
        }
        let dir = format!("{}/Library/Caches/GameBridge textures", crate::fw::foundation::home());
        let shown = dir.clone();
        std::thread::spawn(move || {
            let kept = trim(&shown, LIMIT);
            crate::log!("graphics: unpacked textures are kept in {shown} ({} MB so far)", kept >> 20);
            let held = read_ahead(&shown, AHEAD_LIMIT);
            crate::log!("graphics: {held} unpacked textures read ahead");
        });
        Some(dir)
    }).as_deref()
}

/// At the game's start: saved copies are trimmed and read ahead meanwhile.
pub fn start() {
    let _ = folder();
}

/// A PVRTC texture's pixels, from its saved copy when there is one.
pub fn unpacked(packed: &[u8], width: u32, height: u32, format: u32, unpack: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<Vec<u8>>> {
    match folder() {
        Some(dir) if width.saturating_mul(height) >= KEEP_FROM => unpacked_in(dir, packed, width, height, format, false, unpack),
        _ => unpack().map(Arc::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("gamebridge-texture-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    }

    /// The first time a texture is unpacked and saved; after that (even in
    /// a new run, with nothing in memory) the saved copy is used instead.
    #[test]
    fn a_texture_is_unpacked_once_then_read_back() {
        let dir = folder("once");
        let packed = vec![7u8; 512];
        let pixels: Vec<u8> = (0..16 * 8 * 4).map(|n| n as u8).collect();
        let mut unpacks = 0;
        let first = unpacked_in(&dir, &packed, 16, 8, 0x8C02, true, || { unpacks += 1; Some(pixels.clone()) }).unwrap();
        let again = unpacked_in(&dir, &packed, 16, 8, 0x8C02, true, || { unpacks += 1; Some(pixels.clone()) }).unwrap();
        assert_eq!((&first[..], &again[..], unpacks), (&pixels[..], &pixels[..], 1));
        // Other bytes, size or format: a different texture.
        let other = [
            unpacked_in(&dir, &[8u8; 512], 16, 8, 0x8C02, true, || Some(vec![1; 16 * 8 * 4])),
            unpacked_in(&dir, &packed, 8, 16, 0x8C02, true, || Some(vec![2; 16 * 8 * 4])),
            unpacked_in(&dir, &packed, 16, 8, 0x8C00, true, || Some(vec![3; 16 * 8 * 4])),
        ];
        assert_eq!(other.map(|p| p.unwrap()[0]), [1, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saved copies are read into memory ahead (at the game's start, up to
    /// a limit): a texture asked for later needs no disk read or unpacking.
    #[test]
    fn saved_copies_are_read_ahead() {
        let dir = folder("ahead");
        let pixels: Vec<u8> = (0..16 * 8 * 4).map(|n| (n * 3) as u8).collect();
        unpacked_in(&dir, &[4u8; 512], 16, 8, 0x8C02, true, || Some(pixels.clone())).unwrap();
        unpacked_in(&dir, &[5u8; 512], 16, 8, 0x8C02, true, || Some(pixels.clone())).unwrap();
        // Room for one: only one is read ahead.
        assert_eq!(read_ahead(&dir, (HEADER + pixels.len()) as u64), 1);
        assert_eq!(read_ahead(&dir, 1 << 20), 2);
        for entry in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let kept = unpacked_in(&dir, &[4u8; 512], 16, 8, 0x8C02, true, || panic!("unpacked again")).unwrap();
        assert_eq!(&kept[..], &pixels[..]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A saved file cut short (the phone shut down mid-save) is unpacked again.
    #[test]
    fn a_damaged_copy_is_not_used() {
        let dir = folder("damaged");
        let packed = vec![9u8; 512];
        let pixels = vec![5u8; 16 * 8 * 4];
        unpacked_in(&dir, &packed, 16, 8, 0x8C02, true, || Some(pixels.clone())).unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        }
        let mut unpacks = 0;
        let again = unpacked_in(&dir, &packed, 16, 8, 0x8C02, true, || { unpacks += 1; Some(pixels.clone()) }).unwrap();
        assert_eq!((&again[..], unpacks), (&pixels[..], 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Over the size limit, the copies used longest ago go first.
    #[test]
    fn trimming_drops_the_oldest_copies() {
        let dir = folder("trim");
        let pixels = vec![1u8; 16 * 8 * 4];
        for seed in 0..4u8 {
            unpacked_in(&dir, &[seed; 512], 16, 8, 0x8C02, true, || Some(pixels.clone())).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Using the first one again makes it the newest.
        unpacked_in(&dir, &[0; 512], 16, 8, 0x8C02, true, || None).unwrap();
        let one = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().metadata().unwrap().len();
        trim(&dir, one * 2);
        let mut unpacks = Vec::new();
        for seed in 0..4u8 {
            unpacked_in(&dir, &[seed; 512], 16, 8, 0x8C02, true, || { unpacks.push(seed); Some(pixels.clone()) });
        }
        assert_eq!(unpacks, [1, 2]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
