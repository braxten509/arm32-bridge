//! AudioToolbox's audio queues. The game enqueues buffers of sound (plain
//! PCM or compressed packets: MP3, AAC, ALAC, IMA4, u-law/A-law); a queue
//! either plays them (mixed into the one output stream, fw::sound) or, in
//! offline mode, decodes them into PCM for the game (AudioQueueOfflineRender,
//! which games use as their MP3/AAC decoder).
//!
//! Buffer callbacks: offline queues call back inside AudioQueueOfflineRender
//! (on the caller's thread); playing queues call back from the queue's own
//! thread, or on the run loop the game named.

use crate::fw::audiofile::{self, four, Asbd, FLAG_BIG_ENDIAN, FLAG_FLOAT, FLAG_SIGNED, LPCM};
use crate::fw::runloop::{self, Work};
use crate::fw::sound::{self, Stream};
use crate::objc::runtime::Id;
use crate::symbols::Registry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

const PARAMETER: i32 = -50;
const INVALID_BUFFER: i32 = -66687;
const BUFFER_EMPTY: i32 = -66686;
const INVALID_PROPERTY: i32 = -66684;
const INVALID_PROPERTY_SIZE: i32 = -66683;
const INVALID_PARAMETER: i32 = -66682;
const DISPOSAL_PENDING: i32 = -66685;
const CANNOT_START: i32 = -66681;
const INVALID_RUN_STATE: i32 = -66678;
const ENQUEUE_DURING_RESET: i32 = -66632;
const INVALID_OFFLINE_MODE: i32 = -66626;
const UNSUPPORTED_FORMAT: i32 = four(b"fmt?") as i32;

const IMA4: u32 = four(b"ima4");
const DVI_IMA: u32 = 0x6D73_0011;
const MP3: u32 = four(b".mp3");
const AAC: u32 = four(b"aac ");
const HE_AAC: u32 = four(b"aach");
const HE_AAC_2: u32 = four(b"aacp");
const ALAC: u32 = four(b"alac");
const ULAW: u32 = four(b"ulaw");
const ALAW: u32 = four(b"alaw");

// ----- sample formats -----

/// Linear PCM this layer reads and writes: whole-byte samples, interleaved.
pub fn pcm_supported(f: &Asbd) -> bool {
    f.format == LPCM
        && f.channels > 0
        && f.flags & audiofile::FLAG_NON_INTERLEAVED == 0
        && match (f.flags & FLAG_FLOAT != 0, f.bits) {
            (true, 32 | 64) => true,
            (false, 8 | 16 | 24 | 32) => true,
            _ => false,
        }
}

fn stride(f: &Asbd) -> usize {
    (f.bytes_per_frame as usize).max(f.bits.div_ceil(8) as usize * f.channels as usize)
}

/// Appends PCM bytes as floats (-1 .. 1), interleaved.
pub fn read_pcm(bytes: &[u8], f: &Asbd, out: &mut Vec<f32>) {
    let sample = f.bits.div_ceil(8) as usize;
    let channels = f.channels as usize;
    let (float, big, signed) = (f.flags & FLAG_FLOAT != 0, f.flags & FLAG_BIG_ENDIAN != 0, f.flags & FLAG_SIGNED != 0);
    let fraction = (f.flags >> 7) & 0x3F;
    for frame in bytes.chunks_exact(stride(f)) {
        for c in 0..channels {
            let mut b = [0u8; 8];
            b[..sample].copy_from_slice(&frame[c * sample..(c + 1) * sample]);
            if big {
                b[..sample].reverse();
            }
            out.push(match (float, sample) {
                (true, 4) => f32::from_le_bytes(b[..4].try_into().unwrap()),
                (true, 8) => f64::from_le_bytes(b) as f32,
                (false, 1) if signed => b[0] as i8 as f32 / 128.0,
                (false, 1) => (b[0] as f32 - 128.0) / 128.0,
                (false, 2) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                (false, 3) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8388608.0,
                (false, 4) if fraction > 0 => i32::from_le_bytes(b[..4].try_into().unwrap()) as f32 / (1u64 << fraction) as f32,
                (false, 4) => i32::from_le_bytes(b[..4].try_into().unwrap()) as f32 / 2147483648.0,
                _ => 0.0,
            });
        }
    }
}

/// Writes interleaved floats as PCM bytes in format `f`.
pub fn write_pcm(samples: &[f32], f: &Asbd, out: &mut [u8]) {
    let sample = f.bits.div_ceil(8) as usize;
    let channels = f.channels as usize;
    let (float, big, signed) = (f.flags & FLAG_FLOAT != 0, f.flags & FLAG_BIG_ENDIAN != 0, f.flags & FLAG_SIGNED != 0);
    let fraction = (f.flags >> 7) & 0x3F;
    let stride = stride(f);
    for (frame, values) in out.chunks_exact_mut(stride).zip(samples.chunks_exact(channels)) {
        for (c, &v) in values.iter().enumerate() {
            let v = if float { v } else { v.clamp(-1.0, 1.0) };
            let mut b = [0u8; 8];
            match (float, sample) {
                (true, 4) => b[..4].copy_from_slice(&v.to_le_bytes()),
                (true, 8) => b.copy_from_slice(&(v as f64).to_le_bytes()),
                (false, 1) if signed => b[0] = (v * 127.0).round() as i8 as u8,
                (false, 1) => b[0] = (v * 127.0 + 128.0).round() as u8,
                (false, 2) => b[..2].copy_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
                (false, 3) => b[..3].copy_from_slice(&((v * 8388607.0).round() as i32).to_le_bytes()[..3]),
                (false, 4) if fraction > 0 => b[..4].copy_from_slice(&((v as f64 * (1u64 << fraction) as f64) as i32).to_le_bytes()),
                (false, 4) => b[..4].copy_from_slice(&((v as f64 * 2147483647.0) as i32).to_le_bytes()),
                _ => {}
            }
            if big {
                b[..sample].reverse();
            }
            frame[c * sample..(c + 1) * sample].copy_from_slice(&b[..sample]);
        }
    }
}

/// Changes the number of channels of interleaved samples.
pub fn remap(samples: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    if from == to || from == 0 {
        out.extend_from_slice(samples);
        return;
    }
    for frame in samples.chunks_exact(from) {
        for c in 0..to {
            out.push(match (from, to) {
                (1, _) => frame[0],
                (_, 1) => (frame[0] + frame[1]) * 0.5,
                _ if c < from => frame[c],
                _ => 0.0,
            });
        }
    }
}

// ----- decoders -----

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
    337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358,
    5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_STEPS: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// One IMA ADPCM nibble: updates the predictor and step index, returns the sample.
fn ima_nibble(nibble: u8, predictor: &mut i32, index: &mut i32) -> i16 {
    let step = STEPS[*index as usize];
    let mut diff = step >> 3;
    if nibble & 1 != 0 {
        diff += step >> 2;
    }
    if nibble & 2 != 0 {
        diff += step >> 1;
    }
    if nibble & 4 != 0 {
        diff += step;
    }
    if nibble & 8 != 0 {
        diff = -diff;
    }
    *predictor = (*predictor + diff).clamp(-32768, 32767);
    *index = (*index + INDEX_STEPS[nibble as usize]).clamp(0, 88);
    *predictor as i16
}

