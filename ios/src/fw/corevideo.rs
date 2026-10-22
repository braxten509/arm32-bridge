//! CoreMedia times and sample buffers, and CoreVideo pixel buffers with
//! their OpenGL ES texture cache.
//!
//! On iOS a pixel buffer and the GL textures made from it share one piece of
//! memory. Here they are separate, so the two are kept in step: a texture
//! gets the buffer's pixels when it is made and again after the game writes
//! to the buffer (unlocking it for writing); locking a buffer that has a
//! texture reads the texture back first, so pixels the GPU drew show up.
//!
//! The CVMetal* functions are deliberately left out (weak imports stay
//! empty): there is no Metal here, and an empty Metal symbol is what makes
//! games pick their OpenGL ES path.

use crate::fw::gl::{self, Enum};
use crate::fw::object::define_class;
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Id};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ----- CMTime -----

pub const TIME_VALID: u32 = 1;
pub const TIME_ROUNDED: u32 = 2;
pub const TIME_POSITIVE_INFINITY: u32 = 4;
pub const TIME_NEGATIVE_INFINITY: u32 = 8;
pub const TIME_INDEFINITE: u32 = 16;

/// CMTime, as iOS lays it out (24 bytes; passed and returned through memory).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CmTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CmTime {
    pub const INVALID: CmTime = CmTime { value: 0, timescale: 0, flags: 0, epoch: 0 };

    pub fn new(value: i64, timescale: i32) -> CmTime {
        if timescale <= 0 {
            return CmTime::INVALID;
        }
        CmTime { value, timescale, flags: TIME_VALID, epoch: 0 }
    }

    pub const POSITIVE_INFINITY: CmTime = CmTime { value: 0, timescale: 0, flags: TIME_VALID | TIME_POSITIVE_INFINITY, epoch: 0 };
    pub const NEGATIVE_INFINITY: CmTime = CmTime { value: 0, timescale: 0, flags: TIME_VALID | TIME_NEGATIVE_INFINITY, epoch: 0 };
    pub const INDEFINITE: CmTime = CmTime { value: 0, timescale: 0, flags: TIME_VALID | TIME_INDEFINITE, epoch: 0 };

    /// A plain number of seconds (valid, finite, with a usable timescale).
    pub fn is_numeric(&self) -> bool {
        self.flags & TIME_VALID != 0 && self.flags & (TIME_POSITIVE_INFINITY | TIME_NEGATIVE_INFINITY | TIME_INDEFINITE) == 0 && self.timescale > 0
    }

    /// Valid flag set but a timescale nothing can be counted in: invalid.
    fn is_broken(&self) -> bool {
        self.flags & TIME_VALID != 0 && self.flags & (TIME_POSITIVE_INFINITY | TIME_NEGATIVE_INFINITY | TIME_INDEFINITE) == 0 && self.timescale <= 0
    }

    pub fn seconds(&self) -> f64 {
        if self.flags & TIME_VALID == 0 || self.flags & TIME_INDEFINITE != 0 {
            f64::NAN
        } else if self.flags & TIME_POSITIVE_INFINITY != 0 {
            f64::INFINITY
        } else if self.flags & TIME_NEGATIVE_INFINITY != 0 {
            f64::NEG_INFINITY
        } else if self.timescale <= 0 {
            f64::NAN
        } else {
            self.value as f64 / self.timescale as f64
        }
    }
}

fn infinity(positive: bool) -> CmTime {
    if positive {
        CmTime::POSITIVE_INFINITY
    } else {
        CmTime::NEGATIVE_INFINITY
    }
}

/// CMTimeMakeWithSeconds: the nearest value in `timescale`; if it doesn't
/// fit, the timescale is halved until it does, and past timescale 1 the
/// time is infinite (as iOS does). Not-a-number is an invalid time.
pub fn time_from_seconds(seconds: f64, timescale: i32) -> CmTime {
    if seconds.is_nan() || timescale <= 0 {
        return CmTime::INVALID;
    }
    if seconds.is_infinite() {
        return infinity(seconds > 0.0);
    }
    let mut scale = timescale;
    loop {
        let exact = seconds * scale as f64;
        // 2^63 is the first value past i64 (and exactly representable).
        if exact.round() < 9_223_372_036_854_775_808.0 && exact.round() >= -9_223_372_036_854_775_808.0 {
            let value = exact.round() as i64;
            let rounded = value as f64 / scale as f64 != seconds;
            return CmTime { value, timescale: scale, flags: TIME_VALID | if rounded { TIME_ROUNDED } else { 0 }, epoch: 0 };
        }
        if scale == 1 {
            return infinity(seconds > 0.0);
        }
        scale /= 2;
    }
}

/// CMTimeRoundingMethod (Apple's numbers; 0 and unknown ones act as the
/// default, half away from zero).
const ROUND_HALF_AWAY: u32 = 1;
const ROUND_TOWARD_ZERO: u32 = 2;
const ROUND_AWAY_FROM_ZERO: u32 = 3;
const ROUND_QUICKTIME: u32 = 4;
const ROUND_UP: u32 = 5;
const ROUND_DOWN: u32 = 6;

/// `value * to / from`, rounded by `method`; the flag says whether it had to be.
fn rescale(value: i128, from: i64, to: i64, method: u32) -> (i128, bool) {
    let wide = value * to as i128;
    let (whole, rest) = (wide / from as i128, wide % from as i128);
    if rest == 0 {
        return (whole, false);
    }
    let negative = wide < 0;
    let away = match method {
        ROUND_TOWARD_ZERO => false,
        ROUND_AWAY_FROM_ZERO => true,
        ROUND_UP => !negative,
        ROUND_DOWN => negative,
        // QuickTime: away from zero to a finer timescale, toward zero to a
        // coarser one, but a negative time never rounds up to 0.
        ROUND_QUICKTIME => to > from || (negative && whole == 0),
        _ => rest.abs() * 2 >= from as i128,
    };
    (if away { whole + if negative { -1 } else { 1 } } else { whole }, true)
}

/// The largest timescale a CMTime has (kCMTimeMaxTimescale).
const MAX_TIMESCALE: i64 = i32::MAX as i64;

/// A value in `scale` as a time: halving the timescale while it doesn't
/// fit, infinite past timescale 1.
fn fitted(mut value: i128, mut scale: i64, mut rounded: bool, epoch: i64) -> CmTime {
    while value > i64::MAX as i128 || value < i64::MIN as i128 {
        if scale == 1 {
            return infinity(value > 0);
        }
        let next = scale / 2;
        let wide = value * next as i128;
        let rest = wide % scale as i128;
        value = wide / scale as i128 + if rest.abs() * 2 >= scale as i128 { rest.signum() } else { 0 };
        rounded |= rest != 0;
        scale = next;
    }
    CmTime { value: value as i64, timescale: scale as i32, flags: TIME_VALID | if rounded { TIME_ROUNDED } else { 0 }, epoch }
}

/// CMTimeCompare's order: negative infinity < numeric times < indefinite <
/// positive infinity < invalid; numeric times by epoch, then exact value.
pub fn compare_times(a: &CmTime, b: &CmTime) -> i32 {
    fn rank(t: &CmTime) -> i32 {
        if t.flags & TIME_VALID == 0 || t.is_broken() {
            4
        } else if t.flags & TIME_NEGATIVE_INFINITY != 0 {
            0
        } else if t.flags & TIME_POSITIVE_INFINITY != 0 {
            3
        } else if t.flags & TIME_INDEFINITE != 0 {
            2
        } else {
            1
        }
    }
    let (ra, rb) = (rank(a), rank(b));
    if ra != rb || ra != 1 {
        return (ra - rb).signum();
    }
    if a.epoch != b.epoch {
        return if a.epoch < b.epoch { -1 } else { 1 };
    }
    let left = a.value as i128 * b.timescale as i128;
    let right = b.value as i128 * a.timescale as i128;
    left.cmp(&right) as i32
}

