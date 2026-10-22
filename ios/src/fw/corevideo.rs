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

    pub fn is_numeric(&self) -> bool {
        self.flags & TIME_VALID != 0 && self.flags & (TIME_POSITIVE_INFINITY | TIME_NEGATIVE_INFINITY | TIME_INDEFINITE) == 0
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

/// CMTimeMakeWithSeconds: the nearest value in `timescale`; if it doesn't
/// fit, the timescale is halved until it does (as iOS does). Not-a-number
/// is an invalid time, infinities are the infinite times.
pub fn time_from_seconds(seconds: f64, timescale: i32) -> CmTime {
    if seconds.is_nan() || timescale <= 0 {
        return CmTime::INVALID;
    }
    if seconds.is_infinite() {
        let flags = TIME_VALID | if seconds > 0.0 { TIME_POSITIVE_INFINITY } else { TIME_NEGATIVE_INFINITY };
        return CmTime { value: 0, timescale: 0, flags, epoch: 0 };
    }
    let mut scale = timescale;
    loop {
        let exact = seconds * scale as f64;
        if exact.abs() < 9.2e18 || scale == 1 {
            let value = exact.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64;
            let rounded = value as f64 != exact || scale != timescale;
            return CmTime { value, timescale: scale, flags: TIME_VALID | if rounded { TIME_ROUNDED } else { 0 }, epoch: 0 };
        }
        scale = (scale / 2).max(1);
    }
}

/// CMTimeCompare's order: negative infinity < numeric times < indefinite <
/// positive infinity < invalid; numeric times by epoch, then exact value.
pub fn compare_times(a: &CmTime, b: &CmTime) -> i32 {
    fn rank(t: &CmTime) -> i32 {
        if t.flags & TIME_VALID == 0 {
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

/// The sum or difference of two numeric times: in a timescale both divide
/// (exact) when it fits, else in the finer one, rounded.
fn combine(a: &CmTime, b: &CmTime, negate: bool) -> CmTime {
    if !a.is_numeric() || !b.is_numeric() {
        // Infinities and invalid times: the "larger" kind wins, as on iOS.
        return if compare_times(a, b) >= 0 { *a } else { *b };
    }
    fn gcd(x: i64, y: i64) -> i64 {
        if y == 0 { x } else { gcd(y, x % y) }
    }
    let (sa, sb) = (a.timescale as i64, b.timescale as i64);
    let common = sa / gcd(sa, sb) * sb;
    let exact = common <= i32::MAX as i64;
    let scale = if exact { common } else { sa.max(sb) } as i128;
    let convert = |t: &CmTime| {
        let wide = t.value as i128 * scale;
        let (whole, rest) = (wide / t.timescale as i128, wide % t.timescale as i128);
        whole + (rest * 2 / t.timescale as i128)
    };
    let other = if negate { -convert(b) } else { convert(b) };
    let sum = convert(a) + other;
    let flags = TIME_VALID | if exact { 0 } else { TIME_ROUNDED };
    CmTime { value: sum.clamp(i64::MIN as i128, i64::MAX as i128) as i64, timescale: scale as i32, flags, epoch: a.epoch }
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

extern "C" fn time_convert_scale(time: CmTime, timescale: i32, _method: u32) -> CmTime {
    if !time.is_numeric() {
        return time;
    }
    time_from_seconds(time.seconds(), timescale)
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
        // Freed at the cache's next flush, on a thread with its context.
        if let Some(cache) = CACHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|c| c.get_mut(&texture.cache)) {
            cache.unused.push(texture.name);
        }
    }
    let sample = SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|s| s.remove(&object));
    if let Some(sample) = sample {
        if sample.image != 0 {
            released(sample.image);
        }
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
            let luma = Plane { offset: 0, width, height, bytes_per_pixel: 1, stride: align(width) };
            let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
            let chroma = Plane { offset: luma.stride * height, width: cw, height: ch, bytes_per_pixel: 2, stride: align(cw * 2) };
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
}

impl PixelBuffer {
    pub fn new(format: u32, width: usize, height: usize) -> Option<PixelBuffer> {
        let planes = layout(format, width, height)?;
        let bytes = planes.iter().map(|p| p.offset + p.stride * p.height).max().unwrap_or(0);
        Some(PixelBuffer { width, height, format, planes, memory: vec![0u128; bytes.div_ceil(16)], locks: 0, textures: Vec::new() })
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
        for texture in textures {
            upload_texture(texture);
        }
    }
    RETURN_SUCCESS
}

extern "C" fn pixel_buffer_base_address(object: Id) -> usize {
    with_buffer(object, |b| b.base() as usize).unwrap_or(0)
}

extern "C" fn pixel_buffer_bytes_per_row(object: Id) -> usize {
    with_buffer(object, |b| if b.planes.len() == 1 { b.planes[0].stride } else { 0 }).unwrap_or(0)
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

struct Cache {
    /// Texture names whose objects were released; deleted at the next flush.
    unused: Vec<u32>,
}

struct Texture {
    name: u32,
    target: Enum,
    cache: Id,
    /// The pixel buffer it shows (0 once that is gone) and which plane.
    buffer: Id,
    plane: usize,
    internal: Enum,
    format: Enum,
    kind: Enum,
    width: i32,
    height: i32,
}

static CACHES: Mutex<Option<HashMap<Id, Cache>>> = Mutex::new(None);
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

/// Gives texture `object` its pixel buffer's current pixels.
fn upload_texture(object: Id) {
    let Some((name, target, buffer, plane, internal, format, kind, width, height)) = TEXTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|t| t.get(&object))
        .map(|t| (t.name, t.target, t.buffer, t.plane, t.internal, t.format, t.kind, t.width, t.height))
    else {
        return;
    };
    let Some(buffer) = buffer_of(buffer) else { return };
    if !has_context() {
        crate::log!("CoreVideo: no GL context on this thread; texture {name} keeps its old pixels");
        return;
    }
    let b = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = b.planes.get(plane).copied() else { return };
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
}

/// Before the game reads a buffer: pixels drawn into its (4-byte) texture
/// are copied back into it.
fn read_back(b: &mut PixelBuffer) {
    if b.textures.is_empty() || !has_context() {
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
    CACHES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(cache, Cache { unused: Vec::new() });
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
        Texture { name, target, cache, buffer: image, plane, internal: internal as Enum, format, kind, width, height },
    );
    buffer.lock().unwrap_or_else(|e| e.into_inner()).textures.push(texture);
    upload_texture(texture);
    unsafe { *out = texture };
    RETURN_SUCCESS
}

/// CVOpenGLESTextureCacheFlush: textures released since the last flush go.
extern "C" fn cache_flush(cache: Id, _options: u64) {
    let unused = CACHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|c| c.get_mut(&cache)).map(|c| std::mem::take(&mut c.unused));
    if let Some(names) = unused {
        if !names.is_empty() && has_context() {
            unsafe { glDeleteTextures(names.len() as i32, names.as_ptr()) };
        } else if !names.is_empty() {
            // No context here: try again at the next flush.
            if let Some(c) = CACHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|c| c.get_mut(&cache)) {
                c.unused.extend(names);
            }
        }
    }
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
    pub data: Vec<u8>,
    pub format: Option<crate::fw::audiofile::Asbd>,
    /// The block buffer and format description objects, made when asked for.
    block: Id,
    description: Id,
}

static SAMPLES: Mutex<Option<HashMap<Id, Sample>>> = Mutex::new(None);

/// A sample buffer showing `image` at `time` (owned once by the caller).
pub fn make_image_sample(image: Id, time: CmTime, duration: CmTime) -> Id {
    retained(image);
    make_sample(Sample { image, time, duration, frames: 1, data: Vec::new(), format: None, block: 0, description: 0 })
}

/// A sample buffer of `frames` sound frames in `format` starting at `time`.
pub fn make_audio_sample(data: Vec<u8>, format: crate::fw::audiofile::Asbd, frames: usize, time: CmTime) -> Id {
    let duration = CmTime::new(frames as i64, format.rate.round().max(1.0) as i32);
    make_sample(Sample { image: 0, time, duration, frames, data, format: Some(format), block: 0, description: 0 })
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

/// Block buffers and format descriptions point back at their sample.
static PARTS: Mutex<Option<HashMap<Id, Id>>> = Mutex::new(None);

fn part(sample: Id, which: fn(&mut Sample) -> &mut Id, class: &str) -> Id {
    let Some(existing) = with_sample(sample, |s| *which(s)) else { return 0 };
    if existing != 0 {
        return existing;
    }
    let object = runtime::alloc_instance(runtime::class_named(class));
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(object, sample);
    with_sample(sample, |s| *which(s) = object);
    object
}

fn sample_of_part(object: Id) -> Id {
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|p| p.get(&object).copied()).unwrap_or(0)
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
    with_sample(sample_of_part(block), |s| s.data.len()).unwrap_or(0)
}

/// CMBlockBufferCopyDataBytes(block, offset, length, destination).
extern "C" fn block_copy_bytes(block: Id, offset: usize, length: usize, destination: *mut u8) -> i32 {
    const BAD_OFFSET: i32 = -12762;
    if destination.is_null() {
        return -12760;
    }
    with_sample(sample_of_part(block), |s| match s.data.get(offset..offset.saturating_add(length)) {
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
    with_sample(sample_of_part(block), |s| {
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

/// CMAudioFormatDescriptionGetStreamBasicDescription: lives as long as the sample.
extern "C" fn audio_description_basic(description: Id) -> usize {
    with_sample(sample_of_part(description), |s| s.format.as_ref().map(|f| f as *const _ as usize).unwrap_or(0)).unwrap_or(0)
}

/// CMFormatDescriptionGetMediaType: 'soun' or 'vide'.
extern "C" fn description_media_type(description: Id) -> u32 {
    with_sample(sample_of_part(description), |s| u32::from_be_bytes(if s.format.is_some() { *b"soun" } else { *b"vide" })).unwrap_or(0)
}

/// CMVideoFormatDescriptionGetDimensions: {width, height} as two int32s.
extern "C" fn video_description_dimensions(description: Id) -> u64 {
    let image = with_sample(sample_of_part(description), |s| s.image).unwrap_or(0);
    with_buffer(image, |b| (b.width as u32 as u64) | ((b.height as u32 as u64) << 32)).unwrap_or(0)
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
    }

    #[test]
    fn pixel_planes_and_bgra_round_trip() {
        let planes = layout(FORMAT_420V, 33, 17).unwrap();
        assert_eq!(planes[0].stride, 64);
        assert_eq!((planes[1].width, planes[1].height, planes[1].offset), (17, 9, 64 * 17));
        assert_eq!(planes[1].stride, 64);
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
