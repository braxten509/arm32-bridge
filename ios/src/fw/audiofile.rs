//! AudioToolbox's AudioFile: opens a sound file (from a path or through the
//! game's own read callbacks) and hands out its raw packets and format, the
//! way iOS does. Containers read here: WAV, AIFF/AIFC, CAF, MP3, ADTS (AAC)
//! and MP4/M4A. Decoding is the audio queue's job (fw::audioqueue).

use crate::fw::string;
use crate::objc::runtime::{self, Id};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// AudioStreamBasicDescription, as iOS lays it out (40 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Asbd {
    pub rate: f64,
    pub format: u32,
    pub flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels: u32,
    pub bits: u32,
    pub reserved: u32,
}

impl Asbd {
    pub unsafe fn read(pointer: *const u8) -> Asbd {
        (pointer as *const Asbd).read_unaligned()
    }
}

pub const LPCM: u32 = four(b"lpcm");
pub const FLAG_FLOAT: u32 = 0x1;
pub const FLAG_BIG_ENDIAN: u32 = 0x2;
pub const FLAG_SIGNED: u32 = 0x4;
pub const FLAG_PACKED: u32 = 0x8;
pub const FLAG_NON_INTERLEAVED: u32 = 0x20;

/// Linear PCM with whole-byte samples.
pub fn pcm(rate: f64, channels: u32, bits: u32, float: bool, big_endian: bool) -> Asbd {
    let bytes = bits.div_ceil(8) * channels;
    let mut flags = FLAG_PACKED;
    if float {
        flags |= FLAG_FLOAT;
    } else if bits > 8 {
        flags |= FLAG_SIGNED;
    }
    if big_endian && bits > 8 {
        flags |= FLAG_BIG_ENDIAN;
    }
    Asbd { rate, format: LPCM, flags, bytes_per_packet: bytes, frames_per_packet: 1, bytes_per_frame: bytes, channels, bits, reserved: 0 }
}

