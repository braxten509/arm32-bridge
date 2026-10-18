//! Sound. Games make sound two ways here:
//! - an audio graph (AUGraph): a mixer (3D or multichannel) whose inputs ask
//!   the game for samples through callbacks, feeding the output unit;
//! - AVAudioPlayer for music files (MP3, decoded here).
//! Both are mixed into one stereo stream, played through AAudio on Android.
//! PC tests have no sound device: the stream is pulled in real time and can
//! be saved as a WAV file (IOS_SOUND_FILE).

use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::string;
use crate::objc::runtime::{self, Id, Sel};
use crate::symbols::{leak, Registry};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// ----- the game's audio graph -----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Output,
    Mixer3D,
    MixerMulti,
    Other,
}

fn four_cc(value: u32) -> String {
    value.to_be_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

/// One input of a unit: a mixer bus, or the output unit's own input.
#[derive(Clone, Debug)]
struct Input {
    callback: usize,
    data: usize,
    rate: f64,
    channels: u32,
    bits: u32,
    flags: u32,
    enabled: bool,
    /// Linear volume.
    gain: f32,
    /// -1 (left) .. 1 (right).
    pan: f32,
    /// Playback speed (3D mixer's playback rate).
    speed: f32,
    /// Changes when the sound on this input is replaced, so leftovers of
    /// the previous sound are dropped.
    generation: u64,
}

impl Default for Input {
    fn default() -> Self {
        Input { callback: 0, data: 0, rate: 44100.0, channels: 2, bits: 16, flags: 0xC, enabled: true, gain: 1.0, pan: 0.0, speed: 1.0, generation: 0 }
    }
}

struct Unit {
    kind: Kind,
    description: [u32; 5],
    inputs: BTreeMap<u32, Input>,
    master: f32,
}

#[derive(Default)]
struct Graph {
    units: HashMap<usize, Unit>,
    nodes: HashMap<i32, usize>,
    running: bool,
}

static GRAPH: Mutex<Option<Graph>> = Mutex::new(None);

fn with_graph<R>(f: impl FnOnce(&mut Graph) -> R) -> R {
    let mut guard = GRAPH.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Graph::default))
}

fn with_input<R>(unit: usize, element: u32, f: impl FnOnce(&mut Input) -> R) -> Option<R> {
    with_graph(|g| g.units.get_mut(&unit).map(|u| f(u.inputs.entry(element).or_default())))
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

extern "C" fn new_graph(graph: *mut usize) -> i32 {
    if !graph.is_null() {
        unsafe { *graph = leak([0usize; 8]) };
    }
    0
}

extern "C" fn graph_add_node(_graph: usize, description: *const u32, node: *mut i32) -> i32 {
    static NEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    if node.is_null() || description.is_null() {
        return -50;
    }
    let mut d = [0u32; 5];
    d.copy_from_slice(unsafe { std::slice::from_raw_parts(description, 5) });
    let kind = match (&four_cc(d[0])[..], &four_cc(d[1])[..]) {
        ("auou", _) => Kind::Output,
        ("aumx", "3dem") => Kind::Mixer3D,
        ("aumx", "mcmx") => Kind::MixerMulti,
        _ => Kind::Other,
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let unit = leak([0usize; 4]);
    with_graph(|g| {
        g.units.insert(unit, Unit { kind, description: d, inputs: BTreeMap::new(), master: 1.0 });
        g.nodes.insert(id, unit);
    });
    if kind == Kind::Other {
        crate::log!("audio: a {}/{} unit isn't supported; it stays silent", four_cc(d[0]), four_cc(d[1]));
    }
    unsafe { *node = id };
    0
}

extern "C" fn graph_node_info(_graph: usize, node: i32, description: *mut u32, unit: *mut usize) -> i32 {
    let Some((found, d)) = with_graph(|g| g.nodes.get(&node).map(|&u| (u, g.units[&u].description))) else {
        return -10866;
    };
    if !description.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(d.as_ptr(), description, 5) };
    }
    if !unit.is_null() {
        unsafe { *unit = found };
    }
    0
}

extern "C" fn graph_set_node_input_callback(_graph: usize, node: i32, bus: u32, callback: *const usize) -> i32 {
    let (function, data) = if callback.is_null() { (0, 0) } else { unsafe { (*callback, *callback.add(1)) } };
    let Some(unit) = with_graph(|g| g.nodes.get(&node).copied()) else { return -10866 };
    with_input(unit, bus, |input| {
        input.callback = function;
        input.data = data;
        input.generation = next_generation();
    });
    0
}

extern "C" fn graph_start(_graph: usize) -> i32 {
    with_graph(|g| g.running = true);
    crate::log!("audio: graph started");
    start();
    0
}

extern "C" fn graph_stop(_graph: usize) -> i32 {
    with_graph(|g| g.running = false);
    0
}

extern "C" fn graph_is_running(_graph: usize, running: *mut u8) -> i32 {
    if !running.is_null() {
        unsafe { *running = with_graph(|g| g.running) as u8 };
    }
    0
}

