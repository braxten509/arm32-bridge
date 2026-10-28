//! Sharper pictures from a pack of AI-upscaled ones (owner's ask, made for
//! Rule the Kingdom, which the bridge draws 1.5 times bigger: rtk_zoom.rs).
//!
//! The pack (`assets/gamebridge-textures.pack` in the game's APK, stored, not
//! compressed) holds a picture twice as wide and tall for each picture file
//! the game loads, by the path the game loads it with. A game wrap tells this
//! layer when the game has loaded a picture into memory (`loaded`); when the
//! game then hands those bytes to OpenGL, the 2x picture goes up instead.
//! The game's sprites use texture coordinates from 0 to 1, so a texture with
//! twice the pixels draws the same, only sharper. Everything else the game
//! uploads into such a texture (atlas pieces, text) is made 2x on the CPU so
//! it still lands where the game put it.
//!
//! Nothing changes for a game without a pack.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const GL_ALPHA: u32 = 0x1906;
pub const GL_RGB: u32 = 0x1907;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_LUMINANCE: u32 = 0x1909;
pub const GL_LUMINANCE_ALPHA: u32 = 0x190A;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_UNSIGNED_SHORT_4_4_4_4: u32 = 0x8033;
pub const GL_UNSIGNED_SHORT_5_5_5_1: u32 = 0x8034;
pub const GL_UNSIGNED_SHORT_5_6_5: u32 = 0x8363;

const MAGIC: &[u8; 8] = b"GBTX1\0\0\0";
const PACK_IN_APK: &str = "assets/gamebridge-textures.pack";

/// A picture in the pack: its 1x size as the game loads it, and where its 2x WebP is.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub width: u32,
    pub height: u32,
    /// 4: RGBA (the game's 32 bits), 3: RGB (24 bits).
    pub channels: u8,
    offset: u64,
    length: u32,
}

/// The biggest 2x picture taken, in pixels (bigger means a broken index; it
/// would need a buffer of hundreds of MB).
const MAX_PICTURE_PIXELS: u64 = 64 << 20;

/// The pack's index (its header and entries; the pictures stay in the file).
/// `pack_length`: the pack's size in bytes. Entries whose picture runs past
/// the end, or would be absurdly big, are dropped.
pub fn read_index(reader: &mut impl Read, pack_length: u64) -> Result<HashMap<String, Entry>, String> {
    let mut read = |length: usize| -> Result<Vec<u8>, String> {
        let mut bytes = vec![0u8; length];
        reader.read_exact(&mut bytes).map_err(|e| format!("index cut short: {e}"))?;
        Ok(bytes)
    };
    if read(8)? != MAGIC {
        return Err("not a GBTX1 pack".into());
    }
    let count = u32::from_le_bytes(read(4)?.try_into().unwrap());
    let mut entries = HashMap::new();
    let mut dropped = 0;
    for _ in 0..count {
        let path_length = u16::from_le_bytes(read(2)?.try_into().unwrap()) as usize;
        let path = String::from_utf8(read(path_length)?).map_err(|_| "a path is not UTF-8")?;
        let rest = read(4 + 4 + 1 + 8 + 4)?;
        let u32_at = |at: usize| u32::from_le_bytes(rest[at..at + 4].try_into().unwrap());
        let entry = Entry {
            width: u32_at(0),
            height: u32_at(4),
            channels: rest[8],
            offset: u64::from_le_bytes(rest[9..17].try_into().unwrap()),
            length: u32_at(17),
        };
        let inside = entry.offset.checked_add(entry.length as u64).is_some_and(|end| end <= pack_length);
        let pixels = entry.width as u64 * 2 * entry.height as u64 * 2;
        if !inside || pixels > MAX_PICTURE_PIXELS {
            dropped += 1;
            continue;
        }
        entries.insert(path, entry);
    }
    if dropped > 0 {
        crate::log::line(&format!("[textures] {dropped} broken pack entries dropped (past the end or too big)"));
    }
    Ok(entries)
}

/// Where the stored (not compressed) file `name` inside the zip `file` starts, and its size.
pub fn stored_entry(file: &File, name: &str) -> Result<(u64, u64), String> {
    let at = |offset: u64, length: usize| -> Result<Vec<u8>, String> {
        let mut bytes = vec![0u8; length];
        file.read_exact_at(&mut bytes, offset).map_err(|e| format!("zip read at {offset}: {e}"))?;
        Ok(bytes)
    };
    let u16_in = |data: &[u8], at: usize| u16::from_le_bytes([data[at], data[at + 1]]) as u64;
    let u32_in = |data: &[u8], at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as u64;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    // The end record (0x06054b50) sits in the last 64 KiB + 22 bytes.
    let tail_length = length.min(65_557);
    let tail = at(length - tail_length, tail_length as usize)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_in(&tail, i) == 0x0605_4b50)
        .ok_or("no zip end record")?;
    let (count, size, offset) = (u16_in(&tail, end + 10), u32_in(&tail, end + 12), u32_in(&tail, end + 16));
    let directory = at(offset, size as usize)?;
    let mut entry = 0usize;
    for _ in 0..count {
        if entry + 46 > directory.len() || u32_in(&directory, entry) != 0x0201_4b50 {
            break;
        }
        let name_length = u16_in(&directory, entry + 28) as usize;
        let skip = (u16_in(&directory, entry + 30) + u16_in(&directory, entry + 32)) as usize;
        let found = directory.get(entry + 46..entry + 46 + name_length) == Some(name.as_bytes());
        if found {
            let method = u16_in(&directory, entry + 10);
            if method != 0 {
                return Err(format!("{name} is compressed (method {method}); it must be stored"));
            }
            let size = u32_in(&directory, entry + 20);
            // The data follows the local header, whose name and extra lengths
            // can differ from the central ones (zipalign pads the extra).
            let local_at = u32_in(&directory, entry + 42);
            let local = at(local_at, 30)?;
            if u32_in(&local, 0) != 0x0403_4b50 {
                return Err(format!("{name}'s local header is missing"));
            }
            return Ok((local_at + 30 + u16_in(&local, 26) + u16_in(&local, 28), size));
        }
        entry += 46 + name_length + skip;
    }
    Err(format!("no {name} in the APK"))
}

/// An open pack: its index, and the file its pictures are read from on demand.
pub struct Pack {
    file: File,
    /// Where the pack starts in `file` (inside the APK).
    start: u64,
    pub entries: HashMap<String, Entry>,
}