fn compressed(rate: f64, format: u32, channels: u32, bytes_per_packet: u32, frames_per_packet: u32) -> Asbd {
    Asbd { rate, format, flags: 0, bytes_per_packet, frames_per_packet, bytes_per_frame: 0, channels, bits: 0, reserved: 0 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Packet {
    pub offset: usize,
    pub size: u32,
    pub frames: u32,
}

/// An opened sound file: its format and its audio packets.
#[derive(Default, Debug)]
pub struct Parsed {
    pub file_type: u32,
    pub format: Asbd,
    pub cookie: Vec<u8>,
    /// AudioChannelLayout (native byte order); empty when the file has none.
    pub layout: Vec<u8>,
    pub data: Vec<u8>,
    pub data_offset: u64,
    /// One entry per packet; empty when every packet has the format's size.
    pub packets: Vec<Packet>,
    pub priming: u32,
    pub remainder: u32,
}

impl Parsed {
    pub fn packet_count(&self) -> u64 {
        if self.packets.is_empty() {
            self.data.len() as u64 / self.format.bytes_per_packet.max(1) as u64
        } else {
            self.packets.len() as u64
        }
    }

    pub fn packet(&self, index: u64) -> Option<Packet> {
        if self.packets.is_empty() {
            let size = self.format.bytes_per_packet;
            (index < self.packet_count()).then(|| Packet { offset: (index * size as u64) as usize, size, frames: self.format.frames_per_packet })
        } else {
            self.packets.get(index as usize).copied()
        }
    }

    pub fn total_frames(&self) -> u64 {
        if self.packets.is_empty() || self.format.frames_per_packet != 0 {
            self.packet_count() * self.format.frames_per_packet as u64
        } else {
            self.packets.iter().map(|p| p.frames as u64).sum()
        }
    }

    pub fn valid_frames(&self) -> u64 {
        self.total_frames().saturating_sub(self.priming as u64 + self.remainder as u64)
    }

    pub fn max_packet_size(&self) -> u32 {
        if self.packets.is_empty() {
            self.format.bytes_per_packet
        } else {
            self.packets.iter().map(|p| p.size).max().unwrap_or(0)
        }
    }

    fn bitrate(&self) -> u32 {
        let seconds = self.total_frames() as f64 / self.format.rate.max(1.0);
        if seconds > 0.0 {
            (self.data.len() as f64 * 8.0 / seconds) as u32
        } else {
            0
        }
    }

    fn layout_tag(&self) -> u32 {
        if self.layout.len() >= 4 {
            return u32::from_ne_bytes(self.layout[..4].try_into().unwrap());
        }
        match self.format.channels {
            1 => (100 << 16) | 1,
            2 => (101 << 16) | 2,
            n => (147 << 16) | n,
        }
    }
}

// ----- OSStatus values -----

pub const UNSUPPORTED_PROPERTY: i32 = four(b"pty?") as i32;
const BAD_PROPERTY_SIZE: i32 = four(b"!siz") as i32;
const UNSUPPORTED_FILE_TYPE: i32 = four(b"typ?") as i32;
const UNSUPPORTED_DATA_FORMAT: i32 = four(b"fmt?") as i32;
const INVALID_FILE: i32 = four(b"dta?") as i32;
const INVALID_PACKET_OFFSET: i32 = four(b"pck?") as i32;
const NOT_OPEN: i32 = -38;
const PARAMETER: i32 = -50;

// ----- readers -----

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Works out what kind of file `bytes` is and reads it.
pub fn parse(bytes: &[u8]) -> Result<Parsed, i32> {
    let start = bytes.get(..12).ok_or(INVALID_FILE)?;
    let parsed = if &start[..4] == b"RIFF" && &start[8..12] == b"WAVE" {
        parse_wav(bytes)
    } else if &start[..4] == b"FORM" && (&start[8..12] == b"AIFF" || &start[8..12] == b"AIFC") {
        parse_aiff(bytes)
    } else if &start[..4] == b"caff" {
        parse_caf(bytes)
    } else if &start[4..8] == b"ftyp" {
        parse_mp4(bytes)
    } else if let Some(found) = parse_adts(bytes) {
        Ok(found)
    } else {
        parse_mp3(bytes)
    }?;
    if parsed.packet_count() == 0 && parsed.format.format == 0 {
        return Err(INVALID_FILE);
    }
    Ok(parsed)
}

fn parse_wav(b: &[u8]) -> Result<Parsed, i32> {
    let mut at = 12;
    let mut format = None;
    let mut data = None;
    while at + 8 <= b.len() {
        let size = le32(b, at + 4).unwrap() as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(b.len());
        match &b[at..at + 4] {
            b"fmt " if end >= body + 16 => {
                let mut tag = le16(b, body).unwrap();
                let channels = le16(b, body + 2).unwrap() as u32;
                let rate = le32(b, body + 4).unwrap() as f64;
                let align = le16(b, body + 12).unwrap() as u32;
                let bits = le16(b, body + 14).unwrap() as u32;
                let extra = if end >= body + 18 { le16(b, body + 16).unwrap() } else { 0 };
                if tag == 0xFFFE && end >= body + 26 {
                    tag = le16(b, body + 24).unwrap();
                }
                if channels == 0 || align == 0 {
                    return Err(UNSUPPORTED_DATA_FORMAT);
                }
                format = Some(match tag {
                    1 => pcm(rate, channels, bits, false, false),
                    3 => pcm(rate, channels, bits, true, false),
                    6 | 7 => compressed(rate, four(if tag == 6 { b"alaw" } else { b"ulaw" }), channels, channels, 1),
                    0x11 => {
                        // DVI/Intel IMA ADPCM blocks.
                        let per_block = if extra >= 2 && end >= body + 20 {
                            le16(b, body + 18).unwrap() as u32
                        } else {
                            (align - 4 * channels) * 2 / channels + 1
                        };
                        Asbd { bits: 4, ..compressed(rate, 0x6D73_0011, channels, align, per_block) }
                    }
                    _ => {
                        crate::log!("AudioFile: WAV format {tag:#x} isn't supported");
                        return Err(UNSUPPORTED_DATA_FORMAT);
                    }
                });
            }
            b"data" => data = Some((body, end)),
            _ => {}
        }
        at = body.saturating_add(size + (size & 1));
    }
    let (format, (start, end)) = (format.ok_or(INVALID_FILE)?, data.ok_or(INVALID_FILE)?);
    let whole = (end - start) / format.bytes_per_packet as usize * format.bytes_per_packet as usize;
    Ok(Parsed { file_type: four(b"WAVE"), format, data: b[start..start + whole].to_vec(), data_offset: start as u64, ..Parsed::default() })
}

/// An 80-bit extended float (AIFF sample rates).
fn extended(b: &[u8]) -> f64 {
    let exponent = (u16::from_be_bytes([b[0], b[1]]) & 0x7FFF) as i32;
    let mantissa = u64::from_be_bytes(b[2..10].try_into().unwrap());
    if exponent == 0 && mantissa == 0 {
        return 0.0;
    }
    mantissa as f64 * 2f64.powi(exponent - 16383 - 63)
}

fn parse_aiff(b: &[u8]) -> Result<Parsed, i32> {
    let mut at = 12;
    let mut format = None;
    let mut data = None;
    while at + 8 <= b.len() {
        let size = be32(b, at + 4).unwrap() as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(b.len());
        match &b[at..at + 4] {
            b"COMM" if end >= body + 18 => {
                let channels = be16(b, body).unwrap() as u32;
                let bits = be16(b, body + 6).unwrap() as u32;
                let rate = extended(&b[body + 8..body + 18]);
                let compression = if end >= body + 22 { &b[body + 18..body + 22] } else { b"NONE" };
                format = Some(match compression {
                    b"NONE" | b"twos" => pcm(rate, channels, bits.div_ceil(8) * 8, false, true),
                    b"sowt" => pcm(rate, channels, bits.div_ceil(8) * 8, false, false),
                    b"fl32" | b"FL32" => pcm(rate, channels, 32, true, true),
                    b"fl64" | b"FL64" => pcm(rate, channels, 64, true, true),
                    b"ima4" => compressed(rate, four(b"ima4"), channels, 34 * channels, 64),
                    b"ulaw" | b"ULAW" => compressed(rate, four(b"ulaw"), channels, channels, 1),
                    b"alaw" | b"ALAW" => compressed(rate, four(b"alaw"), channels, channels, 1),
                    other => {
                        crate::log!("AudioFile: AIFF compression {} isn't supported", String::from_utf8_lossy(other));
                        return Err(UNSUPPORTED_DATA_FORMAT);
                    }
                });
                // 8-bit AIFF samples are signed.
                if let Some(f) = format.as_mut() {
                    if f.format == LPCM && f.bits == 8 {
                        f.flags |= FLAG_SIGNED;
                    }
                }
            }
            b"SSND" if end >= body + 8 => {
                let offset = be32(b, body).unwrap() as usize;
                data = Some(((body + 8 + offset).min(end), end));
            }
            _ => {}
        }
        at = body.saturating_add(size + (size & 1));
    }
    let (format, (start, end)) = (format.ok_or(INVALID_FILE)?, data.ok_or(INVALID_FILE)?);
    if format.channels == 0 || format.bytes_per_packet == 0 {
        return Err(UNSUPPORTED_DATA_FORMAT);
    }
    let whole = (end - start) / format.bytes_per_packet as usize * format.bytes_per_packet as usize;
    let file_type = four(if &b[8..12] == b"AIFC" { b"AIFC" } else { b"AIFF" });
    Ok(Parsed { file_type, format, data: b[start..start + whole].to_vec(), data_offset: start as u64, ..Parsed::default() })
}

/// A CAF packet table number: 7 bits a byte, high bit = more follow.
fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    loop {
        let byte = *b.get(*at)?;
        *at += 1;
        value = (value << 7) | (byte & 0x7F) as u64;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
}

fn parse_caf(b: &[u8]) -> Result<Parsed, i32> {
    let mut at = 8;
    let mut parsed = Parsed { file_type: four(b"caff"), ..Parsed::default() };
    let mut data = None;
    let mut table = None;
    while at + 12 <= b.len() {
        let size = be64(b, at + 4).unwrap() as i64;
        let body = at + 12;
        let end = if size < 0 { b.len() } else { body.saturating_add(size as usize).min(b.len()) };
        match &b[at..at + 4] {
            b"desc" if end >= body + 32 => {
                let rate = f64::from_bits(be64(b, body).unwrap());
                let id = be32(b, body + 8).unwrap();
                let caf_flags = be32(b, body + 12).unwrap();
                let bytes_per_packet = be32(b, body + 16).unwrap();
                let frames_per_packet = be32(b, body + 20).unwrap();
                let channels = be32(b, body + 24).unwrap();
                let bits = be32(b, body + 28).unwrap();
                parsed.format = if id == LPCM {
                    let mut f = pcm(rate, channels, bits, caf_flags & 1 != 0, caf_flags & 2 == 0);
                    if bits == 8 {
                        f.flags |= FLAG_SIGNED;
                    }
                    f.bytes_per_packet = bytes_per_packet.max(f.bytes_per_packet);
                    f.bytes_per_frame = f.bytes_per_packet;
                    f
                } else {
                    Asbd { flags: caf_flags, bits, ..compressed(rate, id, channels, bytes_per_packet, frames_per_packet) }
                };
            }
            b"data" if end >= body + 4 => data = Some((body + 4, end)),
            b"pakt" => table = Some((body, end)),
            b"kuki" => parsed.cookie = b[body..end].to_vec(),
            b"chan" if end >= body + 12 => {
                // Big-endian AudioChannelLayout: tag, bitmap, count, then 20-byte descriptions.
                parsed.layout = b[body..end].chunks_exact(4).flat_map(|w| u32::from_be_bytes(w.try_into().unwrap()).to_ne_bytes()).collect();
            }
            _ => {}
        }
        if size < 0 {
            break;
        }
        at = end;
    }
    let (start, end) = data.ok_or(INVALID_FILE)?;
    if parsed.format.format == 0 {
        return Err(INVALID_FILE);
    }
    parsed.data = b[start..end].to_vec();
    parsed.data_offset = start as u64;
    let f = parsed.format;
    if f.bytes_per_packet == 0 || f.frames_per_packet == 0 {
        let (body, table_end) = table.ok_or(INVALID_FILE)?;
        let table = &b[body..table_end];
        let count = be64(table, 0).ok_or(INVALID_FILE)?;
        parsed.priming = be32(table, 16).unwrap_or(0);
        parsed.remainder = be32(table, 20).unwrap_or(0);
        let mut at = 24;
        let mut offset = 0usize;
        for _ in 0..count {
            let size = if f.bytes_per_packet == 0 { varint(table, &mut at).ok_or(INVALID_FILE)? as u32 } else { f.bytes_per_packet };
            let frames = if f.frames_per_packet == 0 { varint(table, &mut at).ok_or(INVALID_FILE)? as u32 } else { f.frames_per_packet };
            if offset + size as usize > parsed.data.len() {
                break;
            }
            parsed.packets.push(Packet { offset, size, frames });
            offset += size as usize;
        }
    } else {
        let whole = parsed.data.len() / f.bytes_per_packet as usize * f.bytes_per_packet as usize;
        parsed.data.truncate(whole);
        if let Some((body, table_end)) = table {
            parsed.priming = be32(&b[body..table_end], 16).unwrap_or(0);
            parsed.remainder = be32(&b[body..table_end], 20).unwrap_or(0);
        }
    }
    Ok(parsed)
}

struct Mp3Frame {
    length: usize,
    frames: u32,
    rate: u32,
    channels: u32,
    /// MPEG version and sample rate bits: the same for every frame of a stream.
    kind: u32,
    mpeg1: bool,
}

fn mp3_frame(b: &[u8], at: usize) -> Option<Mp3Frame> {
    let h = be32(b, at)?;
    if h >> 21 != 0x7FF {
        return None;
    }
    let version = (h >> 19) & 3;
    let layer = (h >> 17) & 3;
    let bitrate_index = ((h >> 12) & 0xF) as usize;
    let rate_index = ((h >> 10) & 3) as usize;
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const MPEG1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const MPEG2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let mpeg1 = version == 3;
    let bitrate = if mpeg1 { MPEG1[bitrate_index] } else { MPEG2[bitrate_index] } * 1000;
    let rate = [44100, 48000, 32000][rate_index] >> match version {
        3 => 0,
        2 => 1,
        _ => 2,
    };
    let padding = ((h >> 9) & 1) as usize;
    let length = (if mpeg1 { 144 } else { 72 }) * bitrate as usize / rate as usize + padding;
    let channels = if (h >> 6) & 3 == 3 { 1 } else { 2 };
    Some(Mp3Frame { length, frames: if mpeg1 { 1152 } else { 576 }, rate, channels, kind: h & 0x001E_0C00, mpeg1 })
}

/// Encoder delay and padding from a Xing/Info frame's LAME tag.
fn lame_gap(b: &[u8], frame: &Mp3Frame) -> Option<(u32, u32)> {
    let side = match (frame.mpeg1, frame.channels) {
        (true, 1) => 17,
        (true, _) => 32,
        (false, 1) => 9,
        (false, _) => 17,
    };
    let xing = 4 + side;
    let tag = b.get(xing..xing + 4)?;
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let flags = be32(b, xing + 4)?;
    let mut at = xing + 8;
    for (bit, size) in [(1, 4), (2, 4), (4, 100), (8, 4)] {
        if flags & bit != 0 {
            at += size;
        }
    }
    let three = b.get(at + 21..at + 24)?;
    let delay = ((three[0] as u32) << 4) | (three[1] as u32 >> 4);
    let padding = (((three[1] & 0xF) as u32) << 8) | three[2] as u32;
    Some((delay, padding))
}

fn is_info_frame(b: &[u8], frame: &Mp3Frame) -> bool {
    let side = match (frame.mpeg1, frame.channels) {
        (true, 1) => 17,
        (true, _) => 32,
        (false, 1) => 9,
        (false, _) => 17,
    };
    matches!(b.get(4 + side..8 + side), Some(b"Xing") | Some(b"Info")) || b.get(36..40) == Some(b"VBRI")
}

fn parse_mp3(b: &[u8]) -> Result<Parsed, i32> {
    let mut at = 0;
    // ID3v2 tags (sizes are 7 bits a byte).
    while b.get(at..at + 3) == Some(b"ID3") && at + 10 <= b.len() {
        let size = b[at + 6..at + 10].iter().fold(0usize, |v, &x| (v << 7) | (x & 0x7F) as usize);
        at += 10 + size + if b[at + 5] & 0x10 != 0 { 10 } else { 0 };
    }
    // The first frame header followed by another of the same stream.
    let limit = (at + 64 * 1024).min(b.len());
    let first = loop {
        if at >= limit {
            return Err(UNSUPPORTED_FILE_TYPE);
        }
        if let Some(frame) = mp3_frame(b, at) {
            match mp3_frame(b, at + frame.length) {
                Some(next) if next.kind == frame.kind => break frame,
                None if at + frame.length >= b.len() => break frame,
                _ => {}
            }
        }
        at += 1;
    };
    let mut parsed = Parsed { file_type: four(b"MPG3"), data_offset: at as u64, ..Parsed::default() };
    parsed.format = compressed(first.rate as f64, four(b".mp3"), first.channels, 0, first.frames);
    let mut gap = None;
    let mut index = 0;
    while at < b.len() {
        let Some(frame) = mp3_frame(b, at).filter(|f| f.kind == first.kind) else {
            at += 1;
            continue;
        };
        let end = (at + frame.length).min(b.len());
        if end - at < frame.length {
            break;
        }
        if index == 0 && is_info_frame(&b[at..end], &frame) {
            gap = lame_gap(&b[at..end], &frame);
        } else {
            parsed.packets.push(Packet { offset: parsed.data.len(), size: frame.length as u32, frames: frame.frames });
            parsed.data.extend_from_slice(&b[at..end]);
        }
        index += 1;
        at = end;
    }
    if let Some((delay, padding)) = gap {
        // As iOS counts it: the encoder's delay plus the decoder's own 529 frames.
        parsed.priming = delay + 529;
        parsed.remainder = padding.saturating_sub(529);
    }
    if parsed.packets.is_empty() {
        return Err(INVALID_FILE);
    }
    Ok(parsed)
}

/// MPEG-4 AudioSpecificConfig for plain AAC.
pub fn audio_specific_config(object_type: u32, rate: u32, channels: u32) -> Vec<u8> {
    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    let index = RATES.iter().position(|&r| r == rate).unwrap_or(4) as u32;
    let bits = (object_type << 11) | (index << 7) | (channels << 3);
    (bits as u16).to_be_bytes().to_vec()
}

/// Object type, (core) sample rate and channel count of an AudioSpecificConfig.
pub fn config_info(config: &[u8]) -> Option<(u32, u32, u32)> {
    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    let bits = u64::from_be_bytes(config.iter().copied().chain(std::iter::repeat(0)).take(8).collect::<Vec<_>>().try_into().ok()?);
    if config.len() < 2 {
        return None;
    }
    let object_type = (bits >> 59) as u32;
    let index = ((bits >> 55) & 0xF) as usize;
    let (rate, rest) = if index == 15 { (((bits >> 31) & 0xFF_FFFF) as u32, 31) } else { (*RATES.get(index)?, 55) };
    let channels = ((bits >> (rest - 4)) & 0xF) as u32;
    Some((object_type, rate, channels))
}

/// An 'esds' style magic cookie (ES descriptor) holding `config`.
pub fn esds_cookie(config: &[u8]) -> Vec<u8> {
    let decoder_specific = [&[0x05, config.len() as u8][..], config].concat();
    let decoder_config = [&[0x04, (13 + decoder_specific.len()) as u8, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..], &decoder_specific].concat();
    [&[0x03, (3 + decoder_config.len() + 3) as u8, 0, 0, 0][..], &decoder_config, &[0x06, 1, 2]].concat()
}

fn parse_adts(b: &[u8]) -> Option<Parsed> {
    let mut at = 0;
    while b.get(at..at + 3) == Some(b"ID3") && at + 10 <= b.len() {
        let size = b[at + 6..at + 10].iter().fold(0usize, |v, &x| (v << 7) | (x & 0x7F) as usize);
        at += 10 + size;
    }
    let header = |at: usize| -> Option<(usize, usize, u32, u32, u32, u32)> {
        let h = b.get(at..at + 7)?;
        if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
            return None;
        }
        let header_length = if h[1] & 1 != 0 { 7 } else { 9 };
        let profile = (h[2] >> 6) as u32;
        let rate_index = ((h[2] >> 2) & 0xF) as usize;
        let channels = (((h[2] & 1) << 2) | (h[3] >> 6)) as u32;
        let length = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | (h[5] >> 5) as usize;
        let blocks = (h[6] & 3) as u32 + 1;
        const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
        let rate = *RATES.get(rate_index)?;
        (length > header_length).then_some((header_length, length, profile + 1, rate, channels, blocks))
    };
    let (_, first_length, object_type, rate, channels, _) = header(at)?;
    // A second frame must follow straight on (or the file ends).
    if at + first_length < b.len() {
        header(at + first_length)?;
    }
    let mut parsed = Parsed { file_type: four(b"adts"), data_offset: at as u64, ..Parsed::default() };
    parsed.format = compressed(rate as f64, four(b"aac "), channels, 0, 1024);
    parsed.cookie = esds_cookie(&audio_specific_config(object_type, rate, channels));
    while let Some((header_length, length, _, _, _, blocks)) = header(at) {
        let end = at + length;
        if end > b.len() {
            break;
        }
        parsed.packets.push(Packet { offset: parsed.data.len(), size: (length - header_length) as u32, frames: 1024 * blocks });
        parsed.data.extend_from_slice(&b[at + header_length..end]);
        at = end;
    }
    Some(parsed)
}

fn parse_mp4(b: &[u8]) -> Result<Parsed, i32> {
    use symphonia::core::codecs::{CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_MP3};
    use symphonia::core::formats::{FormatOptions, FormatReader};
    use symphonia::core::io::MediaSourceStream;
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(b.to_vec())), Default::default());
    let mut reader = symphonia::default::formats::IsoMp4Reader::try_new(stream, &FormatOptions { enable_gapless: false, ..Default::default() })
        .map_err(|e| {
            crate::log!("AudioFile: MP4 file not readable ({e})");
            INVALID_FILE
        })?;
    let track = reader.default_track().ok_or(INVALID_FILE)?.clone();
    let p = &track.codec_params;
    let rate = p.sample_rate.unwrap_or(44100);
    let channels = p.channels.map(|c| c.count() as u32).unwrap_or(2);
    let extra = p.extra_data.as_deref().unwrap_or(&[]).to_vec();
    let mut parsed = Parsed { file_type: four(if &b[8..11] == b"M4A" { b"m4af" } else { b"mp4f" }), ..Parsed::default() };
    let (format, frames) = if p.codec == CODEC_TYPE_AAC {
        // HE-AAC is decoded at its core (plain AAC) rate here, so it is
        // described that way.
        let (config, core_rate, channels) = match config_info(&extra) {
            Some((5 | 29, core, ch)) => (audio_specific_config(2, core, ch), core, ch),
            Some((_, core, ch)) => (extra.clone(), core, if ch == 0 { channels } else { ch }),
            None => (audio_specific_config(2, rate, channels), rate, channels),
        };
        parsed.cookie = esds_cookie(&config);
        (compressed(core_rate as f64, four(b"aac "), channels, 0, 1024), 1024)
    } else if p.codec == CODEC_TYPE_ALAC {
        parsed.cookie = extra.clone();
        let frames = p.max_frames_per_packet.unwrap_or(4096) as u32;
        (Asbd { bits: p.bits_per_sample.unwrap_or(16), ..compressed(rate as f64, four(b"alac"), channels, 0, frames) }, frames)
    } else if p.codec == CODEC_TYPE_MP3 {
        (compressed(rate as f64, four(b".mp3"), channels, 0, 1152), 1152)
    } else {
        crate::log!("AudioFile: this MP4 sound format isn't supported");
        return Err(UNSUPPORTED_DATA_FORMAT);
    };
    parsed.format = format;
    while let Ok(packet) = reader.next_packet() {
        if packet.track_id() != track.id {
            continue;
        }
        let frames = if packet.dur > 0 { packet.dur as u32 } else { frames };
        parsed.packets.push(Packet { offset: parsed.data.len(), size: packet.data.len() as u32, frames });
        parsed.data.extend_from_slice(&packet.data);
    }
    // Every packet holds the same number of frames but the last.
    if parsed.packets.iter().rev().skip(1).any(|p| p.frames != frames) {
        parsed.format.frames_per_packet = 0;
    }
    Ok(parsed)
}