extern "C" fn ok_1(_a: usize) -> i32 {
    0
}

extern "C" fn graph_connect(_graph: usize, _from: i32, _from_output: u32, _to: i32, _to_input: u32) -> i32 {
    0
}

const STREAM_FORMAT: u32 = 8;
const ELEMENT_COUNT: u32 = 11;
const SET_RENDER_CALLBACK: u32 = 23;
const INPUT_SCOPE: u32 = 1;
const OUTPUT_SCOPE: u32 = 2;

/// AudioUnitSetProperty(unit, id, scope, element, data, size).
extern "C" fn unit_set_property(unit: usize, id: u32, scope: u32, element: u32, data: *const u8, size: u32) -> i32 {
    match id {
        STREAM_FORMAT if scope == INPUT_SCOPE && !data.is_null() && size >= 40 => {
            let rate = unsafe { *(data as *const f64) };
            let w = unsafe { std::slice::from_raw_parts(data.add(8) as *const u32, 7) };
            let (format, flags, channels, bits) = (w[0], w[1], w[5], w[6]);
            if four_cc(format) != "lpcm" {
                crate::log!("audio: input format {} isn't supported", four_cc(format));
                return -10868;
            }
            with_input(unit, element, |input| {
                input.rate = if rate > 0.0 { rate } else { 44100.0 };
                input.flags = flags;
                input.channels = channels.max(1);
                input.bits = bits;
                input.generation = next_generation();
            });
            0
        }
        SET_RENDER_CALLBACK if !data.is_null() && size >= 16 => {
            let (function, refcon) = unsafe { (*(data as *const usize), *(data as *const usize).add(1)) };
            with_input(unit, element, |input| {
                input.callback = function;
                input.data = refcon;
                input.generation = next_generation();
            });
            0
        }
        _ => 0,
    }
}

/// AudioUnitGetProperty(unit, id, scope, element, data, *size).
extern "C" fn unit_get_property(unit: usize, id: u32, scope: u32, element: u32, data: *mut u8, size: *mut u32) -> i32 {
    if data.is_null() || size.is_null() {
        return -50;
    }
    match id {
        STREAM_FORMAT if unsafe { *size } >= 40 => {
            let input = if scope == INPUT_SCOPE { with_input(unit, element, |i| i.clone()).unwrap_or_default() } else { Input { bits: 32, flags: 0x9, ..Input::default() } };
            let bytes_per_sample = input.bits / 8;
            let non_interleaved = input.flags & 0x20 != 0;
            let frame = if non_interleaved { bytes_per_sample } else { bytes_per_sample * input.channels };
            unsafe {
                *(data as *mut f64) = input.rate;
                let w = std::slice::from_raw_parts_mut(data.add(8) as *mut u32, 8);
                w.copy_from_slice(&[u32::from_be_bytes(*b"lpcm"), input.flags, frame, 1, frame, input.channels, input.bits, 0]);
                *size = 40;
            }
            0
        }
        ELEMENT_COUNT if unsafe { *size } >= 4 => {
            unsafe { *(data as *mut u32) = with_graph(|g| g.units.get(&unit).map(|u| u.inputs.len().max(1) as u32)).unwrap_or(1) };
            0
        }
        _ => -10879,
    }
}

/// AudioUnitSetParameter(unit, id, scope, element, value, frames).
extern "C" fn unit_set_parameter(unit: usize, id: u32, scope: u32, element: u32, value: f32, _frames: u32) -> i32 {
    let Some(kind) = with_graph(|g| g.units.get(&unit).map(|u| u.kind)) else { return -10879 };
    let db = |v: f32| 10f32.powf(v / 20.0);
    if scope == OUTPUT_SCOPE {
        let master = match (kind, id) {
            (Kind::Mixer3D, 3) => Some(db(value)),
            (Kind::MixerMulti, 0) => Some(value),
            _ => None,
        };
        if let Some(master) = master {
            with_graph(|g| g.units.get_mut(&unit).map(|u| u.master = master));
        }
        return 0;
    }
    with_input(unit, element, |input| match (kind, id) {
        // 3D mixer: azimuth (degrees), gain (dB), playback rate, enable.
        (Kind::Mixer3D, 0) => input.pan = value.to_radians().sin().clamp(-1.0, 1.0),
        (Kind::Mixer3D, 3) => input.gain = db(value),
        (Kind::Mixer3D, 4) => input.speed = value.clamp(0.25, 4.0),
        (Kind::Mixer3D, 5) | (Kind::MixerMulti, 1) => {
            let enabled = value != 0.0;
            if enabled && !input.enabled {
                input.generation = next_generation();
            }
            input.enabled = enabled;
        }
        // Multichannel mixer: volume (linear), pan.
        (Kind::MixerMulti, 0) => input.gain = value,
        (Kind::MixerMulti, 2) => input.pan = value.clamp(-1.0, 1.0),
        _ => {}
    });
    0
}

