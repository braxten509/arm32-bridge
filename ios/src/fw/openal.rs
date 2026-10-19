//! OpenAL 1.1 (iOS's OpenAL.framework): sources play buffers of samples,
//! placed around a listener. There is one device and one context; every
//! playing source is mixed into the same stereo stream as the rest of the
//! game's sound (fw::sound), so it pauses with it while the app is away.
//! Apple's extensions alBufferDataStatic and alcMacOSXMixerOutputRate are
//! there too (games ask for them by name).

use crate::symbols::{leak, leak_cstr, Registry};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// Errors
const AL_NO_ERROR: i32 = 0;
const AL_INVALID_NAME: i32 = 0xA001;
const AL_INVALID_ENUM: i32 = 0xA002;
const AL_INVALID_VALUE: i32 = 0xA003;
const AL_INVALID_OPERATION: i32 = 0xA004;
// Source and listener properties
const AL_SOURCE_RELATIVE: i32 = 0x202;
const AL_CONE_INNER_ANGLE: i32 = 0x1001;
const AL_CONE_OUTER_ANGLE: i32 = 0x1002;
const AL_PITCH: i32 = 0x1003;
const AL_POSITION: i32 = 0x1004;
const AL_DIRECTION: i32 = 0x1005;
const AL_VELOCITY: i32 = 0x1006;
const AL_LOOPING: i32 = 0x1007;
const AL_BUFFER: i32 = 0x1009;
const AL_GAIN: i32 = 0x100A;
const AL_MIN_GAIN: i32 = 0x100D;
const AL_MAX_GAIN: i32 = 0x100E;
const AL_ORIENTATION: i32 = 0x100F;
const AL_SOURCE_STATE: i32 = 0x1010;
const AL_INITIAL: i32 = 0x1011;
const AL_PLAYING: i32 = 0x1012;
const AL_PAUSED: i32 = 0x1013;
const AL_STOPPED: i32 = 0x1014;
const AL_BUFFERS_QUEUED: i32 = 0x1015;
const AL_BUFFERS_PROCESSED: i32 = 0x1016;
const AL_REFERENCE_DISTANCE: i32 = 0x1020;
const AL_ROLLOFF_FACTOR: i32 = 0x1021;
const AL_CONE_OUTER_GAIN: i32 = 0x1022;
const AL_MAX_DISTANCE: i32 = 0x1023;
const AL_SEC_OFFSET: i32 = 0x1024;
const AL_SAMPLE_OFFSET: i32 = 0x1025;
const AL_BYTE_OFFSET: i32 = 0x1026;
const AL_SOURCE_TYPE: i32 = 0x1027;
const AL_STATIC: i32 = 0x1028;
const AL_STREAMING: i32 = 0x1029;
const AL_UNDETERMINED: i32 = 0x1030;
// Buffer formats and properties
const AL_FORMAT_MONO8: i32 = 0x1100;
const AL_FORMAT_MONO16: i32 = 0x1101;
const AL_FORMAT_STEREO8: i32 = 0x1102;
const AL_FORMAT_STEREO16: i32 = 0x1103;
const AL_FREQUENCY: i32 = 0x2001;
const AL_BITS: i32 = 0x2002;
const AL_CHANNELS: i32 = 0x2003;
const AL_SIZE: i32 = 0x2004;
// Strings and global state
const AL_VENDOR: i32 = 0xB001;
const AL_VERSION: i32 = 0xB002;
const AL_RENDERER: i32 = 0xB003;
const AL_EXTENSIONS: i32 = 0xB004;
const AL_DOPPLER_FACTOR: i32 = 0xC000;
const AL_SPEED_OF_SOUND: i32 = 0xC003;
const AL_DISTANCE_MODEL: i32 = 0xD000;
const AL_INVERSE_DISTANCE: i32 = 0xD001;
const AL_INVERSE_DISTANCE_CLAMPED: i32 = 0xD002;
const AL_LINEAR_DISTANCE: i32 = 0xD003;
const AL_LINEAR_DISTANCE_CLAMPED: i32 = 0xD004;
const AL_EXPONENT_DISTANCE: i32 = 0xD005;
const AL_EXPONENT_DISTANCE_CLAMPED: i32 = 0xD006;
// ALC
const ALC_MAJOR_VERSION: i32 = 0x1000;
const ALC_MINOR_VERSION: i32 = 0x1001;
const ALC_ATTRIBUTES_SIZE: i32 = 0x1002;
const ALC_ALL_ATTRIBUTES: i32 = 0x1003;
const ALC_DEFAULT_DEVICE_SPECIFIER: i32 = 0x1004;
const ALC_DEVICE_SPECIFIER: i32 = 0x1005;
const ALC_EXTENSIONS: i32 = 0x1006;
const ALC_FREQUENCY: i32 = 0x1007;
const ALC_INVALID_DEVICE: i32 = 0xA001;
const ALC_INVALID_ENUM: i32 = 0xA003;

/// Samples of one buffer, as floats (interleaved when stereo).
struct Samples {
    data: Vec<f32>,
    channels: usize,
    rate: f64,
    bits: i32,
}

impl Samples {
    fn frames(&self) -> usize {
        self.data.len() / self.channels
    }
}

#[derive(Clone)]
struct Source {
    state: i32,
    gain: f32,
    min_gain: f32,
    max_gain: f32,
    pitch: f32,
    looping: bool,
    relative: bool,
    position: [f32; 3],
    velocity: [f32; 3],
    direction: [f32; 3],
    reference_distance: f32,
    rolloff: f32,
    max_distance: f32,
    cone: [f32; 3],
    source_type: i32,
    /// Buffers in play order; `current` is the one playing, the ones before
    /// it are processed.
    queue: VecDeque<u32>,
    current: usize,
    /// Position in the current buffer, in its frames.
    offset: f64,
}

impl Default for Source {
    fn default() -> Self {
        Source {
            state: AL_INITIAL,
            gain: 1.0,
            min_gain: 0.0,
            max_gain: 1.0,
            pitch: 1.0,
            looping: false,
            relative: false,
            position: [0.0; 3],
            velocity: [0.0; 3],
            direction: [0.0; 3],
            reference_distance: 1.0,
            rolloff: 1.0,
            max_distance: f32::MAX,
            cone: [360.0, 360.0, 0.0],
            source_type: AL_UNDETERMINED,
            queue: VecDeque::new(),
            current: 0,
            offset: 0.0,
        }
    }
}