impl Pack {
    /// The pack inside the APK at `apk`.
    pub fn in_apk(apk: &Path) -> Result<Pack, String> {
        let file = File::open(apk).map_err(|e| format!("{}: {e}", apk.display()))?;
        let (start, length) = stored_entry(&file, PACK_IN_APK)?;
        Pack::at(file, start, length)
    }

    /// A pack file on its own (for tests).
    pub fn file(path: &Path) -> Result<Pack, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        Pack::at(file, 0, length)
    }

    fn at(file: File, start: u64, length: u64) -> Result<Pack, String> {
        // Buffered: the index is many small reads.
        let mut reader = std::io::BufReader::new(Offset { file: &file, at: start });
        let entries = read_index(&mut reader, length)?;
        Ok(Pack { file, start, entries })
    }

    /// The 2x picture `path` as the game would hold it.
    pub fn picture(&self, path: &str) -> Result<Vec<u8>, String> {
        let entry = self.entries.get(path).ok_or_else(|| format!("{path} is not in the pack"))?;
        let mut webp = vec![0u8; entry.length as usize];
        self.file.read_exact_at(&mut webp, self.start + entry.offset).map_err(|e| format!("{path}: {e}"))?;
        decode(entry, &webp).map_err(|e| format!("{path}: {e}"))
    }
}

/// Reads a file from `at` on (positioned reads, so threads can share the file).
struct Offset<'a> {
    file: &'a File,
    at: u64,
}

impl Read for Offset<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

/// A 2x WebP picture as the game would hold it: 2w x 2h, RGBA premultiplied
/// like the game does (4 channels), or RGB.
pub fn decode(entry: &Entry, webp: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(webp)).map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    if (width, height) != (entry.width * 2, entry.height * 2) {
        return Err(format!("{width}x{height}, not 2x of {}x{}", entry.width, entry.height));
    }
    let size = decoder.output_buffer_size().ok_or("too big")?;
    let mut decoded = vec![0u8; size];
    decoder.read_image(&mut decoded).map_err(|e| e.to_string())?;
    let has_alpha = decoder.has_alpha();
    let pixels = match (entry.channels, has_alpha) {
        (4, true) | (3, false) => decoded,
        (4, false) => decoded.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        (3, true) => decoded.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        (other, _) => return Err(format!("{other} channels")),
    };
    let mut pixels = pixels;
    if entry.channels == 4 {
        premultiply(&mut pixels);
    }
    Ok(pixels)
}

/// Premultiplies RGBA pixels the way Render::GetImageData does: each colour
/// becomes (int)(c * a / 255.0f), cut off, not rounded.
pub fn premultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha == 255 {
            continue;
        }
        for colour in &mut pixel[..3] {
            *colour = ((*colour as u32 * alpha) as f32 / 255.0f32) as u8;
        }
    }
}

/// How many channels a picture of GL `format` has (None: one this layer leaves alone).
pub fn channels(format: u32) -> Option<u32> {
    match format {
        GL_ALPHA | GL_LUMINANCE => Some(1),
        GL_LUMINANCE_ALPHA => Some(2),
        GL_RGB => Some(3),
        GL_RGBA => Some(4),
        _ => None,
    }
}

/// Bytes per pixel of game pixels in `format` and `kind` (None: left alone).
pub fn pixel_size(format: u32, kind: u32) -> Option<u32> {
    match (format, kind) {
        (_, GL_UNSIGNED_BYTE) => channels(format),
        (GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1) | (GL_RGB, GL_UNSIGNED_SHORT_5_6_5) => Some(2),
        _ => None,
    }
}

/// Bytes from one row's start to the next's, as GL reads them (GL_UNPACK_ALIGNMENT).
pub fn row_stride(width: u32, pixel_size: u32, alignment: u32) -> u32 {
    let alignment = alignment.max(1);
    (width * pixel_size).div_ceil(alignment) * alignment
}

/// Game pixels (rows `stride` bytes apart) as 8 bits per channel, rows packed.
pub fn to_bytes(data: &[u8], width: u32, height: u32, stride: u32, format: u32, kind: u32) -> Option<Vec<u8>> {
    let size = pixel_size(format, kind)?;
    let row_bytes = (width * size) as usize;
    if height == 0 || data.len() < stride as usize * (height as usize - 1) + row_bytes {
        return None;
    }
    let mut out = Vec::with_capacity((width * height * channels(format)?) as usize);
    // A 4- or 5-bit number spread over 0-255.
    let four = |v: u16| (v & 0xF) as u8 * 17;
    let five = |v: u16| ((v & 0x1F) << 3 | (v & 0x1F) >> 2) as u8;
    let six = |v: u16| ((v & 0x3F) << 2 | (v & 0x3F) >> 4) as u8;
    for row in data.chunks(stride as usize).take(height as usize) {
        let row = &row[..row_bytes];
        if kind == GL_UNSIGNED_BYTE {
            out.extend_from_slice(row);
            continue;
        }
        for pair in row.chunks_exact(2) {
            let v = u16::from_le_bytes([pair[0], pair[1]]);
            match kind {
                GL_UNSIGNED_SHORT_4_4_4_4 => out.extend([four(v >> 12), four(v >> 8), four(v >> 4), four(v)]),
                GL_UNSIGNED_SHORT_5_5_5_1 => out.extend([five(v >> 11), five(v >> 6), five(v >> 1), (v & 1) as u8 * 255]),
                _ => out.extend([five(v >> 11), six(v >> 5), five(v)]),
            }
        }
    }
    Some(out)
}

/// Pixels made twice as wide and tall (each new pixel mixes its 4 nearest
/// old ones 9:3:3:1; edges repeat, so atlas neighbours don't bleed in).
pub fn upscale(pixels: &[u8], width: u32, height: u32, channels: u32) -> Vec<u8> {
    let (w, h, c) = (width as usize, height as usize, channels as usize);
    let mut out = vec![0u8; w * h * c * 4];
    if w == 0 || h == 0 || pixels.len() < w * h * c {
        return out;
    }
    // A new pixel's nearer old pixel, and the one it leans 1/4 toward.
    let near = |new: usize, size: usize| {
        let old = new / 2;
        let other = if new % 2 == 0 { old.saturating_sub(1) } else { (old + 1).min(size - 1) };
        (old, other)
    };
    for y in 0..h * 2 {
        let (y0, y1) = near(y, h);
        for x in 0..w * 2 {
            let (x0, x1) = near(x, w);
            for k in 0..c {
                let at = |x: usize, y: usize| pixels[(y * w + x) * c + k] as u32;
                let mixed = 9 * at(x0, y0) + 3 * at(x1, y0) + 3 * at(x0, y1) + at(x1, y1);
                out[(y * w * 2 + x) * c + k] = ((mixed + 8) / 16) as u8;
            }
        }
    }
    out
}