/// AudioUnitGetParameter(unit, id, scope, element, *value).
extern "C" fn unit_get_parameter(unit: usize, id: u32, _scope: u32, element: u32, value: *mut f32) -> i32 {
    if value.is_null() {
        return -50;
    }
    let found = with_input(unit, element, |input| match id {
        3 => 20.0 * input.gain.max(1e-6).log10(),
        5 | 1 => input.enabled as u32 as f32,
        _ => 0.0,
    });
    unsafe { *value = found.unwrap_or(0.0) };
    0
}

// ----- pulling samples from the game -----

type RenderCallback = extern "C" fn(usize, *mut u32, *const u8, u32, u32, *mut u8) -> i32;

const OUTPUT_IS_SILENCE: u32 = 1 << 4;

/// Asks one input's callback for `frames` frames; returns them as stereo
/// floats (left, right, left, ...), or None if the game reported an error.
fn pull(input: &Input, bus: u32, frames: usize, sample_time: f64) -> Option<Vec<f32>> {
    let callback: RenderCallback = unsafe { std::mem::transmute(input.callback) };
    let channels = input.channels.max(1) as usize;
    let bytes = (input.bits / 8).max(1) as usize;
    let separate = input.flags & 0x20 != 0;
    let buffers = if separate { channels } else { 1 };
    let per_buffer = if separate { frames * bytes } else { frames * bytes * channels };
    let mut memory = vec![0u8; per_buffer * buffers];
    // AudioBufferList: count, then {channels, byte size, data} per buffer.
    let mut list = vec![0u8; 8 + 16 * buffers];
    unsafe {
        *(list.as_mut_ptr() as *mut u32) = buffers as u32;
        for b in 0..buffers {
            let entry = list.as_mut_ptr().add(8 + 16 * b);
            *(entry as *mut u32) = if separate { 1 } else { channels as u32 };
            *(entry.add(4) as *mut u32) = per_buffer as u32;
            *(entry.add(8) as *mut usize) = memory.as_mut_ptr().add(b * per_buffer) as usize;
        }
    }
    // AudioTimeStamp: sample time valid.
    let mut stamp = [0u8; 64];
    stamp[..8].copy_from_slice(&sample_time.to_ne_bytes());
    stamp[56..60].copy_from_slice(&1u32.to_ne_bytes());
    let mut action = 0u32;
    if callback(input.data, &mut action, stamp.as_ptr(), bus, frames as u32, list.as_mut_ptr()) != 0 {
        return None;
    }
    let mut out = vec![0f32; frames * 2];
    if action & OUTPUT_IS_SILENCE != 0 {
        return Some(out);
    }
    let float = input.flags & 1 != 0;
    let fraction = (input.flags >> 7) & 0x3F;
    let read = |pointer: *const u8, index: usize| -> f32 {
        unsafe {
            match (float, bytes) {
                (true, 4) => *(pointer as *const f32).add(index),
                (true, 8) => *(pointer as *const f64).add(index) as f32,
                (false, 2) => *(pointer as *const i16).add(index) as f32 / 32768.0,
                (false, 4) if fraction > 0 => *(pointer as *const i32).add(index) as f32 / (1u64 << fraction) as f32,
                (false, 4) => *(pointer as *const i32).add(index) as f32 / 2147483648.0,
                (false, 1) => (*pointer.add(index) as f32 - 128.0) / 128.0,
                _ => 0.0,
            }
        }
    };
    // The game may have pointed the buffers at its own memory.
    let pointers: Vec<*const u8> = (0..buffers).map(|b| unsafe { *(list.as_ptr().add(8 + 16 * b + 8) as *const usize) as *const u8 }).collect();
    if pointers.iter().any(|p| p.is_null()) {
        return Some(out);
    }
    for frame in 0..frames {
        let sample = |channel: usize| if separate { read(pointers[channel], frame) } else { read(pointers[0], frame * channels + channel) };
        let left = sample(0);
        let right = if channels > 1 { sample(1) } else { left };
        out[frame * 2] = left;
        out[frame * 2 + 1] = right;
    }
    Some(out)
}

/// A source's samples waiting to be played, and where playback is in them.
#[derive(Default)]
struct Stream {
    pending: Vec<f32>,
    position: f64,
    generation: u64,
    sample_time: f64,
}

impl Stream {
    fn frames(&self) -> usize {
        self.pending.len() / 2
    }

    /// Adds `frames` output frames to `out`, reading at `step` source frames
    /// per output frame (linear interpolation), with left/right volumes.
    fn mix_into(&mut self, out: &mut [f32], frames: usize, step: f64, left: f32, right: f32) {
        for i in 0..frames {
            let at = self.position + i as f64 * step;
            let index = at as usize;
            let part = (at - index as f64) as f32;
            if index + 1 >= self.frames() {
                break;
            }
            let (a, b) = (index * 2, index * 2 + 2);
            out[i * 2] += (self.pending[a] + (self.pending[b] - self.pending[a]) * part) * left;
            out[i * 2 + 1] += (self.pending[a + 1] + (self.pending[b + 1] - self.pending[a + 1]) * part) * right;
        }
        let end = self.position + frames as f64 * step;
        let used = (end as usize).min(self.frames());
        self.pending.drain(..used * 2);
        self.position = end - used as f64;
    }