/// CMTimeAdd / CMTimeSubtract. Numeric times add exactly in the least
/// common multiple of their timescales, rounded once into
/// kCMTimeMaxTimescale if that is too fine; a sum too large halves the
/// timescale, then becomes infinite. Invalid times give an invalid time;
/// infinities win over indefinite, and opposite ones cancel to invalid.
/// Epochs: adding a duration (epoch 0) keeps the time's epoch; the
/// difference of two times in one epoch is a duration (epoch 0); times of
/// different epochs don't mix.
fn combine(a: &CmTime, b: &CmTime, negate: bool) -> CmTime {
    let valid = |t: &CmTime| t.flags & TIME_VALID != 0 && !t.is_broken();
    if !valid(a) || !valid(b) {
        return CmTime::INVALID;
    }
    // +1 / -1 / 0 for each side's infinity (b's turned around when subtracting).
    let sign = |t: &CmTime| if t.flags & TIME_POSITIVE_INFINITY != 0 { 1 } else if t.flags & TIME_NEGATIVE_INFINITY != 0 { -1 } else { 0 };
    let (ia, ib) = (sign(a), if negate { -sign(b) } else { sign(b) });
    if ia != 0 || ib != 0 {
        return if ia != 0 && ib != 0 && ia != ib { CmTime::INVALID } else { infinity(ia + ib > 0) };
    }
    if (a.flags | b.flags) & TIME_INDEFINITE != 0 {
        return CmTime::INDEFINITE;
    }
    let epoch = match (a.epoch, b.epoch) {
        (x, y) if x == y => if negate { 0 } else { x },
        (x, 0) => x,
        (0, y) if !negate => y,
        _ => return CmTime::INVALID,
    };
    fn gcd(x: i64, y: i64) -> i64 {
        if y == 0 { x } else { gcd(y, x % y) }
    }
    let (sa, sb) = (a.timescale as i64, b.timescale as i64);
    let common = sa / gcd(sa, sb) * sb;
    let (va, vb) = (a.value as i128 * (common / sa) as i128, b.value as i128 * (common / sb) as i128);
    let exact = if negate { va - vb } else { va + vb };
    let scale = common.min(MAX_TIMESCALE);
    let (sum, rounded) = rescale(exact, common, scale, ROUND_HALF_AWAY);
    fitted(sum, scale, rounded || (a.flags | b.flags) & TIME_ROUNDED != 0, epoch)
}

/// CMTimeRange (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CmTimeRange {
    pub start: CmTime,
    pub duration: CmTime,
}

extern "C" fn time_make(value: i64, timescale: i32) -> CmTime {
    CmTime::new(value, timescale)
}

extern "C" fn time_make_with_seconds(seconds: f64, timescale: i32) -> CmTime {
    time_from_seconds(seconds, timescale)
}

extern "C" fn time_make_with_epoch(value: i64, timescale: i32, epoch: i64) -> CmTime {
    CmTime { epoch, ..CmTime::new(value, timescale) }
}

extern "C" fn time_compare(a: CmTime, b: CmTime) -> i32 {
    compare_times(&a, &b)
}

extern "C" fn time_add(a: CmTime, b: CmTime) -> CmTime {
    combine(&a, &b, false)
}

extern "C" fn time_subtract(a: CmTime, b: CmTime) -> CmTime {
    combine(&a, &b, true)
}

/// CMTimeConvertScale(time, timescale, method): exact, or rounded by
/// `method` with the rounded flag set; non-numeric times stay as they are.
extern "C" fn time_convert_scale(time: CmTime, timescale: i32, method: u32) -> CmTime {
    if time.is_broken() || timescale <= 0 {
        return CmTime::INVALID;
    }
    if !time.is_numeric() {
        return time;
    }
    let (value, rounded) = rescale(time.value as i128, time.timescale as i64, timescale as i64, method);
    if value > i64::MAX as i128 || value < i64::MIN as i128 {
        return CmTime { epoch: time.epoch, ..infinity(value > 0) };
    }
    let flags = TIME_VALID | if rounded || time.flags & TIME_ROUNDED != 0 { TIME_ROUNDED } else { 0 };
    CmTime { value: value as i64, timescale, flags, epoch: time.epoch }
}

extern "C" fn time_range_make(start: CmTime, duration: CmTime) -> CmTimeRange {
    CmTimeRange { start, duration }
}

extern "C" fn time_range_get_end(range: CmTimeRange) -> CmTime {
    combine(&range.start, &range.duration, false)
}

// ----- CoreFoundation-style objects with reference counts -----
// CFRelease is otherwise a no-op here; these objects hold memory and GL
// textures, so their counts are kept and the last release frees them.

static COUNTS: Mutex<Option<HashMap<Id, usize>>> = Mutex::new(None);
/// Set once any counted object exists, so CFRetain/CFRelease on everything
/// else stay cheap in games that never make one.
static ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn counted(object: Id) {
    ANY.store(true, std::sync::atomic::Ordering::Relaxed);
    COUNTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(object, 1);
}

/// CFRetain on one of ours: one more owner.
pub fn retained(object: Id) {
    if !ANY.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(count) = COUNTS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|c| c.get_mut(&object)) {
        *count += 1;
    }
}

/// CFRelease on one of ours: the last owner gone frees what it holds.
pub fn released(object: Id) {
    if !ANY.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let last = {
        let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(counts) = guard.as_mut() else { return };
        let Some(count) = counts.get_mut(&object) else { return };
        *count -= 1;
        if *count == 0 {
            counts.remove(&object);
            true
        } else {
            false
        }
    };
    if last {
        free(object);
    }
}

fn free(object: Id) {
    if let Some(buffer) = BUFFERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|b| b.remove(&object)) {
        let textures = std::mem::take(&mut buffer.lock().unwrap_or_else(|e| e.into_inner()).textures);
        for texture in textures {
            // The texture keeps its own copy until it is released itself.
            if let Some(t) = TEXTURES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|t| t.get_mut(&texture)) {
                t.buffer = 0;
            }
        }
    }
    let texture = TEXTURES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|t| t.remove(&object));
    if let Some(texture) = texture {
        if let Some(buffer) = buffer_of(texture.buffer) {
            buffer.lock().unwrap_or_else(|e| e.into_inner()).textures.retain(|&t| t != object);
        }
        // Deleted at the next cache flush (or texture made) that runs in the
        // context the name was made in.
        WAITING_NAMES.lock().unwrap_or_else(|e| e.into_inner()).push((texture.name, texture.context));
    }
    let sample = SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|s| s.remove(&object));
    if let Some(sample) = sample {
        for owned in [sample.image, sample.block, sample.description] {
            if owned != 0 {
                released(owned);
            }
        }
    }
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|p| p.remove(&object));
    CACHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|c| c.remove(&object));
}

/// Texture names of released textures, with the EGL context each was made
/// in. GL deletes a name in whatever context is current, which may be
/// another one's (a different texture), so each waits for its own.
static WAITING_NAMES: Mutex<Vec<(u32, usize)>> = Mutex::new(Vec::new());