// ----- AudioFileID handles -----

static FILES: Mutex<Option<HashMap<usize, Arc<Parsed>>>> = Mutex::new(None);

fn file(id: usize) -> Option<Arc<Parsed>> {
    FILES.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&id).cloned()
}

fn add(parsed: Parsed, out: *mut usize) -> i32 {
    let f = parsed.format;
    crate::log!(
        "AudioFile: opened a {} file: {} at {} Hz, {} channels, {} packets",
        crate::fw::sound::four_cc(parsed.file_type),
        crate::fw::sound::four_cc(f.format),
        f.rate,
        f.channels,
        parsed.packet_count()
    );
    let id = leak([0usize; 2]);
    FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, Arc::new(parsed));
    unsafe { *out = id };
    0
}

fn opened(bytes: &[u8], out: *mut usize) -> i32 {
    if out.is_null() {
        return PARAMETER;
    }
    match parse(bytes) {
        Ok(parsed) => add(parsed, out),
        Err(e) => {
            crate::log!("AudioFile: can't open this sound ({})", crate::fw::sound::four_cc(e as u32));
            e
        }
    }
}

type ReadProc = extern "C" fn(usize, i64, u32, *mut u8, *mut u32) -> i32;
type SizeProc = extern "C" fn(usize) -> i64;

