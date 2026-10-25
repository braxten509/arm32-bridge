//! Sound. Games make sound two ways here:
//! - an audio graph (AUGraph): a mixer (3D or multichannel) whose inputs ask
//!   the game for samples through callbacks, feeding the output unit;
//! - AVAudioPlayer for music files (MP3, decoded here);
//! - OpenAL sources (fw::openal).
//! All are mixed into one stereo stream, played through AAudio on Android.
//! PC tests have no sound device: the stream is pulled in real time and can
//! be saved as a WAV file (IOS_SOUND_FILE).

use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property_get, property_set, property_set_object};
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

pub(crate) fn four_cc(value: u32) -> String {
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
    /// Bytes from one frame to the next (in one buffer).
    stride: u32,
    enabled: bool,
    /// Linear volume.
    gain: f32,
    /// 3D mixer: direction in degrees (0 ahead, 90 right) and distance.
    azimuth: f32,
    distance: f32,
    /// Multichannel mixer: -1 (left) .. 1 (right).
    pan: f32,
    /// Playback speed (3D mixer's playback rate).
    speed: f32,
    /// Changes when the sound on this input is replaced, so leftovers of
    /// the previous sound are dropped.
    generation: u64,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            callback: 0,
            data: 0,
            rate: 44100.0,
            channels: 2,
            bits: 16,
            flags: 0xC,
            stride: 4,
            enabled: true,
            gain: 1.0,
            azimuth: 0.0,
            distance: 0.0,
            pan: 0.0,
            speed: 1.0,
            generation: 0,
        }
    }
}

impl Input {
    /// Left/right placement (-1 .. 1) and volume, as the mixer kind does it.
    fn placement(&self, kind: Kind) -> (f32, f32) {
        match kind {
            Kind::Mixer3D => {
                // Apple's default "inverse" distance curve, reference distance 1.
                let distance = if self.distance > 1.0 { 1.0 / self.distance } else { 1.0 };
                (self.azimuth.to_radians().sin().clamp(-1.0, 1.0), self.gain * distance)
            }
            Kind::MixerMulti => (self.pan, self.gain),
            _ => (0.0, self.gain),
        }
    }
}

struct Unit {
    kind: Kind,
    graph: usize,
    description: [u32; 5],
    inputs: BTreeMap<u32, Input>,
    master: f32,
    /// The most frames one render call asks an input for (iOS's
    /// kAudioUnitProperty_MaximumFramesPerSlice; games size their own
    /// buffers by it).
    max_frames: u32,
}

/// iOS's default maximum frames per render call.
const DEFAULT_MAX_FRAMES: u32 = 1156;

