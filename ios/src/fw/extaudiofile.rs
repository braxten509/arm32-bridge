//! ExtAudioFile: a sound file read as linear PCM in the format the game
//! asks for (its "client" format). The whole file is decoded when opened;
//! setting the client format converts it once (channels, then rate by
//! linear interpolation); reads hand out frames from there.

use crate::fw::audiofile::{Asbd, FLAG_BIG_ENDIAN, FLAG_FLOAT, FLAG_NON_INTERLEAVED, FLAG_SIGNED, LPCM as LINEAR_PCM};
use crate::fw::string;
use crate::objc::runtime::{self, Id};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const FILE_DATA_FORMAT: u32 = four(b"ffmt");
const CLIENT_DATA_FORMAT: u32 = four(b"cfmt");
const FILE_LENGTH_FRAMES: u32 = four(b"#frm");
const PARAMETER: i32 = -50;
const BAD_PROPERTY_SIZE: i32 = four(b"!siz") as i32;
const UNKNOWN_PROPERTY: i32 = four(b"who?") as i32;
const FORMAT_UNSUPPORTED: i32 = four(b"fmt?") as i32;

struct ExtFile {
    rate: f64,
    channels: u32,
    /// The file's sound, interleaved.
    decoded: Vec<f32>,
    client: Asbd,
    /// `decoded` in the client's channels and rate, interleaved.
    converted: Vec<f32>,
    /// Next frame to read (client frames).
    position: usize,
}

static FILES: Mutex<Option<HashMap<usize, ExtFile>>> = Mutex::new(None);

fn with_file<R>(id: usize, work: impl FnOnce(&mut ExtFile) -> R) -> Option<R> {
    FILES.lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.get_mut(&id).map(work)
}

/// The file's format as games read it: 16-bit packed PCM at its own rate and channels.
fn file_format(rate: f64, channels: u32) -> Asbd {
    crate::fw::audiofile::pcm(rate, channels, 16, false, false)
}

/// Every frame of a sound file as interleaved floats: (rate, channels, samples).
pub fn decode_all(path: &str) -> Result<(f64, u32, Vec<f32>), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut hint = Hint::new();
    if let Some(extension) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, MediaSourceStream::new(Box::new(file), Default::default()), &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("{path}: not a sound file this layer reads ({e})"))?;
    let mut reader = probed.format;
    let track = reader.default_track().ok_or("no sound in the file")?;
    let (track_id, mut rate, mut channels) =
        (track.id, track.codec_params.sample_rate.unwrap_or(44100) as f64, track.codec_params.channels.map_or(0, |c| c.count() as u32));
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("{path}: can't decode it ({e})"))?;
    let mut samples = Vec::new();
    while let Ok(packet) = reader.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else { continue };
        let spec = *decoded.spec();
        rate = spec.rate as f64;
        channels = spec.channels.count() as u32;
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buffer.samples());
    }
    Ok((rate, channels.max(1), samples))
}

/// Interleaved `samples` (`from` channels at `from_rate`) in `to` channels at `to_rate`.
pub fn convert(samples: &[f32], from: u32, from_rate: f64, to: u32, to_rate: f64) -> Vec<f32> {
    let (from, to) = (from.max(1) as usize, to.max(1) as usize);
    let frames = samples.len() / from;
    let remixed: Vec<f32> = (0..frames)
        .flat_map(|frame| {
            let input = &samples[frame * from..(frame + 1) * from];
            (0..to).map(move |channel| match (from, to) {
                (f, t) if f == t => input[channel],
                (_, 1) => input.iter().sum::<f32>() / from as f32,
                (1, _) => input[0],
                _ => input.get(channel).copied().unwrap_or(0.0),
            })
        })
        .collect();
    if from_rate <= 0.0 || to_rate <= 0.0 || (from_rate - to_rate).abs() < 1e-9 || frames == 0 {
        return remixed;
    }
    let out_frames = ((frames as f64) * to_rate / from_rate).round() as usize;
    let step = from_rate / to_rate;
    let mut out = Vec::with_capacity(out_frames * to);
    for i in 0..out_frames {
        let at = i as f64 * step;
        let (left, f) = (at.floor() as usize, (at - at.floor()) as f32);
        let right = (left + 1).min(frames - 1);
        for channel in 0..to {
            let a = remixed[left.min(frames - 1) * to + channel];
            let b = remixed[right * to + channel];
            out.push(a + (b - a) * f);
        }
    }
    out
}