    /// Source frames needed to produce `frames` output frames.
    fn needed(&self, frames: usize, step: f64) -> usize {
        (self.position + frames as f64 * step) as usize + 2
    }
}

/// Left/right volumes for a pan of -1 .. 1 (equal power, centre = full).
fn pan_gains(pan: f32, volume: f32, stereo_source: bool) -> (f32, f32) {
    if stereo_source || pan == 0.0 {
        return (volume, volume);
    }
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    let boost = std::f32::consts::SQRT_2;
    ((angle.cos() * boost).min(1.0) * volume, (angle.sin() * boost).min(1.0) * volume)
}

/// Kept by the sound thread only.
static STREAMS: Mutex<Option<HashMap<(usize, u32), Stream>>> = Mutex::new(None);

fn mix_graph(out: &mut [f32], frames: usize, out_rate: f64) {
    let sources: Vec<(usize, u32, Input, f32)> = with_graph(|g| {
        if !g.running {
            return Vec::new();
        }
        g.units
            .iter()
            .filter(|(_, u)| matches!(u.kind, Kind::Mixer3D | Kind::MixerMulti | Kind::Output))
            .flat_map(|(&unit, u)| {
                u.inputs.iter().filter(|(_, i)| i.enabled && i.callback != 0 && i.gain > 0.0001).map(move |(&bus, i)| (unit, bus, i.clone(), u.master))
            })
            .collect()
    });
    let mut guard = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    let streams = guard.get_or_insert_with(HashMap::new);
    for (unit, bus, input, master) in sources {
        let stream = streams.entry((unit, bus)).or_default();
        if stream.generation != input.generation {
            *stream = Stream { generation: input.generation, ..Stream::default() };
        }
        let step = input.rate * input.speed as f64 / out_rate;
        while stream.frames() < stream.needed(frames, step) {
            let want = (stream.needed(frames, step) - stream.frames()).clamp(64, 4096);
            match pull(&input, bus, want, stream.sample_time) {
                Some(samples) => {
                    stream.sample_time += want as f64;
                    stream.pending.extend_from_slice(&samples);
                }
                None => break,
            }
        }
        let (left, right) = pan_gains(input.pan, input.gain * master, input.channels > 1);
        stream.mix_into(out, frames, step, left, right);
    }
}

// ----- music: AVAudioPlayer -----

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Where a player's sound comes from.
#[derive(Clone)]
enum Source {
    File(String),
    Bytes(Arc<Vec<u8>>),
}

struct Opened {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
}

fn open(source: &Source) -> Result<(Opened, f64), String> {
    let media: Box<dyn MediaSource> = match source {
        Source::File(path) => Box::new(std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?),
        Source::Bytes(bytes) => Box::new(std::io::Cursor::new(bytes.as_ref().clone())),
    };
    let stream = MediaSourceStream::new(media, Default::default());
    let mut hint = Hint::new();
    if let Source::File(path) = source {
        if let Some(extension) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("not a sound file this layer reads ({e})"))?;
    let reader = probed.format;
    let track = reader.default_track().ok_or("no sound in the file")?;
    let rate = track.codec_params.sample_rate.unwrap_or(44100) as f64;
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("can't decode it ({e})"))?;
    let track = track.id;
    Ok((Opened { reader, decoder, track }, rate))
}

struct Music {
    source: Source,
    opened: Option<Opened>,
    rate: f64,
    stream: Stream,
    playing: bool,
    volume: f32,
    pan: f32,
    /// Times to play again after this one; -1 = forever.
    loops: isize,
    /// Source frames played so far in this pass (for currentTime).
    played: f64,
    duration: Option<f64>,
}

impl Music {
    /// Decodes the next packet into the stream; false at the end.
    fn decode_more(&mut self) -> bool {
        if self.opened.is_none() {
            match open(&self.source) {
                Ok((opened, rate)) => {
                    self.opened = Some(opened);
                    self.rate = rate;
                }
                Err(e) => {
                    crate::log!("music: {e}");
                    return false;
                }
            }
        }
        let opened = self.opened.as_mut().unwrap();
        loop {
            let packet = match opened.reader.next_packet() {
                Ok(packet) => packet,
                Err(_) => return false,
            };
            if packet.track_id() != opened.track {
                continue;
            }
            let Ok(decoded) = opened.decoder.decode(&packet) else { continue };
            let spec = *decoded.spec();
            let channels = spec.channels.count().max(1);
            let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            samples.copy_interleaved_ref(decoded);
            for frame in samples.samples().chunks_exact(channels) {
                let left = frame[0];
                let right = if channels > 1 { frame[1] } else { left };
                self.stream.pending.push(left);
                self.stream.pending.push(right);
            }
            return true;
        }
    }