/// AudioFileOpenWithCallbacks(client, read, write, getSize, setSize, typeHint, *file).
extern "C" fn open_with_callbacks(client: usize, read: usize, _write: usize, get_size: usize, _set_size: usize, _hint: u32, out: *mut usize) -> i32 {
    if read == 0 || get_size == 0 {
        return PARAMETER;
    }
    let read: ReadProc = unsafe { std::mem::transmute(read) };
    let get_size: SizeProc = unsafe { std::mem::transmute(get_size) };
    let size = get_size(client).max(0) as usize;
    let mut bytes = vec![0u8; size];
    let mut at = 0;
    while at < size {
        let want = (size - at).min(1 << 20) as u32;
        let mut got = 0u32;
        let status = read(client, at as i64, want, bytes[at..].as_mut_ptr(), &mut got);
        if got == 0 || (status != 0 && status != -39) {
            break;
        }
        at += got as usize;
    }
    bytes.truncate(at);
    opened(&bytes, out)
}

/// AudioFileOpenURL(url, permissions, typeHint, *file).
extern "C" fn open_url(url: Id, _permissions: i8, _hint: u32, out: *mut usize) -> i32 {
    if url == 0 {
        return PARAMETER;
    }
    let path = string::get(runtime::send(url, "path", &[]));
    match std::fs::read(&path) {
        Ok(bytes) => opened(&bytes, out),
        Err(e) => {
            crate::log!("AudioFile: {path}: {e}");
            -43
        }
    }
}

