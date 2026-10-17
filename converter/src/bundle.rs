//! Bundles: one game split over several APKs in one zip file. Covers .xapk
//! (APKPure), .apkm (APKMirror) and .apks (SAI and bundletool): each holds a
//! base APK, split APKs (config.armeabi_v7a.apk and the like, asset packs)
//! and sometimes game data (OBB files) under Android/obb/. Android installs
//! the pieces together, and they must all carry the same signature.
//!
//! Pieces are read in place when the bundle stores them uncompressed (the
//! usual case), so a large asset pack isn't copied just to look inside it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::Path;

use zip::{CompressionMethod, ZipArchive};

/// A byte range of a file, read as a file of its own.
pub struct Window {
    file: File,
    start: u64,
    len: u64,
    at: u64,
}

impl Read for Window {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = (self.len - self.at.min(self.len)) as usize;
        let count = buffer.len().min(left);
        let read = self.file.read_at(&mut buffer[..count], self.start + self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

impl Seek for Window {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.len.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.at.checked_add_signed(offset),
        };
        self.at = target.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.at)
    }
}

pub struct Piece {
    /// The file name it is written out under.
    pub name: String,
    pub archive: ZipArchive<Window>,
    /// The piece's own bytes, for copying it out whole.
    bytes: Window,
}

impl Piece {
    pub fn names(&self) -> Vec<String> {
        self.archive.file_names().map(String::from).collect()
    }

    /// Whether it holds Java code (the base APK does; config splits and asset packs don't).
    pub fn has_code(&self) -> bool {
        self.archive.file_names().any(|name| name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/'))
    }

    /// Whether it is the base APK: it has Java code and its manifest names no
    /// split (feature pieces have code too, but say which split they are).
    pub fn is_base(&mut self) -> bool {
        if !self.has_code() {
            return false;
        }
        let mut manifest = Vec::new();
        let read = self.archive.by_name("AndroidManifest.xml").ok().and_then(|mut file| std::io::Read::read_to_end(&mut file, &mut manifest).ok());
        read.is_some() && crate::manifest::split_name(&manifest).is_none()
    }
}

pub struct Bundle {
    pub pieces: Vec<Piece>,
    /// Game data files: (index in the bundle, path under Android/obb/).
    pub game_data: Vec<(usize, String)>,
}

/// Opens `path` as a bundle; None when it is a plain APK (or not a bundle at
/// all). Compressed pieces are unpacked into `work` first.
pub fn open(path: &Path, work: &Path) -> Result<Option<Bundle>, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut outer = ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    if outer.index_for_name("AndroidManifest.xml").is_some() {
        return Ok(None);
    }
    let apk_entries: Vec<(usize, String)> = (0..outer.len())
        .filter_map(|index| {
            let name = outer.name_for_index(index)?.to_string();
            name.to_ascii_lowercase().ends_with(".apk").then_some((index, name))
        })
        .collect();
    if apk_entries.is_empty() {
        return Ok(None);
    }
    // bundletool's .apks also holds whole-app copies for old phones next to the split pieces.
    let has_splits = apk_entries.iter().any(|(_, name)| !name.starts_with("standalones/"));
    let mut pieces = Vec::new();
    for (index, entry_name) in apk_entries {
        if has_splits && entry_name.starts_with("standalones/") {
            continue;
        }
        let file_name = entry_name.rsplit('/').next().unwrap().to_string();
        let mut name = file_name.clone();
        let mut copy = 2;
        while pieces.iter().any(|piece: &Piece| piece.name == name) {
            name = format!("{copy}-{file_name}");
            copy += 1;
        }
        let bytes = piece_window(&mut outer, path, index, work, &name)?;
        let window = Window { file: bytes.file.try_clone().map_err(|e| e.to_string())?, ..bytes };
        let archive = ZipArchive::new(window).map_err(|e| {
            format!("{entry_name} inside the bundle can't be read ({e}); it may be locked by the site it came from")
        })?;
        pieces.push(Piece { name, archive, bytes });
    }
    let game_data = (0..outer.len())
        .filter_map(|index| {
            let name = outer.name_for_index(index)?;
            let relative = name.strip_prefix("Android/obb/")?;
            (!relative.is_empty() && !name.ends_with('/')).then(|| (index, relative.to_string()))
        })
        .collect();
    Ok(Some(Bundle { pieces, game_data }))
}

/// The piece's bytes: straight from the bundle when stored, else unpacked into `work`.
fn piece_window(outer: &mut ZipArchive<File>, path: &Path, index: usize, work: &Path, name: &str) -> Result<Window, String> {
    let error = |e: &dyn std::fmt::Display| format!("read {name} from the bundle: {e}");
    let entry = outer.by_index_raw(index).map_err(|e| error(&e))?;
    if entry.compression() == CompressionMethod::Stored {
        if let Some(start) = entry.data_start() {
            let len = entry.compressed_size();
            drop(entry);
            return Ok(Window { file: File::open(path).map_err(|e| error(&e))?, start, len, at: 0 });
        }
    }
    drop(entry);
    std::fs::create_dir_all(work).map_err(|e| error(&e))?;
    let unpacked = work.join(format!("piece-{index}-{name}"));
    let mut out = File::create(&unpacked).map_err(|e| error(&e))?;
    std::io::copy(&mut outer.by_index(index).map_err(|e| error(&e))?, &mut out).map_err(|e| error(&e))?;
    let file = File::open(&unpacked).map_err(|e| error(&e))?;
    // Unlinked now; the open file keeps the bytes until the converter is done.
    let _ = std::fs::remove_file(&unpacked);
    let len = file.metadata().map_err(|e| error(&e))?.len();
    Ok(Window { file, start: 0, len, at: 0 })
}

/// Copies a whole piece out unchanged.
pub fn save(piece: &mut Piece, output: &Path) -> std::io::Result<()> {
    piece.bytes.at = 0;
    std::io::copy(&mut piece.bytes, &mut File::create(output)?)?;
    Ok(())
}