struct Listener {
    position: [f32; 3],
    velocity: [f32; 3],
    at: [f32; 3],
    up: [f32; 3],
    gain: f32,
}

struct State {
    buffers: HashMap<u32, Arc<Samples>>,
    sources: HashMap<u32, Source>,
    listener: Listener,
    distance_model: i32,
    doppler: f32,
    speed_of_sound: f32,
    next_name: u32,
    error: i32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.get_or_insert_with(|| State {
        buffers: HashMap::new(),
        sources: HashMap::new(),
        listener: Listener { position: [0.0; 3], velocity: [0.0; 3], at: [0.0, 0.0, -1.0], up: [0.0, 1.0, 0.0], gain: 1.0 },
        distance_model: AL_INVERSE_DISTANCE_CLAMPED,
        doppler: 1.0,
        speed_of_sound: 343.3,
        next_name: 1,
        error: AL_NO_ERROR,
    });
    f(state)
}

impl State {
    fn fail(&mut self, error: i32) {
        if self.error == AL_NO_ERROR {
            self.error = error;
        }
    }

    fn new_name(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        name
    }
}

/// Runs `f` on a source, or records AL_INVALID_NAME.
fn with_source(name: u32, f: impl FnOnce(&mut Source, &HashMap<u32, Arc<Samples>>)) {
    with(|state| match state.sources.get_mut(&name) {
        Some(source) => f(source, &state.buffers),
        None => state.fail(AL_INVALID_NAME),
    })
}

// ----- buffers -----

extern "C" fn al_gen_buffers(count: i32, out: *mut u32) {
    if count < 0 || out.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        for index in 0..count as usize {
            let name = state.new_name();
            state.buffers.insert(name, Arc::new(Samples { data: Vec::new(), channels: 1, rate: 44100.0, bits: 16 }));
            unsafe { *out.add(index) = name };
        }
    })
}

extern "C" fn al_delete_buffers(count: i32, names: *const u32) {
    if count < 0 || (count > 0 && names.is_null()) {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        for index in 0..count as usize {
            let name = unsafe { *names.add(index) };
            if name != 0 && state.buffers.remove(&name).is_none() {
                state.fail(AL_INVALID_NAME);
            }
        }
    })
}

extern "C" fn al_is_buffer(name: u32) -> u8 {
    with(|state| (name == 0 || state.buffers.contains_key(&name)) as u8)
}

/// Turns the game's samples into floats (done before taking the lock, so
/// the sound thread never waits on a big conversion).
fn convert(format: i32, data: *const u8, size: i32, rate: i32) -> Result<Samples, i32> {
    let (channels, bits) = match format {
        AL_FORMAT_MONO8 => (1, 8),
        AL_FORMAT_MONO16 => (1, 16),
        AL_FORMAT_STEREO8 => (2, 8),
        AL_FORMAT_STEREO16 => (2, 16),
        _ => return Err(AL_INVALID_ENUM),
    };
    if size < 0 || rate <= 0 || (size > 0 && data.is_null()) {
        return Err(AL_INVALID_VALUE);
    }
    let bytes = if size == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data, size as usize) } };
    let samples: Vec<f32> = if bits == 8 {
        bytes.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect()
    } else {
        bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0).collect()
    };
    let frames = samples.len() / channels;
    let mut samples = samples;
    samples.truncate(frames * channels);
    Ok(Samples { data: samples, channels, rate: rate as f64, bits })
}

extern "C" fn al_buffer_data(buffer: u32, format: i32, data: *const u8, size: i32, rate: i32) {
    let converted = convert(format, data, size, rate);
    with(|state| {
        if !state.buffers.contains_key(&buffer) {
            return state.fail(AL_INVALID_NAME);
        }
        // A buffer still queued on a playing or paused source can't change.
        let in_use = state.sources.values().any(|s| (s.state == AL_PLAYING || s.state == AL_PAUSED) && s.queue.contains(&buffer));
        if in_use {
            return state.fail(AL_INVALID_OPERATION);
        }
        match converted {
            Ok(samples) => {
                state.buffers.insert(buffer, Arc::new(samples));
            }
            Err(error) => state.fail(error),
        }
    })
}

/// Apple's alBufferDataStatic: the game keeps the memory; here the samples
/// are copied like alBufferData (they don't change while queued).
extern "C" fn al_buffer_data_static(buffer: u32, format: i32, data: *const u8, size: i32, rate: i32) {
    al_buffer_data(buffer, format, data, size, rate)
}

fn buffer_value(state: &mut State, buffer: u32, parameter: i32) -> Option<i32> {
    let Some(samples) = state.buffers.get(&buffer) else {
        state.fail(AL_INVALID_NAME);
        return None;
    };
    let value = match parameter {
        AL_FREQUENCY => samples.rate as i32,
        AL_BITS => samples.bits,
        AL_CHANNELS => samples.channels as i32,
        AL_SIZE => (samples.data.len() * samples.bits as usize / 8) as i32,
        _ => {
            state.fail(AL_INVALID_ENUM);
            return None;
        }
    };
    Some(value)
}

extern "C" fn al_get_bufferi(buffer: u32, parameter: i32, out: *mut i32) {
    with(|state| {
        if let Some(value) = buffer_value(state, buffer, parameter) {
            if !out.is_null() {
                unsafe { *out = value };
            }
        }
    })
}

extern "C" fn al_get_bufferf(buffer: u32, parameter: i32, out: *mut f32) {
    with(|state| {
        if let Some(value) = buffer_value(state, buffer, parameter) {
            if !out.is_null() {
                unsafe { *out = value as f32 };
            }
        }
    })
}

// ----- sources -----

extern "C" fn al_gen_sources(count: i32, out: *mut u32) {
    if count < 0 || out.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        for index in 0..count as usize {
            let name = state.new_name();
            state.sources.insert(name, Source::default());
            unsafe { *out.add(index) = name };
        }
    })
}