/// Apple IMA4: per channel 34 bytes (2-byte header, 64 samples).
pub fn decode_ima4(packet: &[u8], channels: usize, out: &mut Vec<f32>) {
    let start = out.len();
    out.resize(start + 64 * channels, 0.0);
    for (c, block) in packet.chunks_exact(34).take(channels).enumerate() {
        let header = u16::from_be_bytes([block[0], block[1]]);
        let mut predictor = (header & 0xFF80) as i16 as i32;
        let mut index = ((header & 0x7F) as i32).min(88);
        for (i, byte) in block[2..].iter().enumerate() {
            for (half, nibble) in [byte & 0xF, byte >> 4].into_iter().enumerate() {
                let sample = ima_nibble(nibble, &mut predictor, &mut index);
                out[start + (i * 2 + half) * channels + c] = sample as f32 / 32768.0;
            }
        }
    }
}

/// DVI/Intel IMA ADPCM (WAV): per channel a 4-byte header, then 4-byte groups.
fn decode_dvi(block: &[u8], channels: usize, out: &mut Vec<f32>) {
    if block.len() < 4 * channels {
        return;
    }
    let frames = 1 + (block.len() - 4 * channels) * 2 / channels;
    let start = out.len();
    out.resize(start + frames * channels, 0.0);
    for c in 0..channels {
        let h = &block[4 * c..4 * c + 4];
        let mut predictor = i16::from_le_bytes([h[0], h[1]]) as i32;
        let mut index = (h[2] as i32).min(88);
        out[start + c] = predictor as f32 / 32768.0;
        let mut frame = 1;
        let mut group = 4 * channels + 4 * c;
        while group + 4 <= block.len() && frame < frames {
            for byte in &block[group..group + 4] {
                for nibble in [byte & 0xF, byte >> 4] {
                    if frame < frames {
                        out[start + frame * channels + c] = ima_nibble(nibble, &mut predictor, &mut index) as f32 / 32768.0;
                        frame += 1;
                    }
                }
            }
            group += 4 * channels;
        }
    }
}

fn ulaw(u: u8) -> f32 {
    let u = !u;
    let t = ((((u & 0x0F) as i32) << 3) + 0x84) << ((u & 0x70) >> 4);
    (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as f32 / 32768.0
}

fn alaw(a: u8) -> f32 {
    let a = a ^ 0x55;
    let mut t = ((a & 0x0F) as i32) << 4;
    let segment = (a & 0x70) >> 4;
    match segment {
        0 => t += 8,
        1 => t += 0x108,
        _ => t = (t + 0x108) << (segment - 1),
    }
    (if a & 0x80 != 0 { t } else { -t }) as f32 / 32768.0
}

/// The AudioSpecificConfig inside an AAC magic cookie (an ES descriptor,
/// maybe inside an 'esds' atom), or the cookie itself if it is one.
pub fn config_from_cookie(cookie: &[u8]) -> Option<Vec<u8>> {
    fn descriptor(b: &[u8], at: usize) -> Option<(u8, usize, usize)> {
        let tag = *b.get(at)?;
        let mut length = 0usize;
        let mut i = at + 1;
        for _ in 0..4 {
            let byte = *b.get(i)?;
            i += 1;
            length = (length << 7) | (byte & 0x7F) as usize;
            if byte & 0x80 == 0 {
                break;
            }
        }
        Some((tag, i, length))
    }
    let start = match cookie.windows(4).position(|w| w == b"esds") {
        Some(at) => at + 8,
        None if cookie.first() == Some(&0x03) => 0,
        None => return (cookie.len() >= 2).then(|| cookie.to_vec()),
    };
    let (tag, mut at, _) = descriptor(cookie, start)?;
    if tag != 0x03 {
        return None;
    }
    let flags = *cookie.get(at + 2)?;
    at += 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + *cookie.get(at)? as usize;
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let (tag, body, _) = descriptor(cookie, at)?;
    if tag != 0x04 {
        return None;
    }
    let (tag, body, length) = descriptor(cookie, body + 13)?;
    (tag == 0x05).then(|| cookie.get(body..body + length).map(|c| c.to_vec()))?
}

/// The 24-byte ALAC configuration inside an ALAC magic cookie.
fn alac_config(cookie: &[u8]) -> Vec<u8> {
    if cookie.len() == 24 || cookie.len() == 48 {
        return cookie.to_vec();
    }
    for at in 4..cookie.len().saturating_sub(4) {
        if &cookie[at..at + 4] == b"alac" && u32::from_be_bytes(cookie[at - 4..at].try_into().unwrap()) == 36 && cookie.len() >= at + 32 {
            return cookie[at + 8..at + 32].to_vec();
        }
    }
    cookie[cookie.len().saturating_sub(24)..].to_vec()
}

enum Kind {
    Pcm,
    Ima4,
    Dvi,
    Ulaw,
    Alaw,
    Other(Box<dyn symphonia::core::codecs::Decoder>),
}

/// Turns packets of one format into floats with the format's channel count.
pub struct Decoder {
    kind: Kind,
    format: Asbd,
    /// The rate decoded sound comes out at (HE-AAC decodes at its core rate).
    pub rate: f64,
}

impl Decoder {
    pub fn supported(f: &Asbd) -> bool {
        match f.format {
            LPCM => pcm_supported(f),
            IMA4 | DVI_IMA | ULAW | ALAW | MP3 | AAC | HE_AAC | HE_AAC_2 | ALAC => f.channels > 0,
            _ => false,
        }
    }

    pub fn new(f: &Asbd, cookie: &[u8]) -> Result<Decoder, String> {
        use symphonia::core::audio::Channels;
        use symphonia::core::codecs::{CodecParameters, DecoderOptions, CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_MP3};
        let kind = match f.format {
            LPCM if pcm_supported(f) => Kind::Pcm,
            IMA4 => Kind::Ima4,
            DVI_IMA => Kind::Dvi,
            ULAW => Kind::Ulaw,
            ALAW => Kind::Alaw,
            MP3 | AAC | HE_AAC | HE_AAC_2 | ALAC => {
                let mut params = CodecParameters::new();
                params.with_sample_rate(f.rate.max(1.0) as u32).with_channels(Channels::from_bits_truncate((1u32 << f.channels.clamp(1, 8)) - 1));
                match f.format {
                    MP3 => {
                        params.for_codec(CODEC_TYPE_MP3);
                    }
                    ALAC => {
                        params.for_codec(CODEC_TYPE_ALAC).with_extra_data(alac_config(cookie).into_boxed_slice());
                    }
                    _ => {
                        let config = config_from_cookie(cookie).unwrap_or_else(|| {
                            let core = if f.format == AAC { f.rate } else { f.rate / 2.0 };
                            audiofile::audio_specific_config(2, core as u32, f.channels)
                        });
                        params.for_codec(CODEC_TYPE_AAC).with_extra_data(config.into_boxed_slice());
                    }
                }
                let made = std::panic::catch_unwind(|| symphonia::default::get_codecs().make(&params, &DecoderOptions::default()));
                Kind::Other(made.map_err(|_| "the decoder refused the stream".to_string())?.map_err(|e| e.to_string())?)
            }
            _ => return Err(format!("format {} isn't supported", sound::four_cc(f.format))),
        };
        Ok(Decoder { kind, format: *f, rate: f.rate })
    }

    /// Appends one packet's sound (`frames` is how many it should hold;
    /// a packet that won't decode becomes that much silence).
    pub fn decode(&mut self, packet: &[u8], frames: u32, out: &mut Vec<f32>) {
        let channels = self.format.channels.max(1) as usize;
        match &mut self.kind {
            Kind::Pcm => read_pcm(packet, &self.format, out),
            Kind::Ima4 => {
                for p in packet.chunks_exact(34 * channels) {
                    decode_ima4(p, channels, out);
                }
            }
            Kind::Dvi => {
                let block = (self.format.bytes_per_packet as usize).max(4 * channels);
                for b in packet.chunks(block) {
                    decode_dvi(b, channels, out);
                }
            }
            Kind::Ulaw => out.extend(packet.iter().map(|&u| ulaw(u))),
            Kind::Alaw => out.extend(packet.iter().map(|&a| alaw(a))),
            Kind::Other(decoder) => {
                use symphonia::core::audio::SampleBuffer;
                let packet = symphonia::core::formats::Packet::new_from_slice(0, 0, frames as u64, packet);
                // A broken packet can make the decoder panic: that is silence too.
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let decoded = decoder.decode(&packet).ok()?;
                    let spec = *decoded.spec();
                    let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    samples.copy_interleaved_ref(decoded);
                    Some((spec, samples))
                }));
                match decoded {
                    Ok(Some((spec, samples))) => {
                        self.rate = spec.rate as f64;
                        remap(samples.samples(), spec.channels.count(), channels, out);
                    }
                    _ => {
                        // Silence at the rate the decoder works in.
                        let n = (frames as f64 * self.rate / self.format.rate.max(1.0)) as usize;
                        out.resize(out.len() + n * channels, 0.0);
                    }
                }
            }
        }
    }
}