extern "C" fn close(id: usize) -> i32 {
    match FILES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|files| files.remove(&id)) {
        Some(_) => 0,
        None => NOT_OPEN,
    }
}

/// A property's value, as bytes.
fn property(f: &Parsed, id: u32) -> Option<Vec<u8>> {
    let format = unsafe { std::slice::from_raw_parts(&f.format as *const Asbd as *const u8, 40) };
    Some(match &id.to_be_bytes() {
        b"ffmt" => f.file_type.to_ne_bytes().to_vec(),
        b"dfmt" => format.to_vec(),
        b"flst" => [format, &f.layout_tag().to_ne_bytes(), &[0; 4]].concat(),
        b"mgic" if !f.cookie.is_empty() => f.cookie.clone(),
        b"cmap" if !f.layout.is_empty() => f.layout.clone(),
        b"bcnt" => (f.data.len() as u64).to_ne_bytes().to_vec(),
        b"pcnt" => f.packet_count().to_ne_bytes().to_vec(),
        b"psze" | b"pkub" => f.max_packet_size().to_ne_bytes().to_vec(),
        b"dofs" => (f.data_offset as i64).to_ne_bytes().to_vec(),
        b"pnfo" => [(f.valid_frames() as i64).to_ne_bytes().as_slice(), &(f.priming as i32).to_ne_bytes(), &(f.remainder as i32).to_ne_bytes()].concat(),
        b"edur" => (f.valid_frames() as f64 / f.format.rate.max(1.0)).to_ne_bytes().to_vec(),
        b"brat" => f.bitrate().to_ne_bytes().to_vec(),
        b"isop" => 1u32.to_ne_bytes().to_vec(),
        b"dlay" => 0u32.to_ne_bytes().to_vec(),
        _ => return None,
    })
}