/// Whether this layer writes the client format: linear PCM as 32-bit
/// floats, signed 16/32-bit integers or 8-bit integers (unsigned 16/32-bit
/// samples are refused, as Apple's converter refuses them).
fn writable(client: &Asbd) -> bool {
    let signed = client.flags & FLAG_SIGNED != 0;
    client.format == LINEAR_PCM
        && client.channels > 0
        && client.flags & FLAG_BIG_ENDIAN == 0
        && match (client.flags & FLAG_FLOAT != 0, client.bits) {
            (true, bits) => bits == 32,
            (false, 8) => true,
            (false, 16 | 32) => signed,
            _ => false,
        }
}

/// One sample in the client's format, little-endian.
fn encode(value: f32, client: &Asbd, out: &mut Vec<u8>) {
    let v = value.clamp(-1.0, 1.0);
    match (client.flags & FLAG_FLOAT != 0, client.bits) {
        (true, _) => out.extend_from_slice(&value.to_le_bytes()),
        (false, 8) if client.flags & FLAG_SIGNED != 0 => out.push((v * 127.0).round() as i8 as u8),
        (false, 8) => out.push(((v * 127.0).round() + 128.0) as u8),
        (false, 16) => out.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
        _ => out.extend_from_slice(&((v as f64 * 2147483647.0).round() as i32).to_le_bytes()),
    }
}

impl ExtFile {
    fn set_client(&mut self, client: Asbd) {
        self.converted = convert(&self.decoded, self.channels, self.rate, client.channels, client.rate);
        let old = self.client;
        self.position = if old.rate > 0.0 { (self.position as f64 * client.rate / old.rate) as usize } else { 0 };
        self.client = client;
    }

    fn frames(&self) -> usize {
        self.converted.len() / self.client.channels.max(1) as usize
    }

    /// The buffers a read fills: one interleaved, or one per channel.
    fn buffers(&self) -> usize {
        if self.client.flags & FLAG_NON_INTERLEAVED != 0 { self.client.channels.max(1) as usize } else { 1 }
    }

    /// Bytes one frame takes in each buffer.
    fn frame_bytes(&self) -> usize {
        let sample = self.client.bits.div_ceil(8).max(1) as usize;
        if self.buffers() == 1 { sample * self.client.channels.max(1) as usize } else { sample }
    }

    /// Seeks to `frame` of the file (file frames, as Apple counts them);
    /// past the end reads give nothing.
    fn seek(&mut self, frame: i64) -> Result<(), i32> {
        if frame < 0 {
            return Err(PARAMETER);
        }
        let client = (frame as f64 * self.client.rate / self.rate).round() as usize;
        self.position = client.min(self.frames());
        Ok(())
    }

    /// The position in file frames.
    fn tell(&self) -> i64 {
        (self.position as f64 * self.rate / self.client.rate).round() as i64
    }

    /// Up to `wanted` frames from the current position, encoded: one block
    /// per buffer (one interleaved block, or one per channel).
    fn read(&mut self, wanted: usize) -> (usize, Vec<Vec<u8>>) {
        let channels = self.client.channels.max(1) as usize;
        let start = self.position.min(self.frames());
        let count = wanted.min(self.frames() - start);
        let frames = &self.converted[start * channels..(start + count) * channels];
        self.position = start + count;
        let blocks = if self.client.flags & FLAG_NON_INTERLEAVED != 0 {
            (0..channels)
                .map(|channel| {
                    let mut block = Vec::new();
                    frames.iter().skip(channel).step_by(channels).for_each(|&s| encode(s, &self.client, &mut block));
                    block
                })
                .collect()
        } else {
            let mut block = Vec::new();
            frames.iter().for_each(|&s| encode(s, &self.client, &mut block));
            vec![block]
        };
        (count, blocks)
    }
}

fn asbd_bytes(format: &Asbd) -> [u8; 40] {
    unsafe { std::mem::transmute(*format) }
}

/// ExtAudioFileOpenURL(url, *file).
extern "C" fn open_url(url: Id, out: *mut usize) -> i32 {
    if url == 0 || out.is_null() {
        return PARAMETER;
    }
    let path = string::get(runtime::send(url, "path", &[]));
    match decode_all(&path) {
        Ok((rate, channels, decoded)) => {
            let client = file_format(rate, channels);
            let file = ExtFile { rate, channels, converted: decoded.clone(), decoded, client, position: 0 };
            let id = crate::symbols::leak(0u64);
            FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, file);
            unsafe { *out = id };
            0
        }
        Err(e) => {
            crate::log!("ExtAudioFile: {e}");
            -43
        }
    }
}

extern "C" fn dispose(id: usize) -> i32 {
    match FILES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|files| files.remove(&id)) {
        Some(_) => 0,
        None => PARAMETER,
    }
}