/// A quick fingerprint of a picture's bytes (to see whether the game changed
/// them between loading and uploading, as its glow filter does).
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        hash = (hash.rotate_left(5) ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(0x517C_C1B7_2722_0A95);
    }
    for &byte in words.remainder() {
        hash = (hash.rotate_left(5) ^ byte as u64).wrapping_mul(0x517C_C1B7_2722_0A95);
    }
    hash
}

/// A glTexImage2D or glTexSubImage2D call's numbers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Call {
    pub level: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub kind: u32,
    /// The guest address of the pixels (0: none).
    pub data: u32,
}

/// What to do with a glTexImage2D.
#[derive(Debug, PartialEq)]
pub enum Image {
    /// Pass it on unchanged; the texture is 1x.
    AsAsked,
    /// Upload the pack's 2x picture `path` (`width` x `height` at 1x) instead.
    Replace { path: String, width: u32, height: u32, channels: u8 },
    /// Make the empty texture this size and format (8 bits per channel).
    Allocate { width: u32, height: u32, format: u32 },
}

/// What to do with a glTexSubImage2D.
#[derive(Debug, PartialEq)]
pub enum SubImage {
    /// Pass it on unchanged (a 1x texture).
    AsAsked,
    /// Upload rows [first_row, first_row + rows) of the pack's 2x picture
    /// `path` at (x, y). `last`: no more pieces of it will come.
    Rows { path: String, width: u32, channels: u8, first_row: u32, rows: u32, x: i32, y: i32, last: bool },
    /// Make the game's piece 2x on the CPU and upload it at (x, y).
    Upscale { x: i32, y: i32 },
}

/// A picture the game loaded that the pack has, waiting to be uploaded.
#[derive(Clone, Debug)]
struct Pending {
    address: u32,
    width: u32,
    height: u32,
    channels: u8,
    path: String,
    fingerprint: u64,
    /// A few of its pixels as loaded (SAMPLE_PIXELS, evenly spread), to
    /// compare with 16-bit uploads, which a fingerprint can't.
    sample: Vec<u8>,
    /// Rows already uploaded (the game may upload a tall picture in two pieces).
    rows_uploaded: u32,
    /// Whether the bytes were found unchanged at the first piece.
    checked: bool,
}

/// At most this many loaded pictures wait for their upload (the oldest go first).
const MAX_PENDING: usize = 32;

/// How many of a loaded picture's pixels are kept to compare with a 16-bit upload.
const SAMPLE_PIXELS: u32 = 64;

/// Guest memory as the upload decisions read it.
pub trait Memory {
    /// `fingerprint` of `length` bytes at `address`.
    fn fingerprint(&self, address: u32, length: usize) -> u64;
    /// A copy of `length` bytes at `address` (small reads).
    fn read(&self, address: u32, length: usize) -> Vec<u8>;
    /// Whether all `length` bytes at `address` are zero.
    fn zero(&self, address: u32, length: usize) -> bool;
}

/// The running game's memory.
struct Guest;

impl Memory for Guest {
    fn fingerprint(&self, address: u32, length: usize) -> u64 {
        fingerprint(crate::runtime::mem().bytes(address, length))
    }
    fn read(&self, address: u32, length: usize) -> Vec<u8> {
        crate::runtime::mem().bytes(address, length).to_vec()
    }
    fn zero(&self, address: u32, length: usize) -> bool {
        let memory = crate::runtime::mem();
        let bytes = memory.bytes(address, length);
        // Eight bytes at a time: atlases are megabytes.
        let (start, words, end) = unsafe { bytes.align_to::<u64>() };
        start.iter().chain(end).all(|&b| b == 0) && words.iter().all(|&w| w == 0)
    }
}

/// Which pixels (by number, row after row) are sampled from a picture of `count` pixels.
fn sampled(count: u32) -> impl Iterator<Item = u32> {
    let taken = count.min(SAMPLE_PIXELS);
    (0..taken).map(move |i| (i as u64 * count as u64 / taken as u64) as u32)
}

/// Which textures are 2x, and the loaded pictures waiting for upload.
#[derive(Default)]
pub struct Uploads {
    pending: VecDeque<Pending>,
    /// 2x textures (by GL name) and the format they are stored in.
    doubled: HashMap<u32, u32>,
}

impl Uploads {
    /// The game loaded picture `path` (in the pack) into `address`.
    pub fn add_pending(&mut self, address: u32, width: u32, height: u32, channels: u8, path: &str, memory: impl Memory) {
        // A new buffer at an old one's address: the old one was freed.
        self.pending.retain(|pending| pending.address != address);
        if self.pending.len() >= MAX_PENDING {
            self.pending.pop_front();
        }
        let path = path.to_string();
        let size = channels as u32;
        let fingerprint = memory.fingerprint(address, (width * height * size) as usize);
        let sample = sampled(width * height).flat_map(|pixel| memory.read(address + pixel * size, size as usize)).collect();
        let pending = Pending { address, width, height, channels, path, fingerprint, sample, rows_uploaded: 0, checked: false };
        self.pending.push_back(pending);
    }

    /// Whether texture `texture` is 2x.
    pub fn doubled(&self, texture: u32) -> bool {
        self.doubled.contains_key(&texture)
    }

    /// The texture was deleted or redone at 1x.
    pub fn forget(&mut self, texture: u32) {
        self.doubled.remove(&texture);
    }

    /// glTexImage2D into `texture`. `max_size`: GL's largest texture side;
    /// `memory`: the guest's, to check the pixels are still as loaded.
    pub fn image(&mut self, texture: u32, call: Call, max_size: u32, memory: impl Memory) -> Image {
        // Only the full-size level is made 2x (the game makes no smaller ones).
        if call.level != 0 || call.width <= 0 || call.height <= 0 {
            return Image::AsAsked;
        }
        self.forget(texture);
        let (width, height) = (call.width as u32, call.height as u32);
        let fits = width * 2 <= max_size && height * 2 <= max_size;
        let allocate = |uploads: &mut Uploads| {
            uploads.doubled.insert(texture, call.format);
            Image::Allocate { width: width * 2, height: height * 2, format: call.format }
        };
        let size = pixel_size(call.format, call.kind);
        if call.data == 0 {
            return if fits && size.is_some() { allocate(self) } else { Image::AsAsked };
        }
        let found = self.pending.iter().position(|p| {
            p.address == call.data && p.width == width && p.height == height && call.format == format_of(p.channels)
        });
        let Some(index) = found else {
            // A buffer of zeros is an empty texture too: Render::CreateAtlas
            // (Ghidra 0x1fa980) makes its atlases so, then puts pictures in.
            let empty = size.is_some_and(|size| fits && memory.zero(call.data, (width * height * size) as usize));
            return if empty { allocate(self) } else { Image::AsAsked };
        };
        let pending = self.pending.remove(index).unwrap();
        if !fits || !unchanged(&pending, call, &memory) {
            return Image::AsAsked;
        }
        self.doubled.insert(texture, call.format);
        Image::Replace { path: pending.path, width, height, channels: pending.channels }
    }