// ----- the queues -----

/// A buffer the game enqueued, copied.
struct Pending {
    buffer: usize,
    data: Vec<u8>,
    /// (offset, size, frames) per packet.
    packets: Vec<(usize, usize, u32)>,
    trim_start: u32,
    trim_end: u32,
}

#[derive(Default)]
struct State {
    format: Asbd,
    cookie: Vec<u8>,
    layout: Vec<u8>,
    callback: usize,
    user: usize,
    run_loop: Id,
    pending: VecDeque<Pending>,
    decoder: Option<Decoder>,
    running: bool,
    /// Asked to stop once everything enqueued has played.
    stopping: bool,
    /// Offline mode: the PCM format OfflineRender produces.
    offline: Option<Asbd>,
    /// Offline: decoded sound (render format's channels) not yet handed out.
    decoded: Vec<f32>,
    decoded_rate: f64,
    position: f64,
    /// Playing: stereo sound ready for the output, at `stream_rate`.
    stream: Stream,
    stream_rate: f64,
    /// Source frames played since the queue started.
    played: f64,
    volume: f32,
    pan: f32,
    listeners: Vec<(u32, usize, usize)>,
    /// IsRunning changed: listeners still to be told.
    tell_running: bool,
    buffers: Vec<usize>,
    /// Every enqueued buffer not yet handed back (waiting, being decoded or
    /// with its callback posted), with the epoch it was enqueued in. A
    /// buffer is handed back by whoever takes it out of here, so only once.
    outstanding: Vec<(usize, u64)>,
    /// Changes on every stop or reset; work begun before one is dropped.
    epoch: u64,
    quit: bool,
    /// Stop or Reset is handing flushed buffers back: enqueueing is refused.
    resetting: u32,
    /// Properties stored as set (metering, time-pitch, codec policy, ...).
    settings: HashMap<u32, Vec<u8>>,
}

/// How long Stop, Reset and Dispose wait for a callback another thread is
/// running before giving up (it may be waiting on their caller).
const CALLBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Which thread is running one of the queue's callbacks, and how deeply.
#[derive(Default)]
struct Busy {
    owner: Option<std::thread::ThreadId>,
    depth: u32,
}

struct Queue {
    state: Mutex<State>,
    wake: Condvar,
    /// The playing thread was started.
    worker: Mutex<bool>,
    busy: Mutex<Busy>,
    idle: Condvar,
}

/// Holds a queue's callback turn; one thread at a time, re-entrant.
struct Turn<'a>(&'a Queue);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut busy = self.0.busy.lock().unwrap_or_else(|e| e.into_inner());
        busy.depth -= 1;
        if busy.depth == 0 {
            busy.owner = None;
            self.0.idle.notify_all();
        }
    }
}

impl Queue {
    fn new(state: State) -> Queue {
        Queue { state: Mutex::new(state), wake: Condvar::new(), worker: Mutex::new(false), busy: Mutex::default(), idle: Condvar::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes the callback turn, waiting while another thread has it (at most
    /// `limit`; None if it is still busy then).
    fn take_turn(&self, limit: Option<std::time::Duration>) -> Option<Turn<'_>> {
        let me = std::thread::current().id();
        let deadline = limit.map(|l| std::time::Instant::now() + l);
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while busy.depth > 0 && busy.owner != Some(me) {
            busy = match deadline {
                None => self.idle.wait(busy).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    self.idle.wait_timeout(busy, left).unwrap_or_else(|e| e.into_inner()).0
                }
            };
        }
        busy.owner = Some(me);
        busy.depth += 1;
        Some(Turn(self))
    }

    fn turn(&self) -> Turn<'_> {
        self.take_turn(None).expect("an unlimited wait ends with the turn")
    }
}

/// Runs a game callback in the queue's turn, unless the queue was disposed.
/// With `owed`, only if that buffer (from that epoch) is still waiting to be
/// handed back; it is taken off the list, so no one else hands it back.
fn deliver(q: &Queue, owed: Option<(usize, u64)>, call: impl FnOnce()) {
    let _turn = q.turn();
    let mut state = q.lock();
    let mut live = !state.quit;
    if let Some(owed) = owed {
        match state.outstanding.iter().position(|&o| o == owed) {
            Some(at) if live => {
                state.outstanding.remove(at);
                live = state.buffers.contains(&owed.0);
            }
            _ => live = false,
        }
    }
    drop(state);
    if live {
        call();
    }
}

static QUEUES: Mutex<Option<HashMap<usize, Arc<Queue>>>> = Mutex::new(None);

fn queue(handle: usize) -> Option<Arc<Queue>> {
    QUEUES.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&handle).cloned()
}

type OutputCallback = extern "C" fn(usize, usize, usize);
type PropertyListener = extern "C" fn(usize, usize, u32);

const IS_RUNNING: u32 = four(b"aqrn");

/// Decodes one enqueued buffer into floats with the format's channels.
fn decode_pending(decoder: &mut Decoder, pending: &Pending, out: &mut Vec<f32>, channels: usize) {
    let start = out.len();
    for &(offset, size, frames) in &pending.packets {
        if let Some(packet) = pending.data.get(offset..offset + size) {
            decoder.decode(packet, frames, out);
        }
    }
    // Trimming (AudioQueueEnqueueBufferWithParameters) counts frames.
    let frames = (out.len() - start) / channels;
    let cut_end = (pending.trim_end as usize).min(frames);
    out.truncate(out.len() - cut_end * channels);
    let cut_start = (pending.trim_start as usize).min(frames - cut_end);
    out.drain(start..start + cut_start * channels);
}