/// Takes out the names made in `current` (none without a context).
fn take_owned(names: &mut Vec<(u32, usize)>, current: usize) -> Vec<u32> {
    let mut owned = Vec::new();
    if current != 0 {
        names.retain(|&(name, context)| context != current || {
            owned.push(name);
            false
        });
    }
    owned
}

/// Deletes the waiting names that belong to this thread's current context.
fn delete_waiting_names() {
    let current = unsafe { gl::eglGetCurrentContext() } as usize;
    let names = take_owned(&mut WAITING_NAMES.lock().unwrap_or_else(|e| e.into_inner()), current);
    if !names.is_empty() {
        unsafe { glDeleteTextures(names.len() as i32, names.as_ptr()) };
    }
}

fn new_object(class: &str) -> Id {
    let object = runtime::alloc_instance(runtime::class_named(class));
    counted(object);
    object
}

// ----- CVPixelBuffer -----

const RETURN_SUCCESS: i32 = 0;
const RETURN_INVALID_ARGUMENT: i32 = -6661;
const RETURN_INVALID_PIXEL_FORMAT: i32 = -6680;
const RETURN_INVALID_SIZE: i32 = -6681;
const RETURN_NOT_OPENGL_COMPATIBLE: i32 = -6683;

pub const FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
pub const FORMAT_RGBA: u32 = u32::from_be_bytes(*b"RGBA");
pub const FORMAT_ARGB: u32 = 0x20;
pub const FORMAT_RGB24: u32 = 0x18;
pub const FORMAT_565: u32 = u32::from_be_bytes(*b"L565");
pub const FORMAT_L8: u32 = u32::from_be_bytes(*b"L008");
pub const FORMAT_420V: u32 = u32::from_be_bytes(*b"420v");
pub const FORMAT_420F: u32 = u32::from_be_bytes(*b"420f");

const LOCK_READ_ONLY: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub offset: usize,
    pub width: usize,
    pub height: usize,
    pub bytes_per_pixel: usize,
    pub stride: usize,
}

/// Room before the planes of a planar buffer for its
/// CVPlanarPixelBufferInfo_YCbCrBiPlanar (what its base address points at).
const PLANAR_HEADER: usize = 64;

/// Planes of a `width` x `height` picture in `format` (rows start on 64
/// bytes, like iOS's buffers), or None for a format this layer doesn't make.
pub fn layout(format: u32, width: usize, height: usize) -> Option<Vec<Plane>> {
    let align = |bytes: usize| bytes.div_ceil(64) * 64;
    let single = |bytes_per_pixel: usize| vec![Plane { offset: 0, width, height, bytes_per_pixel, stride: align(width * bytes_per_pixel) }];
    Some(match format {
        FORMAT_BGRA | FORMAT_RGBA | FORMAT_ARGB => single(4),
        FORMAT_RGB24 => single(3),
        FORMAT_565 => single(2),
        FORMAT_L8 => single(1),
        FORMAT_420V | FORMAT_420F => {
            let luma = Plane { offset: PLANAR_HEADER, width, height, bytes_per_pixel: 1, stride: align(width) };
            let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
            let chroma = Plane { offset: luma.offset + luma.stride * height, width: cw, height: ch, bytes_per_pixel: 2, stride: align(cw * 2) };
            vec![luma, chroma]
        }
        _ => return None,
    })
}

pub struct PixelBuffer {
    pub width: usize,
    pub height: usize,
    pub format: u32,
    pub planes: Vec<Plane>,
    /// 16-byte aligned pixel memory; never moves while the buffer lives.
    memory: Vec<u128>,
    locks: usize,
    /// Textures made from this buffer (their objects).
    textures: Vec<Id>,
    /// The game wrote pixels the textures couldn't be given (no GL context
    /// then): the buffer is newer, so it isn't read back from them.
    textures_behind: bool,
}

impl PixelBuffer {
    pub fn new(format: u32, width: usize, height: usize) -> Option<PixelBuffer> {
        let planes = layout(format, width, height)?;
        let bytes = planes.iter().map(|p| p.offset + p.stride * p.height).max().unwrap_or(0);
        let mut buffer = PixelBuffer { width, height, format, planes, memory: vec![0u128; bytes.div_ceil(16)], locks: 0, textures: Vec::new(), textures_behind: false };
        if buffer.planes.len() > 1 {
            // Planar: each plane's offset from the base and its row bytes, big-endian.
            let header: Vec<u8> = buffer.planes.iter().flat_map(|p| [(p.offset as u32).to_be_bytes(), (p.stride as u32).to_be_bytes()]).flatten().collect();
            buffer.bytes_mut()[..header.len()].copy_from_slice(&header);
        }
        Some(buffer)
    }

    pub fn base(&self) -> *mut u8 {
        self.memory.as_ptr() as *mut u8
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        let len = self.memory.len() * 16;
        unsafe { std::slice::from_raw_parts_mut(self.memory.as_mut_ptr() as *mut u8, len) }
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.memory.as_ptr() as *const u8, self.memory.len() * 16) }
    }
}

static BUFFERS: Mutex<Option<HashMap<Id, Arc<Mutex<PixelBuffer>>>>> = Mutex::new(None);

pub fn buffer_of(object: Id) -> Option<Arc<Mutex<PixelBuffer>>> {
    BUFFERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&object).cloned()
}

/// Makes a pixel buffer object (owned once by the caller).
pub fn make_buffer(buffer: PixelBuffer) -> Id {
    let object = new_object("CVPixelBuffer");
    BUFFERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(object, Arc::new(Mutex::new(buffer)));
    object
}

fn with_buffer<R>(object: Id, f: impl FnOnce(&mut PixelBuffer) -> R) -> Option<R> {
    let buffer = buffer_of(object)?;
    let mut guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&mut guard))
}

/// CVPixelBufferCreate(allocator, width, height, format, attributes, *out).
extern "C" fn pixel_buffer_create(_allocator: usize, width: usize, height: usize, format: u32, attributes: Id, out: *mut Id) -> i32 {
    if out.is_null() {
        return RETURN_INVALID_ARGUMENT;
    }
    unsafe { *out = 0 };
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return RETURN_INVALID_SIZE;
    }
    let Some(buffer) = PixelBuffer::new(format, width, height) else {
        crate::log!("CoreVideo: pixel format {} isn't supported", crate::fw::sound::four_cc(format));
        return RETURN_INVALID_PIXEL_FORMAT;
    };
    let _ = attributes;
    unsafe { *out = make_buffer(buffer) };
    RETURN_SUCCESS
}

extern "C" fn pixel_buffer_lock(object: Id, flags: u64) -> i32 {
    let Some(buffer) = buffer_of(object) else { return RETURN_INVALID_ARGUMENT };
    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
    if b.locks == 0 {
        read_back(&mut b);
    }
    b.locks += 1;
    let _ = flags;
    RETURN_SUCCESS
}

extern "C" fn pixel_buffer_unlock(object: Id, flags: u64) -> i32 {
    let Some(buffer) = buffer_of(object) else { return RETURN_INVALID_ARGUMENT };
    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
    b.locks = b.locks.saturating_sub(1);
    if flags & LOCK_READ_ONLY == 0 {
        // The game may have written: textures made from it see the change.
        let textures = b.textures.clone();
        drop(b);
        let all = textures.into_iter().fold(true, |all, texture| upload_texture(texture) && all);
        buffer.lock().unwrap_or_else(|e| e.into_inner()).textures_behind = !all;
    }
    RETURN_SUCCESS
}

/// The pixels, or for a planar buffer its plane table (as on iOS).
extern "C" fn pixel_buffer_base_address(object: Id) -> usize {
    with_buffer(object, |b| b.base() as usize).unwrap_or(0)
}