    /// glTexSubImage2D into `texture`; `stride` is the bytes between the piece's rows.
    pub fn sub_image(&mut self, texture: u32, call: Call, stride: u32, memory: impl Memory) -> SubImage {
        if call.level != 0 || !self.doubled(texture) || call.data == 0 || call.width <= 0 || call.height <= 0 || stride == 0 {
            return SubImage::AsAsked;
        }
        let upscale = SubImage::Upscale { x: call.x * 2, y: call.y * 2 };
        let (width, rows) = (call.width as u32, call.height as u32);
        // The loaded picture this piece is whole rows of.
        let found = self.pending.iter().position(|p| {
            let past = call.data.wrapping_sub(p.address);
            call.data >= p.address
                && p.width == width
                && call.format == format_of(p.channels)
                && past % stride == 0
                && past / stride + rows <= p.height
        });
        let Some(index) = found else { return upscale };
        let pending = &mut self.pending[index];
        // The fingerprint is of the whole picture: checked at its first piece.
        if !pending.checked {
            if !unchanged(pending, call, &memory) {
                self.pending.remove(index);
                return upscale;
            }
            pending.checked = true;
        }
        let first_row = (call.data - pending.address) / stride;
        pending.rows_uploaded += rows;
        let last = pending.rows_uploaded >= pending.height;
        let rows = SubImage::Rows {
            path: pending.path.clone(),
            width,
            channels: pending.channels,
            first_row: first_row * 2,
            rows: rows * 2,
            x: call.x * 2,
            y: call.y * 2,
            last,
        };
        if last {
            self.pending.remove(index);
        }
        rows
    }
}

/// Whether a loaded picture is still as the game loaded it when `call` uploads
/// it: in its own format, with the same bytes. Bytes the game made 16-bit in
/// place (Convert32To16Bits) can't be fingerprinted; their sampled pixels are compared instead.
fn unchanged(pending: &Pending, call: Call, memory: &impl Memory) -> bool {
    match call.kind {
        GL_UNSIGNED_BYTE => {
            let length = pending.width as usize * pending.height as usize * pending.channels as usize;
            memory.fingerprint(pending.address, length) == pending.fingerprint
        }
        GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1 => pending.channels == 4 && sample_matches(pending, call.kind, memory),
        GL_UNSIGNED_SHORT_5_6_5 => pending.channels == 3 && sample_matches(pending, call.kind, memory),
        _ => false,
    }
}

/// Whether the sampled pixels, now 16-bit at the picture's address, are the
/// loaded ones at 16-bit precision (one step either way for the game's rounding).
fn sample_matches(pending: &Pending, kind: u32, memory: &impl Memory) -> bool {
    // Each channel's place in the 16-bit value: (shift, bits).
    let fields: &[(u32, u32)] = match kind {
        GL_UNSIGNED_SHORT_4_4_4_4 => &[(12, 4), (8, 4), (4, 4), (0, 4)],
        GL_UNSIGNED_SHORT_5_5_5_1 => &[(11, 5), (6, 5), (1, 5), (0, 1)],
        _ => &[(11, 5), (5, 6), (0, 5)],
    };
    let size = pending.channels as usize;
    // Render::Convert32To16Bits (Ghidra 0x1fb8f0) starts each 16-bit row on
    // an even pixel, so odd widths pad rows to 4 bytes.
    let stride = row_stride(pending.width, 2, 4);
    sampled(pending.width * pending.height).zip(pending.sample.chunks_exact(size)).all(|(pixel, loaded)| {
        let (x, y) = (pixel % pending.width, pixel / pending.width);
        let bytes = memory.read(pending.address + y * stride + x * 2, 2);
        let value = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
        fields.iter().zip(loaded).all(|(&(shift, bits), &channel)| {
            let now = (value >> shift) & ((1 << bits) - 1);
            // A single alpha bit can't be told from the game's rounding: not compared.
            bits == 1 || now.abs_diff(channel as u32 >> (8 - bits)) <= 1
        })
    })
}

/// The GL format of a pack picture with `channels`.
fn format_of(channels: u8) -> u32 {
    if channels == 4 { GL_RGBA } else { GL_RGB }
}

// ---- the running game's pack ----

/// The pack, found once (None: this game has none, so nothing here does anything).
static PACK: OnceLock<Option<Pack>> = OnceLock::new();
static UPLOADS: Mutex<Option<Uploads>> = Mutex::new(None);
/// The last 2x picture decoded, kept while more pieces of it are to come.
static DECODED: Mutex<Option<(String, Arc<Vec<u8>>)>> = Mutex::new(None);

/// A pack file to use instead of the APK's (tests): this environment
/// variable, else this phone setting. "0" turns packs off.
pub const PACK_VARIABLE: &str = "GAMEBRIDGE_TEXTURE_PACK";
pub const PACK_SETTING: &str = "debug.arm32bridge.texturepack";

fn pack() -> Option<&'static Pack> {
    PACK.get_or_init(open).as_ref()
}

/// Whether this game has a pack.
pub fn active() -> bool {
    pack().is_some()
}

fn open() -> Option<Pack> {
    let chosen = std::env::var(PACK_VARIABLE).ok().filter(|v| !v.is_empty()).or_else(|| crate::dynarec::property(PACK_SETTING));
    let (opened, from) = match chosen {
        Some(off) if off == "0" => {
            crate::log::line("[textures] picture pack turned off");
            return None;
        }
        Some(path) => (Pack::file(Path::new(&path)), path),
        None => {
            let apk = installed_apk()?;
            (Pack::in_apk(&apk), apk.display().to_string())
        }
    };
    match opened {
        Ok(pack) => {
            crate::log::line(&format!("[textures] {} 2x pictures in the pack ({from})", pack.entries.len()));
            Some(pack)
        }
        // Most games have no pack: nothing to say.
        Err(why) if why == format!("no {PACK_IN_APK} in the APK") => None,
        Err(why) => {
            crate::log::line(&format!("[textures] can't read the picture pack: {why}"));
            None
        }
    }
}