fn ensure_decoder(state: &mut State) -> bool {
    if state.decoder.is_none() {
        match Decoder::new(&state.format, &state.cookie) {
            Ok(d) => state.decoder = Some(d),
            Err(e) => {
                crate::log!("AudioQueue: can't decode {} ({e})", sound::four_cc(state.format.format));
                return false;
            }
        }
    }
    true
}

/// AudioQueueNewOutput(format, callback, userData, runLoop, mode, flags, *queue).
extern "C" fn new_output(format: *const u8, callback: usize, user: usize, run_loop: Id, _mode: Id, _flags: u32, out: *mut usize) -> i32 {
    if format.is_null() || out.is_null() {
        return PARAMETER;
    }
    let format = unsafe { Asbd::read(format) };
    if !Decoder::supported(&format) || format.channels > 64 || !(format.rate > 0.0 && format.rate < 1e7) {
        crate::log!("AudioQueue: format {} flags {:#x} {} bits isn't supported", sound::four_cc(format.format), format.flags, format.bits);
        return UNSUPPORTED_FORMAT;
    }
    let handle = crate::symbols::leak([0usize; 4]);
    let state = State { format, callback, user, run_loop, volume: 1.0, stream_rate: format.rate, decoded_rate: format.rate, ..State::default() };
    let made = Arc::new(Queue::new(state));
    QUEUES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(handle, made);
    crate::log!("AudioQueue: new output queue for {} at {} Hz, {} channels", sound::four_cc(format.format), format.rate, format.channels);
    unsafe { *out = handle };
    0
}

fn allocate(handle: usize, size: u32, descriptions: u32, out: *mut usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    if out.is_null() || size == 0 {
        return PARAMETER;
    }
    let header = 0x38usize;
    let total = header + size as usize + 16 * descriptions as usize + 16;
    let block = unsafe { libc::calloc(1, total) } as usize;
    if block == 0 {
        return -108;
    }
    unsafe {
        *(block as *mut u32) = size;
        *((block + 8) as *mut usize) = block + header;
        *((block + 0x20) as *mut u32) = descriptions;
        let table = (block + header + size as usize + 7) & !7;
        *((block + 0x28) as *mut usize) = if descriptions > 0 { table } else { 0 };
        *out = block;
    }
    q.lock().buffers.push(block);
    0
}

/// AudioQueueAllocateBuffer(queue, size, *buffer).
extern "C" fn allocate_buffer(handle: usize, size: u32, out: *mut usize) -> i32 {
    allocate(handle, size, 0, out)
}

/// AudioQueueAllocateBufferWithPacketDescriptions(queue, size, descriptions, *buffer).
extern "C" fn allocate_buffer_with_descriptions(handle: usize, size: u32, descriptions: u32, out: *mut usize) -> i32 {
    allocate(handle, size, descriptions, out)
}

/// AudioQueueFreeBuffer(queue, buffer).
extern "C" fn free_buffer(handle: usize, buffer: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    let Some(at) = state.buffers.iter().position(|&b| b == buffer) else { return INVALID_BUFFER };
    state.buffers.remove(at);
    state.pending.retain(|p| p.buffer != buffer);
    state.outstanding.retain(|o| o.0 != buffer);
    unsafe { libc::free(buffer as *mut libc::c_void) };
    0
}

fn enqueue(handle: usize, buffer: usize, count: u32, descriptions: *const u8, trim_start: u32, trim_end: u32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    if !state.buffers.contains(&buffer) {
        return INVALID_BUFFER;
    }
    if state.resetting > 0 {
        return ENQUEUE_DURING_RESET;
    }
    let (capacity, data, size) = unsafe { (*(buffer as *const u32) as usize, *((buffer + 8) as *const usize), *((buffer + 0x10) as *const u32) as usize) };
    if size == 0 || data == 0 {
        return BUFFER_EMPTY;
    }
    if size > capacity {
        return PARAMETER;
    }
    let (count, descriptions) = if count == 0 || descriptions.is_null() {
        // The buffer's own table holds at most the count it was made with.
        unsafe { ((*((buffer + 0x30) as *const u32)).min(*((buffer + 0x20) as *const u32)), *((buffer + 0x28) as *const usize) as *const u8) }
    } else {
        (count, descriptions)
    };
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, size) }.to_vec();
    let f = state.format;
    let mut packets = Vec::new();
    if count > 0 && !descriptions.is_null() && f.format != LPCM {
        for i in 0..count as usize {
            let d = unsafe { descriptions.add(16 * i) };
            let (offset, frames, length) = unsafe { ((d as *const i64).read_unaligned(), (d.add(8) as *const u32).read_unaligned(), (d.add(12) as *const u32).read_unaligned()) };
            let frames = if frames > 0 { frames } else { f.frames_per_packet };
            if offset >= 0 && (offset as u64).saturating_add(length as u64) <= size as u64 {
                packets.push((offset as usize, length as usize, frames));
            }
        }
    } else if f.bytes_per_packet > 0 {
        // Fixed-size packets: PCM, IMA4, u-law and A-law.
        let per = if f.format == LPCM { size / stride(&f) * stride(&f) } else { f.bytes_per_packet as usize };
        let mut at = 0;
        while per > 0 && at + per <= size {
            packets.push((at, per, f.frames_per_packet.max(1) * (per / f.bytes_per_packet as usize) as u32));
            at += per;
        }
    } else {
        crate::log!("AudioQueue: a buffer of {} packets came without packet descriptions", sound::four_cc(f.format));
        return PARAMETER;
    }
    state.pending.push_back(Pending { buffer, data: bytes, packets, trim_start, trim_end });
    let epoch = state.epoch;
    state.outstanding.push((buffer, epoch));
    drop(state);
    q.wake.notify_all();
    0
}

/// AudioQueueEnqueueBuffer(queue, buffer, descriptionCount, descriptions).
extern "C" fn enqueue_buffer(handle: usize, buffer: usize, count: u32, descriptions: *const u8) -> i32 {
    enqueue(handle, buffer, count, descriptions, 0, 0)
}

/// AudioQueueEnqueueBufferWithParameters(queue, buffer, count, descriptions,
/// trimStart, trimEnd, parameterCount, parameters, startTime, *actualStartTime).
extern "C" fn enqueue_buffer_with_parameters(handle: usize, buffer: usize, count: u32, descriptions: *const u8, trim_start: u32, trim_end: u32, _n: u32, _p: usize, _start: usize, actual: *mut u8) -> i32 {
    if !actual.is_null() {
        unsafe { std::ptr::write_bytes(actual, 0, 64) };
    }
    enqueue(handle, buffer, count, descriptions, trim_start, trim_end)
}