extern "C" fn al_delete_sources(count: i32, names: *const u32) {
    if count < 0 || (count > 0 && names.is_null()) {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        for index in 0..count as usize {
            let name = unsafe { *names.add(index) };
            if state.sources.remove(&name).is_none() {
                state.fail(AL_INVALID_NAME);
            }
        }
    })
}

extern "C" fn al_is_source(name: u32) -> u8 {
    with(|state| state.sources.contains_key(&name) as u8)
}

fn set_float(source: &mut Source, parameter: i32, value: f32) -> Result<(), i32> {
    let check = |ok: bool| if ok { Ok(()) } else { Err(AL_INVALID_VALUE) };
    match parameter {
        AL_GAIN => check(value >= 0.0).map(|_| source.gain = value),
        AL_MIN_GAIN => check((0.0..=1.0).contains(&value)).map(|_| source.min_gain = value),
        AL_MAX_GAIN => check((0.0..=1.0).contains(&value)).map(|_| source.max_gain = value),
        AL_PITCH => check(value > 0.0).map(|_| source.pitch = value),
        AL_REFERENCE_DISTANCE => check(value >= 0.0).map(|_| source.reference_distance = value),
        AL_ROLLOFF_FACTOR => check(value >= 0.0).map(|_| source.rolloff = value),
        AL_MAX_DISTANCE => check(value >= 0.0).map(|_| source.max_distance = value),
        AL_CONE_INNER_ANGLE => Ok(source.cone[0] = value),
        AL_CONE_OUTER_ANGLE => Ok(source.cone[1] = value),
        AL_CONE_OUTER_GAIN => Ok(source.cone[2] = value),
        AL_SEC_OFFSET | AL_SAMPLE_OFFSET | AL_BYTE_OFFSET => Err(AL_INVALID_OPERATION),
        _ => Err(AL_INVALID_ENUM),
    }
}

/// Seeks a source to an offset counted over its whole queue.
fn seek(source: &mut Source, buffers: &HashMap<u32, Arc<Samples>>, parameter: i32, value: f64) -> Result<(), i32> {
    let mut left = value.max(0.0);
    for (index, name) in source.queue.iter().enumerate() {
        let Some(samples) = buffers.get(name) else { continue };
        let frames = match parameter {
            AL_SEC_OFFSET => left * samples.rate,
            AL_SAMPLE_OFFSET => left,
            _ => left / (samples.channels * samples.bits as usize / 8) as f64,
        };
        if frames < samples.frames() as f64 {
            source.current = index;
            source.offset = frames;
            return Ok(());
        }
        left -= match parameter {
            AL_SEC_OFFSET => samples.frames() as f64 / samples.rate,
            AL_SAMPLE_OFFSET => samples.frames() as f64,
            _ => (samples.data.len() * samples.bits as usize / 8) as f64,
        };
    }
    Err(AL_INVALID_VALUE)
}