/// The game's installed APK, from where Android loaded this library.
#[cfg(target_os = "android")]
fn installed_apk() -> Option<PathBuf> {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    if unsafe { libc::dladdr(active as *const libc::c_void, &mut info) } == 0 || info.dli_fname.is_null() {
        return None;
    }
    let library = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned();
    apk_of_library(&library)
}

#[cfg(not(target_os = "android"))]
fn installed_apk() -> Option<PathBuf> {
    None
}

/// …/<package>-<id>/lib/arm64/libarm32_bridge.so -> …/<package>-<id>/base.apk;
/// a library loaded straight from the APK (…/base.apk!/lib/…) names it itself.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn apk_of_library(library: &str) -> Option<PathBuf> {
    if let Some((apk, _)) = library.split_once(".apk!/") {
        return Some(PathBuf::from(format!("{apk}.apk")));
    }
    Some(Path::new(library).parent()?.parent()?.parent()?.join("base.apk"))
}

fn with_uploads<R>(work: impl FnOnce(&mut Uploads) -> R) -> R {
    work(UPLOADS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Uploads::default))
}

/// Logs `text` while `count` of its kind stays under `limit` (not per frame).
fn note(count: &std::sync::atomic::AtomicU32, limit: u32, text: impl FnOnce() -> String) {
    if count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < limit {
        crate::log::line(&format!("[textures] {}", text()));
    }
}

/// The game loaded picture `path` into `address` (`bits` per pixel): if the
/// pack has it at this size, its upload will get the 2x one.
pub fn loaded(path: &str, address: u32, width: u32, height: u32, bits: u32) {
    static MISSING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static DIFFERENT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let Some(pack) = pack() else { return };
    let channels = match bits {
        32 => 4,
        24 => 3,
        _ => return,
    };
    let Some(entry) = pack.entries.get(path) else {
        note(&MISSING, 20, || format!("{path} ({width}x{height}) is not in the pack"));
        return;
    };
    if (entry.width, entry.height, entry.channels) != (width, height, channels) {
        note(&DIFFERENT, 20, || {
            format!("{path} is {width}x{height} {bits}-bit, the pack's is {}x{} with {} channels", entry.width, entry.height, entry.channels)
        });
        return;
    }
    with_uploads(|uploads| uploads.add_pending(address, width, height, channels, path, Guest));
}

/// glTexImage2D's decision (gles.rs).
pub fn image(texture: u32, call: Call, max_size: u32) -> Image {
    with_uploads(|uploads| uploads.image(texture, call, max_size, Guest))
}

/// glTexSubImage2D's decision (gles.rs).
pub fn sub_image(texture: u32, call: Call, stride: u32) -> SubImage {
    with_uploads(|uploads| uploads.sub_image(texture, call, stride, Guest))
}

/// The texture is gone or 1x again.
pub fn forget(texture: u32) {
    with_uploads(|uploads| uploads.forget(texture));
}

/// The 2x picture `path`, decoded (or the one kept from its last piece);
/// `keep`: more pieces of it will come. None: it can't be read (logged).
pub fn picture(path: &str, keep: bool) -> Option<Arc<Vec<u8>>> {
    static FAILED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static SIZES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    let mut decoded = DECODED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((kept, pixels)) = decoded.as_ref() {
        if kept == path {
            let pixels = Arc::clone(pixels);
            if !keep {
                *decoded = None;
            }
            return Some(pixels);
        }
    }
    // One picture held at a time: they can be 64 MB.
    *decoded = None;
    drop(decoded);
    let pack = pack()?;
    match pack.picture(path) {
        Ok(pixels) => {
            let pixels = Arc::new(pixels);
            if keep {
                *DECODED.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_string(), Arc::clone(&pixels)));
            }
            // The first picture of each size, so the log shows what got sharper.
            let entry = &pack.entries[path];
            let mut sizes = SIZES.lock().unwrap_or_else(|e| e.into_inner());
            if sizes.len() < 40 && !sizes.contains(&(entry.width, entry.height)) {
                sizes.push((entry.width, entry.height));
                crate::log::line(&format!("[textures] 2x {path} ({}x{} -> {}x{})", entry.width, entry.height, entry.width * 2, entry.height * 2));
            }
            Some(pixels)
        }
        Err(why) => {
            note(&FAILED, 20, || format!("can't use the 2x picture: {why}"));
            None
        }
    }
}