/// Whole-number properties a game asks for with a fixed size.
fn variable_size(id: u32) -> bool {
    matches!(&id.to_be_bytes(), b"mgic" | b"cmap" | b"flst")
}

/// AudioFileGetPropertyInfo(file, id, *size, *writable).
extern "C" fn get_property_info(id: usize, property_id: u32, size: *mut u32, writable: *mut u32) -> i32 {
    let Some(f) = file(id) else { return NOT_OPEN };
    let Some(value) = property(&f, property_id) else { return UNSUPPORTED_PROPERTY };
    if !size.is_null() {
        unsafe { *size = value.len() as u32 };
    }
    if !writable.is_null() {
        unsafe { *writable = 0 };
    }
    0
}

/// AudioFileGetProperty(file, id, *size, data).
extern "C" fn get_property(id: usize, property_id: u32, size: *mut u32, data: *mut u8) -> i32 {
    let Some(f) = file(id) else { return NOT_OPEN };
    if size.is_null() || data.is_null() {
        return PARAMETER;
    }
    // Packet <-> frame and byte translations work on a struct the game fills in.
    match &property_id.to_be_bytes() {
        b"pkfr" | b"frpk" if unsafe { *size } >= 20 => {
            // AudioFramePacketTranslation { frame, packet, frameOffsetInPacket }
            let fpp = f.format.frames_per_packet.max(1) as i64;
            unsafe {
                let frame = data as *mut i64;
                let packet = data.add(8) as *mut i64;
                let offset = data.add(16) as *mut u32;
                if property_id == four(b"pkfr") {
                    *frame = *packet * fpp;
                } else {
                    *packet = *frame / fpp;
                    *offset = (*frame % fpp) as u32;
                }
            }
            return 0;
        }
        _ => {}
    }
    let Some(value) = property(&f, property_id) else { return UNSUPPORTED_PROPERTY };
    let room = unsafe { *size } as usize;
    if room < value.len() && !variable_size(property_id) {
        return BAD_PROPERTY_SIZE;
    }
    let n = room.min(value.len());
    unsafe {
        std::ptr::copy_nonoverlapping(value.as_ptr(), data, n);
        *size = n as u32;
    }
    0
}