/// Row bytes; for a planar buffer (as CVPixelBuffer.h says) a value that
/// times the height covers the whole buffer, every plane included.
fn bytes_per_row(b: &PixelBuffer) -> usize {
    if b.planes.len() == 1 {
        return b.planes[0].stride;
    }
    let end = b.planes.iter().map(|p| p.offset + p.stride * p.height).max().unwrap_or(0);
    end.div_ceil(b.height.max(1))
}

extern "C" fn pixel_buffer_bytes_per_row(object: Id) -> usize {
    with_buffer(object, |b| bytes_per_row(b)).unwrap_or(0)
}

extern "C" fn pixel_buffer_width(object: Id) -> usize {
    with_buffer(object, |b| b.width).unwrap_or(0)
}

extern "C" fn pixel_buffer_height(object: Id) -> usize {
    with_buffer(object, |b| b.height).unwrap_or(0)
}

extern "C" fn pixel_buffer_format(object: Id) -> u32 {
    with_buffer(object, |b| b.format).unwrap_or(0)
}

extern "C" fn pixel_buffer_data_size(object: Id) -> usize {
    with_buffer(object, |b| b.planes.iter().map(|p| p.offset + p.stride * p.height).max().unwrap_or(0)).unwrap_or(0)
}

extern "C" fn pixel_buffer_is_planar(object: Id) -> u8 {
    with_buffer(object, |b| (b.planes.len() > 1) as u8).unwrap_or(0)
}

extern "C" fn pixel_buffer_plane_count(object: Id) -> usize {
    with_buffer(object, |b| if b.planes.len() > 1 { b.planes.len() } else { 0 }).unwrap_or(0)
}

fn plane_of(object: Id, index: usize) -> Option<(usize, Plane)> {
    with_buffer(object, |b| b.planes.get(index).map(|p| (b.base() as usize + p.offset, *p))).flatten()
}

extern "C" fn pixel_buffer_plane_base_address(object: Id, index: usize) -> usize {
    plane_of(object, index).map(|(a, _)| a).unwrap_or(0)
}

extern "C" fn pixel_buffer_plane_bytes_per_row(object: Id, index: usize) -> usize {
    plane_of(object, index).map(|(_, p)| p.stride).unwrap_or(0)
}

extern "C" fn pixel_buffer_plane_width(object: Id, index: usize) -> usize {
    plane_of(object, index).map(|(_, p)| p.width).unwrap_or(0)
}

extern "C" fn pixel_buffer_plane_height(object: Id, index: usize) -> usize {
    plane_of(object, index).map(|(_, p)| p.height).unwrap_or(0)
}

extern "C" fn buffer_retain(object: Id) -> Id {
    if object != 0 {
        retained(object);
    }
    object
}

extern "C" fn buffer_release(object: Id) {
    if object != 0 {
        released(object);
    }
}

// ----- CVOpenGLESTextureCache -----

struct Texture {
    name: u32,
    target: Enum,
    /// The EGL context current when the name was made (it owns the name).
    context: usize,
    /// The pixel buffer it shows (0 once that is gone) and which plane.
    buffer: Id,
    plane: usize,
    internal: Enum,
    format: Enum,
    kind: Enum,
    width: i32,
    height: i32,
}

/// Texture caches and the EAGL context each was made for.
static CACHES: Mutex<Option<HashMap<Id, Id>>> = Mutex::new(None);
static TEXTURES: Mutex<Option<HashMap<Id, Texture>>> = Mutex::new(None);

gl::system! {
    glGenTextures(count: i32, out: *mut u32);
    glDeleteTextures(count: i32, names: *const u32);
}

const TEXTURE_2D: Enum = 0x0DE1;
const TEXTURE_BINDING_2D: Enum = 0x8069;
const FRAMEBUFFER: Enum = 0x8D40;
const FRAMEBUFFER_BINDING: Enum = 0x8CA6;
const FRAMEBUFFER_COMPLETE: Enum = 0x8CD5;
const COLOR_ATTACHMENT0: Enum = 0x8CE0;
const PIXEL_PACK_BUFFER: Enum = 0x88EB;
const PIXEL_PACK_BUFFER_BINDING: Enum = 0x88ED;
const PIXEL_UNPACK_BUFFER: Enum = 0x88EC;
const PIXEL_UNPACK_BUFFER_BINDING: Enum = 0x88EF;
const PACK_ALIGNMENT: Enum = 0x0D05;
const UNPACK_ALIGNMENT: Enum = 0x0CF5;
const UNPACK_ROW_LENGTH: Enum = 0x0CF2;
const UNPACK_SKIP_ROWS: Enum = 0x0CF3;
const UNPACK_SKIP_PIXELS: Enum = 0x0CF4;
const PACK_ROW_LENGTH: Enum = 0x0D02;
const TEXTURE_MIN_FILTER: Enum = 0x2801;
const TEXTURE_MAG_FILTER: Enum = 0x2800;
const TEXTURE_WRAP_S: Enum = 0x2802;
const TEXTURE_WRAP_T: Enum = 0x2803;
const LINEAR: i32 = 0x2601;
const CLAMP_TO_EDGE: i32 = 0x812F;
const UNSIGNED_BYTE: Enum = 0x1401;
const UNSIGNED_SHORT_565: Enum = 0x8363;
const RGBA: Enum = 0x1908;
const RGB: Enum = 0x1907;
const BGRA: Enum = 0x80E1;
const RED: Enum = 0x1903;
const RG: Enum = 0x8227;
const LUMINANCE: Enum = 0x1909;
const LUMINANCE_ALPHA: Enum = 0x190A;
const ALPHA: Enum = 0x1906;

/// Bytes per pixel a GL format/type reads, or None if not one we upload.
fn gl_pixel_size(format: Enum, kind: Enum) -> Option<usize> {
    match (format, kind) {
        (RGBA | BGRA, UNSIGNED_BYTE) => Some(4),
        (RGB, UNSIGNED_BYTE) => Some(3),
        (RGB, UNSIGNED_SHORT_565) => Some(2),
        (RG | LUMINANCE_ALPHA, UNSIGNED_BYTE) => Some(2),
        (RED | LUMINANCE | ALPHA, UNSIGNED_BYTE) => Some(1),
        _ => None,
    }
}