extern "C" fn get_property(id: usize, property: u32, size: *mut u32, out: *mut u8) -> i32 {
    if size.is_null() || out.is_null() {
        return PARAMETER;
    }
    let value: Option<Result<Vec<u8>, i32>> = with_file(id, |f| match property {
        FILE_DATA_FORMAT => Ok(asbd_bytes(&file_format(f.rate, f.channels)).to_vec()),
        CLIENT_DATA_FORMAT => Ok(asbd_bytes(&f.client).to_vec()),
        FILE_LENGTH_FRAMES => Ok(((f.decoded.len() / f.channels.max(1) as usize) as i64).to_le_bytes().to_vec()),
        _ => Err(UNKNOWN_PROPERTY),
    });
    match value {
        None => PARAMETER,
        Some(Err(e)) => {
            crate::log!("ExtAudioFileGetProperty: property {property:#x} unknown here");
            e
        }
        Some(Ok(bytes)) => unsafe {
            if (*size as usize) < bytes.len() {
                return BAD_PROPERTY_SIZE;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
            *size = bytes.len() as u32;
            0
        },
    }
}

extern "C" fn set_property(id: usize, property: u32, size: u32, data: *const u8) -> i32 {
    if data.is_null() {
        return PARAMETER;
    }
    if property != CLIENT_DATA_FORMAT {
        crate::log!("ExtAudioFileSetProperty: property {property:#x} ignored");
        return 0;
    }
    if size < 40 {
        return BAD_PROPERTY_SIZE;
    }
    let client = unsafe { Asbd::read(data) };
    if !writable(&client) {
        crate::log!("ExtAudioFileSetProperty: client format {:#x} flags {:#x} {} bits not supported", client.format, client.flags, client.bits);
        return FORMAT_UNSUPPORTED;
    }
    with_file(id, |f| f.set_client(client)).map_or(PARAMETER, |_| 0)
}

/// The 64-bit AudioBufferList: { UInt32 count; (pad); { UInt32 channels; UInt32 bytes; void* data } ... }.
pub const BUFFER_LIST_FIRST: usize = 8;
pub const BUFFER_SIZE: usize = 16;

/// ExtAudioFileRead(file, *frames, list): fills the list's buffers with the
/// whole frames they hold (up to *frames), sets their byte counts and
/// *frames (0 at the end). The list must have one buffer with memory per
/// interleaved stream (one, or one per channel).
extern "C" fn read(id: usize, frames: *mut u32, list: *mut u8) -> i32 {
    if frames.is_null() || list.is_null() {
        return PARAMETER;
    }
    let wanted = unsafe { *frames } as usize;
    let buffer = |i: usize| unsafe { list.add(BUFFER_LIST_FIRST + i * BUFFER_SIZE) };
    let count = unsafe { *(list as *const u32) } as usize;
    let rooms: Vec<(usize, *mut u8)> = (0..count).map(|i| unsafe { (*(buffer(i).add(4) as *const u32) as usize, *(buffer(i).add(8) as *const *mut u8)) }).collect();
    let result = with_file(id, |f| {
        if rooms.len() != f.buffers() || rooms.iter().any(|(_, data)| data.is_null()) {
            return Err(PARAMETER);
        }
        let fit = rooms.iter().map(|(room, _)| room / f.frame_bytes()).min().unwrap_or(0);
        Ok(f.read(wanted.min(fit)))
    });
    let (read, blocks) = match result {
        Some(Ok(done)) => done,
        Some(Err(status)) => return status,
        None => return PARAMETER,
    };
    for (i, (block, (_, data))) in blocks.iter().zip(&rooms).enumerate() {
        unsafe {
            std::ptr::copy_nonoverlapping(block.as_ptr(), *data, block.len());
            *(buffer(i).add(4) as *mut u32) = block.len() as u32;
        }
    }
    unsafe { *frames = read as u32 };
    0
}

/// ExtAudioFileSeek(file, frame): `frame` counts the file's frames.
extern "C" fn seek(id: usize, frame: i64) -> i32 {
    match with_file(id, |f| f.seek(frame)) {
        Some(Ok(())) => 0,
        Some(Err(status)) => status,
        None => PARAMETER,
    }
}

/// ExtAudioFileTell(file, *frame): the position in the file's frames.
extern "C" fn tell(id: usize, out: *mut i64) -> i32 {
    match with_file(id, |f| f.tell()) {
        Some(position) if !out.is_null() => {
            unsafe { *out = position as i64 };
            0
        }
        _ => PARAMETER,
    }
}

pub fn register(r: &mut Registry) {
    r.function("ExtAudioFileOpenURL", open_url as usize);
    r.function("ExtAudioFileDispose", dispose as usize);
    r.function("ExtAudioFileGetProperty", get_property as usize);
    r.function("ExtAudioFileSetProperty", set_property as usize);
    r.function("ExtAudioFileRead", read as usize);
    r.function("ExtAudioFileSeek", seek as usize);
    r.function("ExtAudioFileTell", tell as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_formats_get_converted_frames() {
        // Mono at 100 Hz to stereo at 200 Hz: each channel doubled, frames doubled.
        let out = convert(&[0.0, 1.0], 1, 100.0, 2, 200.0);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..4], &[0.0, 0.0, 0.5, 0.5]);
        let mut file = ExtFile { rate: 100.0, channels: 2, decoded: vec![0.5, -0.5, 1.0, -1.0], client: file_format(100.0, 2), converted: vec![], position: 0 };
        let mut client = crate::fw::audiofile::pcm(100.0, 2, 16, false, false);
        client.flags |= FLAG_NON_INTERLEAVED;
        file.set_client(client);
        let (count, blocks) = file.read(5);
        assert_eq!(count, 2);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], [0x00, 0x40, 0xff, 0x7f]);
        assert_eq!(file.read(5).0, 0, "nothing left");
    }

    /// An open file (as ExtAudioFileOpenURL makes) holding these frames.
    fn add(rate: f64, channels: u32, decoded: Vec<f32>) -> usize {
        let client = file_format(rate, channels);
        let file = ExtFile { rate, channels, converted: decoded.clone(), decoded, client, position: 0 };
        let id = crate::symbols::leak(0u64);
        FILES.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, file);
        id
    }

    /// Reads up to `wanted` frames into one buffer of `room` bytes: (status, frames, bytes, data).
    fn read_into(id: usize, wanted: u32, room: u32) -> (i32, u32, u32, Vec<u8>) {
        let mut data = vec![0u8; room as usize];
        let mut list = [0u64; 3];
        let bytes = list.as_mut_ptr() as *mut u8;
        unsafe {
            *(bytes as *mut u32) = 1;
            *(bytes.add(8) as *mut u32) = 1;
            *(bytes.add(12) as *mut u32) = room;
            *(bytes.add(16) as *mut *mut u8) = data.as_mut_ptr();
        }
        let mut frames = wanted;
        let status = read(id, &mut frames, bytes);
        (status, frames, unsafe { *(bytes.add(12) as *const u32) }, data)
    }

    fn set_client(id: usize, client: Asbd) -> i32 {
        set_property(id, CLIENT_DATA_FORMAT, 40, asbd_bytes(&client).as_ptr())
    }

    #[test]
    fn reads_past_the_end_give_no_frames() {
        let id = add(100.0, 1, vec![0.25; 10]);
        assert_eq!(seek(id, 11), 0);
        assert_eq!(read_into(id, 5, 64).1, 0);
        assert_eq!(seek(id, 10), 0);
        assert_eq!(read_into(id, 5, 64).1, 0);
    }

    #[test]
    fn seek_and_tell_count_file_frames() {
        // 2 s at 22050 Hz, read at 44100 Hz: file frame 22050 is 1.0 s in.
        let ramp: Vec<f32> = (0..44100).map(|i| i as f32 / 44100.0).collect();
        let id = add(22050.0, 1, ramp);
        assert_eq!(set_client(id, crate::fw::audiofile::pcm(44100.0, 1, 32, true, false)), 0);
        assert_eq!(seek(id, 22050), 0);
        let mut at = 0i64;
        assert_eq!(tell(id, &mut at), 0);
        assert_eq!(at, 22050);
        let (_, frames, _, data) = read_into(id, 1, 4);
        assert_eq!(frames, 1);
        assert!((f32::from_le_bytes(data[..4].try_into().unwrap()) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn reads_stop_at_what_the_buffers_hold() {
        let id = add(100.0, 1, vec![0.0; 100]);
        // Room for 10 16-bit frames out of the 100 asked for.
        let (status, frames, bytes, _) = read_into(id, 100, 20);
        assert_eq!((status, frames, bytes), (0, 10, 20));
        let mut at = 0i64;
        tell(id, &mut at);
        assert_eq!(at, 10);
        // A buffer without memory is refused.
        let mut list = [0u64; 3];
        unsafe { *(list.as_mut_ptr() as *mut u32) = 1 };
        let mut wanted = 4;
        assert_eq!(read(id, &mut wanted, list.as_mut_ptr() as *mut u8), PARAMETER);
    }

    #[test]
    fn unsigned_wide_samples_are_refused() {
        let id = add(100.0, 1, vec![0.0; 4]);
        let mut client = crate::fw::audiofile::pcm(100.0, 1, 16, false, false);
        client.flags &= !FLAG_SIGNED;
        assert_eq!(set_client(id, client), FORMAT_UNSUPPORTED);
        client.bits = 32;
        assert_eq!(set_client(id, client), FORMAT_UNSUPPORTED);
        // 8-bit samples are unsigned in WAV files and stay allowed.
        assert_eq!(set_client(id, crate::fw::audiofile::pcm(100.0, 1, 8, false, false)), 0);
    }
}