/// Runs a playing queue: decodes ahead of the output and hands buffers back.
fn work(handle: usize, q: Arc<Queue>) {
    let mut scratch = Vec::new();
    loop {
        let mut state = q.lock();
        loop {
            if state.quit {
                return;
            }
            let ahead = (state.stream.frames() as f64) < state.stream_rate.max(8000.0) * 0.4;
            if state.tell_running || (state.running && state.offline.is_none() && ahead && !state.pending.is_empty()) {
                break;
            }
            state = q.wake.wait_timeout(state, std::time::Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
        }
        if state.tell_running {
            state.tell_running = false;
            let listeners: Vec<_> = state.listeners.iter().filter(|l| l.0 == IS_RUNNING).copied().collect();
            drop(state);
            for (id, listener, data) in listeners {
                let listener: PropertyListener = unsafe { std::mem::transmute(listener) };
                deliver(&q, None, || listener(data, handle, id));
            }
            continue;
        }
        if !ensure_decoder(&mut state) {
            // Nothing can be played: hand the buffers back so the game isn't stuck.
            let gone: Vec<usize> = state.pending.drain(..).map(|p| p.buffer).collect();
            let (callback, user, run_loop, epoch) = (state.callback, state.user, state.run_loop, state.epoch);
            drop(state);
            for buffer in gone {
                call_back(&q, handle, callback, user, run_loop, epoch, buffer);
            }
            continue;
        }
        let pending = state.pending.pop_front().unwrap();
        let mut decoder = state.decoder.take().unwrap();
        let (epoch, channels) = (state.epoch, state.format.channels.max(1) as usize);
        drop(state);
        scratch.clear();
        decode_pending(&mut decoder, &pending, &mut scratch, channels);
        let mut state = q.lock();
        if state.epoch != epoch {
            // Stop or Reset came meanwhile and handed the buffer back.
            continue;
        }
        state.decoder = Some(decoder);
        let rate = state.decoder.as_ref().unwrap().rate;
        if (rate - state.stream_rate).abs() > 0.5 {
            state.stream_rate = rate;
        }
        let mut stereo = Vec::with_capacity(scratch.len() / channels * 2);
        remap(&scratch, channels, 2, &mut stereo);
        state.stream.pending.extend_from_slice(&stereo);
        let (callback, user, run_loop) = (state.callback, state.user, state.run_loop);
        drop(state);
        call_back(&q, handle, callback, user, run_loop, epoch, pending.buffer);
    }
}

/// Hands a buffer back to the game: on the queue's thread, or posted to the
/// run loop the game gave.
fn call_back(q: &Arc<Queue>, handle: usize, callback: usize, user: usize, run_loop: Id, epoch: u64, buffer: usize) {
    if callback == 0 {
        return;
    }
    let output: OutputCallback = unsafe { std::mem::transmute(callback) };
    if run_loop == 0 {
        deliver(q, Some((buffer, epoch)), || output(user, handle, buffer));
        return;
    }
    let weak = Arc::downgrade(q);
    runloop::post_to_object(
        run_loop,
        Work::Native(Box::new(move || {
            if let Some(q) = weak.upgrade() {
                deliver(&q, Some((buffer, epoch)), || output(user, handle, buffer));
            }
        })),
    );
}

/// Stop(immediate) and Reset: in the queue's callback turn (so no callback
/// runs on another thread meanwhile, and none is half done), drop what is
/// enqueued and hand every outstanding buffer back, in order, before
/// returning; enqueueing from those callbacks fails, as on iOS. If another
/// thread's callback doesn't finish in time nothing changes and the call
/// fails, so the game still owns a working queue.
fn flush_all(q: &Queue, handle: usize, stop: bool) -> i32 {
    let Some(_turn) = q.take_turn(Some(CALLBACK_WAIT)) else {
        crate::log!("AudioQueue{}: a buffer callback on another thread is still running; nothing was done", if stop { "Stop" } else { "Reset" });
        return INVALID_RUN_STATE;
    };
    let (flushed, callback, user) = {
        let mut state = q.lock();
        if stop {
            if state.running {
                state.tell_running = !state.listeners.is_empty();
            }
            state.running = false;
            state.stopping = false;
            state.played = 0.0;
        }
        state.resetting += 1;
        (clear(&mut state), state.callback, state.user)
    };
    q.wake.notify_all();
    if callback != 0 {
        let output: OutputCallback = unsafe { std::mem::transmute(callback) };
        for buffer in flushed {
            let live = {
                let state = q.lock();
                !state.quit && state.buffers.contains(&buffer)
            };
            if live {
                output(user, handle, buffer);
            }
        }
    }
    q.lock().resetting -= 1;
    0
}

/// AudioQueueStart(queue, startTime).
extern "C" fn start(handle: usize, _time: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    let was = state.running;
    state.running = true;
    state.stopping = false;
    let offline = state.offline.is_some();
    if !was {
        state.tell_running = !state.listeners.is_empty();
    }
    drop(state);
    if !offline {
        sound::start();
        let mut worker = q.worker.lock().unwrap_or_else(|e| e.into_inner());
        if !*worker {
            let me = q.clone();
            // A thread that can't be made is tried again at the next Start.
            if std::thread::Builder::new().name("ios-audioqueue".into()).spawn(move || work(handle, me)).is_err() {
                drop(worker);
                q.lock().running = false;
                return CANNOT_START;
            }
            *worker = true;
        }
    }
    q.wake.notify_all();
    0
}

/// Drops what's enqueued and decoded; returns every buffer not yet handed
/// back (the caller hands them back or frees them).
fn clear(state: &mut State) -> Vec<usize> {
    state.pending.clear();
    let flushed = state.outstanding.drain(..).map(|o| o.0).collect();
    state.stream = Stream::default();
    state.decoded.clear();
    state.position = 0.0;
    state.decoder = None;
    state.epoch += 1;
    flushed
}

/// AudioQueueStop(queue, immediate).
extern "C" fn stop(handle: usize, immediate: u8) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    if immediate != 0 || state.offline.is_some() || !state.running {
        drop(state);
        return flush_all(&q, handle, true);
    }
    state.stopping = true;
    drop(state);
    q.wake.notify_all();
    0
}

/// AudioQueuePause(queue).
extern "C" fn pause(handle: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    q.lock().running = false;
    0
}

/// AudioQueueFlush(queue): everything enqueued will be decoded fully (it is anyway).
extern "C" fn flush(handle: usize) -> i32 {
    if queue(handle).is_some() {
        0
    } else {
        PARAMETER
    }
}

/// AudioQueueReset(queue): drops what's enqueued, handing it back; keeps running.
extern "C" fn reset(handle: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    flush_all(&q, handle, false)
}

/// AudioQueuePrime(queue, frames, *prepared).
extern "C" fn prime(handle: usize, _frames: u32, prepared: *mut u32) -> i32 {
    if !prepared.is_null() {
        unsafe { *prepared = 0 };
    }
    if queue(handle).is_some() {
        0
    } else {
        PARAMETER
    }
}

/// AudioQueueDispose(queue, immediate): no callback runs after it returns.
/// Done in the queue's callback turn, so one running on another thread
/// finishes first; if it doesn't in time, the queue stays and the call
/// fails. (One on this thread, disposing from inside it, checks `quit`
/// when it comes back.)
extern "C" fn dispose(handle: usize, _immediate: u8) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let Some(turn) = q.take_turn(Some(CALLBACK_WAIT)) else {
        crate::log!("AudioQueueDispose: a buffer callback on another thread is still running; the queue stays");
        return INVALID_RUN_STATE;
    };
    if QUEUES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|queues| queues.remove(&handle)).is_none() {
        return PARAMETER;
    }
    let buffers = {
        let mut state = q.lock();
        clear(&mut state);
        state.quit = true;
        std::mem::take(&mut state.buffers)
    };
    drop(turn);
    q.wake.notify_all();
    // The output may be mixing it right now; wait for that turn to finish.
    sound::wait_for_mix();
    for buffer in buffers {
        unsafe { libc::free(buffer as *mut libc::c_void) };
    }
    0
}