/// The pixels of `plane` (width x height of it), tightly packed in what GL
/// is given: BGRA becomes RGBA (standard GL ES has no BGRA upload).
pub fn packed_pixels(source: &[u8], plane: &Plane, width: usize, height: usize, format: Enum) -> Vec<u8> {
    let size = plane.bytes_per_pixel;
    let mut out = Vec::with_capacity(width * height * size);
    for row in 0..height {
        let start = plane.offset + row * plane.stride;
        let line = &source[start..start + width * size];
        if format == BGRA && size == 4 {
            for p in line.chunks_exact(4) {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// The reverse of `packed_pixels`: tight RGBA rows read from GL written into
/// the plane (as BGRA when the texture's outside format is BGRA).
pub fn store_pixels(rgba: &[u8], plane: &Plane, width: usize, height: usize, format: Enum, target: &mut [u8]) {
    for row in 0..height {
        let start = plane.offset + row * plane.stride;
        let line = &mut target[start..start + width * 4];
        let from = &rgba[row * width * 4..(row + 1) * width * 4];
        if format == BGRA {
            for (to, p) in line.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                to.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        } else {
            line.copy_from_slice(from);
        }
    }
}

/// Runs `f` with the unpack state set for tightly packed bytes, then puts
/// the game's state back.
unsafe fn with_plain_unpack(f: impl FnOnce()) {
    let mut saved = [0i32; 5];
    for (slot, name) in saved.iter_mut().zip([UNPACK_ALIGNMENT, UNPACK_ROW_LENGTH, UNPACK_SKIP_ROWS, UNPACK_SKIP_PIXELS, PIXEL_UNPACK_BUFFER_BINDING]) {
        gl::glGetIntegerv(name, slot);
    }
    gl::glBindBuffer(PIXEL_UNPACK_BUFFER, 0);
    gl::glPixelStorei(UNPACK_ALIGNMENT, 1);
    gl::glPixelStorei(UNPACK_ROW_LENGTH, 0);
    gl::glPixelStorei(UNPACK_SKIP_ROWS, 0);
    gl::glPixelStorei(UNPACK_SKIP_PIXELS, 0);
    f();
    gl::glPixelStorei(UNPACK_ALIGNMENT, saved[0]);
    gl::glPixelStorei(UNPACK_ROW_LENGTH, saved[1]);
    gl::glPixelStorei(UNPACK_SKIP_ROWS, saved[2]);
    gl::glPixelStorei(UNPACK_SKIP_PIXELS, saved[3]);
    gl::glBindBuffer(PIXEL_UNPACK_BUFFER, saved[4] as u32);
}

fn has_context() -> bool {
    unsafe { gl::eglGetCurrentContext() != 0 }
}

/// Gives texture `object` its pixel buffer's current pixels; `false` if
/// there is no GL context to do it with.
fn upload_texture(object: Id) -> bool {
    let Some((name, target, buffer, plane, internal, format, kind, width, height)) = TEXTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|t| t.get(&object))
        .map(|t| (t.name, t.target, t.buffer, t.plane, t.internal, t.format, t.kind, t.width, t.height))
    else {
        return true;
    };
    let Some(buffer) = buffer_of(buffer) else { return true };
    if !has_context() {
        crate::log!("CoreVideo: no GL context on this thread; texture {name} keeps its old pixels");
        return false;
    }
    let b = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = b.planes.get(plane).copied() else { return true };
    let pixels = packed_pixels(b.bytes(), &p, width as usize, height as usize, format);
    drop(b);
    let (upload_internal, upload_format) = if format == BGRA { (RGBA, RGBA) } else { (internal, format) };
    unsafe {
        let mut bound = 0;
        gl::glGetIntegerv(TEXTURE_BINDING_2D, &mut bound);
        gl::glBindTexture(target, name);
        with_plain_unpack(|| gl::glTexImage2D(target, 0, upload_internal as i32, width, height, 0, upload_format, kind, pixels.as_ptr()));
        gl::glBindTexture(target, bound as u32);
    }
    true
}

/// Before the game reads a buffer: pixels drawn into its (4-byte) texture
/// are copied back into it, unless the buffer holds newer pixels the
/// texture never got.
fn read_back(b: &mut PixelBuffer) {
    if b.textures.is_empty() || b.textures_behind || !has_context() {
        return;
    }
    let found = {
        let textures = TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
        textures.as_ref().and_then(|t| t.get(&b.textures[0])).map(|t| (t.name, t.target, t.plane, t.format, t.kind, t.width, t.height))
    };
    let Some((name, target, plane, format, kind, width, height)) = found else { return };
    if !matches!(format, RGBA | BGRA) || kind != UNSIGNED_BYTE || target != TEXTURE_2D {
        return;
    }
    let Some(p) = b.planes.get(plane).copied() else { return };
    if p.bytes_per_pixel != 4 {
        return;
    }
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let mut saved = [0i32; 4];
        for (slot, name) in saved.iter_mut().zip([FRAMEBUFFER_BINDING, PIXEL_PACK_BUFFER_BINDING, PACK_ALIGNMENT, PACK_ROW_LENGTH]) {
            gl::glGetIntegerv(name, slot);
        }
        let mut framebuffer = 0;
        gl::glGenFramebuffers(1, &mut framebuffer);
        gl::glBindFramebuffer(FRAMEBUFFER, framebuffer);
        gl::glFramebufferTexture2D(FRAMEBUFFER, COLOR_ATTACHMENT0, target, name, 0);
        let complete = gl::glCheckFramebufferStatus(FRAMEBUFFER) == FRAMEBUFFER_COMPLETE;
        if complete {
            gl::glBindBuffer(PIXEL_PACK_BUFFER, 0);
            gl::glPixelStorei(PACK_ALIGNMENT, 1);
            gl::glPixelStorei(PACK_ROW_LENGTH, 0);
            gl::glReadPixels(0, 0, width, height, RGBA, UNSIGNED_BYTE, rgba.as_mut_ptr());
        }
        gl::glBindFramebuffer(FRAMEBUFFER, saved[0] as u32);
        gl::glDeleteFramebuffers(1, &framebuffer);
        gl::glBindBuffer(PIXEL_PACK_BUFFER, saved[1] as u32);
        gl::glPixelStorei(PACK_ALIGNMENT, saved[2]);
        gl::glPixelStorei(PACK_ROW_LENGTH, saved[3]);
        if !complete {
            return;
        }
    }
    store_pixels(&rgba, &p, width as usize, height as usize, format, b.bytes_mut());
}

/// CVOpenGLESTextureCacheCreate(allocator, attributes, context, textureAttributes, *out).
extern "C" fn cache_create(_allocator: usize, _attributes: Id, context: Id, _texture_attributes: Id, out: *mut Id) -> i32 {
    if out.is_null() || context == 0 {
        return RETURN_INVALID_ARGUMENT;
    }
    let cache = new_object("CVOpenGLESTextureCache");
    CACHES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(cache, context);
    unsafe { *out = cache };
    RETURN_SUCCESS
}

/// CVOpenGLESTextureCacheCreateTextureFromImage(allocator, cache, image,
/// attributes, target, internalFormat, width, height, format, type,
/// planeIndex, *out). Apple packs format and type into one stack slot.
extern "C" fn cache_create_texture(
    _allocator: usize,
    cache: Id,
    image: Id,
    _attributes: Id,
    target: Enum,
    internal: i32,
    width: i32,
    height: i32,
    format_and_type: u64,
    plane: usize,
    out: *mut Id,
) -> i32 {
    let (format, kind) = (format_and_type as u32, (format_and_type >> 32) as u32);
    if out.is_null() || width <= 0 || height <= 0 {
        return RETURN_INVALID_ARGUMENT;
    }
    unsafe { *out = 0 };
    let known_cache = CACHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.contains_key(&cache));
    let Some(buffer) = buffer_of(image) else { return RETURN_INVALID_ARGUMENT };
    if !known_cache {
        return RETURN_INVALID_ARGUMENT;
    }
    {
        let b = buffer.lock().unwrap_or_else(|e| e.into_inner());
        let fits = b.planes.get(plane).is_some_and(|p| {
            gl_pixel_size(format, kind) == Some(p.bytes_per_pixel) && width as usize <= p.width && height as usize <= p.height
        });
        if !fits {
            crate::log!(
                "CoreVideo: a {width}x{height} texture (format {format:#x}/{kind:#x}) can't be made from plane {plane} of a {} {}x{} buffer",
                crate::fw::sound::four_cc(b.format),
                b.width,
                b.height
            );
            return RETURN_NOT_OPENGL_COMPATIBLE;
        }
    }
    if !has_context() {
        crate::log!("CoreVideo: a texture was asked for with no GL context on this thread");
        return RETURN_INVALID_ARGUMENT;
    }
    delete_waiting_names();
    let context = unsafe { gl::eglGetCurrentContext() } as usize;
    let mut name = 0;
    unsafe {
        glGenTextures(1, &mut name);
        let mut bound = 0;
        gl::glGetIntegerv(TEXTURE_BINDING_2D, &mut bound);
        gl::glBindTexture(target, name);
        gl::glTexParameteri(target, TEXTURE_MIN_FILTER, LINEAR);
        gl::glTexParameteri(target, TEXTURE_MAG_FILTER, LINEAR);
        gl::glTexParameteri(target, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
        gl::glTexParameteri(target, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
        gl::glBindTexture(target, bound as u32);
    }
    let texture = new_object("CVOpenGLESTexture");
    TEXTURES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
        texture,
        Texture { name, target, context, buffer: image, plane, internal: internal as Enum, format, kind, width, height },
    );
    buffer.lock().unwrap_or_else(|e| e.into_inner()).textures.push(texture);
    if upload_texture(texture) {
        let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
        if b.textures.len() == 1 {
            b.textures_behind = false;
        }
    }
    unsafe { *out = texture };
    RETURN_SUCCESS
}

/// CVOpenGLESTextureCacheFlush: textures released since the last flush go
/// (those made in another context wait until a flush runs in that one).
extern "C" fn cache_flush(_cache: Id, _options: u64) {
    delete_waiting_names();
}

extern "C" fn texture_name(texture: Id) -> u32 {
    TEXTURES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|t| t.get(&texture)).map(|t| t.name).unwrap_or(0)
}

extern "C" fn texture_target(texture: Id) -> Enum {
    TEXTURES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|t| t.get(&texture)).map(|t| t.target).unwrap_or(0)
}