    fn mix_into(&mut self, out: &mut [f32], frames: usize, out_rate: f64) -> bool {
        let mut finished = false;
        let step = self.rate / out_rate;
        while self.stream.frames() < self.stream.needed(frames, step) {
            if !self.decode_more() {
                if self.loops != 0 && self.opened.is_some() {
                    if self.loops > 0 {
                        self.loops -= 1;
                    }
                    self.opened = None;
                    self.played = 0.0;
                    continue;
                }
                finished = true;
                break;
            }
        }
        let step = self.rate / out_rate;
        let (left, right) = pan_gains(self.pan, self.volume, false);
        let before = self.stream.position + self.stream.frames() as f64;
        self.stream.mix_into(out, frames, step, left, right);
        self.played += before - (self.stream.position + self.stream.frames() as f64);
        if finished && self.stream.frames() < 2 {
            self.playing = false;
            self.opened = None;
            self.stream = Stream::default();
            self.played = 0.0;
            return true;
        }
        false
    }

    fn seek(&mut self, seconds: f64) {
        self.stream = Stream::default();
        self.opened = None;
        self.played = 0.0;
        if seconds <= 0.0 {
            return;
        }
        if let Ok((mut opened, rate)) = open(&self.source) {
            let to = SeekTo::Time { time: symphonia::core::units::Time::from(seconds), track_id: Some(opened.track) };
            if opened.reader.seek(SeekMode::Coarse, to).is_ok() {
                opened.decoder.reset();
                self.played = seconds * rate;
            }
            self.rate = rate;
            self.opened = Some(opened);
        }
    }

    fn duration(&mut self) -> f64 {
        if let Some(d) = self.duration {
            return d;
        }
        let mut total = 0u64;
        let mut rate = 44100.0;
        if let Ok((mut opened, r)) = open(&self.source) {
            rate = r;
            while let Ok(packet) = opened.reader.next_packet() {
                if packet.track_id() == opened.track {
                    total += packet.dur;
                }
            }
        }
        let d = total as f64 / rate;
        self.duration = Some(d);
        d
    }
}

static PLAYERS: Mutex<Option<HashMap<Id, Arc<Mutex<Music>>>>> = Mutex::new(None);

fn music_of(player: Id) -> Option<Arc<Mutex<Music>>> {
    PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&player).cloned()
}

fn with_music<R: Default>(player: Id, f: impl FnOnce(&mut Music) -> R) -> R {
    match music_of(player) {
        Some(music) => f(&mut music.lock().unwrap_or_else(|e| e.into_inner())),
        None => R::default(),
    }
}

fn add_player(player: Id, source: Source, error: *mut Id) -> Id {
    let mut music = Music { source, opened: None, rate: 44100.0, stream: Stream::default(), playing: false, volume: 1.0, pan: 0.0, loops: 0, played: 0.0, duration: None };
    match open(&music.source) {
        Ok((opened, rate)) => {
            music.opened = Some(opened);
            music.rate = rate;
        }
        Err(e) => {
            crate::log!("music: {e}");
            if !error.is_null() {
                unsafe { *error = crate::fw::misc::error("NSOSStatusErrorDomain", 1954115647, &e) };
            }
            return 0;
        }
    }
    PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(player, Arc::new(Mutex::new(music)));
    player
}

extern "C" fn player_init_with_url(this: Id, _: Sel, url: Id, error: *mut Id) -> Id {
    let path = string::get(runtime::send(url, "path", &[]));
    property_set(this, "url", url as u64);
    add_player(this, Source::File(path), error)
}

extern "C" fn player_init_with_data(this: Id, _: Sel, data: Id, error: *mut Id) -> Id {
    add_player(this, Source::Bytes(Arc::new(crate::fw::data::bytes(data))), error)
}

extern "C" fn player_play(this: Id, _: Sel) -> bool {
    start();
    with_music(this, |m| {
        m.playing = true;
        true
    })
}

extern "C" fn player_pause(this: Id, _: Sel) {
    with_music(this, |m| m.playing = false);
}

extern "C" fn player_prepare(this: Id, _: Sel) -> bool {
    music_of(this).is_some()
}

extern "C" fn player_is_playing(this: Id, _: Sel) -> bool {
    with_music(this, |m| m.playing)
}

extern "C" fn player_set_volume(this: Id, _: Sel, volume: f32) {
    with_music(this, |m| m.volume = volume.clamp(0.0, 1.0));
}

extern "C" fn player_volume(this: Id, _: Sel) -> f32 {
    with_music(this, |m| m.volume)
}

extern "C" fn player_set_pan(this: Id, _: Sel, pan: f32) {
    with_music(this, |m| m.pan = pan.clamp(-1.0, 1.0));
}

extern "C" fn player_pan(this: Id, _: Sel) -> f32 {
    with_music(this, |m| m.pan)
}

extern "C" fn player_set_loops(this: Id, _: Sel, loops: isize) {
    with_music(this, |m| m.loops = loops);
}

extern "C" fn player_loops(this: Id, _: Sel) -> isize {
    with_music(this, |m| m.loops)
}

extern "C" fn player_current_time(this: Id, _: Sel) -> f64 {
    with_music(this, |m| m.played / m.rate)
}