/// AudioQueueSetProperty(queue, id, data, size).
extern "C" fn set_property(handle: usize, id: u32, data: *const u8, size: u32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let bytes = if data.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(data, size as usize) } };
    let mut state = q.lock();
    match &id.to_be_bytes() {
        b"aqmc" => {
            state.cookie = bytes.to_vec();
            state.decoder = None;
        }
        b"aqcl" => state.layout = bytes.to_vec(),
        // Metering (meters read as silence), time-pitch, codec policy and
        // decode buffer size: kept so they read back; the sound is the same.
        b"aqme" | b"q_tp" | b"qtpa" | b"qtpb" | b"aqcp" | b"dcbf" => {
            if bytes.len() < 4 {
                return INVALID_PROPERTY_SIZE;
            }
            state.settings.insert(id, bytes[..4].to_vec());
        }
        // Output device and channel assignments: there is one output.
        b"aqcd" | b"aqca" => {}
        other => {
            crate::log!("AudioQueueSetProperty '{}': not known", String::from_utf8_lossy(other));
            return INVALID_PROPERTY;
        }
    }
    0
}

fn property(state: &State, id: u32) -> Option<Vec<u8>> {
    if let Some(value) = state.settings.get(&id) {
        return Some(value.clone());
    }
    let format = unsafe { std::slice::from_raw_parts(&state.format as *const Asbd as *const u8, 40) };
    Some(match &id.to_be_bytes() {
        b"aqrn" => (state.running as u32).to_ne_bytes().to_vec(),
        b"aqft" => format.to_vec(),
        b"aqmc" => state.cookie.clone(),
        b"aqcl" if !state.layout.is_empty() => state.layout.clone(),
        b"aqsr" => (sound::output_rate() as f64).to_ne_bytes().to_vec(),
        b"aqdc" => 2u32.to_ne_bytes().to_vec(),
        b"xops" | b"dcbf" => 4096u32.to_ne_bytes().to_vec(),
        // Off / default policy / no converter error.
        b"aqcp" | b"aqme" | b"q_tp" | b"qtpb" | b"qcve" => 0u32.to_ne_bytes().to_vec(),
        // kAudioQueueTimePitchAlgorithm_TimeDomain, the iPhone's default.
        b"qtpa" => four(b"tido").to_ne_bytes().to_vec(),
        // AudioQueueLevelMeterState (average, peak) per channel: silence.
        b"aqmv" => vec![0; 8 * state.format.channels.clamp(1, 64) as usize],
        b"aqmd" => (0..2 * state.format.channels.clamp(1, 64)).flat_map(|_| (-120.0f32).to_ne_bytes()).collect(),
        _ => return None,
    })
}

/// AudioQueueGetProperty(queue, id, data, *size).
extern "C" fn get_property(handle: usize, id: u32, data: *mut u8, size: *mut u32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let Some(value) = property(&q.lock(), id) else { return INVALID_PROPERTY };
    if size.is_null() || data.is_null() {
        return PARAMETER;
    }
    let room = unsafe { *size } as usize;
    if room < value.len() && !matches!(&id.to_be_bytes(), b"aqmc" | b"aqcl" | b"aqmv" | b"aqmd") {
        return INVALID_PROPERTY_SIZE;
    }
    let n = room.min(value.len());
    unsafe {
        std::ptr::copy_nonoverlapping(value.as_ptr(), data, n);
        *size = n as u32;
    }
    0
}

/// AudioQueueGetPropertySize(queue, id, *size).
extern "C" fn get_property_size(handle: usize, id: u32, size: *mut u32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let Some(value) = property(&q.lock(), id) else { return INVALID_PROPERTY };
    if !size.is_null() {
        unsafe { *size = value.len() as u32 };
    }
    0
}

/// AudioQueueAddPropertyListener(queue, id, proc, data).
extern "C" fn add_listener(handle: usize, id: u32, listener: usize, data: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    if listener == 0 {
        return PARAMETER;
    }
    q.lock().listeners.push((id, listener, data));
    0
}

/// AudioQueueRemovePropertyListener(queue, id, proc, data).
extern "C" fn remove_listener(handle: usize, id: u32, listener: usize, data: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    q.lock().listeners.retain(|&l| l != (id, listener, data));
    0
}

const VOLUME: u32 = 1;
const PAN: u32 = 13;

/// AudioQueueSetParameter(queue, id, value).
extern "C" fn set_parameter(handle: usize, id: u32, value: f32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    match id {
        VOLUME => state.volume = value.clamp(0.0, 1.0),
        PAN => state.pan = value.clamp(-1.0, 1.0),
        // Play rate, pitch and volume ramp time.
        2..=4 => {}
        _ => return INVALID_PARAMETER,
    }
    0
}

/// AudioQueueGetParameter(queue, id, *value).
extern "C" fn get_parameter(handle: usize, id: u32, value: *mut f32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    if value.is_null() {
        return PARAMETER;
    }
    let state = q.lock();
    unsafe {
        *value = match id {
            VOLUME => state.volume,
            PAN => state.pan,
            2 => 1.0,
            3 | 4 => 0.0,
            _ => return INVALID_PARAMETER,
        }
    };
    0
}

/// AudioQueueGetCurrentTime(queue, timeline, *timeStamp, *discontinuity).
extern "C" fn get_current_time(handle: usize, _timeline: usize, stamp: *mut u8, discontinuity: *mut u8) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    if !stamp.is_null() {
        let played = q.lock().played;
        unsafe {
            std::ptr::write_bytes(stamp, 0, 64);
            (stamp as *mut f64).write_unaligned(played);
            (stamp.add(56) as *mut u32).write_unaligned(1);
        }
    }
    if !discontinuity.is_null() {
        unsafe { *discontinuity = 0 };
    }
    0
}

/// AudioQueueSetOfflineRenderFormat(queue, format, layout): NULL format goes back to playing.
extern "C" fn set_offline_render_format(handle: usize, format: *const u8, _layout: usize) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    let mut state = q.lock();
    if format.is_null() {
        state.offline = None;
        return 0;
    }
    let format = unsafe { Asbd::read(format) };
    if !pcm_supported(&format) || format.channels > 64 || !(format.rate > 0.0 && format.rate < 1e7) {
        crate::log!("AudioQueue: offline render format flags {:#x} {} bits isn't supported", format.flags, format.bits);
        return UNSUPPORTED_FORMAT;
    }
    state.offline = Some(format);
    // Sound decoded for the old format starts over; enqueued buffers stay.
    state.decoded.clear();
    state.position = 0.0;
    state.stream = Stream::default();
    0
}

/// Output frames that the decoded sound can give now.
fn ready_frames(state: &State, out: &Asbd) -> usize {
    let channels = out.channels.max(1) as usize;
    let have = state.decoded.len() / channels;
    if (state.decoded_rate - out.rate).abs() < 0.5 {
        return have;
    }
    let step = state.decoded_rate / out.rate;
    if have < 2 {
        return 0;
    }
    (((have - 1) as f64 - state.position) / step).max(0.0) as usize
}