/// The buffer's first row is the texture's first row (texture coordinate
/// t = 0), so pictures are not upside down.
extern "C" fn texture_is_flipped(_texture: Id) -> u8 {
    0
}

/// CVOpenGLESTextureGetCleanTexCoords: the whole texture.
extern "C" fn texture_clean_coords(_texture: Id, lower_left: *mut f32, lower_right: *mut f32, upper_right: *mut f32, upper_left: *mut f32) {
    for (pointer, value) in [(lower_left, [0.0, 0.0]), (lower_right, [1.0, 0.0]), (upper_right, [1.0, 1.0]), (upper_left, [0.0, 1.0])] {
        if !pointer.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), pointer, 2) };
        }
    }
}

// ----- CMSampleBuffer, CMBlockBuffer, CMFormatDescription -----

/// What a sample buffer holds: a picture, or a run of sound frames.
pub struct Sample {
    pub image: Id,
    pub time: CmTime,
    pub duration: CmTime,
    pub frames: usize,
    pub data: Arc<Vec<u8>>,
    pub format: Option<Arc<Asbd>>,
    /// The block buffer and format description objects, made when asked for
    /// (the sample owns one count of each).
    block: Id,
    description: Id,
}

use crate::fw::audiofile::Asbd;

/// A block buffer or format description: shares its sample's bytes and
/// format, so it stays good if the game keeps it after the sample is gone.
struct Part {
    data: Arc<Vec<u8>>,
    format: Option<Arc<Asbd>>,
    width: u32,
    height: u32,
}

static SAMPLES: Mutex<Option<HashMap<Id, Sample>>> = Mutex::new(None);

/// A sample buffer showing `image` at `time` (owned once by the caller).
pub fn make_image_sample(image: Id, time: CmTime, duration: CmTime) -> Id {
    retained(image);
    make_sample(Sample { image, time, duration, frames: 1, data: Arc::default(), format: None, block: 0, description: 0 })
}

/// A sample buffer of `samples` (frames for PCM, packets otherwise) holding
/// `frames` sound frames in `format` starting at `time`.
pub fn make_audio_sample(data: Vec<u8>, format: Asbd, samples: usize, frames: usize, time: CmTime) -> Id {
    let duration = CmTime::new(frames as i64, format.rate.round().clamp(1.0, i32::MAX as f64) as i32);
    make_sample(Sample { image: 0, time, duration, frames: samples, data: Arc::new(data), format: Some(Arc::new(format)), block: 0, description: 0 })
}

fn make_sample(sample: Sample) -> Id {
    let object = new_object("CMSampleBuffer");
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(object, sample);
    object
}

fn with_sample<R>(object: Id, f: impl FnOnce(&mut Sample) -> R) -> Option<R> {
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.get_mut(&object).map(f)
}

extern "C" fn sample_image_buffer(sample: Id) -> Id {
    with_sample(sample, |s| s.image).unwrap_or(0)
}

extern "C" fn sample_presentation_time(sample: Id) -> CmTime {
    with_sample(sample, |s| s.time).unwrap_or(CmTime::INVALID)
}

extern "C" fn sample_duration(sample: Id) -> CmTime {
    with_sample(sample, |s| s.duration).unwrap_or(CmTime::INVALID)
}

extern "C" fn sample_count(sample: Id) -> i64 {
    with_sample(sample, |s| s.frames as i64).unwrap_or(0)
}

extern "C" fn sample_is_valid(sample: Id) -> u8 {
    with_sample(sample, |_| 1).unwrap_or(0)
}

static PARTS: Mutex<Option<HashMap<Id, Part>>> = Mutex::new(None);

/// The sample's block buffer or format description, made on first ask.
fn part(sample: Id, which: fn(&mut Sample) -> &mut Id, class: &str) -> Id {
    let Some((existing, data, format, image)) = with_sample(sample, |s| (*which(s), s.data.clone(), s.format.clone(), s.image)) else { return 0 };
    if existing != 0 {
        return existing;
    }
    let (width, height) = with_buffer(image, |b| (b.width as u32, b.height as u32)).unwrap_or((0, 0));
    let object = new_object(class);
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(object, Part { data, format, width, height });
    with_sample(sample, |s| *which(s) = object);
    object
}

fn with_part<R>(object: Id, f: impl FnOnce(&Part) -> R) -> Option<R> {
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&object).map(f)
}

/// CMSampleBufferGetDataBuffer: sound samples have one; pictures don't.
extern "C" fn sample_data_buffer(sample: Id) -> Id {
    if with_sample(sample, |s| s.format.is_none()).unwrap_or(true) {
        return 0;
    }
    part(sample, |s| &mut s.block, "CMBlockBuffer")
}

extern "C" fn sample_format_description(sample: Id) -> Id {
    if with_sample(sample, |_| ()).is_none() {
        return 0;
    }
    part(sample, |s| &mut s.description, "CMFormatDescription")
}

extern "C" fn block_data_length(block: Id) -> usize {
    with_part(block, |p| p.data.len()).unwrap_or(0)
}

/// CMBlockBufferCopyDataBytes(block, offset, length, destination).
extern "C" fn block_copy_bytes(block: Id, offset: usize, length: usize, destination: *mut u8) -> i32 {
    const BAD_OFFSET: i32 = -12762;
    if destination.is_null() {
        return -12760;
    }
    with_part(block, |s| match s.data.get(offset..offset.saturating_add(length)) {
        Some(bytes) => {
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len()) };
            0
        }
        None => BAD_OFFSET,
    })
    .unwrap_or(-12760)
}

/// CMBlockBufferGetDataPointer(block, offset, *lengthAtOffset, *totalLength, *pointer).
extern "C" fn block_data_pointer(block: Id, offset: usize, at_offset: *mut usize, total: *mut usize, pointer: *mut usize) -> i32 {
    with_part(block, |s| {
        if offset > s.data.len() {
            return -12762;
        }
        unsafe {
            if !at_offset.is_null() {
                *at_offset = s.data.len() - offset;
            }
            if !total.is_null() {
                *total = s.data.len();
            }
            if !pointer.is_null() {
                *pointer = s.data.as_ptr() as usize + offset;
            }
        }
        0
    })
    .unwrap_or(-12760)
}