/// A unique scratch file for a test.
#[cfg(test)]
fn scratch_file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("arm32-bridge-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip of `files` (name, bytes, method); a local extra field longer than
    /// the central one, as zipalign writes, so the data offset must come from the local header.
    fn zip(files: &[(&str, &[u8], u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, bytes, method) in files {
            let offset = out.len() as u32;
            let extra = [0u8; 7];
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]); // time, date, crc
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend((extra.len() as u16).to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(extra);
            out.extend(*bytes);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend((bytes.len() as u32).to_le_bytes());
            central.extend((bytes.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]); // extra, comment, disk, attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let central_at = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(central_at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    /// A pack of (path, w1, h1, channels, blob), blobs after the index.
    fn pack(pictures: &[(&str, u32, u32, u8, &[u8])]) -> Vec<u8> {
        let index_length: usize = 12 + pictures.iter().map(|(path, ..)| 2 + path.len() + 4 + 4 + 1 + 8 + 4).sum::<usize>();
        let mut out = Vec::from(&MAGIC[..]);
        out.extend((pictures.len() as u32).to_le_bytes());
        let mut offset = index_length as u64;
        for (path, w, h, channels, blob) in pictures {
            out.extend((path.len() as u16).to_le_bytes());
            out.extend(path.as_bytes());
            out.extend(w.to_le_bytes());
            out.extend(h.to_le_bytes());
            out.push(*channels);
            out.extend(offset.to_le_bytes());
            out.extend((blob.len() as u32).to_le_bytes());
            offset += blob.len() as u64;
        }
        for (.., blob) in pictures {
            out.extend(*blob);
        }
        out
    }

    fn webp(pixels: &[u8], width: u32, height: u32, color: image_webp::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out).encode(pixels, width, height, color).unwrap();
        out
    }

    /// The pack is found inside the APK (a stored entry, read from where its
    /// local header says), its index read, and a picture comes out at 2x the
    /// way the game holds it: RGBA premultiplied, or RGB. A compressed pack is refused.
    #[test]
    fn pack_pictures_are_read_from_the_apk_as_the_game_holds_them() {
        // A 1x1 RGBA picture at 2x: straight alpha in the file.
        let straight = [200, 100, 50, 128, 255, 255, 255, 255, 10, 20, 30, 0, 90, 60, 30, 64];
        let rgba = webp(&straight, 2, 2, image_webp::ColorType::Rgba8);
        let rgb = webp(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18], 3, 2, image_webp::ColorType::Rgb8);
        let pack_bytes = pack(&[("data/castle.png", 1, 1, 4, &rgba), ("gui/sky.jpg", 1, 1, 3, &rgb)]);
        let apk = zip(&[("classes.dex", b"dex!", 0), ("assets/gamebridge-textures.pack", &pack_bytes, 0)]);
        let path = scratch_file("pack.apk", &apk);
        let pack = Pack::in_apk(&path).unwrap();
        assert_eq!(pack.entries.len(), 2);
        let castle = &pack.entries["data/castle.png"];
        assert_eq!((castle.width, castle.height, castle.channels), (1, 1, 4));
        // 200*128/255 = 100.39 -> 100; 100*128/255 = 50.2 -> 50; 50*128/255 = 25.1 -> 25.
        assert_eq!(
            pack.picture("data/castle.png").unwrap(),
            [100, 50, 25, 128, 255, 255, 255, 255, 0, 0, 0, 0, 22, 15, 7, 64]
        );
        // The RGB entry's 2x is 2x2: a 3x2 WebP is the wrong size.
        assert!(pack.picture("gui/sky.jpg").is_err());
        assert!(pack.picture("missing.png").is_err());

        let compressed = zip(&[("assets/gamebridge-textures.pack", &pack_bytes, 8)]);
        let path_compressed = scratch_file("compressed.apk", &compressed);
        assert!(Pack::in_apk(&path_compressed).is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path_compressed);
    }

    /// The game's own premultiply cuts off: (int)(c * a / 255.0f).
    #[test]
    fn premultiply_cuts_off_like_the_game() {
        let mut pixels = [200, 100, 1, 128, 255, 254, 3, 255, 77, 77, 77, 0, 255, 128, 64, 1];
        premultiply(&mut pixels);
        assert_eq!(pixels, [100, 50, 0, 128, 255, 254, 3, 255, 0, 0, 0, 0, 1, 0, 0, 1]);
    }

    fn call(width: i32, height: i32, format: u32, kind: u32, data: u32) -> Call {
        Call { width, height, format, kind, data, ..Call::default() }
    }

    /// Test memory: `bytes` from `at` on (zeros elsewhere), and every fingerprint is `print`.
    #[derive(Clone, Copy)]
    struct Fake<'a> {
        print: u64,
        at: u32,
        bytes: &'a [u8],
    }

    impl Memory for Fake<'_> {
        fn fingerprint(&self, _address: u32, _length: usize) -> u64 {
            self.print
        }
        fn read(&self, address: u32, length: usize) -> Vec<u8> {
            let start = (address as usize).wrapping_sub(self.at as usize);
            (0..length).map(|i| self.bytes.get(start.wrapping_add(i)).copied().unwrap_or(0)).collect()
        }
        /// Memory outside `bytes` is unknown, so not zero.
        fn zero(&self, address: u32, length: usize) -> bool {
            let start = (address as usize).wrapping_sub(self.at as usize);
            self.bytes.get(start..start.wrapping_add(length)).is_some_and(|bytes| bytes.iter().all(|&b| b == 0))
        }
    }

    fn prints(print: u64) -> Fake<'static> {
        Fake { print, at: 0, bytes: &[] }
    }

    /// Render::Convert32To16Bits (libkingdom 5.11, Ghidra 0x1fb8f0), in place:
    /// RGBA4444 cut off (R&0xF0 | G>>4, then B&0xF0 | A>>4 in memory order),
    /// each row starting on an even pixel, so odd widths pad rows to 4 bytes.
    fn game_4444(pixels: &[u8], width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for row in pixels.chunks(width * 4) {
            for p in row.chunks_exact(4) {
                out.extend([p[2] & 0xF0 | p[3] >> 4, p[0] & 0xF0 | p[1] >> 4]);
            }
            if width % 2 == 1 {
                out.extend([0, 0]);
            }
        }
        out
    }

    /// The game's own 16-bit conversion of an odd-width picture (rows padded)
    /// is the picture; after a glow it is not.
    #[test]
    fn the_games_16_bit_conversion_of_odd_widths_is_the_picture() {
        let at = 0x6000;
        let loaded: Vec<u8> = (0..15 * 8 * 4).map(|i| (i * 13 % 256) as u8).collect();
        let upload = call(15, 8, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, at);
        let mut uploads = Uploads::default();
        uploads.add_pending(at, 15, 8, 4, "anim.jpg", Fake { print: 7, at, bytes: &loaded });
        let converted = game_4444(&loaded, 15);
        let replaced = Image::Replace { path: "anim.jpg".into(), width: 15, height: 8, channels: 4 };
        assert_eq!(uploads.image(1, upload, 4096, Fake { print: 8, at, bytes: &converted }), replaced);
        // A glow over the lower rows.
        let mut glowed = loaded.clone();
        for channel in &mut glowed[15 * 4 * 4..] {
            *channel = channel.saturating_add(96);
        }
        uploads.add_pending(at, 15, 8, 4, "anim.jpg", Fake { print: 7, at, bytes: &loaded });
        let converted = game_4444(&glowed, 15);
        assert_eq!(uploads.image(1, upload, 4096, Fake { print: 8, at, bytes: &converted }), Image::AsAsked);
    }

    /// A texture made from a buffer of zeros (Render::CreateAtlas, Ghidra
    /// 0x1fa980, makes its atlases so) is empty: made 2x like one made from
    /// nothing, so the pictures put into it get their 2x rows. Real pixels stay 1x.
    #[test]
    fn zero_filled_textures_are_made_2x_like_empty_ones() {
        let blank = vec![0u8; 64 * 32 * 2];
        let atlas = call(64, 32, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, 0x8000);
        let mut uploads = Uploads::default();
        let made = uploads.image(1, atlas, 4096, Fake { print: 7, at: 0x8000, bytes: &blank });
        assert_eq!(made, Image::Allocate { width: 128, height: 64, format: GL_RGBA });
        uploads.add_pending(0x2_0000, 8, 4, 4, "decor.jpg", prints(7));
        let piece = Call { x: 16, y: 8, ..call(8, 4, GL_RGBA, GL_UNSIGNED_BYTE, 0x2_0000) };
        assert!(matches!(uploads.sub_image(1, piece, 32, prints(7)), SubImage::Rows { x: 32, y: 16, first_row: 0, rows: 8, .. }));

        let mut drawn = blank.clone();
        drawn[100] = 1;
        assert_eq!(uploads.image(2, atlas, 4096, Fake { print: 7, at: 0x8000, bytes: &drawn }), Image::AsAsked);
        assert!(!uploads.doubled(2));
    }

    /// Pixels the game made 16-bit in place are compared, sampled, with the
    /// pixels it loaded: its conversion (give or take a rounding step) is the
    /// picture; after a glow it is not.
    #[test]
    fn sixteen_bit_uploads_are_the_picture_only_while_their_pixels_match() {
        let at = 0x5000;
        // 16x16 RGBA, the pixels all different.
        let loaded: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7 % 256) as u8).collect();
        let to_4444 = |pixels: &[u8], round: bool| -> Vec<u8> {
            let four = |c: u8| if round { ((c as u16 + 8) >> 4).min(15) } else { c as u16 >> 4 };
            pixels.chunks_exact(4).flat_map(|p| (four(p[0]) << 12 | four(p[1]) << 8 | four(p[2]) << 4 | four(p[3])).to_le_bytes()).collect()
        };
        let upload = call(16, 16, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, at);
        let replaced = Image::Replace { path: "c.png".into(), width: 16, height: 16, channels: 4 };
        let mut uploads = Uploads::default();
        for round in [false, true] {
            uploads.add_pending(at, 16, 16, 4, "c.png", Fake { print: 7, at, bytes: &loaded });
            let converted = to_4444(&loaded, round);
            assert_eq!(uploads.image(1, upload, 4096, Fake { print: 8, at, bytes: &converted }), replaced, "rounded: {round}");
        }
        // A glow: the top 4 rows brightened before the conversion.
        let mut glowed = loaded.clone();
        for channel in &mut glowed[..16 * 4 * 4] {
            *channel = channel.saturating_add(96);
        }
        uploads.add_pending(at, 16, 16, 4, "c.png", Fake { print: 7, at, bytes: &loaded });
        let converted = to_4444(&glowed, false);
        assert_eq!(uploads.image(1, upload, 4096, Fake { print: 8, at, bytes: &converted }), Image::AsAsked);

        // RGB pictures made 565 likewise.
        let rgb: Vec<u8> = (0..8 * 8 * 3).map(|i| (i * 11 % 256) as u8).collect();
        let to_565 = |pixels: &[u8]| -> Vec<u8> {
            pixels.chunks_exact(3).flat_map(|p| ((p[0] as u16 >> 3) << 11 | (p[1] as u16 >> 2) << 5 | p[2] as u16 >> 3).to_le_bytes()).collect()
        };
        let upload = call(8, 8, GL_RGB, GL_UNSIGNED_SHORT_5_6_5, at);
        uploads.add_pending(at, 8, 8, 3, "d.png", Fake { print: 7, at, bytes: &rgb });
        let converted = to_565(&rgb);
        assert!(matches!(uploads.image(2, upload, 4096, Fake { print: 8, at, bytes: &converted }), Image::Replace { .. }));
        let mut glowed = rgb.clone();
        for channel in &mut glowed[..8 * 3 * 2] {
            *channel = channel.saturating_add(96);
        }
        uploads.add_pending(at, 8, 8, 3, "d.png", Fake { print: 7, at, bytes: &rgb });
        let converted = to_565(&glowed);
        assert_eq!(uploads.image(2, upload, 4096, Fake { print: 8, at, bytes: &converted }), Image::AsAsked);
    }

    /// An index entry whose picture runs past the pack's end, or that would be
    /// absurdly big at 2x, is dropped, so nothing huge is ever read.
    #[test]
    fn index_entries_past_the_end_or_absurdly_big_are_dropped() {
        let mut bytes = pack(&[("good.png", 4, 4, 4, b"good"), ("huge.png", 5000, 5000, 4, b"huge"), ("cut.png", 4, 4, 3, b"cut!")]);
        bytes.pop(); // the last picture now runs past the end
        let entries = read_index(&mut &bytes[..], bytes.len() as u64).unwrap();
        assert_eq!(entries.keys().collect::<Vec<_>>(), ["good.png"]);
    }

    /// A loaded picture's upload becomes the 2x one and the texture 2x, unless
    /// the game changed its bytes first (its glow filter); bytes it made
    /// 16-bit in place still count. Empty textures are made 2x when they fit.
    #[test]
    fn uploads_of_loaded_pictures_are_replaced_and_their_textures_doubled() {
        let same = prints(7);
        let changed = prints(8);
        let mut uploads = Uploads::default();
        uploads.add_pending(0x1000, 64, 32, 4, "a.png", same);
        // Another buffer, another size, another format: not it.
        assert_eq!(uploads.image(1, call(64, 32, GL_RGBA, GL_UNSIGNED_BYTE, 0x2000), 4096, same), Image::AsAsked);
        assert_eq!(uploads.image(1, call(64, 16, GL_RGBA, GL_UNSIGNED_BYTE, 0x1000), 4096, same), Image::AsAsked);
        assert_eq!(uploads.image(1, call(64, 32, GL_RGB, GL_UNSIGNED_BYTE, 0x1000), 4096, same), Image::AsAsked);
        assert!(!uploads.doubled(1));
        let replaced = Image::Replace { path: "a.png".into(), width: 64, height: 32, channels: 4 };
        assert_eq!(uploads.image(1, call(64, 32, GL_RGBA, GL_UNSIGNED_BYTE, 0x1000), 4096, same), replaced);
        assert!(uploads.doubled(1));
        // Used up: the same buffer again is something else.
        assert_eq!(uploads.image(2, call(64, 32, GL_RGBA, GL_UNSIGNED_BYTE, 0x1000), 4096, same), Image::AsAsked);

        uploads.add_pending(0x1000, 64, 32, 4, "a.png", same);
        assert_eq!(uploads.image(2, call(64, 32, GL_RGBA, GL_UNSIGNED_BYTE, 0x1000), 4096, changed), Image::AsAsked);
        uploads.add_pending(0x1000, 64, 32, 4, "a.png", same);
        assert_eq!(uploads.image(2, call(64, 32, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, 0x1000), 4096, changed), replaced);
        // Too big for GL at 2x.
        uploads.add_pending(0x1000, 64, 32, 4, "a.png", same);
        assert_eq!(uploads.image(3, call(64, 32, GL_RGBA, GL_UNSIGNED_BYTE, 0x1000), 100, same), Image::AsAsked);

        let empty = uploads.image(4, call(2048, 1024, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, 0), 4096, same);
        assert_eq!(empty, Image::Allocate { width: 4096, height: 2048, format: GL_RGBA });
        assert!(uploads.doubled(4));
        assert_eq!(uploads.image(4, call(4096, 1024, GL_RGBA, GL_UNSIGNED_BYTE, 0), 4096, same), Image::AsAsked);
        assert!(!uploads.doubled(4), "redone at 1x");
        assert_eq!(uploads.image(5, call(16, 16, 0x1902, 0x1405, 0), 4096, same), Image::AsAsked, "depth stays 1x");
        uploads.forget(1);
        assert!(!uploads.doubled(1));

        // Only the newest loads are kept.
        for i in 0..MAX_PENDING as u32 + 1 {
            uploads.add_pending(0x10_0000 + i * 0x1000, 8, 8, 3, "b.png", same);
        }
        assert_eq!(uploads.image(6, call(8, 8, GL_RGB, GL_UNSIGNED_BYTE, 0x10_0000), 4096, same), Image::AsAsked);
        assert!(matches!(uploads.image(6, call(8, 8, GL_RGB, GL_UNSIGNED_BYTE, 0x10_1000), 4096, same), Image::Replace { .. }));
    }

    /// Pieces of a loaded picture put into a 2x texture (an atlas, or a tall
    /// picture split in two side by side) get the matching rows of the 2x
    /// picture at twice the place; any other piece is made 2x on the CPU, and
    /// 1x textures are left alone.
    #[test]
    fn pieces_of_a_loaded_picture_get_the_matching_2x_rows() {
        let same = prints(7);
        let mut uploads = Uploads::default();
        assert!(matches!(uploads.image(1, call(2048, 1500, GL_RGBA, GL_UNSIGNED_BYTE, 0), 4096, same), Image::Allocate { .. }));
        // A 1024 x 3000 picture: rows [0,1500) at (0,0), the rest at (1024,0).
        uploads.add_pending(0x10_0000, 1024, 3000, 4, "tall.png", same);
        let stride = 1024 * 4;
        let first = Call { x: 0, y: 0, ..call(1024, 1500, GL_RGBA, GL_UNSIGNED_BYTE, 0x10_0000) };
        assert_eq!(
            uploads.sub_image(1, first, stride, same),
            SubImage::Rows { path: "tall.png".into(), width: 1024, channels: 4, first_row: 0, rows: 3000, x: 0, y: 0, last: false }
        );
        let second = Call { x: 1024, y: 0, ..call(1024, 1500, GL_RGBA, GL_UNSIGNED_BYTE, 0x10_0000 + 1500 * stride) };
        assert_eq!(
            uploads.sub_image(1, second, stride, same),
            SubImage::Rows { path: "tall.png".into(), width: 1024, channels: 4, first_row: 3000, rows: 3000, x: 2048, y: 0, last: true }
        );
        // All rows done: the buffer is no longer the picture.
        assert_eq!(uploads.sub_image(1, first, stride, same), SubImage::Upscale { x: 0, y: 0 });

        // An atlas piece, 16-bit (made in place), 3 bytes a pixel RGB rows padded to 4.
        uploads.add_pending(0x20_0000, 30, 10, 3, "icon.png", same);
        let rgb_stride = row_stride(30, 3, 4);
        assert_eq!(rgb_stride, 92);
        let middle = Call { x: 100, y: 40, ..call(30, 4, GL_RGB, GL_UNSIGNED_BYTE, 0x20_0000 + 3 * rgb_stride) };
        assert_eq!(
            uploads.sub_image(1, middle, rgb_stride, same),
            SubImage::Rows { path: "icon.png".into(), width: 30, channels: 3, first_row: 6, rows: 8, x: 200, y: 80, last: false }
        );
        // Not on a row start, or past the picture's end: some other piece.
        let off_row = Call { x: 5, y: 6, ..call(30, 4, GL_RGB, GL_UNSIGNED_BYTE, 0x20_0000 + 3 * rgb_stride + 3) };
        assert_eq!(uploads.sub_image(1, off_row, rgb_stride, same), SubImage::Upscale { x: 10, y: 12 });
        let past = call(30, 8, GL_RGB, GL_UNSIGNED_BYTE, 0x20_0000 + 3 * rgb_stride);
        assert_eq!(uploads.sub_image(1, past, rgb_stride, same), SubImage::Upscale { x: 0, y: 0 });
        // Changed bytes (a glow): made 2x from the game's own.
        uploads.add_pending(0x30_0000, 8, 8, 4, "glow.png", same);
        let glow = call(8, 8, GL_RGBA, GL_UNSIGNED_BYTE, 0x30_0000);
        assert_eq!(uploads.sub_image(1, glow, 32, prints(9)), SubImage::Upscale { x: 0, y: 0 });

        uploads.add_pending(0x40_0000, 8, 8, 4, "small.png", same);
        assert_eq!(uploads.sub_image(9, call(8, 8, GL_RGBA, GL_UNSIGNED_BYTE, 0x40_0000), 32, same), SubImage::AsAsked);
    }

    /// Game pixels in rows padded by GL_UNPACK_ALIGNMENT and in 16-bit
    /// formats come out as packed 8-bit channels, then twice the size.
    #[test]
    fn game_pixels_are_widened_and_doubled() {
        // Two RGBA4444 pixels (0xF00F: red, opaque; 0x0880: half green, half blue, clear) and 2 padding bytes.
        let data = [0x0F, 0xF0, 0x80, 0x08, 0xAA, 0xAA];
        assert_eq!(pixel_size(GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4), Some(2));
        assert_eq!(to_bytes(&data, 2, 1, 6, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4).unwrap(), [255, 0, 0, 255, 0, 136, 136, 0]);
        // 565 white and black, one row each, rows 4 bytes apart.
        let rows = [0xFF, 0xFF, 0, 0, 0, 0, 0, 0];
        assert_eq!(to_bytes(&rows, 1, 2, 4, GL_RGB, GL_UNSIGNED_SHORT_5_6_5).unwrap(), [255, 255, 255, 0, 0, 0]);
        // One channel, 2x1: [0, 160] -> rows of [0, 40, 120, 160].
        assert_eq!(upscale(&[0, 160], 2, 1, 1), [0, 40, 120, 160, 0, 40, 120, 160]);
        // 1x2 of two channels: the middle rows mix 3:1.
        assert_eq!(upscale(&[0, 255, 160, 255], 1, 2, 2), [0, 255, 0, 255, 40, 255, 40, 255, 120, 255, 120, 255, 160, 255, 160, 255]);
        assert_eq!(channels(GL_LUMINANCE_ALPHA), Some(2));
        assert_eq!(pixel_size(0x80E1, GL_UNSIGNED_BYTE), None, "BGRA is left alone");
    }
}