/// Copies whole packets from `start`, as many as `packets` asks and
/// `bytes` holds. Returns (bytes, packets) copied.
pub fn read_packets(f: &Parsed, start: i64, bytes: u32, packets: u32, descriptions: *mut u8, out: *mut u8) -> Result<(u32, u32), i32> {
    if start < 0 {
        return Err(INVALID_PACKET_OFFSET);
    }
    let constant = f.format.frames_per_packet != 0;
    let (mut used, mut count) = (0u32, 0u32);
    while count < packets {
        let Some(p) = f.packet(start as u64 + count as u64) else { break };
        if used + p.size > bytes {
            if count == 0 {
                return Err(PARAMETER);
            }
            break;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(f.data.as_ptr().add(p.offset), out.add(used as usize), p.size as usize);
            if !descriptions.is_null() {
                // AudioStreamPacketDescription { startOffset, variableFrames, size }
                let d = descriptions.add(16 * count as usize);
                (d as *mut i64).write_unaligned(used as i64);
                (d.add(8) as *mut u32).write_unaligned(if constant { 0 } else { p.frames });
                (d.add(12) as *mut u32).write_unaligned(p.size);
            }
        }
        used += p.size;
        count += 1;
    }
    Ok((used, count))
}

/// AudioFileReadPacketData(file, useCache, *ioBytes, descriptions, start, *ioPackets, buffer).
extern "C" fn read_packet_data(id: usize, _cache: u8, bytes: *mut u32, descriptions: *mut u8, start: i64, packets: *mut u32, out: *mut u8) -> i32 {
    let Some(f) = file(id) else { return NOT_OPEN };
    if bytes.is_null() || packets.is_null() || out.is_null() {
        return PARAMETER;
    }
    match read_packets(&f, start, unsafe { *bytes }, unsafe { *packets }, descriptions, out) {
        Ok((used, count)) => {
            unsafe {
                *bytes = used;
                *packets = count;
            }
            0
        }
        Err(e) => e,
    }
}