/// AudioQueueOfflineRender(queue, timeStamp, buffer, frames): decodes what
/// is enqueued into `buffer` in the offline format, handing finished
/// buffers back to the game (which enqueues more) as it goes.
extern "C" fn offline_render(handle: usize, _stamp: *const u8, buffer: usize, frames: u32) -> i32 {
    let Some(q) = queue(handle) else { return PARAMETER };
    // The game's callbacks run here, in the queue's turn: Stop, Reset and
    // Dispose from other threads wait for this to finish.
    let _turn = q.turn();
    let mut state = q.lock();
    if !state.buffers.contains(&buffer) {
        return INVALID_BUFFER;
    }
    let (capacity, data) = unsafe { (*(buffer as *const u32) as usize, *((buffer + 8) as *const usize)) };
    let Some(out) = state.offline else { return INVALID_OFFLINE_MODE };
    let bytes_per_frame = stride(&out);
    let frames = (frames as usize).min(capacity / bytes_per_frame);
    if frames == 0 || data == 0 {
        unsafe { *((buffer + 0x10) as *mut u32) = 0 };
        return 0;
    }
    let channels = out.channels as usize;
    let source_channels = state.format.channels.max(1) as usize;
    let mut scratch = Vec::new();
    while ready_frames(&state, &out) < frames {
        if state.pending.is_empty() || !ensure_decoder(&mut state) {
            break;
        }
        let pending = state.pending.pop_front().unwrap();
        let mut decoder = state.decoder.take().unwrap();
        let epoch = state.epoch;
        drop(state);
        scratch.clear();
        decode_pending(&mut decoder, &pending, &mut scratch, source_channels);
        state = q.lock();
        if state.epoch == epoch {
            let rate = decoder.rate;
            state.decoder = Some(decoder);
            if (rate - state.decoded_rate).abs() > 0.5 {
                // The decoder's real rate (HE-AAC's core rate).
                let leftover = state.decoded.len();
                if leftover == 0 {
                    state.decoded_rate = rate;
                    state.position = 0.0;
                }
            }
            let mut decoded = std::mem::take(&mut state.decoded);
            remap(&scratch, source_channels, channels, &mut decoded);
            state.decoded = decoded;
        }
        let (callback, user) = (state.callback, state.user);
        // Handed back here unless something else already did (taken off the list).
        let owed = state.outstanding.iter().position(|&o| o == (pending.buffer, epoch)).map(|at| state.outstanding.remove(at));
        let give_back = callback != 0 && owed.is_some() && state.buffers.contains(&pending.buffer);
        drop(state);
        if give_back {
            let output: OutputCallback = unsafe { std::mem::transmute(callback) };
            output(user, handle, pending.buffer);
        }
        state = q.lock();
        // The callback may have disposed of the queue, freed the target
        // buffer or left offline mode.
        if state.quit {
            return DISPOSAL_PENDING;
        }
        if !state.buffers.contains(&buffer) {
            return INVALID_BUFFER;
        }
        if state.offline != Some(out) {
            return INVALID_OFFLINE_MODE;
        }
    }
    let made = ready_frames(&state, &out).min(frames);
    let mut samples = Vec::with_capacity(made * channels);
    if (state.decoded_rate - out.rate).abs() < 0.5 {
        samples.extend(state.decoded.drain(..made * channels));
    } else {
        let step = state.decoded_rate / out.rate;
        let decoded = &state.decoded;
        for i in 0..made {
            let at = state.position + i as f64 * step;
            let index = at as usize;
            let part = (at - index as f64) as f32;
            for c in 0..channels {
                let a = decoded[index * channels + c];
                let b = decoded.get((index + 1) * channels + c).copied().unwrap_or(a);
                samples.push(a + (b - a) * part);
            }
        }
        let end = state.position + made as f64 * step;
        let used = (end as usize).min(state.decoded.len() / channels);
        state.decoded.drain(..used * channels);
        state.position = end - used as f64;
    }
    state.played += made as f64;
    drop(state);
    let target = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, made * bytes_per_frame) };
    write_pcm(&samples, &out, target);
    unsafe { *((buffer + 0x10) as *mut u32) = (made * bytes_per_frame) as u32 };
    0
}

/// Mixes every playing queue into `out` (stereo).
pub fn mix(out: &mut [f32], frames: usize, out_rate: f64) {
    let Ok(guard) = QUEUES.try_lock() else { return };
    let Some(queues) = guard.as_ref() else { return };
    let playing: Vec<Arc<Queue>> = queues.values().cloned().collect();
    drop(guard);
    for q in playing {
        let Ok(mut state) = q.state.try_lock() else { continue };
        if !state.running || state.offline.is_some() || state.stream_rate <= 0.0 {
            continue;
        }
        let step = state.stream_rate / out_rate;
        let (left, right) = sound::pan_gains(state.pan, state.volume, false);
        let played = state.stream.mix_into(out, frames, step, left, right);
        // Counted in the queue format's frames (HE-AAC decodes at half rate).
        state.played += played * state.format.rate / state.stream_rate;
        let low = (state.stream.frames() as f64) < state.stream_rate * 0.2;
        if state.stopping && state.pending.is_empty() && state.stream.frames() < 2 {
            // Played out after AudioQueueStop(queue, false).
            state.running = false;
            state.stopping = false;
            state.tell_running = !state.listeners.is_empty();
            drop(state);
            q.wake.notify_all();
        } else if low {
            drop(state);
            q.wake.notify_all();
        }
    }
}