extern "C" fn player_set_current_time(this: Id, _: Sel, seconds: f64) {
    with_music(this, |m| m.seek(seconds));
}

extern "C" fn player_duration(this: Id, _: Sel) -> f64 {
    with_music(this, |m| m.duration())
}

extern "C" fn player_channels(_this: Id, _: Sel) -> usize {
    2
}

extern "C" fn player_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
}

extern "C" fn player_delegate(this: Id, _: Sel) -> Id {
    property_get(this, "delegate").unwrap_or(0) as Id
}

extern "C" fn player_url(this: Id, _: Sel) -> Id {
    property_get(this, "url").unwrap_or(0) as Id
}

extern "C" fn player_dealloc(this: Id, _: Sel) {
    if let Some(players) = PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        players.remove(&this);
    }
}

extern "C" fn player_ignore_bool(_this: Id, _: Sel, _value: bool) {}

/// Tells the player's delegate, on the main thread, that it played to the end.
fn finished(player: Id) {
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let delegate = property_get(player, "delegate").unwrap_or(0) as Id;
        if delegate != 0 && runtime::responds(delegate, "audioPlayerDidFinishPlaying:successfully:") {
            runtime::send(delegate, "audioPlayerDidFinishPlaying:successfully:", &[player, 1]);
        }
    })));
}

fn mix_music(out: &mut [f32], frames: usize, out_rate: f64) {
    let playing: Vec<(Id, Arc<Mutex<Music>>)> = match PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(players) => players.iter().map(|(&id, m)| (id, m.clone())).collect(),
        None => return,
    };
    for (player, music) in playing {
        let mut music = music.lock().unwrap_or_else(|e| e.into_inner());
        if music.playing && music.mix_into(out, frames, out_rate) {
            finished(player);
        }
    }
}

// ----- the output: one stereo stream -----

static OUT_RATE: AtomicU32 = AtomicU32::new(44100);
static STARTED: AtomicBool = AtomicBool::new(false);

/// Fills `out` (stereo, interleaved) with everything that is playing.
pub fn mix(out: &mut [f32], rate: u32) {
    out.fill(0.0);
    let frames = out.len() / 2;
    mix_graph(out, frames, rate as f64);
    mix_music(out, frames, rate as f64);
    for sample in out.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

/// Starts playing sound (once): AAudio on Android, else a steady pull in
/// real time (optionally saved to IOS_SOUND_FILE as a WAV file).
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // A test that saves the sound to a file plays nothing out loud.
    #[cfg(target_os = "android")]
    if std::env::var_os("IOS_SOUND_FILE").is_none() && aaudio::open() {
        return;
    }
    std::thread::Builder::new().name("ios-sound".into()).spawn(silent_device).expect("sound thread");
}

fn silent_device() {
    const FRAMES: usize = 512;
    let rate = OUT_RATE.load(Ordering::Relaxed);
    let mut file = std::env::var("IOS_SOUND_FILE").ok().and_then(|path| match std::fs::File::create(&path) {
        Ok(file) => {
            crate::log!("sound: saving to {path}");
            Some(file)
        }
        Err(e) => {
            crate::log!("sound: can't save to {path}: {e}");
            None
        }
    });
    let mut written = 0u32;
    let mut buffer = vec![0f32; FRAMES * 2];
    let period = std::time::Duration::from_secs_f64(FRAMES as f64 / rate as f64);
    let mut next = std::time::Instant::now();
    loop {
        mix(&mut buffer, rate);
        if let Some(file) = file.as_mut() {
            use std::io::{Seek, SeekFrom, Write};
            let bytes: Vec<u8> = buffer.iter().flat_map(|s| ((s * 32767.0) as i16).to_le_bytes()).collect();
            written += bytes.len() as u32;
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(&wav_header(rate, written));
            let _ = file.seek(SeekFrom::End(0));
            let _ = file.write_all(&bytes);
        }
        next += period;
        let now = std::time::Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

fn wav_header(rate: u32, data_bytes: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * 4).to_le_bytes());
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_bytes.to_le_bytes());
    h
}

#[cfg(target_os = "android")]
mod aaudio {
    //! Android's low-latency sound output, loaded by name.
    use super::{mix, OUT_RATE};
    use std::sync::atomic::Ordering;

    type Builder = usize;
    type Stream = usize;

    struct Api {
        create_builder: extern "C" fn(*mut Builder) -> i32,
        set_channels: extern "C" fn(Builder, i32),
        set_format: extern "C" fn(Builder, i32),
        set_performance: extern "C" fn(Builder, i32),
        set_usage: Option<extern "C" fn(Builder, i32)>,
        set_data_callback: extern "C" fn(Builder, extern "C" fn(Stream, usize, *mut f32, i32) -> i32, usize),
        set_error_callback: extern "C" fn(Builder, extern "C" fn(Stream, usize, i32), usize),
        open_stream: extern "C" fn(Builder, *mut Stream) -> i32,
        delete_builder: extern "C" fn(Builder) -> i32,
        start: extern "C" fn(Stream) -> i32,
        close: extern "C" fn(Stream) -> i32,
        sample_rate: extern "C" fn(Stream) -> i32,
    }