/// AudioFileReadPackets(file, useCache, *outBytes, descriptions, start, *ioPackets, buffer):
/// the older form; the buffer is taken to hold every packet asked for.
extern "C" fn read_packets_old(id: usize, _cache: u8, bytes: *mut u32, descriptions: *mut u8, start: i64, packets: *mut u32, out: *mut u8) -> i32 {
    let Some(f) = file(id) else { return NOT_OPEN };
    if packets.is_null() || out.is_null() {
        return PARAMETER;
    }
    let room = unsafe { *packets }.saturating_mul(f.max_packet_size());
    match read_packets(&f, start, room, unsafe { *packets }, descriptions, out) {
        Ok((used, count)) => {
            unsafe {
                if !bytes.is_null() {
                    *bytes = used;
                }
                *packets = count;
            }
            0
        }
        Err(e) => e,
    }
}

/// AudioFileReadBytes(file, useCache, start, *ioBytes, buffer): raw audio data.
extern "C" fn read_bytes(id: usize, _cache: u8, start: i64, bytes: *mut u32, out: *mut u8) -> i32 {
    let Some(f) = file(id) else { return NOT_OPEN };
    if bytes.is_null() || out.is_null() || start < 0 {
        return PARAMETER;
    }
    let from = (start as usize).min(f.data.len());
    let n = (unsafe { *bytes } as usize).min(f.data.len() - from);
    unsafe {
        std::ptr::copy_nonoverlapping(f.data.as_ptr().add(from), out, n);
        *bytes = n as u32;
    }
    if n == 0 {
        -39
    } else {
        0
    }
}

pub fn register(r: &mut Registry) {
    r.function("AudioFileOpenWithCallbacks", open_with_callbacks as usize);
    r.function("AudioFileOpenURL", open_url as usize);
    r.function("AudioFileClose", close as usize);
    r.function("AudioFileGetPropertyInfo", get_property_info as usize);
    r.function("AudioFileGetProperty", get_property as usize);
    r.function("AudioFileReadPacketData", read_packet_data as usize);
    r.function("AudioFileReadPackets", read_packets_old as usize);
    r.function("AudioFileReadBytes", read_bytes as usize);
}