pub fn register(r: &mut Registry) {
    r.function("AudioQueueNewOutput", new_output as usize);
    r.function("AudioQueueAllocateBuffer", allocate_buffer as usize);
    r.function("AudioQueueAllocateBufferWithPacketDescriptions", allocate_buffer_with_descriptions as usize);
    r.function("AudioQueueFreeBuffer", free_buffer as usize);
    r.function("AudioQueueEnqueueBuffer", enqueue_buffer as usize);
    r.function("AudioQueueEnqueueBufferWithParameters", enqueue_buffer_with_parameters as usize);
    r.function("AudioQueueStart", start as usize);
    r.function("AudioQueueStop", stop as usize);
    r.function("AudioQueuePause", pause as usize);
    r.function("AudioQueueFlush", flush as usize);
    r.function("AudioQueueReset", reset as usize);
    r.function("AudioQueuePrime", prime as usize);
    r.function("AudioQueueDispose", dispose as usize);
    r.function("AudioQueueSetProperty", set_property as usize);
    r.function("AudioQueueGetProperty", get_property as usize);
    r.function("AudioQueueGetPropertySize", get_property_size as usize);
    r.function("AudioQueueAddPropertyListener", add_listener as usize);
    r.function("AudioQueueRemovePropertyListener", remove_listener as usize);
    r.function("AudioQueueSetParameter", set_parameter as usize);
    r.function("AudioQueueGetParameter", get_parameter as usize);
    r.function("AudioQueueGetCurrentTime", get_current_time as usize);
    r.function("AudioQueueSetOfflineRenderFormat", set_offline_render_format as usize);
    r.function("AudioQueueOfflineRender", offline_render as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 16-bit WAV file: `seconds` of a 441 Hz tone at `rate`.
    fn wav(rate: u32, seconds: f64) -> Vec<u8> {
        let frames = (rate as f64 * seconds) as u32;
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        for v in [16u32.to_le_bytes(), (1u32 | 1 << 16).to_le_bytes(), rate.to_le_bytes(), (rate * 2).to_le_bytes(), (2u32 | 16 << 16).to_le_bytes()] {
            file.extend_from_slice(&v);
        }
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            let v = ((i as f64 / rate as f64 * 441.0 * std::f64::consts::TAU).sin() * 12000.0) as i16;
            file.extend_from_slice(&v.to_le_bytes());
        }
        file
    }

    struct Feeder {
        queue: usize,
        data: Vec<u8>,
        at: usize,
        handed_back: usize,
        refused: usize,
    }

    /// The game's side, as FMOD does it: each buffer handed back is refilled
    /// from the file and enqueued again until the file runs out.
    extern "C" fn refill(user: usize, queue: usize, buffer: usize) {
        let feeder = unsafe { &mut *(user as *mut Feeder) };
        feeder.handed_back += 1;
        fill(feeder, queue, buffer);
    }

    fn fill(feeder: &mut Feeder, queue: usize, buffer: usize) {
        let (capacity, data) = unsafe { (*(buffer as *const u32) as usize, *((buffer + 8) as *const usize)) };
        let take = capacity.min(feeder.data.len() - feeder.at);
        if take == 0 {
            return;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(feeder.data[feeder.at..].as_ptr(), data as *mut u8, take);
            *((buffer + 0x10) as *mut u32) = take as u32;
        }
        feeder.at += take;
        match enqueue_buffer(queue, buffer, 0, std::ptr::null()) {
            0 => {}
            ENQUEUE_DURING_RESET => feeder.refused += 1,
            e => panic!("enqueue: {e}"),
        }
    }

    #[test]
    fn offline_render_decodes_resamples_and_hands_buffers_back() {
        let parsed = audiofile::parse(&wav(22050, 0.5)).unwrap();
        assert_eq!((parsed.format.rate, parsed.format.channels, parsed.format.bits), (22050.0, 1, 16));
        // AudioFile property IDs as Apple has them (DataOffset 'doff', IsOptimized 'optm').
        assert_eq!(audiofile::property(&parsed, four(b"doff")), Some(44i64.to_ne_bytes().to_vec()));
        assert!(audiofile::property(&parsed, four(b"optm")).is_some() && audiofile::property(&parsed, four(b"dofs")).is_none());
        // A WAV claiming 0-bit samples, or a block too small for its channels, is refused.
        let mut broken = wav(22050, 0.1);
        broken[34..36].copy_from_slice(&0u16.to_le_bytes());
        assert!(audiofile::parse(&broken).is_err());
        let mut ima = wav(22050, 0.1);
        ima[20..22].copy_from_slice(&0x11u16.to_le_bytes());
        ima[32..34].copy_from_slice(&2u16.to_le_bytes());
        assert!(audiofile::parse(&ima).is_err());
        let data = parsed.data.clone();
        let feeder = Box::into_raw(Box::new(Feeder { queue: 0, data, at: 0, handed_back: 0, refused: 0 }));
        let mut q = 0;
        assert_eq!(new_output(&parsed.format as *const Asbd as *const u8, refill as usize, feeder as usize, 0, 0, 0, &mut q), 0);
        unsafe { (*feeder).queue = q };
        let out = audiofile::pcm(44100.0, 2, 16, false, false);
        assert_eq!(set_offline_render_format(q, &out as *const Asbd as *const u8, 0), 0);
        let (mut a, mut b, mut target) = (0, 0, 0);
        assert_eq!(allocate_buffer(q, 4096, &mut a), 0);
        assert_eq!(allocate_buffer(q, 4096, &mut b), 0);
        assert_eq!(allocate_buffer(q, 4096 * 4, &mut target), 0);
        assert_eq!(start(q, 0), 0);
        // Primed with zero frames first, as Apple's sample code and FMOD do.
        assert_eq!(offline_render(q, std::ptr::null(), target, 0), 0);
        unsafe {
            fill(&mut *feeder, q, a);
            fill(&mut *feeder, q, b);
        }
        let mut samples: Vec<i16> = Vec::new();
        loop {
            assert_eq!(offline_render(q, std::ptr::null(), target, 4096), 0);
            let (size, data) = unsafe { (*((target + 0x10) as *const u32) as usize, *((target + 8) as *const usize)) };
            if size == 0 {
                break;
            }
            let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, size) };
            samples.extend(bytes.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]])));
        }
        let frames = samples.len() / 2;
        assert!((22000..=22050).contains(&frames), "0.5 s at 44100 Hz: {frames} frames");
        assert!(samples.chunks_exact(2).all(|f| f[0] == f[1]), "mono goes to both sides");
        // 441 Hz keeps its pitch through the rate change: ~220 cycles.
        let crossings = samples.chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        assert!((218..=222).contains(&crossings), "{crossings} cycles");
        let handed_back = unsafe { (*feeder).handed_back };
        assert!(handed_back >= 5, "every used buffer goes back to the game ({handed_back})");
        // Reset hands an enqueued buffer back; enqueueing from that callback is refused.
        unsafe {
            (*feeder).at = 0;
            fill(&mut *feeder, q, a);
        }
        assert_eq!(reset(q), 0);
        unsafe { assert_eq!(((*feeder).handed_back, (*feeder).refused), (handed_back + 1, 1)) };
        // A buffer already taken out for decoding (as the playing thread does)
        // is still handed back by Reset, exactly once.
        unsafe { fill(&mut *feeder, q, a) };
        assert!(super::queue(q).unwrap().lock().pending.pop_front().is_some());
        assert_eq!(reset(q), 0);
        unsafe { assert_eq!((*feeder).handed_back, handed_back + 2) };
        assert_eq!(reset(q), 0);
        unsafe { assert_eq!((*feeder).handed_back, handed_back + 2, "handed back once") };
        // The time-pitch algorithm reads back before it is set (TimeDomain, iOS's default).
        let (mut value, mut size) = (0u32, 4u32);
        assert_eq!(get_property(q, four(b"qtpa"), &mut value as *mut u32 as *mut u8, &mut size), 0);
        assert_eq!(value, four(b"tido"));
        // Stop never reports success while a callback on another thread still runs.
        let queue_ref = super::queue(q).unwrap();
        let (started, done) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let (s, d) = (started.clone(), done.clone());
        let thread = std::thread::spawn(move || {
            deliver(&queue_ref, None, || {
                s.store(true, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(CALLBACK_WAIT + std::time::Duration::from_millis(300));
                d.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        });
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }
        let stopped = stop(q, 1);
        assert!(stopped != 0 || done.load(std::sync::atomic::Ordering::SeqCst), "Stop said done while a callback ran");
        thread.join().unwrap();
        assert_eq!(stop(q, 1), 0);
        // Apple's property IDs: metering is kept and reads back; made-up IDs are refused.
        let on = 1u32;
        assert_eq!(set_property(q, four(b"aqme"), &on as *const u32 as *const u8, 4), 0);
        assert_eq!(get_property(q, four(b"aqme"), &mut value as *mut u32 as *mut u8, &mut size), 0);
        assert_eq!(value, 1);
        assert_eq!(set_property(q, four(b"aqtp"), &on as *const u32 as *const u8, 4), INVALID_PROPERTY);
        // Dispose returns only after a callback running on another thread has finished.
        let queue_ref = super::queue(q).unwrap();
        let (started, done) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let (s, d) = (started.clone(), done.clone());
        let thread = std::thread::spawn(move || {
            deliver(&queue_ref, None, || {
                s.store(true, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(150));
                d.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        });
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert_eq!(dispose(q, 1), 0);
        assert!(done.load(std::sync::atomic::Ordering::SeqCst), "dispose waited for the callback");
        thread.join().unwrap();
        assert_eq!(start(q, 0), PARAMETER, "a disposed queue is gone");
    }
}