    fn api() -> Option<&'static Api> {
        static API: std::sync::OnceLock<Option<Api>> = std::sync::OnceLock::new();
        API.get_or_init(|| unsafe {
            let library = libc::dlopen(c"libaaudio.so".as_ptr(), libc::RTLD_NOW);
            if library.is_null() {
                return None;
            }
            let find = |name: &std::ffi::CStr| {
                let f = libc::dlsym(library, name.as_ptr());
                (!f.is_null()).then_some(f as usize)
            };
            Some(Api {
                create_builder: std::mem::transmute(find(c"AAudio_createStreamBuilder")?),
                set_channels: std::mem::transmute(find(c"AAudioStreamBuilder_setChannelCount")?),
                set_format: std::mem::transmute(find(c"AAudioStreamBuilder_setFormat")?),
                set_performance: std::mem::transmute(find(c"AAudioStreamBuilder_setPerformanceMode")?),
                set_usage: find(c"AAudioStreamBuilder_setUsage").map(|f| std::mem::transmute(f)),
                set_data_callback: std::mem::transmute(find(c"AAudioStreamBuilder_setDataCallback")?),
                set_error_callback: std::mem::transmute(find(c"AAudioStreamBuilder_setErrorCallback")?),
                open_stream: std::mem::transmute(find(c"AAudioStreamBuilder_openStream")?),
                delete_builder: std::mem::transmute(find(c"AAudioStreamBuilder_delete")?),
                start: std::mem::transmute(find(c"AAudioStream_requestStart")?),
                close: std::mem::transmute(find(c"AAudioStream_close")?),
                sample_rate: std::mem::transmute(find(c"AAudioStream_getSampleRate")?),
            })
        })
        .as_ref()
    }

    extern "C" fn on_data(_stream: Stream, _user: usize, data: *mut f32, frames: i32) -> i32 {
        if !data.is_null() && frames > 0 {
            let out = unsafe { std::slice::from_raw_parts_mut(data, frames as usize * 2) };
            mix(out, OUT_RATE.load(Ordering::Relaxed));
        }
        0
    }

    /// The device changed (headphones in or out): open a new stream, from
    /// another thread as AAudio asks.
    extern "C" fn on_error(stream: Stream, _user: usize, error: i32) {
        crate::log!("sound: output stopped (AAudio {error}); opening it again");
        std::thread::spawn(move || {
            if let Some(api) = api() {
                (api.close)(stream);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            open();
        });
    }

    /// Opens and starts the output; false if this phone can't.
    pub fn open() -> bool {
        let Some(api) = api() else {
            crate::log!("sound: no AAudio on this phone");
            return false;
        };
        let mut builder = 0;
        if (api.create_builder)(&mut builder) != 0 {
            return false;
        }
        (api.set_channels)(builder, 2);
        (api.set_format)(builder, 2); // float
        (api.set_performance)(builder, 12); // low latency
        if let Some(set_usage) = api.set_usage {
            set_usage(builder, 14); // game
        }
        (api.set_data_callback)(builder, on_data, 0);
        (api.set_error_callback)(builder, on_error, 0);
        let mut stream = 0;
        let opened = (api.open_stream)(builder, &mut stream);
        (api.delete_builder)(builder);
        if opened != 0 {
            crate::log!("sound: AAudio would not open (error {opened})");
            return false;
        }
        let rate = (api.sample_rate)(stream);
        if rate > 0 {
            OUT_RATE.store(rate as u32, Ordering::Relaxed);
        }
        if (api.start)(stream) != 0 {
            (api.close)(stream);
            crate::log!("sound: AAudio would not start");
            return false;
        }
        crate::log!("sound: playing through AAudio at {rate} Hz");
        true
    }
}