/// CMAudioFormatDescriptionGetStreamBasicDescription: good while the description lives.
extern "C" fn audio_description_basic(description: Id) -> usize {
    with_part(description, |p| p.format.as_ref().map(|f| Arc::as_ptr(f) as usize).unwrap_or(0)).unwrap_or(0)
}

/// CMFormatDescriptionGetMediaType: 'soun' or 'vide'.
extern "C" fn description_media_type(description: Id) -> u32 {
    with_part(description, |p| u32::from_be_bytes(if p.format.is_some() { *b"soun" } else { *b"vide" })).unwrap_or(0)
}

/// CMVideoFormatDescriptionGetDimensions: {width, height} as two int32s.
extern "C" fn video_description_dimensions(description: Id) -> u64 {
    with_part(description, |p| p.width as u64 | (p.height as u64) << 32).unwrap_or(0)
}

pub fn register(r: &mut Registry) {
    for name in ["CVPixelBuffer", "CVOpenGLESTextureCache", "CVOpenGLESTexture", "CMSampleBuffer", "CMBlockBuffer", "CMFormatDescription"] {
        define_class(name, "NSObject", 8);
    }
    r.function("CMTimeMake", time_make as usize);
    r.function("CMTimeMakeWithSeconds", time_make_with_seconds as usize);
    r.function("CMTimeMakeWithEpoch", time_make_with_epoch as usize);
    r.function("CMTimeCompare", time_compare as usize);
    r.function("CMTimeAdd", time_add as usize);
    r.function("CMTimeSubtract", time_subtract as usize);
    r.function("CMTimeConvertScale", time_convert_scale as usize);
    r.function("CMTimeRangeMake", time_range_make as usize);
    r.function("CMTimeRangeGetEnd", time_range_get_end as usize);
    r.data("kCMTimeInvalid", leak(CmTime::INVALID));
    r.data("kCMTimeIndefinite", leak(CmTime { flags: TIME_VALID | TIME_INDEFINITE, ..CmTime::INVALID }));
    r.data("kCMTimePositiveInfinity", leak(CmTime { flags: TIME_VALID | TIME_POSITIVE_INFINITY, ..CmTime::INVALID }));
    r.data("kCMTimeNegativeInfinity", leak(CmTime { flags: TIME_VALID | TIME_NEGATIVE_INFINITY, ..CmTime::INVALID }));
    r.data("kCMTimeRangeZero", leak(CmTimeRange { start: CmTime::new(0, 1), duration: CmTime::new(0, 1) }));

    r.function("CMSampleBufferGetImageBuffer", sample_image_buffer as usize);
    r.function("CMSampleBufferGetPresentationTimeStamp", sample_presentation_time as usize);
    r.function("CMSampleBufferGetOutputPresentationTimeStamp", sample_presentation_time as usize);
    r.function("CMSampleBufferGetDuration", sample_duration as usize);
    r.function("CMSampleBufferGetNumSamples", sample_count as usize);
    r.function("CMSampleBufferIsValid", sample_is_valid as usize);
    r.function("CMSampleBufferDataIsReady", sample_is_valid as usize);
    r.function("CMSampleBufferGetDataBuffer", sample_data_buffer as usize);
    r.function("CMSampleBufferGetFormatDescription", sample_format_description as usize);
    r.function("CMBlockBufferGetDataLength", block_data_length as usize);
    r.function("CMBlockBufferCopyDataBytes", block_copy_bytes as usize);
    r.function("CMBlockBufferGetDataPointer", block_data_pointer as usize);
    r.function("CMAudioFormatDescriptionGetStreamBasicDescription", audio_description_basic as usize);
    r.function("CMFormatDescriptionGetMediaType", description_media_type as usize);
    r.function("CMVideoFormatDescriptionGetDimensions", video_description_dimensions as usize);

    r.function("CVPixelBufferCreate", pixel_buffer_create as usize);
    r.function("CVPixelBufferLockBaseAddress", pixel_buffer_lock as usize);
    r.function("CVPixelBufferUnlockBaseAddress", pixel_buffer_unlock as usize);
    r.function("CVPixelBufferGetBaseAddress", pixel_buffer_base_address as usize);
    r.function("CVPixelBufferGetBytesPerRow", pixel_buffer_bytes_per_row as usize);
    r.function("CVPixelBufferGetWidth", pixel_buffer_width as usize);
    r.function("CVPixelBufferGetHeight", pixel_buffer_height as usize);
    r.function("CVPixelBufferGetPixelFormatType", pixel_buffer_format as usize);
    r.function("CVPixelBufferGetDataSize", pixel_buffer_data_size as usize);
    r.function("CVPixelBufferIsPlanar", pixel_buffer_is_planar as usize);
    r.function("CVPixelBufferGetPlaneCount", pixel_buffer_plane_count as usize);
    r.function("CVPixelBufferGetBaseAddressOfPlane", pixel_buffer_plane_base_address as usize);
    r.function("CVPixelBufferGetBytesPerRowOfPlane", pixel_buffer_plane_bytes_per_row as usize);
    r.function("CVPixelBufferGetWidthOfPlane", pixel_buffer_plane_width as usize);
    r.function("CVPixelBufferGetHeightOfPlane", pixel_buffer_plane_height as usize);
    r.function("CVPixelBufferRetain", buffer_retain as usize);
    r.function("CVPixelBufferRelease", buffer_release as usize);
    r.function("CVBufferRetain", buffer_retain as usize);
    r.function("CVBufferRelease", buffer_release as usize);

    r.function("CVOpenGLESTextureCacheCreate", cache_create as usize);
    r.function("CVOpenGLESTextureCacheCreateTextureFromImage", cache_create_texture as usize);
    r.function("CVOpenGLESTextureCacheFlush", cache_flush as usize);
    r.function("CVOpenGLESTextureGetName", texture_name as usize);
    r.function("CVOpenGLESTextureGetTarget", texture_target as usize);
    r.function("CVOpenGLESTextureIsFlipped", texture_is_flipped as usize);
    r.function("CVOpenGLESTextureGetCleanTexCoords", texture_clean_coords as usize);

    // Apple's own key texts.
    for (name, value) in [
        ("kCVPixelBufferPixelFormatTypeKey", "PixelFormatType"),
        ("kCVPixelBufferWidthKey", "Width"),
        ("kCVPixelBufferHeightKey", "Height"),
        ("kCVPixelBufferBytesPerRowAlignmentKey", "BytesPerRowAlignment"),
        ("kCVPixelBufferIOSurfacePropertiesKey", "IOSurfaceProperties"),
        ("kCVPixelBufferOpenGLESCompatibilityKey", "OpenGLESCompatibility"),
        ("kCVPixelBufferMetalCompatibilityKey", "MetalCompatibility"),
        ("kCVPixelBufferCGImageCompatibilityKey", "CGImageCompatibility"),
        ("kCVPixelBufferCGBitmapContextCompatibilityKey", "CGBitmapContextCompatibility"),
    ] {
        r.data(name, leak(string::make(value)));
    }
    let _ = collections::number_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_compare_and_round_like_ios() {
        let half = CmTime::new(1, 2);
        let thirds = CmTime::new(2, 3);
        assert_eq!(compare_times(&half, &thirds), -1);
        assert_eq!(compare_times(&CmTime::new(300, 600), &half), 0, "same time, other timescale");
        let infinite = CmTime { flags: TIME_VALID | TIME_POSITIVE_INFINITY, ..CmTime::INVALID };
        let indefinite = CmTime { flags: TIME_VALID | TIME_INDEFINITE, ..CmTime::INVALID };
        let minus = CmTime { flags: TIME_VALID | TIME_NEGATIVE_INFINITY, ..CmTime::INVALID };
        assert_eq!(compare_times(&indefinite, &infinite), -1);
        assert_eq!(compare_times(&CmTime::INVALID, &infinite), 1, "invalid is the largest");
        assert_eq!(compare_times(&minus, &CmTime::new(i64::MIN, 1)), -1);
        assert_eq!(compare_times(&CmTime { epoch: 1, ..half }, &CmTime::new(5, 1)), 1, "a later epoch is later");

        let t = time_from_seconds(1.5, 600);
        assert_eq!((t.value, t.timescale, t.flags), (900, 600, TIME_VALID));
        let t = time_from_seconds(0.1234, 600);
        assert_eq!((t.value, t.flags & TIME_ROUNDED), (74, TIME_ROUNDED), "74.04 rounds to 74");
        assert_eq!(time_from_seconds(f64::NAN, 600).flags & TIME_VALID, 0);
        let big = time_from_seconds(1e17, 1_000_000_000);
        assert!(big.timescale < 1_000_000_000 && (big.seconds() - 1e17).abs() < 1e3, "{big:?}");
        let sum = combine(&half, &thirds, false);
        assert_eq!((sum.value, sum.timescale, sum.flags), (7, 6, TIME_VALID), "1/2 + 2/3 = 7/6 exactly");
        let difference = combine(&half, &thirds, true);
        assert_eq!((difference.value, difference.timescale), (-1, 6));
        // Past timescale 1 a time is infinite; sums too big halve, then overflow to infinity.
        assert_eq!(time_from_seconds(-1e30, 600), CmTime::NEGATIVE_INFINITY);
        let huge = CmTime::new(i64::MAX, 2);
        let doubled = combine(&huge, &huge, false);
        assert_eq!((doubled.value, doubled.timescale, doubled.flags), (i64::MAX, 1, TIME_VALID), "{doubled:?}");
        assert_eq!(combine(&CmTime::new(i64::MAX, 1), &CmTime::new(1, 1), false), CmTime::POSITIVE_INFINITY);
        // Epochs: a duration (epoch 0) moves a time within its epoch; two times
        // of one epoch differ by a duration; different epochs don't mix.
        let later = combine(&CmTime { epoch: 3, ..half }, &half, false);
        assert_eq!((later.value, later.timescale, later.epoch), (2, 2, 3), "{later:?}");
        assert_eq!(combine(&CmTime { epoch: 3, ..thirds }, &CmTime { epoch: 3, ..half }, true).epoch, 0);
        assert_eq!(combine(&CmTime { epoch: 1, ..half }, &CmTime { epoch: 2, ..half }, false), CmTime::INVALID);
        // Infinities win over indefinite; opposite infinities and timescale 0 are invalid.
        assert_eq!(combine(&infinite, &infinite, true), CmTime::INVALID);
        assert_eq!(combine(&indefinite, &infinite, false), CmTime::POSITIVE_INFINITY);
        assert_eq!(combine(&indefinite, &half, false), CmTime::INDEFINITE);
        // The exact sum is rounded once (not each side on its own).
        let fine = combine(&half, &CmTime::new(1, 1_073_741_825), false);
        assert_eq!((fine.value, fine.timescale), (1_073_741_825, i32::MAX), "{fine:?}");
        // Seconds that fit in 64 bits stay finite.
        assert_eq!(time_from_seconds(9.21e18, 1).value, 9_210_000_000_000_000_000);
        let broken = CmTime { value: 5, timescale: 0, flags: TIME_VALID, epoch: 0 };
        assert_eq!(combine(&broken, &half, false), CmTime::INVALID);
        assert_eq!(time_convert_scale(broken, 600, 0), CmTime::INVALID);
        // CMTimeConvertScale rounds by the method asked for and says so.
        let t = CmTime::new(-5, 4); // -1.25 s
        // Apple's kCMTimeRoundingMethod numbers, literally: 1 half away from
        // zero, 2 toward zero, 3 away from zero, 4 QuickTime, 5 toward +inf, 6 toward -inf.
        let at = |method| time_convert_scale(t, 2, method).value;
        assert_eq!((at(1), at(2), at(3), at(5), at(6)), (-3, -2, -3, -2, -3));
        assert_eq!(time_convert_scale(t, 2, 5).flags, TIME_VALID | TIME_ROUNDED);
        assert_eq!(time_convert_scale(t, 8, 1), CmTime::new(-10, 8), "exact: not rounded");
        // QuickTime: toward zero to a coarser timescale (a negative time
        // never becomes 0, at least -1), away from zero to a finer one.
        assert_eq!(at(4), -2);
        assert_eq!(time_convert_scale(CmTime::new(-1, 4), 2, 4).value, -1);
        assert_eq!(time_convert_scale(CmTime::new(1, 4), 2, 4).value, 0);
        assert_eq!(time_convert_scale(CmTime::new(1, 3), 4, 4).value, 2);
    }

    #[test]
    fn pixel_planes_and_bgra_round_trip() {
        let planes = layout(FORMAT_420V, 33, 17).unwrap();
        assert_eq!(planes[0].stride, 64);
        assert_eq!((planes[1].width, planes[1].height, planes[1].offset), (17, 9, 64 + 64 * 17));
        assert_eq!(planes[1].stride, 64);
        // A planar buffer's base address holds CVPlanarPixelBufferInfo_YCbCrBiPlanar.
        let planar = PixelBuffer::new(FORMAT_420V, 33, 17).unwrap();
        let header: Vec<u32> = planar.bytes()[..16].chunks_exact(4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).collect();
        assert_eq!(header, [64, 64, 64 + 64 * 17, 64]);
        // Its bytes per row times the height covers every plane.
        let end = planar.planes[1].offset + planar.planes[1].stride * planar.planes[1].height;
        assert!(bytes_per_row(&planar) * 17 >= end && bytes_per_row(&planar) * 16 < end);
        // Texture names are deleted only in the context they were made in.
        let mut waiting = vec![(1, 0xA), (2, 0xB), (3, 0xA)];
        assert!(take_owned(&mut waiting, 0).is_empty(), "no context: nothing deleted");
        assert_eq!(take_owned(&mut waiting, 0xA), [1, 3]);
        assert_eq!(waiting, [(2, 0xB)]);
        let mut buffer = PixelBuffer::new(FORMAT_BGRA, 3, 2).unwrap();
        let plane = buffer.planes[0];
        assert_eq!(plane.stride, 64);
        for (i, byte) in buffer.bytes_mut().iter_mut().enumerate() {
            *byte = i as u8;
        }
        let rgba = packed_pixels(buffer.bytes(), &plane, 3, 2, BGRA);
        assert_eq!(&rgba[..4], &[2, 1, 0, 3]);
        assert_eq!(&rgba[12..16], &[66, 65, 64, 67], "second row starts at the stride");
        let mut back = vec![0u8; buffer.bytes().len()];
        store_pixels(&rgba, &plane, 3, 2, BGRA, &mut back);
        assert_eq!(&back[..12], &buffer.bytes()[..12]);
        assert_eq!(&back[64..76], &buffer.bytes()[64..76]);
    }
}