#[derive(Default)]
struct Graph {
    units: HashMap<usize, Unit>,
    nodes: HashMap<i32, usize>,
    /// Graphs (by their handle) and whether each is running.
    running: HashMap<usize, bool>,
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

/// Held by the sound thread while it calls into the game. Stopping or
/// disposing a graph waits for it, so no callback runs after they return.
static RENDERING: Mutex<()> = Mutex::new(());
static RENDER_THREAD: AtomicU32 = AtomicU32::new(0);

fn wait_for_render() {
    let me = unsafe { libc::gettid() } as u32;
    if RENDER_THREAD.load(Ordering::Relaxed) != me {
        drop(RENDERING.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

extern "C" fn new_graph(graph: *mut usize) -> i32 {
    if graph.is_null() {
        return -50;
    }
    let handle = leak([0usize; 8]);
    with_graph(|g| g.running.insert(handle, false));
    unsafe { *graph = handle };
    0
}

fn kind_of(d: &[u32; 5]) -> Kind {
    match (&four_cc(d[0])[..], &four_cc(d[1])[..]) {
        ("auou", _) => Kind::Output,
        ("aumx", "3dem") => Kind::Mixer3D,
        ("aumx", "mcmx") => Kind::MixerMulti,
        _ => Kind::Other,
    }
}

/// Makes a unit in `graph` (None: the unit is its own graph, as one made by
/// AudioComponentInstanceNew). Returns the unit and its node number.
fn add_unit(graph: Option<usize>, d: [u32; 5]) -> (usize, i32) {
    static NEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    let kind = kind_of(&d);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let unit = leak([0usize; 4]);
    let graph = graph.unwrap_or(unit);
    with_graph(|g| {
        g.units.insert(unit, Unit { kind, graph, description: d, inputs: BTreeMap::new(), master: 1.0, max_frames: DEFAULT_MAX_FRAMES });
        g.nodes.insert(id, unit);
        if graph == unit {
            g.running.insert(unit, false);
        }
    });
    if kind == Kind::Other {
        crate::log!("audio: a {}/{} unit isn't supported; it stays silent", four_cc(d[0]), four_cc(d[1]));
    }
    (unit, id)
}

extern "C" fn graph_add_node(graph: usize, description: *const u32, node: *mut i32) -> i32 {
    if node.is_null() || description.is_null() {
        return -50;
    }
    let mut d = [0u32; 5];
    d.copy_from_slice(unsafe { std::slice::from_raw_parts(description, 5) });
    let (_, id) = add_unit(Some(graph), d);
    unsafe { *node = id };
    0
}

// ----- units on their own: AudioComponent -----

/// The units this layer has: RemoteIO and voice-processing output, and the two mixers.
fn components() -> &'static [usize] {
    static MADE: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
    MADE.get_or_init(|| {
        let appl = u32::from_be_bytes(*b"appl");
        [(b"auou", b"rioc"), (b"auou", b"vpio"), (b"aumx", b"3dem"), (b"aumx", b"mcmx")]
            .iter()
            .map(|(kind, sub)| leak([u32::from_be_bytes(**kind), u32::from_be_bytes(**sub), appl, 0u32, 0u32]))
            .collect()
    })
}

/// AudioComponentFindNext(after, description): zero fields match anything.
extern "C" fn component_find_next(after: usize, description: *const u32) -> usize {
    if description.is_null() {
        return 0;
    }
    let want = unsafe { std::slice::from_raw_parts(description, 3) };
    let list = components();
    let from = if after == 0 { 0 } else { list.iter().position(|&c| c == after).map_or(list.len(), |i| i + 1) };
    for &component in &list[from..] {
        let d = unsafe { &*(component as *const [u32; 5]) };
        if (0..3).all(|i| want[i] == 0 || want[i] == d[i]) {
            return component;
        }
    }
    crate::log!("AudioComponentFindNext: no {}/{} unit here", four_cc(want[0]), four_cc(want[1]));
    0
}

extern "C" fn component_get_description(component: usize, description: *mut u32) -> i32 {
    if component == 0 || description.is_null() || !components().contains(&component) {
        return -50;
    }
    unsafe { std::ptr::copy_nonoverlapping(component as *const u32, description, 5) };
    0
}

/// AudioComponentInstanceNew(component, *unit).
extern "C" fn component_instance_new(component: usize, unit: *mut usize) -> i32 {
    if unit.is_null() || !components().contains(&component) {
        return -50;
    }
    let d = unsafe { *(component as *const [u32; 5]) };
    unsafe { *unit = add_unit(None, d).0 };
    0
}

/// AudioComponentInstanceDispose(unit): no callback runs after this returns.
extern "C" fn component_instance_dispose(unit: usize) -> i32 {
    let graph = with_graph(|g| g.units.get(&unit).map(|u| u.graph));
    match graph {
        Some(graph) if graph == unit => graph_dispose(graph),
        Some(_) => 0,
        None => -50,
    }
}

thread_local! {
    static RENDER_SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// AudioUnitRender(unit, *flags, timeStamp, bus, frames, list). The only
/// sound a unit can be asked for here is input (the microphone), and there
/// is none, as on an iPhone without microphone access: silence.
extern "C" fn unit_render(unit: usize, flags: *mut u32, _stamp: *const u8, bus: u32, frames: u32, list: *mut u8) -> i32 {
    let Some(kind) = with_graph(|g| g.units.get(&unit).map(|u| u.kind)) else { return -10879 };
    if list.is_null() {
        return -50;
    }
    static TOLD: AtomicBool = AtomicBool::new(false);
    if !(kind == Kind::Output && bus == 1) && !TOLD.swap(true, Ordering::Relaxed) {
        crate::log!("AudioUnitRender: rendering a unit's output on demand isn't supported; it gives silence");
    }
    unsafe {
        let count = *(list as *const u32) as usize;
        // Buffers the caller asked the unit to supply: sized all at once, so
        // growing the scratch never moves memory already handed out.
        let wants: Vec<(usize, usize)> = (0..count)
            .filter(|&b| (*(list.add(8 + 16 * b + 8) as *const usize)) == 0)
            .map(|b| {
                let entry = list.add(8 + 16 * b);
                let channels = (*(entry as *const u32)).max(1) as usize;
                (b, (frames as usize * channels * 4).max(*(entry.add(4) as *const u32) as usize))
            })
            .collect();
        if !wants.is_empty() {
            let total: usize = wants.iter().map(|w| w.1).sum();
            let base = RENDER_SCRATCH.with(|s| {
                let mut s = s.borrow_mut();
                if s.len() < total {
                    s.resize(total, 0);
                }
                s.as_mut_ptr()
            });
            let mut at = 0;
            for &(b, want) in &wants {
                let entry = list.add(8 + 16 * b);
                *(entry.add(4) as *mut u32) = want as u32;
                *(entry.add(8) as *mut usize) = base.add(at) as usize;
                at += want;
            }
        }
        for b in 0..count {
            let entry = list.add(8 + 16 * b);
            let data = *(entry.add(8) as *const usize) as *mut u8;
            std::ptr::write_bytes(data, 0, (*(entry.add(4) as *const u32)) as usize);
        }
        if !flags.is_null() {
            *flags |= OUTPUT_IS_SILENCE;
        }
    }
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

extern "C" fn graph_start(graph: usize) -> i32 {
    with_graph(|g| g.running.insert(graph, true));
    crate::log!("audio: graph started");
    start();
    0
}

extern "C" fn graph_stop(graph: usize) -> i32 {
    with_graph(|g| g.running.insert(graph, false));
    wait_for_render();
    0
}

extern "C" fn graph_is_running(graph: usize, running: *mut u8) -> i32 {
    if !running.is_null() {
        unsafe { *running = with_graph(|g| g.running.get(&graph).copied().unwrap_or(false)) as u8 };
    }
    0
}

/// DisposeAUGraph: its units go away; no callback runs after this returns.
extern "C" fn graph_dispose(graph: usize) -> i32 {
    with_graph(|g| {
        g.running.remove(&graph);
        let gone: Vec<usize> = g.units.iter().filter(|(_, u)| u.graph == graph).map(|(&k, _)| k).collect();
        for unit in &gone {
            g.units.remove(unit);
        }
        g.nodes.retain(|_, unit| !gone.contains(unit));
    });
    // Their streams go on the sound thread's next turn (this may be running
    // inside one of its callbacks, which holds the mixer).
    wait_for_render();
    0
}

/// AudioOutputUnitStart/Stop on a unit used without a graph.
extern "C" fn unit_start(unit: usize) -> i32 {
    let graph = with_graph(|g| g.units.get(&unit).map(|u| u.graph)).unwrap_or(0);
    graph_start(graph)
}

extern "C" fn unit_stop(unit: usize) -> i32 {
    let graph = with_graph(|g| g.units.get(&unit).map(|u| u.graph)).unwrap_or(0);
    graph_stop(graph)
}

extern "C" fn ok_1(_a: usize) -> i32 {
    0
}

extern "C" fn graph_connect(_graph: usize, _from: i32, _from_output: u32, _to: i32, _to_input: u32) -> i32 {
    0
}

const STREAM_FORMAT: u32 = 8;
const ELEMENT_COUNT: u32 = 11;
const MAXIMUM_FRAMES_PER_SLICE: u32 = 14;
const SET_RENDER_CALLBACK: u32 = 23;
const INPUT_SCOPE: u32 = 1;
const OUTPUT_SCOPE: u32 = 2;
const FORMAT_NOT_SUPPORTED: i32 = -10868;

const FLAG_FLOAT: u32 = 0x1;
const FLAG_BIG_ENDIAN: u32 = 0x2;
const FLAG_SIGNED: u32 = 0x4;
const FLAG_NON_INTERLEAVED: u32 = 0x20;

/// Linear PCM layouts this layer reads: native-endian float (32/64 bit),
/// signed 16 or 32 bit (32 may be fixed point, like Apple's 8.24), or
/// unsigned 8 bit.
fn readable(flags: u32, bits: u32) -> bool {
    if flags & FLAG_BIG_ENDIAN != 0 {
        return false;
    }
    match (flags & FLAG_FLOAT != 0, flags & FLAG_SIGNED != 0, bits) {
        (true, _, 32 | 64) => true,
        (false, true, 16 | 32) => true,
        (false, false, 8) => true,
        _ => false,
    }
}

/// AudioUnitSetProperty(unit, id, scope, element, data, size).
extern "C" fn unit_set_property(unit: usize, id: u32, scope: u32, element: u32, data: *const u8, size: u32) -> i32 {
    match id {
        STREAM_FORMAT if scope == INPUT_SCOPE && !data.is_null() && size >= 40 => {
            let rate = unsafe { *(data as *const f64) };
            let w = unsafe { std::slice::from_raw_parts(data.add(8) as *const u32, 7) };
            let (format, flags, bytes_per_frame, channels, bits) = (w[0], w[1], w[4], w[5], w[6]);
            if four_cc(format) != "lpcm" || !readable(flags, bits) || channels == 0 || channels as usize > MAX_CHANNELS {
                crate::log!("audio: input format {} flags {flags:#x} {bits} bits isn't supported", four_cc(format));
                return FORMAT_NOT_SUPPORTED;
            }
            let sample = bits / 8;
            let packed = if flags & FLAG_NON_INTERLEAVED != 0 { sample } else { sample * channels };
            with_input(unit, element, |input| {
                input.rate = if rate > 0.0 { rate } else { 44100.0 };
                input.flags = flags;
                input.channels = channels;
                input.bits = bits;
                input.stride = bytes_per_frame.max(packed);
                input.generation = next_generation();
            });
            0
        }
        MAXIMUM_FRAMES_PER_SLICE if !data.is_null() && size >= 4 => {
            let frames = unsafe { *(data as *const u32) }.max(1);
            with_graph(|g| g.units.get_mut(&unit).map(|u| u.max_frames = frames));
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
            let input = if scope == INPUT_SCOPE {
                with_input(unit, element, |i| i.clone()).unwrap_or_default()
            } else {
                Input { bits: 32, flags: FLAG_FLOAT | 0x8, stride: 8, ..Input::default() }
            };
            unsafe {
                *(data as *mut f64) = input.rate;
                let w = std::slice::from_raw_parts_mut(data.add(8) as *mut u32, 8);
                w.copy_from_slice(&[u32::from_be_bytes(*b"lpcm"), input.flags, input.stride, 1, input.stride, input.channels, input.bits, 0]);
                *size = 40;
            }
            0
        }
        ELEMENT_COUNT if unsafe { *size } >= 4 => {
            unsafe { *(data as *mut u32) = with_graph(|g| g.units.get(&unit).map(|u| u.inputs.len().max(1) as u32)).unwrap_or(1) };
            0
        }
        MAXIMUM_FRAMES_PER_SLICE if unsafe { *size } >= 4 => {
            unsafe {
                *(data as *mut u32) = with_graph(|g| g.units.get(&unit).map(|u| u.max_frames)).unwrap_or(DEFAULT_MAX_FRAMES);
                *size = 4;
            }
            0
        }
        _ => {
            crate::log!("AudioUnitGetProperty {id} scope {scope}: not known");
            -10879
        }
    }
}

fn db(value: f32) -> f32 {
    10f32.powf(value / 20.0)
}

fn to_db(value: f32) -> f32 {
    20.0 * value.max(1e-6).log10()
}

/// AudioUnitSetParameter(unit, id, scope, element, value, frames).
extern "C" fn unit_set_parameter(unit: usize, id: u32, scope: u32, element: u32, value: f32, _frames: u32) -> i32 {
    let Some(kind) = with_graph(|g| g.units.get(&unit).map(|u| u.kind)) else { return -10879 };
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
        // 3D mixer: azimuth (degrees), distance, gain (dB), playback rate, enable.
        (Kind::Mixer3D, 0) => input.azimuth = value,
        (Kind::Mixer3D, 2) => input.distance = value.max(0.0),
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
extern "C" fn unit_get_parameter(unit: usize, id: u32, scope: u32, element: u32, value: *mut f32) -> i32 {
    if value.is_null() {
        return -50;
    }
    let Some((kind, master)) = with_graph(|g| g.units.get(&unit).map(|u| (u.kind, u.master))) else { return -10879 };
    let found = if scope == OUTPUT_SCOPE {
        match (kind, id) {
            (Kind::Mixer3D, 3) => to_db(master),
            (Kind::MixerMulti, 0) => master,
            _ => 0.0,
        }
    } else {
        with_input(unit, element, |input| match (kind, id) {
            (Kind::Mixer3D, 0) => input.azimuth,
            (Kind::Mixer3D, 2) => input.distance,
            (Kind::Mixer3D, 3) => to_db(input.gain),
            (Kind::Mixer3D, 4) => input.speed,
            (Kind::Mixer3D, 5) | (Kind::MixerMulti, 1) => input.enabled as u32 as f32,
            (Kind::MixerMulti, 0) => input.gain,
            (Kind::MixerMulti, 2) => input.pan,
            _ => 0.0,
        })
        .unwrap_or(0.0)
    };
    unsafe { *value = found };
    0
}

// ----- pulling samples from the game -----

type RenderCallback = extern "C" fn(usize, *mut u32, *const u8, u32, u32, *mut u8) -> i32;

const OUTPUT_IS_SILENCE: u32 = 1 << 4;
const MAX_CHANNELS: usize = 8;

/// Asks one input's callback for `frames` frames and appends them to `out`
/// as stereo floats (left, right, ...). Anything the game doesn't give
/// (an error, a short buffer) is silence, so timing stays right.
fn pull(input: &Input, bus: u32, frames: usize, sample_time: f64, scratch: &mut Vec<u8>, out: &mut Vec<f32>) {
    let start = out.len();
    out.resize(start + frames * 2, 0.0);
    let callback: RenderCallback = unsafe { std::mem::transmute(input.callback) };
    let channels = input.channels.max(1) as usize;
    let sample = (input.bits / 8).max(1) as usize;
    let separate = input.flags & FLAG_NON_INTERLEAVED != 0;
    let buffers = if separate { channels } else { 1 };
    let stride = if separate { sample } else { (input.stride as usize).max(sample * channels) };
    let per_buffer = frames * stride;
    let list_size = 8 + 16 * buffers;
    scratch.clear();
    scratch.resize(list_size + per_buffer * buffers, 0);
    let (list, memory) = scratch.split_at_mut(list_size);
    // AudioBufferList: count, then {channels, byte size, data} per buffer.
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
    if callback(input.data, &mut action, stamp.as_ptr(), bus, frames as u32, list.as_mut_ptr()) != 0 || action & OUTPUT_IS_SILENCE != 0 {
        return;
    }
    // The game may have pointed the buffers at its own memory and size.
    if unsafe { *(list.as_ptr() as *const u32) } as usize != buffers {
        return;
    }
    let mut pointers = [std::ptr::null::<u8>(); MAX_CHANNELS];
    let mut available = frames;
    for (b, slot) in pointers.iter_mut().enumerate().take(buffers) {
        let entry = unsafe { list.as_ptr().add(8 + 16 * b) };
        let size = unsafe { *(entry.add(4) as *const u32) } as usize;
        let pointer = unsafe { *(entry.add(8) as *const usize) } as *const u8;
        if pointer.is_null() {
            return;
        }
        available = available.min(size / stride);
        *slot = pointer;
    }
    let float = input.flags & FLAG_FLOAT != 0;
    let fraction = (input.flags >> 7) & 0x3F;
    let read = |pointer: *const u8| -> f32 {
        unsafe {
            match (float, sample) {
                (true, 4) => (pointer as *const f32).read_unaligned(),
                (true, 8) => (pointer as *const f64).read_unaligned() as f32,
                (false, 2) => (pointer as *const i16).read_unaligned() as f32 / 32768.0,
                (false, 4) if fraction > 0 => (pointer as *const i32).read_unaligned() as f32 / (1u64 << fraction) as f32,
                (false, 4) => (pointer as *const i32).read_unaligned() as f32 / 2147483648.0,
                (false, 1) => (*pointer as f32 - 128.0) / 128.0,
                _ => 0.0,
            }
        }
    };
    for frame in 0..available {
        let at = |channel: usize| unsafe {
            if separate {
                read(pointers[channel].add(frame * stride))
            } else {
                read(pointers[0].add(frame * stride + channel * sample))
            }
        };
        let left = at(0);
        let right = if channels > 1 { at(1) } else { left };
        out[start + frame * 2] = left;
        out[start + frame * 2 + 1] = right;
    }
}

/// A source's samples waiting to be played, and where playback is in them.
#[derive(Default)]
pub(crate) struct Stream {
    pub(crate) pending: Vec<f32>,
    position: f64,
    generation: u64,
    sample_time: f64,
}

impl Stream {
    pub(crate) fn frames(&self) -> usize {
        self.pending.len() / 2
    }

    /// Source frames needed to produce `frames` output frames.
    fn needed(&self, frames: usize, step: f64) -> usize {
        (self.position + frames as f64 * step) as usize + 2
    }

    /// Adds up to `frames` output frames to `out`, reading at `step` source
    /// frames per output frame (linear interpolation). Returns the source
    /// frames played (never past the samples there are).
    pub(crate) fn mix_into(&mut self, out: &mut [f32], frames: usize, step: f64, left: f32, right: f32) -> f64 {
        let start = self.position;
        let mut made = 0;
        while made < frames {
            let at = self.position + made as f64 * step;
            let index = at as usize;
            if index + 1 >= self.frames() {
                break;
            }
            let part = (at - index as f64) as f32;
            let (a, b) = (index * 2, index * 2 + 2);
            out[made * 2] += (self.pending[a] + (self.pending[b] - self.pending[a]) * part) * left;
            out[made * 2 + 1] += (self.pending[a + 1] + (self.pending[b + 1] - self.pending[a + 1]) * part) * right;
            made += 1;
        }
        let end = self.position + made as f64 * step;
        let used = (end as usize).min(self.frames());
        self.pending.drain(..used * 2);
        self.position = end - used as f64;
        end - start
    }
}

/// Left/right volumes for a placement of -1 .. 1 (equal power, centre = full).
pub(crate) fn pan_gains(pan: f32, volume: f32, stereo_source: bool) -> (f32, f32) {
    if stereo_source || pan == 0.0 {
        return (volume, volume);
    }
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    let boost = std::f32::consts::SQRT_2;
    ((angle.cos() * boost).min(1.0) * volume, (angle.sin() * boost).min(1.0) * volume)
}

/// The sound thread's own state, kept between turns so it doesn't allocate.
#[derive(Default)]
struct Mixer {
    streams: HashMap<(usize, u32), Stream>,
    scratch: Vec<u8>,
    sources: Vec<(usize, u32, Input, Kind, f32, u32)>,
}

static MIXER: Mutex<Option<Mixer>> = Mutex::new(None);

fn mix_graph(out: &mut [f32], frames: usize, out_rate: f64) {
    let _rendering = RENDERING.lock().unwrap_or_else(|e| e.into_inner());
    RENDER_THREAD.store(unsafe { libc::gettid() } as u32, Ordering::Relaxed);
    let mut guard = MIXER.lock().unwrap_or_else(|e| e.into_inner());
    let Mixer { streams, scratch, sources } = guard.get_or_insert_with(Mixer::default);
    sources.clear();
    with_graph(|g| {
        // Streams of disposed units go.
        streams.retain(|(unit, _), _| g.units.contains_key(unit));
        let running = &g.running;
        sources.extend(
            g.units
                .iter()
                .filter(|(_, u)| matches!(u.kind, Kind::Mixer3D | Kind::MixerMulti | Kind::Output) && running.get(&u.graph).copied().unwrap_or(false))
                .flat_map(|(&unit, u)| u.inputs.iter().filter(|(_, i)| i.enabled && i.callback != 0).map(move |(&bus, i)| (unit, bus, i.clone(), u.kind, u.master, u.max_frames))),
        )
    });
    for (unit, bus, input, kind, master, max_frames) in sources.drain(..) {
        let stream = streams.entry((unit, bus)).or_default();
        if stream.generation != input.generation {
            *stream = Stream { generation: input.generation, ..Stream::default() };
        }
        let step = input.rate * input.speed as f64 / out_rate;
        let want = stream.needed(frames, step).saturating_sub(stream.frames());
        // A callback can stop, change or dispose of its unit (or another one
        // in this list): check before every call that it is still wanted.
        let wanted = || {
            with_graph(|g| {
                g.units.get(&unit).is_some_and(|u| {
                    g.running.get(&u.graph).copied().unwrap_or(false)
                        && u.inputs.get(&bus).is_some_and(|i| i.enabled && i.callback == input.callback && i.generation == input.generation)
                })
            })
        };
        if want > 0 {
            // Like iOS, never ask for more than the unit's maximum in one call.
            let mut left = want.max(64);
            while left > 0 {
                if !wanted() {
                    break;
                }
                let chunk = left.min(max_frames as usize);
                pull(&input, bus, chunk, stream.sample_time, scratch, &mut stream.pending);
                stream.sample_time += chunk as f64;
                left -= chunk;
            }
        }
        let (pan, volume) = input.placement(kind);
        let (left, right) = pan_gains(pan, volume * master, input.channels > 1);
        stream.mix_into(out, frames, step, left, right);
    }
    RENDER_THREAD.store(0, Ordering::Relaxed);
}

// ----- music: AVAudioPlayer -----
// Each player decodes on its own worker thread into a short queue; the
// sound thread only takes ready samples (and skips a player for one turn
// rather than wait for it), so file reading and decoding never hold it up.

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

enum Opened {
    /// What Symphonia reads itself (MP3, WAV, MP4/M4A, ADTS).
    Read { reader: Box<dyn FormatReader>, decoder: Box<dyn Decoder>, track: u32 },
    /// Containers only AudioFile reads (CAF, AIFF), decoded as the audio
    /// queue does: `next` packet, `skip` decoded frames still to drop
    /// (an AAC file's priming).
    Packets { file: crate::fw::audiofile::Parsed, decoder: crate::fw::audioqueue::Decoder, next: u64, skip: usize },
}

/// Sound files Symphonia doesn't read here: CAF and AIFF.
fn audiofile_only(head: &[u8]) -> bool {
    head.starts_with(b"caff") || (head.starts_with(b"FORM") && head.get(8..12).is_some_and(|kind| kind == b"AIFF" || kind == b"AIFC"))
}

fn open(source: &Source) -> Result<(Opened, f64), String> {
    let mut head = [0u8; 12];
    match source {
        Source::File(path) => {
            use std::io::Read;
            let _ = std::fs::File::open(path).and_then(|mut file| file.read(&mut head));
        }
        Source::Bytes(bytes) => {
            let n = bytes.len().min(12);
            head[..n].copy_from_slice(&bytes[..n]);
        }
    }
    if audiofile_only(&head) {
        return open_packets(source, 0.0).map(|(opened, rate, _)| (opened, rate));
    }
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
    Ok((Opened::Read { reader, decoder, track }, rate))
}

/// A CAF or AIFF file through AudioFile, placed at `seconds` (at or just
/// before it, on a packet's start): the opened file, the rate its sound
/// decodes at, and the time it starts from.
fn open_packets(source: &Source, seconds: f64) -> Result<(Opened, f64, f64), String> {
    use crate::fw::audioqueue::Decoder as PacketDecoder;
    let parsed = match source {
        Source::File(path) => crate::fw::audiofile::parse_path(path),
        Source::Bytes(bytes) => crate::fw::audiofile::parse(bytes),
    };
    let file = parsed.map_err(|status| format!("not a sound file this layer reads (AudioFile {})", four_cc(status as u32)))?;
    let format = file.format;
    if !PacketDecoder::supported(&format) {
        return Err(format!("can't decode it (format {})", four_cc(format.format)));
    }
    // The rate sound comes out at (HE-AAC's core rate): seen on the first packet.
    let mut first = PacketDecoder::new(&format, &file.cookie)?;
    if let Some(packet) = file.packet(0) {
        first.decode(file.data.get(packet.offset..packet.offset + packet.size as usize).unwrap_or(&[]), packet.frames, &mut Vec::new());
    }
    let rate = first.rate;
    let decoder = PacketDecoder::new(&format, &file.cookie)?;
    // File frames to skip: the priming, then the time asked for.
    let wanted = file.priming as u64 + (seconds.max(0.0) * format.rate) as u64;
    let (mut next, mut at) = (0u64, 0u64);
    while let Some(packet) = file.packet(next) {
        if at + packet.frames as u64 > wanted {
            break;
        }
        at += packet.frames as u64;
        next += 1;
    }
    let skip = (file.priming as u64).saturating_sub(at);
    let actual = at.saturating_sub(file.priming as u64) as f64 / format.rate;
    let skip = (skip as f64 * rate / format.rate) as usize;
    Ok((Opened::Packets { file, decoder, next, skip }, rate, actual))
}

/// Frames decoded here at a time from a file of fixed-size packets.
const JOINED_FRAMES: u64 = 4096;

/// The next packet as stereo floats, or None at the end of the file.
fn decode_packet(opened: &mut Opened) -> Option<Vec<f32>> {
    match opened {
        Opened::Read { reader, decoder, track } => loop {
            let packet = reader.next_packet().ok()?;
            if packet.track_id() != *track {
                continue;
            }
            let Ok(decoded) = decoder.decode(&packet) else { continue };
            let spec = *decoded.spec();
            let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            samples.copy_interleaved_ref(decoded);
            return Some(stereo(samples.samples(), spec.channels.count()));
        },
        Opened::Packets { file, decoder, next, skip } => {
            let first = file.packet(*next)?;
            let mut count = 1;
            if file.packets.is_empty() && decoder.joins_packets() {
                count = (JOINED_FRAMES / first.frames.max(1) as u64).max(1).min(file.packet_count() - *next);
            }
            let size = first.size as usize * count as usize;
            let bytes = file.data.get(first.offset..first.offset + size)?;
            *next += count;
            let mut samples = Vec::new();
            decoder.decode(bytes, first.frames * count as u32, &mut samples);
            let mut out = stereo(&samples, file.format.channels as usize);
            let dropped = (*skip * 2).min(out.len());
            out.drain(..dropped);
            *skip -= dropped / 2;
            Some(out)
        }
    }
}

/// Interleaved samples of `channels` channels as stereo (mono doubled).
fn stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let mut out = Vec::with_capacity(samples.len() / channels * 2);
    for frame in samples.chunks_exact(channels) {
        out.push(frame[0]);
        out.push(if channels > 1 { frame[1] } else { frame[0] });
    }
    out
}

/// `opened` moved to `seconds` (or just before it): the time it plays
/// from, or None when it can't move (it is left as it was).
fn seek_opened(opened: &mut Opened, source: &Source, seconds: f64, rate: f64) -> Option<f64> {
    match opened {
        Opened::Read { reader, decoder, track } => {
            let to = SeekTo::Time { time: symphonia::core::units::Time::from(seconds), track_id: Some(*track) };
            let seeked = reader.seek(SeekMode::Coarse, to).ok()?;
            decoder.reset();
            Some(seeked.actual_ts as f64 / rate)
        }
        Opened::Packets { .. } => {
            let (fresh, _, actual) = open_packets(source, seconds).ok()?;
            *opened = fresh;
            Some(actual)
        }
    }
}

/// Seconds of decoded sound kept ready per player.
const READY_SECONDS: f64 = 0.75;

/// Shared by the game (controls), the worker (fills) and the sound thread (plays).
#[derive(Default)]
struct Queue {
    stream: Stream,
    rate: f64,
    /// Source frames put in / played since the last seek.
    pushed: f64,
    played: f64,
    /// `played` where the pass being heard started, and its time then.
    pass_start: f64,
    pass_seconds: f64,
    /// `pushed` values where a new pass (a loop) begins.
    boundaries: std::collections::VecDeque<f64>,
    /// The file has no more to give (no loops left).
    ended: bool,
    loops: isize,
    loops_left: isize,
    /// Asked to move to this time (seconds).
    seek: Option<f64>,
    /// Changes on every seek; the worker drops work done for an older one.
    epoch: u64,
    quit: bool,
}

struct Player {
    source: Source,
    queue: Mutex<Queue>,
    wake: std::sync::Condvar,
    playing: AtomicBool,
    volume: AtomicU32,
    pan: AtomicU32,
    duration: Mutex<Option<f64>>,
    worker: std::sync::Once,
}

impl Player {
    fn new(source: Source, rate: f64) -> Arc<Player> {
        Arc::new(Player {
            source,
            queue: Mutex::new(Queue { rate, ..Queue::default() }),
            wake: std::sync::Condvar::new(),
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1f32.to_bits()),
            pan: AtomicU32::new(0f32.to_bits()),
            duration: Mutex::new(None),
            worker: std::sync::Once::new(),
        })
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ensure_worker(self: &Arc<Self>) {
        let me = self.clone();
        self.worker.call_once(move || {
            std::thread::Builder::new().name("ios-music".into()).spawn(move || me.work()).expect("music thread");
        });
    }

    fn work(&self) {
        let mut opened: Option<Opened> = None;
        let mut skip = 0usize;
        loop {
            let (epoch, seek) = {
                let mut q = self.queue();
                loop {
                    if q.quit {
                        return;
                    }
                    let hungry = q.seek.is_some() || (!q.ended && (q.stream.frames() as f64) < q.rate.max(8000.0) * READY_SECONDS);
                    if hungry {
                        break;
                    }
                    q = self.wake.wait_timeout(q, std::time::Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
                }
                (q.epoch, q.seek.take())
            };
            if let Some(seconds) = seek {
                opened = None;
                skip = 0;
                let mut actual = 0.0;
                if let Ok((mut fresh, rate)) = open(&self.source) {
                    if seconds > 0.0 {
                        if let Some(at) = seek_opened(&mut fresh, &self.source, seconds, rate) {
                            actual = at;
                            skip = ((seconds - actual).max(0.0) * rate) as usize;
                        }
                    }
                    let mut q = self.queue();
                    if q.epoch == epoch {
                        q.rate = rate;
                        q.pass_seconds = actual + skip as f64 / rate;
                    }
                    opened = Some(fresh);
                }
                continue;
            }
            if opened.is_none() {
                match open(&self.source) {
                    Ok((fresh, rate)) => {
                        self.queue().rate = rate;
                        opened = Some(fresh);
                    }
                    Err(e) => {
                        crate::log!("music: {e}");
                        let mut q = self.queue();
                        if q.epoch == epoch {
                            q.ended = true;
                        }
                        continue;
                    }
                }
            }
            let decoded = decode_packet(opened.as_mut().unwrap());
            let mut q = self.queue();
            if q.epoch != epoch {
                continue;
            }
            match decoded {
                Some(mut samples) => {
                    if skip > 0 {
                        let drop = (skip * 2).min(samples.len());
                        samples.drain(..drop);
                        skip -= drop / 2;
                    }
                    q.pushed += (samples.len() / 2) as f64;
                    q.stream.pending.extend_from_slice(&samples);
                }
                None if q.loops_left != 0 => {
                    if q.loops_left > 0 {
                        q.loops_left -= 1;
                    }
                    let at = q.pushed;
                    q.boundaries.push_back(at);
                    opened = None;
                }
                None => q.ended = true,
            }
        }
    }

    fn current_time(&self) -> f64 {
        let q = self.queue();
        if q.rate <= 0.0 {
            return 0.0;
        }
        q.pass_seconds + (q.played - q.pass_start) / q.rate
    }

    fn seek(&self, seconds: f64) {
        let mut q = self.queue();
        q.epoch += 1;
        q.seek = Some(seconds.max(0.0));
        q.stream = Stream::default();
        q.pushed = 0.0;
        q.played = 0.0;
        q.pass_start = 0.0;
        q.pass_seconds = seconds.max(0.0);
        q.boundaries.clear();
        q.ended = false;
        q.loops_left = q.loops;
        drop(q);
        self.wake.notify_all();
    }

    /// Plays into `out`; true when the sound just reached its end.
    fn mix_into(&self, out: &mut [f32], frames: usize, out_rate: f64) -> bool {
        let Ok(mut q) = self.queue.try_lock() else { return false };
        if q.rate <= 0.0 {
            return false;
        }
        let step = q.rate / out_rate;
        let (left, right) = pan_gains(f32::from_bits(self.pan.load(Ordering::Relaxed)), f32::from_bits(self.volume.load(Ordering::Relaxed)), false);
        let advanced = q.stream.mix_into(out, frames, step, left, right);
        q.played += advanced;
        while let Some(&boundary) = q.boundaries.front() {
            if q.played < boundary {
                break;
            }
            q.boundaries.pop_front();
            q.pass_start = boundary;
            q.pass_seconds = 0.0;
        }
        let low = (q.stream.frames() as f64) < q.rate * READY_SECONDS / 2.0;
        if q.ended && q.stream.frames() < 2 {
            // As on iPhone: stopped, back at the start, loops as set.
            q.epoch += 1;
            q.seek = Some(0.0);
            q.stream = Stream::default();
            q.pushed = 0.0;
            q.played = 0.0;
            q.pass_start = 0.0;
            q.pass_seconds = 0.0;
            q.boundaries.clear();
            q.ended = false;
            q.loops_left = q.loops;
            drop(q);
            self.playing.store(false, Ordering::Relaxed);
            self.wake.notify_all();
            return true;
        }
        drop(q);
        if low {
            self.wake.notify_all();
        }
        false
    }

    fn duration(&self) -> f64 {
        let mut known = self.duration.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(seconds) = *known {
            return seconds;
        }
        let seconds = match open(&self.source) {
            Ok((Opened::Read { mut reader, track, .. }, rate)) => {
                let mut total = 0u64;
                while let Ok(packet) = reader.next_packet() {
                    if packet.track_id() == track {
                        total += packet.dur;
                    }
                }
                total as f64 / rate
            }
            Ok((Opened::Packets { file, .. }, _)) => file.valid_frames() as f64 / file.format.rate,
            Err(_) => 0.0,
        };
        *known = Some(seconds);
        seconds
    }
}

static PLAYERS: Mutex<Option<HashMap<Id, Arc<Player>>>> = Mutex::new(None);

fn player_of(player: Id) -> Option<Arc<Player>> {
    PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&player).cloned()
}

fn add_player(player: Id, source: Source, error: *mut Id) -> Id {
    let rate = match open(&source) {
        Ok((_, rate)) => rate,
        Err(e) => {
            crate::log!("music: {e}");
            if !error.is_null() {
                unsafe { *error = crate::fw::misc::error("NSOSStatusErrorDomain", 1954115647, &e) };
            }
            return 0;
        }
    };
    let made = Player::new(source, rate);
    PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(player, made);
    player
}

extern "C" fn player_init_with_url(this: Id, _: Sel, url: Id, error: *mut Id) -> Id {
    let path = string::get(runtime::send(url, "path", &[]));
    property_set_object(this, "url", url);
    add_player(this, Source::File(path), error)
}

extern "C" fn player_init_with_data(this: Id, _: Sel, data: Id, error: *mut Id) -> Id {
    property_set_object(this, "data", data);
    add_player(this, Source::Bytes(Arc::new(crate::fw::data::bytes(data))), error)
}

extern "C" fn player_play(this: Id, _: Sel) -> bool {
    let Some(player) = player_of(this) else { return false };
    start();
    player.ensure_worker();
    player.playing.store(true, Ordering::Relaxed);
    player.wake.notify_all();
    // The music players report themselves once, so a log says whether a
    // game's background music started and how loud it is set.
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        let volume = f32::from_bits(player.volume.load(Ordering::Relaxed));
        let loops = player.queue().loops;
        crate::log!("music: first player playing (volume {volume}, loops {loops})");
    });
    true
}

extern "C" fn player_pause(this: Id, _: Sel) {
    if let Some(player) = player_of(this) {
        player.playing.store(false, Ordering::Relaxed);
    }
}

extern "C" fn player_prepare(this: Id, _: Sel) -> bool {
    match player_of(this) {
        Some(player) => {
            player.ensure_worker();
            true
        }
        None => false,
    }
}

extern "C" fn player_is_playing(this: Id, _: Sel) -> bool {
    player_of(this).is_some_and(|p| p.playing.load(Ordering::Relaxed))
}

extern "C" fn player_set_volume(this: Id, _: Sel, volume: f32) {
    if let Some(player) = player_of(this) {
        player.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

extern "C" fn player_volume(this: Id, _: Sel) -> f32 {
    player_of(this).map(|p| f32::from_bits(p.volume.load(Ordering::Relaxed))).unwrap_or(0.0)
}

extern "C" fn player_set_pan(this: Id, _: Sel, pan: f32) {
    if let Some(player) = player_of(this) {
        player.pan.store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

extern "C" fn player_pan(this: Id, _: Sel) -> f32 {
    player_of(this).map(|p| f32::from_bits(p.pan.load(Ordering::Relaxed))).unwrap_or(0.0)
}

extern "C" fn player_set_loops(this: Id, _: Sel, loops: isize) {
    if let Some(player) = player_of(this) {
        let mut q = player.queue();
        q.loops = loops;
        q.loops_left = loops;
    }
}

extern "C" fn player_loops(this: Id, _: Sel) -> isize {
    player_of(this).map(|p| p.queue().loops).unwrap_or(0)
}

extern "C" fn player_current_time(this: Id, _: Sel) -> f64 {
    player_of(this).map(|p| p.current_time()).unwrap_or(0.0)
}

extern "C" fn player_set_current_time(this: Id, _: Sel, seconds: f64) {
    if let Some(player) = player_of(this) {
        player.ensure_worker();
        player.seek(seconds);
    }
}

extern "C" fn player_duration(this: Id, _: Sel) -> f64 {
    player_of(this).map(|p| p.duration()).unwrap_or(0.0)
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

extern "C" fn player_data(this: Id, _: Sel) -> Id {
    property_get(this, "data").unwrap_or(0) as Id
}

extern "C" fn player_dealloc(this: Id, _: Sel) {
    property_set_object(this, "url", 0);
    property_set_object(this, "data", 0);
    let removed = PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|players| players.remove(&this));
    if let Some(player) = removed {
        player.queue().quit = true;
        player.wake.notify_all();
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
    let Ok(guard) = PLAYERS.try_lock() else { return };
    let playing: Vec<(Id, Arc<Player>)> = match guard.as_ref() {
        Some(players) => players.iter().filter(|(_, p)| p.playing.load(Ordering::Relaxed)).map(|(&id, p)| (id, p.clone())).collect(),
        None => return,
    };
    drop(guard);
    for (id, player) in playing {
        if player.mix_into(out, frames, out_rate) {
            finished(id);
        }
    }
}

// ----- system sounds (AudioServices) -----
// A sound made from a file plays once per AudioServicesPlaySystemSound.
// System Sound Services plays one sound at a time: a new play stops the one
// playing, whose decoding worker then ends. The phone's own sounds
// (kSystemSoundID_Vibrate and the other built-in IDs) don't exist here:
// they do nothing.

/// Sounds made from files, by SystemSoundID.
static SYSTEM_SOUNDS: Mutex<Option<HashMap<u32, Source>>> = Mutex::new(None);
/// The system sound playing now.
static SOUNDING: Mutex<Option<Arc<Player>>> = Mutex::new(None);
/// The first ID handed out; Apple's start above the built-in ones too.
const FIRST_SYSTEM_SOUND: u32 = 4097;
/// kAudioServicesSystemSoundUnspecifiedError.
const SYSTEM_SOUND_UNSPECIFIED_ERROR: i32 = -1500;

extern "C" fn create_system_sound(url: Id, out: *mut u32) -> i32 {
    if url == 0 || out.is_null() {
        return SYSTEM_SOUND_UNSPECIFIED_ERROR;
    }
    let source = Source::File(string::get(runtime::send(url, "path", &[])));
    if let Err(e) = open(&source) {
        crate::log!("AudioServicesCreateSystemSoundID: {e}");
        return SYSTEM_SOUND_UNSPECIFIED_ERROR;
    }
    let mut sounds = SYSTEM_SOUNDS.lock().unwrap_or_else(|e| e.into_inner());
    let sounds = sounds.get_or_insert_with(HashMap::new);
    let id = (FIRST_SYSTEM_SOUND..).find(|id| !sounds.contains_key(id)).unwrap_or(FIRST_SYSTEM_SOUND);
    sounds.insert(id, source);
    unsafe { *out = id };
    0
}

extern "C" fn dispose_system_sound(id: u32) -> i32 {
    let removed = SYSTEM_SOUNDS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|s| s.remove(&id));
    if removed.is_some() { 0 } else { SYSTEM_SOUND_UNSPECIFIED_ERROR }
}

extern "C" fn play_system_sound(id: u32) {
    let source = SYSTEM_SOUNDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|s| s.get(&id).cloned());
    let Some(source) = source else { return };
    start();
    play_system_source(source);
}

/// Plays `source` as the system sound, stopping the one playing.
fn play_system_source(source: Source) -> Option<Arc<Player>> {
    let rate = match open(&source) {
        Ok((_, rate)) => rate,
        Err(e) => {
            crate::log!("AudioServicesPlaySystemSound: {e}");
            return None;
        }
    };
    let player = Player::new(source, rate);
    player.ensure_worker();
    player.playing.store(true, Ordering::Relaxed);
    let old = SOUNDING.lock().unwrap_or_else(|e| e.into_inner()).replace(player.clone());
    if let Some(old) = old {
        retire(&old);
    }
    Some(player)
}

/// Stops a system sound for good: its worker ends.
fn retire(player: &Player) {
    player.playing.store(false, Ordering::Relaxed);
    player.queue().quit = true;
    player.wake.notify_all();
}

fn mix_system_sounds(out: &mut [f32], frames: usize, out_rate: f64) {
    let Ok(mut sounding) = SOUNDING.try_lock() else { return };
    let Some(player) = sounding.as_ref() else { return };
    if player.mix_into(out, frames, out_rate) {
        // Played to its end.
        retire(player);
        *sounding = None;
    }
}

// ----- the output: one stereo stream -----

static OUT_RATE: AtomicU32 = AtomicU32::new(44100);
static STARTED: AtomicBool = AtomicBool::new(false);

/// The output's sample rate.
pub fn output_rate() -> u32 {
    OUT_RATE.load(Ordering::Relaxed)
}

/// Held while audio queues are mixed.
static QUEUE_MIXING: Mutex<()> = Mutex::new(());

/// Waits for the output to finish mixing audio queues (if it is right now).
pub fn wait_for_mix() {
    drop(QUEUE_MIXING.lock().unwrap_or_else(|e| e.into_inner()));
}

/// Fills `out` (stereo, interleaved) with everything that is playing.
pub fn mix(out: &mut [f32], rate: u32) {
    out.fill(0.0);
    let frames = out.len() / 2;
    mix_graph(out, frames, rate as f64);
    mix_music(out, frames, rate as f64);
    mix_system_sounds(out, frames, rate as f64);
    {
        let _mixing = QUEUE_MIXING.lock().unwrap_or_else(|e| e.into_inner());
        crate::fw::audioqueue::mix(out, frames, rate as f64);
    }
    crate::fw::openal::mix(out, frames, rate as f64);
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

/// The app went to the background (true) or came back: the sound output
/// is closed while away, like iOS stopping an app's sound.
pub fn set_away(away: bool) {
    #[cfg(target_os = "android")]
    if STARTED.load(Ordering::SeqCst) && std::env::var_os("IOS_SOUND_FILE").is_none() {
        if away {
            aaudio::close();
        } else if !aaudio::is_open() {
            aaudio::open();
        }
    }
    let _ = away;
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// The stream playing now (0 while none), and whether a reopen is under way.
    static CURRENT: AtomicUsize = AtomicUsize::new(0);
    static RECOVERING: AtomicBool = AtomicBool::new(false);

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
    /// another thread as AAudio asks. Keeps trying until one opens, so
    /// sound comes back once the device is ready again.
    extern "C" fn on_error(stream: Stream, _user: usize, error: i32) {
        if CURRENT.compare_exchange(stream, 0, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        crate::log!("sound: output stopped (AAudio {error}); opening it again");
        let _ = std::thread::Builder::new().name("ios-sound-reopen".into()).spawn(move || {
            if let Some(api) = api() {
                (api.close)(stream);
            }
            if RECOVERING.swap(true, Ordering::SeqCst) {
                return;
            }
            loop {
                let mut wait = 200;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(wait));
                    if open() {
                        break;
                    }
                    wait = (wait * 2).min(2000);
                }
                RECOVERING.store(false, Ordering::SeqCst);
                // A new stream that already failed while this one finished
                // would have found a reopen running and left it to us.
                if CURRENT.load(Ordering::SeqCst) != 0 || RECOVERING.swap(true, Ordering::SeqCst) {
                    break;
                }
            }
        });
    }

    pub fn is_open() -> bool {
        CURRENT.load(Ordering::SeqCst) != 0
    }

    /// Stops and closes the output (no reopening: that's for device changes).
    pub fn close() {
        let stream = CURRENT.swap(0, Ordering::SeqCst);
        if stream != 0 {
            if let Some(api) = api() {
                (api.close)(stream);
            }
            crate::log!("sound: output closed while the app is away");
        }
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
        CURRENT.store(stream, Ordering::SeqCst);
        if (api.start)(stream) != 0 {
            CURRENT.store(0, Ordering::SeqCst);
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
    r.function("DisposeAUGraph", graph_dispose as usize);
    r.function("AUGraphConnectNodeInput", graph_connect as usize);
    r.function("AUGraphSetNodeInputCallback", graph_set_node_input_callback as usize);
    r.function("AudioUnitSetProperty", unit_set_property as usize);
    r.function("AudioUnitGetProperty", unit_get_property as usize);
    r.function("AudioUnitSetParameter", unit_set_parameter as usize);
    r.function("AudioUnitGetParameter", unit_get_parameter as usize);
    r.function("AudioUnitInitialize", ok_1 as usize);
    r.function("AudioUnitUninitialize", ok_1 as usize);
    r.function("AudioUnitRender", unit_render as usize);
    r.function("AudioComponentFindNext", component_find_next as usize);
    r.function("AudioComponentGetDescription", component_get_description as usize);
    r.function("AudioComponentInstanceNew", component_instance_new as usize);
    r.function("AudioComponentInstanceDispose", component_instance_dispose as usize);
    r.function("AudioOutputUnitStart", unit_start as usize);
    r.function("AudioOutputUnitStop", unit_stop as usize);
    r.function("CAShow", ok_1 as usize);
    r.function("AudioServicesCreateSystemSoundID", create_system_sound as usize);
    r.function("AudioServicesDisposeSystemSoundID", dispose_system_sound as usize);
    r.function("AudioServicesPlaySystemSound", play_system_sound as usize);
    // An alert sound also vibrates on iPhone; here it only plays.
    r.function("AudioServicesPlayAlertSound", play_system_sound as usize);

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
            ("data", player_data as usize),
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
    /// The most frames `tone` was asked for in one call.
    static TONE_MOST_FRAMES: AtomicU32 = AtomicU32::new(0);

    extern "C" fn tone(_data: usize, _flags: *mut u32, stamp: *const u8, _bus: u32, frames: u32, list: *mut u8) -> i32 {
        TONE_MOST_FRAMES.fetch_max(frames, Ordering::Relaxed);
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

    /// Only the audio graphs at 44100 Hz: tests running meanwhile play music,
    /// system sounds and OpenAL into the full mix.
    fn mix_graphs(out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        mix_graph(out, frames, 44100.0);
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
        with_graph(|g| g.running.insert(0, true));
        let mut out = vec![0f32; 4410 * 2];
        mix_graphs(&mut out);
        let (left, right) = (energy(&out, 0), energy(&out, 1));
        assert!(left > 100.0 && (left - right).abs() < left * 0.01, "centred tone: {left} {right}");
        // 0.1 s needs about 2200 source frames, but one call never asks for more than iOS's maximum.
        let most = TONE_MOST_FRAMES.load(Ordering::Relaxed);
        assert!(most > 0 && most <= DEFAULT_MAX_FRAMES, "{most} frames in one call");
        // Hard right: the left side goes quiet.
        unit_set_parameter(unit, 0, INPUT_SCOPE, 3, 90.0, 0);
        mix_graphs(&mut out);
        assert!(energy(&out, 0) < energy(&out, 1) * 0.01);
        // 22050 Hz played at 44100: the tone keeps its pitch (441 Hz = 100 output frames a cycle).
        let crossings = out.chunks_exact(2).map(|f| f[1]).collect::<Vec<_>>().windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((43..=45).contains(&crossings), "{crossings} cycles in 0.1 s");
        // Disabled: silence.
        unit_set_parameter(unit, 5, INPUT_SCOPE, 3, 0.0, 0);
        mix_graphs(&mut out);
        assert_eq!(energy(&out, 1), 0.0);
        // A callback that disposes of its own graph is not called again.
        let mut graph = 0;
        new_graph(&mut graph);
        DISPOSING_GRAPH.store(graph, Ordering::Relaxed);
        let mut node = 0;
        assert_eq!(graph_add_node(graph, description.as_ptr(), &mut node), 0);
        let callback = [dispose_own_graph as usize, 0];
        graph_set_node_input_callback(graph, node, 0, callback.as_ptr());
        // Small slices, so one turn asks it several times.
        let (mut own, slice) = (0, 16u32);
        graph_node_info(graph, node, std::ptr::null_mut(), &mut own);
        unit_set_property(own, MAXIMUM_FRAMES_PER_SLICE, 0, 0, &slice as *const u32 as *const u8, 4);
        graph_start(graph);
        mix_graphs(&mut out);
        assert_eq!(DISPOSING_CALLS.load(Ordering::Relaxed), 1, "called after its graph was disposed");
    }

    static DISPOSING_GRAPH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static DISPOSING_CALLS: AtomicU32 = AtomicU32::new(0);

    extern "C" fn dispose_own_graph(_data: usize, _flags: *mut u32, _stamp: *const u8, _bus: u32, _frames: u32, _list: *mut u8) -> i32 {
        DISPOSING_CALLS.fetch_add(1, Ordering::Relaxed);
        graph_dispose(DISPOSING_GRAPH.load(Ordering::Relaxed));
        0
    }

    /// A WAV file in memory: `seconds` of a quiet tone, 16-bit stereo.
    fn wav(rate: u32, seconds: f64) -> Vec<u8> {
        let frames = (rate as f64 * seconds) as u32;
        let mut file = wav_header(rate, frames * 4);
        for i in 0..frames {
            let v = ((i as f64 / rate as f64 * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            file.extend_from_slice(&v.to_le_bytes());
            file.extend_from_slice(&v.to_le_bytes());
        }
        file
    }

    /// Plays `player` for about `seconds` (at 44100 Hz), waiting for its
    /// worker as needed. True if it reached its end.
    fn play_for(player: &Arc<Player>, seconds: f64) -> bool {
        let mut out = vec![0f32; 512 * 2];
        for _ in 0..(seconds * 44100.0 / 512.0).round() as usize {
            for _ in 0..2000 {
                let q = player.queue();
                if q.ended || q.seek.is_none() && q.stream.frames() >= 1200 {
                    break;
                }
                drop(q);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if player.mix_into(&mut out, 512, 44100.0) {
                return true;
            }
        }
        false
    }

    /// A CAF file in memory (games keep music in their own packs): the same
    /// tone as `wav`, 16-bit big-endian stereo.
    fn caf(rate: u32, seconds: f64) -> Vec<u8> {
        let pcm: Vec<u8> = wav(rate, seconds)[44..].chunks_exact(2).flat_map(|s| [s[1], s[0]]).collect();
        let mut file = b"caff\0\x01\0\0desc".to_vec();
        file.extend_from_slice(&32u64.to_be_bytes());
        file.extend_from_slice(&(rate as f64).to_be_bytes());
        file.extend_from_slice(b"lpcm");
        // Flags (integer, big-endian), bytes and frames a packet, channels, bits.
        for value in [0u32, 4, 1, 2, 16] {
            file.extend_from_slice(&value.to_be_bytes());
        }
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(pcm.len() as u64 + 4).to_be_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&pcm);
        file
    }

    /// A player tells what it was made from: -data for one made from data
    /// (nil from a file), -url for one made from a file (nil from data).
    #[test]
    fn a_player_keeps_the_data_or_url_it_was_made_from() {
        crate::run::setup();
        let make = |selector: &str, from: Id| {
            let player = runtime::send(runtime::class_named("AVAudioPlayer"), "alloc", &[]);
            runtime::send(player, selector, &[from, 0])
        };
        let data = crate::fw::data::make(&wav(22050, 0.1));
        let from_data = make("initWithData:error:", data);
        assert_ne!(from_data, 0);
        assert_eq!(runtime::send(from_data, "data", &[]), data);
        assert_eq!(runtime::send(from_data, "url", &[]), 0);
        let path = std::env::temp_dir().join(format!("gamebridge-player-{}.wav", std::process::id()));
        std::fs::write(&path, wav(22050, 0.1)).unwrap();
        let url = runtime::send(runtime::class_named("NSURL"), "fileURLWithPath:", &[string::make(&path.to_string_lossy())]);
        let from_file = make("initWithContentsOfURL:error:", url);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(runtime::send(from_file, "url", &[]), url);
        assert_eq!(runtime::send(from_file, "data", &[]), 0);
    }

    /// AVAudioPlayer reads what AudioFile reads: a CAF file plays its sound,
    /// knows its length and seeks.
    #[test]
    fn music_in_a_caf_file_plays_and_seeks() {
        let source = Source::Bytes(Arc::new(caf(22050, 1.0)));
        let (_, rate) = open(&source).expect("a CAF file opens");
        assert_eq!(rate, 22050.0);
        let player = Player::new(source, rate);
        assert!((player.duration() - 1.0).abs() < 0.01, "{}", player.duration());
        player.ensure_worker();
        player.playing.store(true, Ordering::Relaxed);
        assert!(!play_for(&player, 0.5));
        let time = player.current_time();
        assert!((time - 0.5).abs() < 0.03, "half way: {time}");
        let mut out = vec![0f32; 64 * 2];
        player.mix_into(&mut out, 64, 22050.0);
        assert!(out.iter().any(|&v| v.abs() > 0.1), "the tone is heard");
        player.seek(0.6);
        assert!(!play_for(&player, 0.2));
        let time = player.current_time();
        assert!((time - 0.8).abs() < 0.03, "after a seek to 0.6 and 0.2 s: {time}");
        assert!(play_for(&player, 0.5), "then it ends");
        player.queue().quit = true;
        player.wake.notify_all();
    }

    #[test]
    fn music_player_keeps_time_loops_and_seeks() {
        let player = Player::new(Source::Bytes(Arc::new(wav(22050, 1.0))), 22050.0);
        player.queue().loops = 1;
        player.queue().loops_left = 1;
        player.ensure_worker();
        player.playing.store(true, Ordering::Relaxed);
        assert!(!play_for(&player, 0.5));
        let time = player.current_time();
        assert!((time - 0.5).abs() < 0.03, "half way: {time}");
        assert_eq!(player.queue().loops, 1, "the loop setting doesn't change while playing");
        // Into the second pass: time starts again from zero.
        assert!(!play_for(&player, 0.75));
        let time = player.current_time();
        assert!((time - 0.25).abs() < 0.03, "second pass: {time}");
        // Then the end: stopped and back at the start.
        assert!(play_for(&player, 1.0));
        assert!(!player.playing.load(Ordering::Relaxed));
        // Seeking reports the place the sound plays from.
        player.seek(0.6);
        player.playing.store(true, Ordering::Relaxed);
        assert!(!play_for(&player, 0.2));
        let time = player.current_time();
        assert!((time - 0.8).abs() < 0.03, "after a seek to 0.6 and 0.2 s: {time}");
        assert!((player.duration() - 1.0).abs() < 0.01);
        player.queue().quit = true;
        player.wake.notify_all();
    }

    /// System Sound Services plays one sound at a time: a new play stops the
    /// one playing, and that one's worker thread ends.
    #[test]
    fn a_new_system_sound_stops_the_one_playing() {
        let first = play_system_source(Source::Bytes(Arc::new(wav(22050, 1.0)))).unwrap();
        let second = play_system_source(Source::Bytes(Arc::new(wav(22050, 1.0)))).unwrap();
        assert!(!first.playing.load(Ordering::Relaxed), "the first sound stopped");
        assert!(second.playing.load(Ordering::Relaxed));
        assert!(SOUNDING.lock().unwrap().as_ref().is_some_and(|p| Arc::ptr_eq(p, &second)), "only the new one is playing");
        // The worker holds its own reference until it ends.
        for _ in 0..250 {
            if Arc::strong_count(&first) == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(Arc::strong_count(&first), 1, "the first sound's worker ended");
        retire(&second);
        *SOUNDING.lock().unwrap() = None;
    }

    #[test]
    fn mp3_music_decodes() {
        let Some(path) = std::env::var_os("IOS_TEST_MP3") else { return };
        let (Opened::Read { mut reader, mut decoder, .. }, rate) = open(&Source::File(path.to_string_lossy().into_owned())).unwrap() else {
            panic!("an MP3 file is read by Symphonia")
        };
        assert!(rate > 8000.0);
        let packet = reader.next_packet().unwrap();
        assert!(decoder.decode(&packet).is_ok());
    }
}