extern "C" fn al_sourcef(name: u32, parameter: i32, value: f32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, buffers| {
        let result = match parameter {
            AL_SEC_OFFSET | AL_SAMPLE_OFFSET | AL_BYTE_OFFSET => seek(source, buffers, parameter, value as f64),
            _ => set_float(source, parameter, value),
        };
        if let Err(e) = result {
            error = e;
        }
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

fn set_vector(source: &mut Source, parameter: i32, value: [f32; 3]) -> Result<(), i32> {
    match parameter {
        AL_POSITION => Ok(source.position = value),
        AL_VELOCITY => Ok(source.velocity = value),
        AL_DIRECTION => Ok(source.direction = value),
        _ => Err(AL_INVALID_ENUM),
    }
}

extern "C" fn al_source3f(name: u32, parameter: i32, x: f32, y: f32, z: f32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, _| {
        if let Err(e) = set_vector(source, parameter, [x, y, z]) {
            error = e;
        }
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

extern "C" fn al_sourcefv(name: u32, parameter: i32, values: *const f32) {
    if values.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    match parameter {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => unsafe { al_source3f(name, parameter, *values, *values.add(1), *values.add(2)) },
        _ => al_sourcef(name, parameter, unsafe { *values }),
    }
}

fn set_int(source: &mut Source, buffers: &HashMap<u32, Arc<Samples>>, parameter: i32, value: i32) -> Result<(), i32> {
    match parameter {
        AL_LOOPING => Ok(source.looping = value != 0),
        AL_SOURCE_RELATIVE => Ok(source.relative = value != 0),
        AL_BUFFER => {
            if source.state == AL_PLAYING || source.state == AL_PAUSED {
                return Err(AL_INVALID_OPERATION);
            }
            let buffer = value as u32;
            if buffer != 0 && !buffers.contains_key(&buffer) {
                return Err(AL_INVALID_VALUE);
            }
            source.queue.clear();
            source.current = 0;
            source.offset = 0.0;
            if buffer == 0 {
                source.source_type = AL_UNDETERMINED;
            } else {
                source.queue.push_back(buffer);
                source.source_type = AL_STATIC;
            }
            Ok(())
        }
        AL_SEC_OFFSET | AL_SAMPLE_OFFSET | AL_BYTE_OFFSET => seek(source, buffers, parameter, value as f64),
        AL_REFERENCE_DISTANCE | AL_ROLLOFF_FACTOR | AL_MAX_DISTANCE | AL_CONE_INNER_ANGLE | AL_CONE_OUTER_ANGLE => set_float(source, parameter, value as f32),
        _ => Err(AL_INVALID_ENUM),
    }
}

extern "C" fn al_sourcei(name: u32, parameter: i32, value: i32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, buffers| {
        if let Err(e) = set_int(source, buffers, parameter, value) {
            error = e;
        }
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

extern "C" fn al_source3i(name: u32, parameter: i32, x: i32, y: i32, z: i32) {
    al_source3f(name, parameter, x as f32, y as f32, z as f32)
}

extern "C" fn al_sourceiv(name: u32, parameter: i32, values: *const i32) {
    if values.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    match parameter {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => unsafe { al_source3i(name, parameter, *values, *values.add(1), *values.add(2)) },
        _ => al_sourcei(name, parameter, unsafe { *values }),
    }
}

/// Where a source is in its queue, counted from the queue's start.
fn offset_of(source: &Source, buffers: &HashMap<u32, Arc<Samples>>, parameter: i32) -> f64 {
    let mut total = 0.0;
    for (index, name) in source.queue.iter().enumerate() {
        let Some(samples) = buffers.get(name) else { continue };
        let frames = if index < source.current { samples.frames() as f64 } else if index == source.current { source.offset } else { break };
        total += match parameter {
            AL_SEC_OFFSET => frames / samples.rate,
            AL_SAMPLE_OFFSET => frames,
            _ => frames * (samples.channels * samples.bits as usize / 8) as f64,
        };
    }
    total
}

fn processed(source: &Source) -> usize {
    match source.state {
        AL_STOPPED => source.queue.len(),
        AL_INITIAL => 0,
        _ if source.looping => 0,
        _ => source.current.min(source.queue.len()),
    }
}

fn get_int(source: &Source, buffers: &HashMap<u32, Arc<Samples>>, parameter: i32) -> Result<i32, i32> {
    Ok(match parameter {
        AL_SOURCE_STATE => source.state,
        AL_BUFFERS_QUEUED => source.queue.len() as i32,
        AL_BUFFERS_PROCESSED => processed(source) as i32,
        AL_LOOPING => source.looping as i32,
        AL_SOURCE_RELATIVE => source.relative as i32,
        AL_SOURCE_TYPE => source.source_type,
        AL_BUFFER => source.queue.get(source.current.min(source.queue.len().saturating_sub(1))).copied().unwrap_or(0) as i32,
        AL_SEC_OFFSET | AL_SAMPLE_OFFSET | AL_BYTE_OFFSET => offset_of(source, buffers, parameter) as i32,
        _ => get_float(source, buffers, parameter)? as i32,
    })
}

fn get_float(source: &Source, buffers: &HashMap<u32, Arc<Samples>>, parameter: i32) -> Result<f32, i32> {
    Ok(match parameter {
        AL_GAIN => source.gain,
        AL_MIN_GAIN => source.min_gain,
        AL_MAX_GAIN => source.max_gain,
        AL_PITCH => source.pitch,
        AL_REFERENCE_DISTANCE => source.reference_distance,
        AL_ROLLOFF_FACTOR => source.rolloff,
        AL_MAX_DISTANCE => source.max_distance,
        AL_CONE_INNER_ANGLE => source.cone[0],
        AL_CONE_OUTER_ANGLE => source.cone[1],
        AL_CONE_OUTER_GAIN => source.cone[2],
        AL_SEC_OFFSET | AL_SAMPLE_OFFSET | AL_BYTE_OFFSET => offset_of(source, buffers, parameter) as f32,
        AL_SOURCE_STATE | AL_BUFFERS_QUEUED | AL_BUFFERS_PROCESSED | AL_LOOPING | AL_SOURCE_RELATIVE | AL_SOURCE_TYPE | AL_BUFFER => get_int(source, buffers, parameter)? as f32,
        _ => return Err(AL_INVALID_ENUM),
    })
}

extern "C" fn al_get_sourcei(name: u32, parameter: i32, out: *mut i32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, buffers| match get_int(source, buffers, parameter) {
        Ok(value) if !out.is_null() => unsafe { *out = value },
        Ok(_) => error = AL_INVALID_VALUE,
        Err(e) => error = e,
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

extern "C" fn al_get_sourcef(name: u32, parameter: i32, out: *mut f32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, buffers| match get_float(source, buffers, parameter) {
        Ok(value) if !out.is_null() => unsafe { *out = value },
        Ok(_) => error = AL_INVALID_VALUE,
        Err(e) => error = e,
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

extern "C" fn al_get_source3f(name: u32, parameter: i32, x: *mut f32, y: *mut f32, z: *mut f32) {
    let mut error = AL_NO_ERROR;
    with_source(name, |source, _| {
        let value = match parameter {
            AL_POSITION => source.position,
            AL_VELOCITY => source.velocity,
            AL_DIRECTION => source.direction,
            _ => return error = AL_INVALID_ENUM,
        };
        if x.is_null() || y.is_null() || z.is_null() {
            return error = AL_INVALID_VALUE;
        }
        unsafe { (*x, *y, *z) = (value[0], value[1], value[2]) };
    });
    if error != AL_NO_ERROR {
        with(|s| s.fail(error));
    }
}

extern "C" fn al_get_sourcefv(name: u32, parameter: i32, out: *mut f32) {
    if out.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    match parameter {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => unsafe { al_get_source3f(name, parameter, out, out.add(1), out.add(2)) },
        _ => al_get_sourcef(name, parameter, out),
    }
}

extern "C" fn al_get_sourceiv(name: u32, parameter: i32, out: *mut i32) {
    if out.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    match parameter {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => {
            let mut value = [0.0f32; 3];
            al_get_source3f(name, parameter, &mut value[0], &mut value[1], &mut value[2]);
            for (index, v) in value.iter().enumerate() {
                unsafe { *out.add(index) = *v as i32 };
            }
        }
        _ => al_get_sourcei(name, parameter, out),
    }
}

// ----- playing -----

fn play(source: &mut Source) {
    if source.state != AL_PAUSED {
        // From the start (also after it stopped): every buffer pending again.
        source.current = 0;
        source.offset = 0.0;
    }
    source.state = if source.queue.is_empty() { AL_STOPPED } else { AL_PLAYING };
    static FIRST: std::sync::Once = std::sync::Once::new();
    if source.state == AL_PLAYING {
        FIRST.call_once(|| crate::log!("OpenAL: first sound playing"));
    }
    crate::fw::sound::start();
}

fn stop(source: &mut Source) {
    if source.state != AL_INITIAL {
        source.state = AL_STOPPED;
        source.current = source.queue.len();
        source.offset = 0.0;
    }
}

fn rewind(source: &mut Source) {
    source.state = AL_INITIAL;
    source.current = 0;
    source.offset = 0.0;
}

fn pause(source: &mut Source) {
    if source.state == AL_PLAYING {
        source.state = AL_PAUSED;
    }
}

fn each_source(count: i32, names: *const u32, f: fn(&mut Source)) {
    if count < 0 || (count > 0 && names.is_null()) {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        for index in 0..count as usize {
            let name = unsafe { *names.add(index) };
            match state.sources.get_mut(&name) {
                Some(source) => f(source),
                None => state.fail(AL_INVALID_NAME),
            }
        }
    })
}

extern "C" fn al_source_play(name: u32) {
    each_source(1, &name, play)
}
extern "C" fn al_source_stop(name: u32) {
    each_source(1, &name, stop)
}
extern "C" fn al_source_pause(name: u32) {
    each_source(1, &name, pause)
}
extern "C" fn al_source_rewind(name: u32) {
    each_source(1, &name, rewind)
}
extern "C" fn al_source_playv(count: i32, names: *const u32) {
    each_source(count, names, play)
}
extern "C" fn al_source_stopv(count: i32, names: *const u32) {
    each_source(count, names, stop)
}
extern "C" fn al_source_pausev(count: i32, names: *const u32) {
    each_source(count, names, pause)
}
extern "C" fn al_source_rewindv(count: i32, names: *const u32) {
    each_source(count, names, rewind)
}

extern "C" fn al_source_queue_buffers(name: u32, count: i32, buffers: *const u32) {
    if count < 0 || (count > 0 && buffers.is_null()) {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        let names: Vec<u32> = (0..count as usize).map(|i| unsafe { *buffers.add(i) }).collect();
        if names.iter().any(|b| *b != 0 && !state.buffers.contains_key(b)) {
            return state.fail(AL_INVALID_NAME);
        }
        let Some(source) = state.sources.get_mut(&name) else { return state.fail(AL_INVALID_NAME) };
        if source.source_type == AL_STATIC {
            return state.fail(AL_INVALID_OPERATION);
        }
        source.source_type = AL_STREAMING;
        source.queue.extend(names);
    })
}

extern "C" fn al_source_unqueue_buffers(name: u32, count: i32, out: *mut u32) {
    if count < 0 || (count > 0 && out.is_null()) {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        let Some(source) = state.sources.get_mut(&name) else { return state.fail(AL_INVALID_NAME) };
        let count = count as usize;
        if count > processed(source) {
            return state.fail(AL_INVALID_VALUE);
        }
        for index in 0..count {
            let buffer = source.queue.pop_front().unwrap_or(0);
            unsafe { *out.add(index) = buffer };
        }
        source.current = source.current.saturating_sub(count);
        if source.queue.is_empty() {
            source.source_type = AL_UNDETERMINED;
        }
    })
}

// ----- listener and global state -----

extern "C" fn al_listenerf(parameter: i32, value: f32) {
    with(|state| match parameter {
        AL_GAIN if value >= 0.0 => state.listener.gain = value,
        AL_GAIN => state.fail(AL_INVALID_VALUE),
        _ => state.fail(AL_INVALID_ENUM),
    })
}

extern "C" fn al_listener3f(parameter: i32, x: f32, y: f32, z: f32) {
    with(|state| match parameter {
        AL_POSITION => state.listener.position = [x, y, z],
        AL_VELOCITY => state.listener.velocity = [x, y, z],
        _ => state.fail(AL_INVALID_ENUM),
    })
}

extern "C" fn al_listenerfv(parameter: i32, values: *const f32) {
    if values.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    let v = |i: usize| unsafe { *values.add(i) };
    match parameter {
        AL_ORIENTATION => with(|state| {
            state.listener.at = [v(0), v(1), v(2)];
            state.listener.up = [v(3), v(4), v(5)];
        }),
        AL_POSITION | AL_VELOCITY => al_listener3f(parameter, v(0), v(1), v(2)),
        _ => al_listenerf(parameter, v(0)),
    }
}

extern "C" fn al_listeneri(parameter: i32, value: i32) {
    al_listenerf(parameter, value as f32)
}

extern "C" fn al_get_listenerf(parameter: i32, out: *mut f32) {
    with(|state| match parameter {
        AL_GAIN if !out.is_null() => unsafe { *out = state.listener.gain },
        AL_GAIN => state.fail(AL_INVALID_VALUE),
        _ => state.fail(AL_INVALID_ENUM),
    })
}

extern "C" fn al_get_listenerfv(parameter: i32, out: *mut f32) {
    if out.is_null() {
        return with(|s| s.fail(AL_INVALID_VALUE));
    }
    with(|state| {
        let values: Vec<f32> = match parameter {
            AL_POSITION => state.listener.position.to_vec(),
            AL_VELOCITY => state.listener.velocity.to_vec(),
            AL_ORIENTATION => state.listener.at.iter().chain(state.listener.up.iter()).copied().collect(),
            AL_GAIN => vec![state.listener.gain],
            _ => return state.fail(AL_INVALID_ENUM),
        };
        for (index, value) in values.into_iter().enumerate() {
            unsafe { *out.add(index) = value };
        }
    })
}

extern "C" fn al_get_listener3f(parameter: i32, x: *mut f32, y: *mut f32, z: *mut f32) {
    let mut value = [0.0f32; 6];
    al_get_listenerfv(parameter, value.as_mut_ptr());
    if !x.is_null() && !y.is_null() && !z.is_null() {
        unsafe { (*x, *y, *z) = (value[0], value[1], value[2]) };
    }
}

extern "C" fn al_distance_model(model: i32) {
    with(|state| match model {
        0 | AL_INVERSE_DISTANCE | AL_INVERSE_DISTANCE_CLAMPED | AL_LINEAR_DISTANCE | AL_LINEAR_DISTANCE_CLAMPED | AL_EXPONENT_DISTANCE | AL_EXPONENT_DISTANCE_CLAMPED => {
            state.distance_model = model
        }
        _ => state.fail(AL_INVALID_VALUE),
    })
}

extern "C" fn al_doppler_factor(value: f32) {
    with(|state| state.doppler = value)
}

extern "C" fn al_speed_of_sound(value: f32) {
    with(|state| state.speed_of_sound = value)
}

extern "C" fn al_get_error() -> i32 {
    with(|state| std::mem::replace(&mut state.error, AL_NO_ERROR))
}

extern "C" fn al_get_integer(parameter: i32) -> i32 {
    with(|state| match parameter {
        AL_DISTANCE_MODEL => state.distance_model,
        AL_DOPPLER_FACTOR => state.doppler as i32,
        AL_SPEED_OF_SOUND => state.speed_of_sound as i32,
        _ => {
            state.fail(AL_INVALID_ENUM);
            0
        }
    })
}

extern "C" fn al_get_float(parameter: i32) -> f32 {
    with(|state| match parameter {
        AL_DOPPLER_FACTOR => state.doppler,
        AL_SPEED_OF_SOUND => state.speed_of_sound,
        AL_DISTANCE_MODEL => state.distance_model as f32,
        _ => {
            state.fail(AL_INVALID_ENUM);
            0.0
        }
    })
}

extern "C" fn al_get_string(parameter: i32) -> usize {
    static STRINGS: std::sync::OnceLock<[usize; 4]> = std::sync::OnceLock::new();
    let strings = STRINGS.get_or_init(|| [leak_cstr("Game Bridge"), leak_cstr("1.1"), leak_cstr("Game Bridge mixer"), leak_cstr("")]);
    match parameter {
        AL_VENDOR => strings[0],
        AL_VERSION => strings[1],
        AL_RENDERER => strings[2],
        AL_EXTENSIONS => strings[3],
        _ => {
            with(|s| s.fail(AL_INVALID_ENUM));
            0
        }
    }
}

extern "C" fn al_is_extension_present(_name: *const libc::c_char) -> u8 {
    0
}

/// Enum values by name, for the few games that look them up.
extern "C" fn al_get_enum_value(name: *const libc::c_char) -> i32 {
    if name.is_null() {
        return 0;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    let known = [
        ("AL_NONE", 0),
        ("AL_FALSE", 0),
        ("AL_TRUE", 1),
        ("AL_SOURCE_RELATIVE", AL_SOURCE_RELATIVE),
        ("AL_PITCH", AL_PITCH),
        ("AL_POSITION", AL_POSITION),
        ("AL_VELOCITY", AL_VELOCITY),
        ("AL_LOOPING", AL_LOOPING),
        ("AL_BUFFER", AL_BUFFER),
        ("AL_GAIN", AL_GAIN),
        ("AL_SOURCE_STATE", AL_SOURCE_STATE),
        ("AL_PLAYING", AL_PLAYING),
        ("AL_STOPPED", AL_STOPPED),
        ("AL_FORMAT_MONO16", AL_FORMAT_MONO16),
        ("AL_FORMAT_STEREO16", AL_FORMAT_STEREO16),
    ];
    known.iter().find(|(n, _)| *n == name).map_or(0, |(_, v)| *v)
}

/// alGetProcAddress / alcGetProcAddress: our own functions by name.
extern "C" fn al_get_proc_address(name: *const libc::c_char) -> usize {
    if name.is_null() {
        return 0;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    functions().into_iter().find(|(n, _)| *n == name).map_or(0, |(_, f)| f)
}

extern "C" fn alc_get_proc_address(_device: usize, name: *const libc::c_char) -> usize {
    al_get_proc_address(name)
}

// ----- device and context (one of each) -----

static DEVICE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CONTEXT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CURRENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ALC_ERROR: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

fn device() -> usize {
    *DEVICE.get_or_init(|| leak(0u64))
}

extern "C" fn alc_open_device(_name: *const libc::c_char) -> usize {
    crate::log!("OpenAL: device opened");
    device()
}

extern "C" fn alc_close_device(device_handle: usize) -> u8 {
    (device_handle == device()) as u8
}

extern "C" fn alc_create_context(device_handle: usize, _attributes: *const i32) -> usize {
    if device_handle != device() {
        ALC_ERROR.store(ALC_INVALID_DEVICE, std::sync::atomic::Ordering::Relaxed);
        return 0;
    }
    *CONTEXT.get_or_init(|| leak(0u64))
}

extern "C" fn alc_make_context_current(context: usize) -> u8 {
    CURRENT.store(context, std::sync::atomic::Ordering::Relaxed);
    if context != 0 {
        crate::fw::sound::start();
    }
    1
}

extern "C" fn alc_destroy_context(context: usize) {
    if CURRENT.load(std::sync::atomic::Ordering::Relaxed) == context {
        CURRENT.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

extern "C" fn alc_nothing(_context: usize) {}

extern "C" fn alc_get_current_context() -> usize {
    CURRENT.load(std::sync::atomic::Ordering::Relaxed)
}

extern "C" fn alc_get_contexts_device(context: usize) -> usize {
    if context == 0 { 0 } else { device() }
}

extern "C" fn alc_get_error(_device: usize) -> i32 {
    ALC_ERROR.swap(0, std::sync::atomic::Ordering::Relaxed)
}

extern "C" fn alc_get_string(_device: usize, parameter: i32) -> usize {
    static NAME: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static LIST: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static EMPTY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    match parameter {
        ALC_DEFAULT_DEVICE_SPECIFIER => *NAME.get_or_init(|| leak_cstr("Game Bridge")),
        // A list ends with two zero bytes.
        ALC_DEVICE_SPECIFIER => *LIST.get_or_init(|| leak_cstr("Game Bridge\0")),
        ALC_EXTENSIONS => *EMPTY.get_or_init(|| leak_cstr("")),
        _ => {
            ALC_ERROR.store(ALC_INVALID_ENUM, std::sync::atomic::Ordering::Relaxed);
            0
        }
    }
}

extern "C" fn alc_get_integerv(_device: usize, parameter: i32, size: i32, out: *mut i32) {
    if out.is_null() || size < 1 {
        return;
    }
    let value = match parameter {
        ALC_MAJOR_VERSION => 1,
        ALC_MINOR_VERSION => 1,
        ALC_FREQUENCY => crate::fw::sound::output_rate() as i32,
        ALC_ATTRIBUTES_SIZE => 1,
        ALC_ALL_ATTRIBUTES => 0,
        _ => {
            ALC_ERROR.store(ALC_INVALID_ENUM, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    unsafe { *out = value };
}

extern "C" fn alc_is_extension_present(_device: usize, _name: *const libc::c_char) -> u8 {
    0
}

/// Apple's alcMacOSXMixerOutputRate: the mixer follows the phone's rate.
extern "C" fn alc_mac_os_x_mixer_output_rate(_rate: f64) {}

extern "C" fn alc_mac_os_x_get_mixer_output_rate() -> f64 {
    crate::fw::sound::output_rate() as f64
}

// ----- mixing (on the sound thread) -----

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// Distance gain per the OpenAL 1.1 models.
fn distance_gain(model: i32, distance: f32, source: &Source) -> f32 {
    let reference = source.reference_distance;
    let rolloff = source.rolloff;
    let max = source.max_distance;
    let clamped = matches!(model, AL_INVERSE_DISTANCE_CLAMPED | AL_LINEAR_DISTANCE_CLAMPED | AL_EXPONENT_DISTANCE_CLAMPED);
    let distance = if clamped { distance.max(reference).min(max) } else { distance };
    let gain = match model {
        AL_INVERSE_DISTANCE | AL_INVERSE_DISTANCE_CLAMPED => {
            let denominator = reference + rolloff * (distance - reference);
            if denominator > 0.0 { reference / denominator } else { 1.0 }
        }
        AL_LINEAR_DISTANCE | AL_LINEAR_DISTANCE_CLAMPED => {
            if max > reference { 1.0 - rolloff * (distance.min(max) - reference) / (max - reference) } else { 1.0 }
        }
        AL_EXPONENT_DISTANCE | AL_EXPONENT_DISTANCE_CLAMPED => {
            if reference > 0.0 && distance > 0.0 { (distance / reference).powf(-rolloff) } else { 1.0 }
        }
        _ => 1.0,
    };
    if gain.is_finite() { gain.max(0.0) } else { 1.0 }
}

/// Left/right volumes for a source: its gain, distance and place around the
/// listener (only mono sources are placed, as in OpenAL).
fn volumes(source: &Source, listener: &Listener, model: i32, stereo: bool) -> (f32, f32) {
    let mut gain = source.gain;
    let mut pan = 0.0;
    if !stereo {
        let offset = if source.relative { source.position } else { sub(source.position, listener.position) };
        let distance = length(offset);
        gain *= distance_gain(model, distance, source);
        if distance > 1e-4 {
            let right = cross(listener.at, listener.up);
            let right_length = length(right);
            if right_length > 1e-6 {
                pan = dot(offset, right) / (distance * right_length);
            }
        }
    }
    let gain = gain.clamp(source.min_gain, source.max_gain) * listener.gain;
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    if stereo || pan == 0.0 {
        (gain, gain)
    } else {
        let boost = std::f32::consts::SQRT_2;
        ((angle.cos() * boost).min(1.0) * gain, (angle.sin() * boost).min(1.0) * gain)
    }
}

/// Adds one source's next `frames` frames to `out`; moves it along.
fn mix_source(source: &mut Source, buffers: &HashMap<u32, Arc<Samples>>, listener: &Listener, model: i32, out: &mut [f32], frames: usize, out_rate: f64) {
    let mut made = 0;
    let mut guard = 0;
    while made < frames && source.state == AL_PLAYING {
        let Some(samples) = source.queue.get(source.current).and_then(|name| buffers.get(name)) else {
            source.state = AL_STOPPED;
            source.current = source.queue.len();
            break;
        };
        let total = samples.frames();
        if total == 0 || source.offset >= total as f64 {
            source.offset = 0.0;
            source.current += 1;
            if source.current >= source.queue.len() {
                if source.looping {
                    source.current = 0;
                } else {
                    source.state = AL_STOPPED;
                    break;
                }
            }
            guard += 1;
            if guard > source.queue.len() + 2 {
                // Every buffer is empty: nothing to play.
                break;
            }
            continue;
        }
        guard = 0;
        let stereo = samples.channels == 2;
        let (left, right) = volumes(source, listener, model, stereo);
        let step = samples.rate * source.pitch as f64 / out_rate;
        let data = &samples.data;
        while made < frames && source.offset < total as f64 {
            let index = source.offset as usize;
            let part = (source.offset - index as f64) as f32;
            let next = (index + 1).min(total - 1);
            let (l, r) = if stereo {
                let a = (data[index * 2], data[index * 2 + 1]);
                let b = (data[next * 2], data[next * 2 + 1]);
                (a.0 + (b.0 - a.0) * part, a.1 + (b.1 - a.1) * part)
            } else {
                let value = data[index] + (data[next] - data[index]) * part;
                (value, value)
            };
            out[made * 2] += l * left;
            out[made * 2 + 1] += r * right;
            made += 1;
            source.offset += step;
        }
    }
}

/// Adds every playing source to `out` (stereo, interleaved).
pub fn mix(out: &mut [f32], frames: usize, out_rate: f64) {
    // Never wait on a game thread holding the lock: skip a turn instead.
    let Ok(mut guard) = STATE.try_lock() else { return };
    let Some(state) = guard.as_mut() else { return };
    if CURRENT.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return;
    }
    let State { buffers, sources, listener, distance_model, .. } = state;
    static HEARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let heard = HEARD.load(std::sync::atomic::Ordering::Relaxed);
    let before: f32 = if heard { 0.0 } else { out.iter().map(|v| v.abs()).sum() };
    for source in sources.values_mut() {
        if source.state == AL_PLAYING {
            mix_source(source, buffers, listener, *distance_model, out, frames, out_rate);
        }
    }
    if !heard && (out.iter().map(|v| v.abs()).sum::<f32>() - before).abs() > 1e-3 {
        HEARD.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::log!("OpenAL: first sound mixed into the output");
    }
}

pub fn register(r: &mut Registry) {
    for (name, function) in functions() {
        r.function(name, function);
    }
}

/// Every function here, by its OpenAL name.
fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("alGenBuffers", al_gen_buffers as usize),
        ("alDeleteBuffers", al_delete_buffers as usize),
        ("alIsBuffer", al_is_buffer as usize),
        ("alBufferData", al_buffer_data as usize),
        ("alBufferDataStatic", al_buffer_data_static as usize),
        ("alGetBufferi", al_get_bufferi as usize),
        ("alGetBufferf", al_get_bufferf as usize),
        ("alGenSources", al_gen_sources as usize),
        ("alDeleteSources", al_delete_sources as usize),
        ("alIsSource", al_is_source as usize),
        ("alSourcef", al_sourcef as usize),
        ("alSource3f", al_source3f as usize),
        ("alSourcefv", al_sourcefv as usize),
        ("alSourcei", al_sourcei as usize),
        ("alSource3i", al_source3i as usize),
        ("alSourceiv", al_sourceiv as usize),
        ("alGetSourcei", al_get_sourcei as usize),
        ("alGetSourcef", al_get_sourcef as usize),
        ("alGetSource3f", al_get_source3f as usize),
        ("alGetSourcefv", al_get_sourcefv as usize),
        ("alGetSourceiv", al_get_sourceiv as usize),
        ("alSourcePlay", al_source_play as usize),
        ("alSourceStop", al_source_stop as usize),
        ("alSourcePause", al_source_pause as usize),
        ("alSourceRewind", al_source_rewind as usize),
        ("alSourcePlayv", al_source_playv as usize),
        ("alSourceStopv", al_source_stopv as usize),
        ("alSourcePausev", al_source_pausev as usize),
        ("alSourceRewindv", al_source_rewindv as usize),
        ("alSourceQueueBuffers", al_source_queue_buffers as usize),
        ("alSourceUnqueueBuffers", al_source_unqueue_buffers as usize),
        ("alListenerf", al_listenerf as usize),
        ("alListener3f", al_listener3f as usize),
        ("alListenerfv", al_listenerfv as usize),
        ("alListeneri", al_listeneri as usize),
        ("alGetListenerf", al_get_listenerf as usize),
        ("alGetListener3f", al_get_listener3f as usize),
        ("alGetListenerfv", al_get_listenerfv as usize),
        ("alDistanceModel", al_distance_model as usize),
        ("alDopplerFactor", al_doppler_factor as usize),
        ("alSpeedOfSound", al_speed_of_sound as usize),
        ("alGetError", al_get_error as usize),
        ("alGetInteger", al_get_integer as usize),
        ("alGetFloat", al_get_float as usize),
        ("alGetString", al_get_string as usize),
        ("alIsExtensionPresent", al_is_extension_present as usize),
        ("alGetEnumValue", al_get_enum_value as usize),
        ("alGetProcAddress", al_get_proc_address as usize),
        ("alcGetProcAddress", alc_get_proc_address as usize),
        ("alcOpenDevice", alc_open_device as usize),
        ("alcCloseDevice", alc_close_device as usize),
        ("alcCreateContext", alc_create_context as usize),
        ("alcMakeContextCurrent", alc_make_context_current as usize),
        ("alcDestroyContext", alc_destroy_context as usize),
        ("alcProcessContext", alc_nothing as usize),
        ("alcSuspendContext", alc_nothing as usize),
        ("alcGetCurrentContext", alc_get_current_context as usize),
        ("alcGetContextsDevice", alc_get_contexts_device as usize),
        ("alcGetError", alc_get_error as usize),
        ("alcGetString", alc_get_string as usize),
        ("alcGetIntegerv", alc_get_integerv as usize),
        ("alcIsExtensionPresent", alc_is_extension_present as usize),
        ("alcMacOSXMixerOutputRate", alc_mac_os_x_mixer_output_rate as usize),
        ("alcMacOSXGetMixerOutputRate", alc_mac_os_x_get_mixer_output_rate as usize),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_clamped_distance_halves_at_twice_the_reference() {
        let source = Source { reference_distance: 1.0, rolloff: 1.0, ..Source::default() };
        assert_eq!(distance_gain(AL_INVERSE_DISTANCE_CLAMPED, 0.5, &source), 1.0);
        assert!((distance_gain(AL_INVERSE_DISTANCE_CLAMPED, 2.0, &source) - 0.5).abs() < 1e-6);
        assert_eq!(distance_gain(0, 100.0, &source), 1.0);
    }

    #[test]
    fn queued_buffers_play_in_order_and_are_processed() {
        let mut buffers = HashMap::new();
        buffers.insert(1, Arc::new(Samples { data: vec![0.5; 4], channels: 1, rate: 100.0, bits: 16 }));
        buffers.insert(2, Arc::new(Samples { data: vec![-0.5; 4], channels: 1, rate: 100.0, bits: 16 }));
        let listener = Listener { position: [0.0; 3], velocity: [0.0; 3], at: [0.0, 0.0, -1.0], up: [0.0, 1.0, 0.0], gain: 1.0 };
        let mut source = Source { queue: [1, 2].into_iter().collect(), relative: true, ..Source::default() };
        play(&mut source);
        let mut out = vec![0.0; 12];
        mix_source(&mut source, &buffers, &listener, AL_INVERSE_DISTANCE_CLAMPED, &mut out, 6, 100.0);
        assert_eq!(&out[..8], &[0.5; 8]);
        assert_eq!(&out[8..12], &[-0.5; 4]);
        assert_eq!(processed(&source), 1);
        let mut out = vec![0.0; 12];
        mix_source(&mut source, &buffers, &listener, AL_INVERSE_DISTANCE_CLAMPED, &mut out, 6, 100.0);
        assert_eq!(source.state, AL_STOPPED);
        assert_eq!(processed(&source), 2);
    }

    #[test]
    fn eight_and_sixteen_bit_samples_convert() {
        let samples = convert(AL_FORMAT_STEREO16, [0x00, 0x40, 0x00, 0xC0].as_ptr(), 4, 22050).unwrap();
        assert_eq!(samples.data, vec![0.5, -0.5]);
        assert_eq!(samples.frames(), 1);
        let samples = convert(AL_FORMAT_MONO8, [128u8, 255].as_ptr(), 2, 8000).unwrap();
        assert_eq!(samples.data[0], 0.0);
        assert!(convert(0x1234, std::ptr::null(), 0, 8000).is_err());
    }
}