pub fn register(r: &mut Registry) {
    r.function("NewAUGraph", new_graph as usize);
    r.function("AUGraphAddNode", graph_add_node as usize);
    r.function("AUGraphNodeInfo", graph_node_info as usize);
    r.function("AUGraphOpen", ok_1 as usize);
    r.function("AUGraphInitialize", ok_1 as usize);
    r.function("AUGraphUpdate", ok_1 as usize);
    r.function("AUGraphStart", graph_start as usize);
    r.function("AUGraphStop", graph_stop as usize);
    r.function("AUGraphIsRunning", graph_is_running as usize);
    r.function("AUGraphUninitialize", ok_1 as usize);
    r.function("AUGraphClose", ok_1 as usize);
    r.function("DisposeAUGraph", ok_1 as usize);
    r.function("AUGraphConnectNodeInput", graph_connect as usize);
    r.function("AUGraphSetNodeInputCallback", graph_set_node_input_callback as usize);
    r.function("AudioUnitSetProperty", unit_set_property as usize);
    r.function("AudioUnitGetProperty", unit_get_property as usize);
    r.function("AudioUnitSetParameter", unit_set_parameter as usize);
    r.function("AudioUnitGetParameter", unit_get_parameter as usize);
    r.function("AudioUnitInitialize", ok_1 as usize);
    r.function("AudioOutputUnitStart", graph_start as usize);
    r.function("AudioOutputUnitStop", graph_stop as usize);
    r.function("CAShow", ok_1 as usize);

    let player = define_class("AVAudioPlayer", "NSObject", 8);
    methods(
        player,
        &[
            ("initWithContentsOfURL:error:", player_init_with_url as usize),
            ("initWithData:error:", player_init_with_data as usize),
            ("play", player_play as usize),
            ("prepareToPlay", player_prepare as usize),
            ("pause", player_pause as usize),
            ("stop", player_pause as usize),
            ("isPlaying", player_is_playing as usize),
            ("volume", player_volume as usize),
            ("setVolume:", player_set_volume as usize),
            ("pan", player_pan as usize),
            ("setPan:", player_set_pan as usize),
            ("numberOfLoops", player_loops as usize),
            ("setNumberOfLoops:", player_set_loops as usize),
            ("currentTime", player_current_time as usize),
            ("setCurrentTime:", player_set_current_time as usize),
            ("duration", player_duration as usize),
            ("numberOfChannels", player_channels as usize),
            ("delegate", player_delegate as usize),
            ("setDelegate:", player_set_delegate as usize),
            ("url", player_url as usize),
            ("setMeteringEnabled:", player_ignore_bool as usize),
            ("setEnableRate:", player_ignore_bool as usize),
            ("dealloc", player_dealloc as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A game-style callback: a 441 Hz tone, 16-bit mono.
    extern "C" fn tone(_data: usize, _flags: *mut u32, stamp: *const u8, _bus: u32, frames: u32, list: *mut u8) -> i32 {
        unsafe {
            let start = *(stamp as *const f64);
            let samples = *(list.add(16) as *const *mut i16);
            for i in 0..frames as usize {
                let t = (start + i as f64) / 22050.0;
                *samples.add(i) = ((t * 441.0 * std::f64::consts::TAU).sin() * 16000.0) as i16;
            }
        }
        0
    }

    fn energy(out: &[f32], channel: usize) -> f32 {
        out.iter().skip(channel).step_by(2).map(|s| s * s).sum::<f32>()
    }

    #[test]
    fn mixer_plays_a_game_callback_panned_and_resampled() {
        let description = [u32::from_be_bytes(*b"aumx"), u32::from_be_bytes(*b"3dem"), u32::from_be_bytes(*b"appl"), 0, 0];
        let mut node = 0;
        assert_eq!(graph_add_node(0, description.as_ptr(), &mut node), 0);
        let mut unit = 0;
        graph_node_info(0, node, std::ptr::null_mut(), &mut unit);
        let callback = [tone as usize, 0];
        graph_set_node_input_callback(0, node, 3, callback.as_ptr());
        let mut format = [0u8; 40];
        format[..8].copy_from_slice(&22050f64.to_ne_bytes());
        for (i, v) in [u32::from_be_bytes(*b"lpcm"), 0xC, 2, 1, 2, 1, 16, 0].iter().enumerate() {
            format[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_ne_bytes());
        }
        unit_set_property(unit, STREAM_FORMAT, INPUT_SCOPE, 3, format.as_ptr(), 40);
        unit_set_parameter(unit, 5, INPUT_SCOPE, 3, 1.0, 0);
        unit_set_parameter(unit, 3, INPUT_SCOPE, 3, 0.0, 0);
        with_graph(|g| g.running = true);
        let mut out = vec![0f32; 4410 * 2];
        mix(&mut out, 44100);
        let (left, right) = (energy(&out, 0), energy(&out, 1));
        assert!(left > 100.0 && (left - right).abs() < left * 0.01, "centred tone: {left} {right}");
        // Hard right: the left side goes quiet.
        unit_set_parameter(unit, 0, INPUT_SCOPE, 3, 90.0, 0);
        mix(&mut out, 44100);
        assert!(energy(&out, 0) < energy(&out, 1) * 0.01);
        // 22050 Hz played at 44100: the tone keeps its pitch (441 Hz = 100 output frames a cycle).
        let crossings = out.chunks_exact(2).map(|f| f[1]).collect::<Vec<_>>().windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((43..=45).contains(&crossings), "{crossings} cycles in 0.1 s");
        // Disabled: silence.
        unit_set_parameter(unit, 5, INPUT_SCOPE, 3, 0.0, 0);
        mix(&mut out, 44100);
        assert_eq!(energy(&out, 1), 0.0);
    }

    #[test]
    fn mp3_music_decodes() {
        let Some(path) = std::env::var_os("IOS_TEST_MP3") else { return };
        let (mut opened, rate) = open(&Source::File(path.to_string_lossy().into_owned())).unwrap();
        assert!(rate > 8000.0);
        let packet = opened.reader.next_packet().unwrap();
        assert!(opened.decoder.decode(&packet).is_ok());
    }
}
