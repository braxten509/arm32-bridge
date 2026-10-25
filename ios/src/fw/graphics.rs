//! Core Graphics drawing into memory bitmaps, and the bridge between UIImage
//! and CGImage.
//!
//! Games draw pictures into bitmaps in memory (gradients, filled boxes,
//! turned photos, screenshots) and upload them to OpenGL themselves, or save
//! them as PNG/JPEG. Everything here works like Apple's: the pixel formats
//! (bitmap info: alpha position, premultiplied alpha, byte order), rows from
//! the top of the picture in memory while drawing coordinates start at the
//! bottom left, the current transform (CTM), and UIKit's image contexts that
//! flip it so (0, 0) is the top left.
//!
//! CG objects are small Objective-C objects ({ isa, *mut state }) so the
//! game can keep them in arrays and send them retain/release. Images,
//! contexts and UIImages count references (fw/counted.rs) and give their
//! pixels back at zero; a released object stays readable as an empty one so
//! a late use can't crash. Any thread may read, retain and release them at
//! once: a picture's pixels are shared read-only, a context is locked while
//! it draws.

use crate::fw::counted;
use crate::fw::geometry::{self, Point, Rect, Size, Transform, IDENTITY};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{data, uikit};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use image::ImageDecoder;
use std::cell::RefCell;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

mod shapes;
mod text;

pub use text::{text_box, text_pictures, TextLook};

// ----- pixel formats -----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Space {
    Rgb,
    Gray,
    /// No colour space: alpha-only bitmaps.
    None,
}

pub const ALPHA_MASK: u32 = 0x1F;
pub const ORDER_MASK: u32 = 0x7000;
pub const ORDER_32_LITTLE: u32 = 2 << 12;
pub const ORDER_32_BIG: u32 = 4 << 12;
const FLOAT_COMPONENTS: u32 = 1 << 8;
pub const ALPHA_NONE: u32 = 0;
pub const PREMULTIPLIED_LAST: u32 = 1;
pub const PREMULTIPLIED_FIRST: u32 = 2;
pub const ALPHA_LAST: u32 = 3;
pub const ALPHA_FIRST: u32 = 4;
pub const NONE_SKIP_LAST: u32 = 5;
pub const NONE_SKIP_FIRST: u32 = 6;
pub const ALPHA_ONLY: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Red,
    Green,
    Blue,
    Gray,
    Alpha,
    Skip,
}

/// Where each 8-bit component of one pixel sits in memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    slots: [Slot; 4],
    pub bytes: usize,
    premultiplied: bool,
}

/// The memory layout of an 8-bit-per-component format, or None when it is
/// not one we can read and write.
pub fn layout(space: Space, bits_per_component: usize, bits_per_pixel: usize, info: u32) -> Option<Layout> {
    use Slot::*;
    if bits_per_component != 8 || info & FLOAT_COMPONENTS != 0 {
        return None;
    }
    let alpha = info & ALPHA_MASK;
    let order = info & ORDER_MASK;
    let list: &[Slot] = match (space, alpha) {
        (Space::Rgb, ALPHA_NONE) => &[Red, Green, Blue],
        (Space::Rgb, PREMULTIPLIED_LAST | ALPHA_LAST) => &[Red, Green, Blue, Alpha],
        (Space::Rgb, PREMULTIPLIED_FIRST | ALPHA_FIRST) => &[Alpha, Red, Green, Blue],
        (Space::Rgb, NONE_SKIP_LAST) => &[Red, Green, Blue, Skip],
        (Space::Rgb, NONE_SKIP_FIRST) => &[Skip, Red, Green, Blue],
        (Space::Gray, ALPHA_NONE) => &[Gray],
        (Space::Gray, PREMULTIPLIED_LAST | ALPHA_LAST) => &[Gray, Alpha],
        (Space::Gray, PREMULTIPLIED_FIRST | ALPHA_FIRST) => &[Alpha, Gray],
        (Space::Gray, NONE_SKIP_LAST) => &[Gray, Skip],
        (Space::Gray, NONE_SKIP_FIRST) => &[Skip, Gray],
        (Space::None, ALPHA_ONLY) => &[Alpha],
        _ => return None,
    };
    if bits_per_pixel != list.len() * 8 {
        return None;
    }
    let mut slots = [Skip; 4];
    slots[..list.len()].copy_from_slice(list);
    match order {
        0 | ORDER_32_BIG if order == 0 || list.len() == 4 => {}
        ORDER_32_LITTLE if list.len() == 4 => slots.reverse(),
        _ => return None,
    }
    Some(Layout { slots, bytes: list.len(), premultiplied: matches!(alpha, PREMULTIPLIED_LAST | PREMULTIPLIED_FIRST) })
}

/// The formats a bitmap context can draw into (Apple's list for 8 bits per
/// component on iPhone: RGB with premultiplied or skipped alpha, plain gray,
/// alpha only).
fn context_supported(space: Space, bits_per_pixel: usize, info: u32) -> bool {
    let alpha = info & ALPHA_MASK;
    match space {
        Space::Rgb => bits_per_pixel == 32 && matches!(alpha, PREMULTIPLIED_LAST | PREMULTIPLIED_FIRST | NONE_SKIP_LAST | NONE_SKIP_FIRST),
        Space::Gray => bits_per_pixel == 8 && alpha == ALPHA_NONE,
        Space::None => bits_per_pixel == 8 && alpha == ALPHA_ONLY,
    }
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

impl Layout {
    fn has_alpha(&self) -> bool {
        self.slots[..self.bytes].contains(&Slot::Alpha)
    }

    /// The pixel as stored: red, green, blue, alpha (0..1), premultiplied or not as the format is.
    fn unpack(&self, pixel: &[u8]) -> [f32; 4] {
        let mut out = [0.0, 0.0, 0.0, 1.0];
        for (slot, &byte) in self.slots[..self.bytes].iter().zip(pixel) {
            let v = byte as f32 / 255.0;
            match slot {
                Slot::Red => out[0] = v,
                Slot::Green => out[1] = v,
                Slot::Blue => out[2] = v,
                Slot::Gray => out[..3].fill(v),
                Slot::Alpha => out[3] = v,
                Slot::Skip => {}
            }
        }
        out
    }

    /// The pixel with premultiplied alpha.
    pub fn read(&self, pixel: &[u8]) -> [f32; 4] {
        let mut c = self.unpack(pixel);
        let a = c[3];
        if !self.premultiplied {
            for v in &mut c[..3] {
                *v *= a;
            }
        }
        c
    }

    /// The pixel with straight (not premultiplied) alpha.
    pub fn read_straight(&self, pixel: &[u8]) -> [f32; 4] {
        let mut c = self.unpack(pixel);
        let a = c[3];
        if self.premultiplied {
            for v in &mut c[..3] {
                *v = if a > 0.0 { *v / a } else { 0.0 };
            }
        }
        c
    }

    /// Stores a premultiplied colour.
    pub fn write(&self, pixel: &mut [u8], c: [f32; 4]) {
        let mut c = c;
        let a = c[3];
        if !self.premultiplied && self.has_alpha() {
            for v in &mut c[..3] {
                *v = if a > 0.0 { *v / a } else { 0.0 };
            }
        }
        for (slot, byte) in self.slots[..self.bytes].iter().zip(pixel.iter_mut()) {
            *byte = match slot {
                Slot::Red => to_byte(c[0]),
                Slot::Green => to_byte(c[1]),
                Slot::Blue => to_byte(c[2]),
                Slot::Gray => to_byte(0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]),
                Slot::Alpha => to_byte(c[3]),
                Slot::Skip => 255,
            };
        }
    }
}

/// Source-over: the source (premultiplied) on top of what is there.
fn over(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let keep = 1.0 - src[3];
    [src[0] + dst[0] * keep, src[1] + dst[1] * keep, src[2] + dst[2] * keep, src[3] + dst[3] * keep]
}

fn scaled(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3] * k]
}

fn premultiply(c: [f64; 4]) -> [f32; 4] {
    [(c[0] * c[3]) as f32, (c[1] * c[3]) as f32, (c[2] * c[3]) as f32, c[3] as f32]
}

// ----- pictures in memory (CGImage) -----

pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub bits_per_pixel: usize,
    pub bytes_per_row: usize,
    pub info: u32,
    pub space: Space,
    pub data: Vec<u8>,
}

impl Bitmap {
    fn layout(&self) -> Option<Layout> {
        layout(self.space, 8, self.bits_per_pixel, self.info)
    }

    fn pixel(&self, layout: &Layout, x: usize, row: usize) -> [f32; 4] {
        let at = row * self.bytes_per_row + x * layout.bytes;
        layout.read(&self.data[at..at + layout.bytes])
    }

    /// Bilinear sample at (s, t) in 0..1, t going down from the top row.
    fn sample(&self, layout: &Layout, s: f64, t: f64) -> [f32; 4] {
        let x = (s * self.width as f64 - 0.5).clamp(0.0, self.width as f64 - 1.0);
        let y = (t * self.height as f64 - 0.5).clamp(0.0, self.height as f64 - 1.0);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let mix = |a: [f32; 4], b: [f32; 4], f: f32| [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * f);
        let top = mix(self.pixel(layout, x0, y0), self.pixel(layout, x1, y0), fx);
        let bottom = mix(self.pixel(layout, x0, y1), self.pixel(layout, x1, y1), fx);
        mix(top, bottom, fy)
    }

    /// Rows top to bottom as straight RGBA bytes (no row padding).
    fn straight_rgba(&self) -> Option<Vec<u8>> {
        let layout = self.layout()?;
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for row in 0..self.height {
            for x in 0..self.width {
                let at = row * self.bytes_per_row + x * layout.bytes;
                out.extend(layout.read_straight(&self.data[at..at + layout.bytes]).map(to_byte));
            }
        }
        Some(out)
    }
}

/// The biggest picture read (bytes of pixels), like ImageIO's own limits.
const MOST_PIXEL_BYTES: usize = 1 << 30;

/// A PNG made by Apple's pngcrush ("CgBI"): raw deflate data, BGRA with
/// premultiplied alpha. iOS reads it as that format directly. Broken files
/// give None (nil), never a crash.
fn decode_cgbi(bytes: &[u8]) -> Option<Bitmap> {
    let mut at = 8usize;
    let (mut width, mut height, mut idat) = (0usize, 0usize, Vec::new());
    while at.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body = bytes.get(at + 8..at.checked_add(8 + length)?)?;
        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    return None;
                }
                width = u32::from_be_bytes(body[0..4].try_into().ok()?) as usize;
                height = u32::from_be_bytes(body[4..8].try_into().ok()?) as usize;
                if body[8] != 8 || body[9] != 6 || body[12] != 0 {
                    crate::log!("crushed PNG: bit depth {} colour type {} interlace {} not read", body[8], body[9], body[12]);
                    return None;
                }
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + length;
    }
    let stride = width.checked_mul(4)?;
    if width == 0 || height == 0 || stride.checked_mul(height)? > MOST_PIXEL_BYTES {
        return None;
    }
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&idat, (stride + 1) * height).ok()?;
    let data = unfilter(&raw, width, height, 4)?;
    Some(Bitmap {
        width,
        height,
        bits_per_pixel: 32,
        bytes_per_row: width * 4,
        info: PREMULTIPLIED_FIRST | ORDER_32_LITTLE,
        space: Space::Rgb,
        data,
    })
}

/// Undoes PNG's per-row filters.
fn unfilter(raw: &[u8], width: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    let stride = width.checked_mul(bpp)?;
    if width == 0 || height == 0 || raw.len() < height.checked_mul(stride + 1)? {
        return None;
    }
    let mut out = vec![0u8; stride * height];
    for y in 0..height {
        let filter = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { out[y * stride + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if x >= bpp && y > 0 { out[(y - 1) * stride + x - bpp] } else { 0 };
            let predictor = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return None,
            };
            out[y * stride + x] = line[x].wrapping_add(predictor);
        }
    }
    Some(out)
}

/// UIImageOrientation for an EXIF orientation (1 to 8), as UIImage reads a photo's tag.
pub fn orientation_from_exif(exif: u8) -> i64 {
    match exif {
        2 => 4, // up, mirrored
        3 => 1, // down
        4 => 5, // down, mirrored
        5 => 6, // left, mirrored
        6 => 3, // right
        7 => 7, // right, mirrored
        8 => 2, // left
        _ => 0, // up
    }
}

/// Decodes PNG (also crushed) or JPEG into the formats ImageIO gives on iOS:
/// RGBA with straight alpha last, RGB with a skipped fourth byte, plain gray.
/// Also gives the file's orientation (UIImageOrientation) from its EXIF tag;
/// the pixels stay as stored, as in the CGImage on iOS.
pub fn decode(bytes: &[u8]) -> Option<(Bitmap, i64)> {
    if bytes.len() > 16 && &bytes[1..4] == b"PNG" && &bytes[12..16] == b"CgBI" {
        return decode_cgbi(bytes).map(|b| (b, 0));
    }
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().map_or(0, |o| orientation_from_exif(o.to_exif()));
    let (w, h) = decoder.dimensions();
    if (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)).is_none_or(|n| n > MOST_PIXEL_BYTES) {
        return None;
    }
    let picture = image::DynamicImage::from_decoder(decoder).ok()?;
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let color = picture.color();
    if !color.has_color() && !color.has_alpha() {
        let data = picture.into_luma8().into_raw();
        let bitmap = Bitmap { width, height, bits_per_pixel: 8, bytes_per_row: width, info: ALPHA_NONE, space: Space::Gray, data };
        return Some((bitmap, orientation));
    }
    let (info, data) = if color.has_alpha() {
        (ALPHA_LAST, picture.into_rgba8().into_raw())
    } else {
        let rgb = picture.into_rgb8().into_raw();
        (NONE_SKIP_LAST, rgb.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect())
    };
    Some((Bitmap { width, height, bits_per_pixel: 32, bytes_per_row: width * 4, info, space: Space::Rgb, data }, orientation))
}

/// A picture file's orientation (UIImageOrientation) from its EXIF tag, reading only its header.
pub fn file_orientation(path: &str) -> i64 {
    image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.into_decoder().ok())
        .and_then(|mut d| d.orientation().ok())
        .map_or(0, |o| orientation_from_exif(o.to_exif()))
}

/// A CGImage: its pixels never change; they go (None) when the last reference does.
struct ImageState {
    bitmap: RwLock<Option<Arc<Bitmap>>>,
}

static CLASSES: OnceLock<Classes> = OnceLock::new();

struct Classes {
    image: Class,
    context: Class,
    space: Class,
    gradient: Class,
    path: Class,
    provider: Class,
}

fn classes() -> &'static Classes {
    CLASSES.get().expect("graphics classes")
}

fn is_a(object: Id, class: Class) -> bool {
    object != 0 && lock().class_of(object) == class
}

/// The object's state, shared: what changes inside it is behind its own lock.
fn state<T>(object: Id, class: Class) -> Option<&'static T> {
    if !is_a(object, class) {
        return None;
    }
    unsafe { (*((object + 8) as *const *const T)).as_ref() }
}

fn wrap<T>(class: Class, value: T) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut *mut T) = Box::into_raw(Box::new(value)) };
    object
}

/// A new CGImage with one reference (the caller's, as CGBitmapContextCreateImage gives).
pub fn make_image(bitmap: Bitmap) -> Id {
    let object = wrap(classes().image, ImageState { bitmap: RwLock::new(Some(Arc::new(bitmap))) });
    counted::track(object, free_image);
    object
}

fn free_image(object: Id) {
    if let Some(s) = state::<ImageState>(object, classes().image) {
        *s.bitmap.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The picture's pixels (kept alive while in use, even if released meanwhile).
fn image(object: Id) -> Option<Arc<Bitmap>> {
    state::<ImageState>(object, classes().image)?.bitmap.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Logs the first few uses of a bad object, as Core Graphics does.
fn complain(function: &str, what: &str, object: Id) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) < 30 {
        crate::log!("{function}: invalid {what} {object:#x}");
    }
}

extern "C" fn image_get_width(object: Id) -> usize {
    image(object).map_or(0, |b| b.width)
}
extern "C" fn image_get_height(object: Id) -> usize {
    image(object).map_or(0, |b| b.height)
}
extern "C" fn image_get_bits_per_component(object: Id) -> usize {
    image(object).map_or(0, |_| 8)
}
extern "C" fn image_get_bits_per_pixel(object: Id) -> usize {
    image(object).map_or(0, |b| b.bits_per_pixel)
}
extern "C" fn image_get_bytes_per_row(object: Id) -> usize {
    image(object).map_or(0, |b| b.bytes_per_row)
}
extern "C" fn image_get_bitmap_info(object: Id) -> u32 {
    image(object).map_or(0, |b| b.info)
}
extern "C" fn image_get_alpha_info(object: Id) -> u32 {
    image(object).map_or(0, |b| b.info & ALPHA_MASK)
}
extern "C" fn image_get_color_space(object: Id) -> Id {
    image(object).map_or(0, |b| space_object(b.space))
}

extern "C" fn cf_style_retain(object: Id) -> Id {
    counted::retain(object)
}
extern "C" fn cf_style_release(object: Id) {
    counted::release(object)
}

// ----- colour spaces, colours, gradients -----

fn space_object(space: Space) -> Id {
    static OBJECTS: OnceLock<[Id; 2]> = OnceLock::new();
    let objects = OBJECTS.get_or_init(|| [wrap(classes().space, Space::Rgb), wrap(classes().space, Space::Gray)]);
    match space {
        Space::Rgb => objects[0],
        Space::Gray => objects[1],
        Space::None => 0,
    }
}

fn space_of(object: Id) -> Space {
    state::<Space>(object, classes().space).map_or(Space::None, |s| *s)
}

fn space_components(space: Space) -> usize {
    match space {
        Space::Rgb => 3,
        Space::Gray => 1,
        Space::None => 0,
    }
}

extern "C" fn space_create_rgb() -> Id {
    space_object(Space::Rgb)
}
extern "C" fn space_create_gray() -> Id {
    space_object(Space::Gray)
}
extern "C" fn space_get_number_of_components(space: Id) -> usize {
    space_components(space_of(space))
}

/// Components in a colour space (then alpha) as red, green, blue, alpha.
fn to_rgba(space: Space, values: &[f64]) -> [f64; 4] {
    match space {
        Space::Gray => [values[0], values[0], values[0], values[1]],
        _ => [values[0], values[1], values[2], values[3]],
    }
}

/// CGColor shares UIColor's object ({ isa, r, g, b, a }); UIColor's CGColor is the colour itself.
extern "C" fn color_create(space: Id, components: *const f64) -> Id {
    let space = space_of(space);
    if space == Space::None || components.is_null() {
        return 0;
    }
    let values = unsafe { std::slice::from_raw_parts(components, space_components(space) + 1) };
    let [r, g, b, a] = to_rgba(space, values);
    let object = runtime::alloc_instance(runtime::class_named("UIColor"));
    unsafe { *((object + 8) as *mut [f64; 4]) = [r, g, b, a] };
    object
}

/// A UIColor of straight red, green, blue and alpha.
pub fn make_color(rgba: [f64; 4]) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("UIColor"));
    unsafe { *((object + 8) as *mut [f64; 4]) = rgba };
    object
}

/// A UIColor's (or CGColor's) red, green, blue and alpha.
pub fn color_rgba(color: Id) -> Option<[f64; 4]> {
    if color == 0 {
        return None;
    }
    Some(unsafe { *((color + 8) as *const [f64; 4]) })
}

struct Gradient {
    stops: Vec<(f64, [f64; 4])>,
}

impl Gradient {
    /// The straight colour at t (0..1), between the stops around it.
    fn at(&self, t: f64) -> [f64; 4] {
        let first = self.stops[0];
        if t <= first.0 {
            return first.1;
        }
        for pair in self.stops.windows(2) {
            let ((l0, c0), (l1, c1)) = (pair[0], pair[1]);
            if t <= l1 {
                let f = if l1 > l0 { (t - l0) / (l1 - l0) } else { 1.0 };
                return [0, 1, 2, 3].map(|i| c0[i] + (c1[i] - c0[i]) * f);
            }
        }
        self.stops[self.stops.len() - 1].1
    }
}

extern "C" fn gradient_create_with_color_components(space: Id, components: *const f64, locations: *const f64, count: usize) -> Id {
    let space = space_of(space);
    if space == Space::None || components.is_null() || count == 0 {
        return 0;
    }
    let per = space_components(space) + 1;
    let values = unsafe { std::slice::from_raw_parts(components, per * count) };
    let mut stops: Vec<(f64, [f64; 4])> = (0..count)
        .map(|i| {
            let location = if locations.is_null() {
                if count == 1 { 0.0 } else { i as f64 / (count - 1) as f64 }
            } else {
                unsafe { *locations.add(i) }
            };
            (location, to_rgba(space, &values[i * per..(i + 1) * per]))
        })
        .collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    wrap(classes().gradient, Gradient { stops })
}

// ----- bitmap contexts -----

#[derive(Clone)]
struct GraphicsState {
    ctm: Transform,
    /// Straight red, green, blue, alpha.
    fill: [f64; 4],
    alpha: f64,
    stroke: [f64; 4],
    line_width: f64,
    /// Colour spaces CGContextSetFillColor / SetStrokeColor read components in.
    fill_space: Space,
    stroke_space: Space,
    /// CGBlendMode (only normal, clear and copy differ here).
    blend_mode: i32,
    /// How much of each device pixel (rows from the bottom) drawing may touch.
    clip: Option<Arc<Vec<f32>>>,
    /// CGTextDrawingMode: fill, stroke or both (fw/graphics/text.rs).
    text_mode: i32,
}

impl GraphicsState {
    fn new() -> GraphicsState {
        GraphicsState {
            ctm: IDENTITY,
            fill: [0.0, 0.0, 0.0, 1.0],
            alpha: 1.0,
            stroke: [0.0, 0.0, 0.0, 1.0],
            line_width: 1.0,
            fill_space: Space::Rgb,
            stroke_space: Space::Rgb,
            blend_mode: 0,
            clip: None,
            text_mode: 0,
        }
    }
}

pub struct Canvas {
    width: usize,
    height: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    info: u32,
    space: Space,
    layout: Layout,
    data: *mut u8,
    owned: Option<Vec<u8>>,
    state: GraphicsState,
    saved: Vec<GraphicsState>,
    /// The path being built, in device space.
    path: shapes::Outline,
}

// The memory is the game's or the canvas's own; it's only touched under the context's lock.
unsafe impl Send for Canvas {}

/// A CGContext: drawn into by one thread at a time.
struct ContextState(Mutex<Canvas>);

enum Blend {
    Over,
    Clear,
}

impl Canvas {
    /// A context over `data` (or its own zeroed memory when null), or the
    /// reason Core Graphics would refuse it.
    fn new(data: *mut u8, width: usize, height: usize, bits_per_component: usize, bytes_per_row: usize, space: Space, info: u32) -> Result<Canvas, String> {
        let bits_per_pixel = match (space, info & ALPHA_MASK) {
            (Space::Rgb, ALPHA_NONE) => 24,
            (Space::Rgb, _) => 32,
            (Space::Gray, ALPHA_NONE) => 8,
            (Space::Gray, _) => 16,
            (Space::None, _) => 8,
        } * bits_per_component
            / 8;
        let layout = match layout(space, bits_per_component, bits_per_pixel, info) {
            Some(layout) if context_supported(space, bits_per_pixel, info) => layout,
            _ => {
                return Err(format!(
                    "unsupported parameter combination: {bits_per_component} integer bits/component; {bits_per_pixel} bits/pixel; {}-component color space; bitmap info {info:#x}",
                    space_components(space)
                ))
            }
        };
        if width == 0 || height == 0 {
            return Err(format!("invalid data width/height {width}x{height}"));
        }
        let least = width * layout.bytes;
        let bytes_per_row = match bytes_per_row {
            // Apple picks the row size itself: rounded up to 16 bytes for its own memory.
            0 if data.is_null() => (least + 15) & !15,
            0 => least,
            n if n < least => return Err(format!("invalid data bytes/row: should be at least {least} for {bits_per_component} integer bits/component, {} components", space_components(space))),
            n => n,
        };
        let (data, owned) = if data.is_null() {
            let mut memory = vec![0u8; bytes_per_row * height];
            (memory.as_mut_ptr(), Some(memory))
        } else {
            (data, None)
        };
        Ok(Canvas {
            width,
            height,
            bits_per_pixel,
            bytes_per_row,
            info,
            space,
            layout,
            data,
            owned,
            state: GraphicsState::new(),
            saved: Vec::new(),
            path: shapes::Outline::default(),
        })
    }

    fn close(&mut self) {
        self.owned = None;
        // A clip mask is as big as the pixels: nothing of the context's stays.
        self.state = GraphicsState::new();
        self.saved = Vec::new();
        self.path = shapes::Outline::default();
        self.data = std::ptr::null_mut();
        self.width = 0;
        self.height = 0;
    }

    fn memory(&mut self) -> &mut [u8] {
        if self.data.is_null() {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.data, self.bytes_per_row * self.height) }
    }

    /// Device pixel (x, y from the bottom) composited with `src` (premultiplied).
    fn blend(&mut self, x: usize, y: usize, src: [f32; 4], blend: &Blend) {
        let src = match &self.state.clip {
            Some(clip) => scaled(src, clip.get(y * self.width + x).copied().unwrap_or(0.0)),
            None => src,
        };
        let (layout, at) = (self.layout, (self.height - 1 - y) * self.bytes_per_row + x * self.layout.bytes);
        let clear = self.state.blend_mode == shapes::BLEND_CLEAR;
        let pixel = &mut self.memory()[at..at + layout.bytes];
        let dst = layout.read(pixel);
        let out = match blend {
            Blend::Over if clear => scaled(dst, 1.0 - src[3]),
            Blend::Over => over(src, dst),
            Blend::Clear => scaled(dst, 1.0 - src[3]),
        };
        layout.write(pixel, out);
    }

    /// Paints the user-space rectangle: `shade` gives the premultiplied
    /// colour at a user point inside it. Edges are anti-aliased by how much
    /// of each pixel the rectangle covers.
    fn paint(&mut self, rect: Rect, blend: Blend, shade: &dyn Fn(f64, f64) -> [f32; 4]) {
        let rect = standard(rect);
        let ctm = self.state.ctm;
        let Some(inverse) = geometry::invert(ctm) else { return };
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .map(|(fx, fy)| geometry::apply(ctm, rect.origin.x + fx * rect.size.width, rect.origin.y + fy * rect.size.height));
        let x0 = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min).max(0.0);
        let x1 = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max).min(self.width as f64);
        let y0 = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min).max(0.0);
        let y1 = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max).min(self.height as f64);
        if !(x0 < x1 && y0 < y1) {
            return;
        }
        let straight = (ctm.b == 0.0 && ctm.c == 0.0) || (ctm.a == 0.0 && ctm.d == 0.0);
        let (min_x, max_x) = (rect.origin.x, rect.origin.x + rect.size.width);
        let (min_y, max_y) = (rect.origin.y, rect.origin.y + rect.size.height);
        let inside = |dx: f64, dy: f64| {
            let (ux, uy) = geometry::apply(inverse, dx, dy);
            ux >= min_x && ux <= max_x && uy >= min_y && uy <= max_y
        };
        let alpha = self.state.alpha as f32;
        for y in y0.floor() as usize..y1.ceil() as usize {
            for x in x0.floor() as usize..x1.ceil() as usize {
                let (fx, fy) = (x as f64, y as f64);
                let coverage = if straight {
                    ((fx + 1.0).min(x1) - fx.max(x0)).max(0.0) * ((fy + 1.0).min(y1) - fy.max(y0)).max(0.0)
                } else {
                    let mut hits = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            hits += inside(fx + (sx as f64 + 0.5) / 4.0, fy + (sy as f64 + 0.5) / 4.0) as u32;
                        }
                    }
                    hits as f64 / 16.0
                };
                if coverage <= 0.0 {
                    continue;
                }
                let (ux, uy) = geometry::apply(inverse, fx + 0.5, fy + 0.5);
                let color = shade(ux.clamp(min_x, max_x), uy.clamp(min_y, max_y));
                self.blend(x, y, scaled(color, coverage as f32 * alpha), &blend);
            }
        }
    }

    fn fill_rect(&mut self, rect: Rect) {
        let color = premultiply(self.state.fill);
        self.paint(rect, Blend::Over, &|_, _| color);
    }

    /// The device pixels (x0, x1, y0, y1; y from the bottom, not cut to the
    /// canvas) a user rectangle covers when it covers whole pixels: no
    /// turn and every edge on a pixel edge.
    fn whole_pixels(&self, rect: Rect) -> Option<(i64, i64, i64, i64)> {
        let ctm = self.state.ctm;
        if ctm.b != 0.0 || ctm.c != 0.0 {
            return None;
        }
        let rect = standard(rect);
        let whole = |v: f64| (v - v.round()).abs() < 1e-9 && v.abs() < 1e9;
        let xs = [ctm.a * rect.origin.x + ctm.tx, ctm.a * (rect.origin.x + rect.size.width) + ctm.tx];
        let ys = [ctm.d * rect.origin.y + ctm.ty, ctm.d * (rect.origin.y + rect.size.height) + ctm.ty];
        if !xs.iter().chain(&ys).all(|&v| whole(v)) {
            return None;
        }
        let (x0, x1) = (xs[0].min(xs[1]).round() as i64, xs[0].max(xs[1]).round() as i64);
        let (y0, y1) = (ys[0].min(ys[1]).round() as i64, ys[0].max(ys[1]).round() as i64);
        Some((x0, x1, y0, y1))
    }

    /// The canvas rows and columns of a whole-pixel box, cut to the canvas.
    fn cut(&self, (x0, x1, y0, y1): (i64, i64, i64, i64)) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let range = |a: i64, b: i64, most: usize| a.clamp(0, most as i64) as usize..b.clamp(0, most as i64) as usize;
        (range(x0, x1, self.width), range(y0, y1, self.height))
    }

    /// ClearRect over whole pixels with no clip: every pixel there becomes
    /// the same cleared bytes, so they are set row by row (as `paint` would
    /// set them one by one). Whether it applied.
    fn clear_whole_pixels(&mut self, rect: Rect) -> bool {
        if self.state.clip.is_some() {
            return false;
        }
        let Some(area) = self.whole_pixels(rect) else { return false };
        let (xs, ys) = self.cut(area);
        let (layout, height, bytes_per_row) = (self.layout, self.height, self.bytes_per_row);
        let mut cleared = [0u8; 4];
        layout.write(&mut cleared[..layout.bytes], [0.0; 4]);
        let memory = self.memory();
        for y in ys {
            let row = (height - 1 - y) * bytes_per_row;
            for pixel in memory[row + xs.start * layout.bytes..row + xs.end * layout.bytes].chunks_exact_mut(layout.bytes) {
                pixel.copy_from_slice(&cleared[..layout.bytes]);
            }
        }
        true
    }

    fn clear_rect(&mut self, rect: Rect) {
        if self.clear_whole_pixels(rect) {
            return;
        }
        let alpha = self.state.alpha;
        self.state.alpha = 1.0;
        self.paint(rect, Blend::Clear, &|_, _| [0.0, 0.0, 0.0, 1.0]);
        self.state.alpha = alpha;
    }

    /// Draws `bitmap` into `rect`. `map` turns (u, v) in the rectangle (u
    /// from its left, v down from its top in user space, i.e. its max y)
    /// into (s, t) in the picture (t down from its first row).
    fn draw_bitmap(&mut self, rect: Rect, bitmap: &Bitmap, map: &dyn Fn(f64, f64) -> (f64, f64)) {
        let Some(source) = bitmap.layout() else {
            crate::log!("CGContextDrawImage: picture format {:#x} ({} bits/pixel) not readable", bitmap.info, bitmap.bits_per_pixel);
            return;
        };
        if bitmap.width == 0 || bitmap.height == 0 {
            return;
        }
        let rect = standard(rect);
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return;
        }
        self.paint(rect, Blend::Over, &|ux, uy| {
            let u = (ux - rect.origin.x) / rect.size.width;
            let v = (rect.origin.y + rect.size.height - uy) / rect.size.height;
            let (s, t) = map(u, v);
            bitmap.sample(&source, s, t)
        });
    }

    /// CGContextDrawImage: a picture landing pixel for pixel (one picture
    /// pixel per device pixel, on pixel edges, no turn) is read straight
    /// from its pixels; anything else is sampled by `draw_bitmap`.
    fn draw_image(&mut self, rect: Rect, bitmap: &Bitmap) {
        if !self.draw_whole_pixels(rect, bitmap) {
            self.draw_bitmap(rect, bitmap, &|u, v| (u, v));
        }
    }

    fn draw_whole_pixels(&mut self, rect: Rect, bitmap: &Bitmap) -> bool {
        let Some(source) = bitmap.layout() else { return false };
        let Some(area) = self.whole_pixels(rect) else { return false };
        let (x0, x1, y0, y1) = area;
        if bitmap.width == 0 || (x1 - x0) as usize != bitmap.width || (y1 - y0) as usize != bitmap.height {
            return false;
        }
        // The picture's first row is the rectangle's top (user max y): the
        // device top when y goes up, the device bottom when flipped.
        let (mirrored, upright) = (self.state.ctm.a < 0.0, self.state.ctm.d > 0.0);
        let alpha = self.state.alpha as f32;
        let (xs, ys) = self.cut(area);
        for y in ys {
            let row = (if upright { y1 - 1 - y as i64 } else { y as i64 - y0 }) as usize;
            for x in xs.clone() {
                let column = (if mirrored { x1 - 1 - x as i64 } else { x as i64 - x0 }) as usize;
                let at = row * bitmap.bytes_per_row + column * source.bytes;
                let color = source.read(&bitmap.data[at..at + source.bytes]);
                self.blend(x, y, scaled(color, alpha), &Blend::Over);
            }
        }
        true
    }

    fn draw_linear_gradient(&mut self, gradient: &Gradient, start: Point, end: Point, options: u32) {
        let Some(inverse) = geometry::invert(self.state.ctm) else { return };
        let (dx, dy) = (end.x - start.x, end.y - start.y);
        let length = dx * dx + dy * dy;
        if length == 0.0 {
            return;
        }
        let alpha = self.state.alpha as f32;
        for y in 0..self.height {
            for x in 0..self.width {
                let (ux, uy) = geometry::apply(inverse, x as f64 + 0.5, y as f64 + 0.5);
                let mut t = ((ux - start.x) * dx + (uy - start.y) * dy) / length;
                if t < 0.0 {
                    if options & 1 == 0 {
                        continue;
                    }
                    t = 0.0;
                } else if t > 1.0 {
                    if options & 2 == 0 {
                        continue;
                    }
                    t = 1.0;
                }
                self.blend(x, y, scaled(premultiply(gradient.at(t)), alpha), &Blend::Over);
            }
        }
    }

    /// The picture in it now (a copy, as Core Graphics gives).
    fn snapshot(&mut self) -> Bitmap {
        let data = self.memory().to_vec();
        Bitmap {
            width: self.width,
            height: self.height,
            bits_per_pixel: self.bits_per_pixel,
            bytes_per_row: self.bytes_per_row,
            info: self.info,
            space: self.space,
            data,
        }
    }
}

fn standard(r: Rect) -> Rect {
    let mut r = r;
    if r.size.width < 0.0 {
        r.origin.x += r.size.width;
        r.size.width = -r.size.width;
    }
    if r.size.height < 0.0 {
        r.origin.y += r.size.height;
        r.size.height = -r.size.height;
    }
    r
}

/// The context locked for this thread's use (any state, even closed).
fn locked(context: Id) -> Option<MutexGuard<'static, Canvas>> {
    state::<ContextState>(context, classes().context).map(|s| s.0.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The context to draw in, or None (logged) when it isn't a live context.
fn canvas(function: &str, context: Id) -> Option<MutexGuard<'static, Canvas>> {
    let found = locked(context).filter(|c| !c.data.is_null());
    if found.is_none() {
        complain(function, "context", context);
    }
    found
}

fn free_context(context: Id) {
    if let Some(mut c) = locked(context) {
        c.close();
    }
}

fn make_context(canvas: Canvas) -> Id {
    let object = wrap(classes().context, ContextState(Mutex::new(canvas)));
    counted::track(object, free_context);
    object
}

extern "C" fn bitmap_context_create(data: *mut u8, width: usize, height: usize, bits_per_component: usize, bytes_per_row: usize, space: Id, info: u32) -> Id {
    match Canvas::new(data, width, height, bits_per_component, bytes_per_row, space_of(space), info) {
        Ok(canvas) => make_context(canvas),
        Err(reason) => {
            crate::log!("CGBitmapContextCreate: {reason}");
            0
        }
    }
}

extern "C" fn bitmap_context_create_image(context: Id) -> Id {
    let snapshot = canvas("CGBitmapContextCreateImage", context).map(|mut c| c.snapshot());
    snapshot.map_or(0, make_image)
}

extern "C" fn bitmap_context_get_data(context: Id) -> *mut u8 {
    locked(context).map_or(std::ptr::null_mut(), |c| c.data)
}
extern "C" fn bitmap_context_get_width(context: Id) -> usize {
    locked(context).map_or(0, |c| c.width)
}
extern "C" fn bitmap_context_get_height(context: Id) -> usize {
    locked(context).map_or(0, |c| c.height)
}
extern "C" fn bitmap_context_get_bytes_per_row(context: Id) -> usize {
    locked(context).map_or(0, |c| c.bytes_per_row)
}
extern "C" fn bitmap_context_get_bits_per_component(context: Id) -> usize {
    locked(context).map_or(0, |_| 8)
}
extern "C" fn bitmap_context_get_bits_per_pixel(context: Id) -> usize {
    locked(context).map_or(0, |c| c.bits_per_pixel)
}
extern "C" fn bitmap_context_get_bitmap_info(context: Id) -> u32 {
    locked(context).map_or(0, |c| c.info)
}
extern "C" fn bitmap_context_get_color_space(context: Id) -> Id {
    let space = locked(context).map(|c| c.space);
    space.map_or(0, space_object)
}

extern "C" fn context_concat_ctm(context: Id, t: Transform) {
    if let Some(mut c) = canvas("CGContextConcatCTM", context) {
        c.state.ctm = geometry::concat(t, c.state.ctm);
    }
}
extern "C" fn context_translate_ctm(context: Id, tx: f64, ty: f64) {
    context_concat_ctm(context, Transform { tx, ty, ..IDENTITY });
}
extern "C" fn context_scale_ctm(context: Id, sx: f64, sy: f64) {
    context_concat_ctm(context, Transform { a: sx, d: sy, ..IDENTITY });
}
extern "C" fn context_rotate_ctm(context: Id, angle: f64) {
    let (s, c) = angle.sin_cos();
    context_concat_ctm(context, Transform { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 });
}
extern "C" fn context_get_ctm(context: Id) -> Transform {
    let ctm = canvas("CGContextGetCTM", context).map(|c| c.state.ctm);
    ctm.unwrap_or(IDENTITY)
}
extern "C" fn context_save_gstate(context: Id) {
    if let Some(mut c) = canvas("CGContextSaveGState", context) {
        let state = c.state.clone();
        c.saved.push(state);
    }
}
extern "C" fn context_restore_gstate(context: Id) {
    if let Some(mut c) = canvas("CGContextRestoreGState", context) {
        if let Some(saved) = c.saved.pop() {
            c.state = saved;
        }
    }
}
extern "C" fn context_set_fill_color_with_color(context: Id, color: Id) {
    let Some(rgba) = color_rgba(color) else { return };
    if let Some(mut c) = canvas("CGContextSetFillColorWithColor", context) {
        c.state.fill = rgba;
    }
}
extern "C" fn context_set_rgb_fill_color(context: Id, r: f64, g: f64, b: f64, a: f64) {
    if let Some(mut c) = canvas("CGContextSetRGBFillColor", context) {
        c.state.fill = [r, g, b, a];
    }
}
extern "C" fn context_set_alpha(context: Id, alpha: f64) {
    if let Some(mut c) = canvas("CGContextSetAlpha", context) {
        c.state.alpha = alpha.clamp(0.0, 1.0);
    }
}
extern "C" fn context_fill_rect(context: Id, rect: Rect) {
    if let Some(mut c) = canvas("CGContextFillRect", context) {
        c.fill_rect(rect);
    }
}
extern "C" fn context_clear_rect(context: Id, rect: Rect) {
    if let Some(mut c) = canvas("CGContextClearRect", context) {
        c.clear_rect(rect);
    }
}
extern "C" fn context_draw_image(context: Id, rect: Rect, picture: Id) {
    // The picture first: it may be this same context's picture or need the runtime.
    let Some(bitmap) = image(picture) else {
        complain("CGContextDrawImage", "image", picture);
        return;
    };
    if let Some(mut c) = canvas("CGContextDrawImage", context) {
        c.draw_image(rect, &bitmap);
    }
}
extern "C" fn context_draw_linear_gradient(context: Id, gradient: Id, start: Point, end: Point, options: u32) {
    let Some(g) = state::<Gradient>(gradient, classes().gradient) else {
        complain("CGContextDrawLinearGradient", "gradient", gradient);
        return;
    };
    if let Some(mut c) = canvas("CGContextDrawLinearGradient", context) {
        c.draw_linear_gradient(g, start, end, options);
    }
}

extern "C" fn nothing(_object: Id) {}

// ----- UIKit image contexts -----

struct ImageContext {
    context: Id,
    scale: f64,
    /// Made by UIGraphicsBeginImageContext (not UIGraphicsPushContext).
    own: bool,
}

thread_local! {
    static CONTEXTS: RefCell<Vec<ImageContext>> = const { RefCell::new(Vec::new()) };
}

fn current_context() -> Id {
    CONTEXTS.with(|stack| stack.borrow().last().map_or(0, |c| c.context))
}

extern "C" fn graphics_begin_image_context_with_options(size: Size, opaque: bool, scale: f64) {
    let scale = if scale > 0.0 { scale } else { uikit::screen_points().2 };
    let (width, height) = ((size.width * scale).ceil(), (size.height * scale).ceil());
    let info = ORDER_32_LITTLE | if opaque { NONE_SKIP_FIRST } else { PREMULTIPLIED_FIRST };
    if !(width >= 1.0 && height >= 1.0) {
        crate::log!("UIGraphicsBeginImageContext() failed to allocate CGBitampContext: size={{{}, {}}}, scale={scale}", size.width, size.height);
        return;
    }
    let context = bitmap_context_create(std::ptr::null_mut(), width as usize, height as usize, 8, 0, space_object(Space::Rgb), info);
    if let Some(mut c) = canvas("UIGraphicsBeginImageContext", context) {
        // UIKit's flip: (0, 0) at the top left, one unit per point.
        c.state.ctm = Transform { a: scale, b: 0.0, c: 0.0, d: -scale, tx: 0.0, ty: height };
    }
    CONTEXTS.with(|stack| stack.borrow_mut().push(ImageContext { context, scale, own: true }));
}

extern "C" fn graphics_begin_image_context(size: Size) {
    graphics_begin_image_context_with_options(size, false, 1.0);
}

extern "C" fn graphics_get_current_context() -> Id {
    current_context()
}

extern "C" fn graphics_push_context(context: Id) {
    CONTEXTS.with(|stack| stack.borrow_mut().push(ImageContext { context, scale: 1.0, own: false }));
}

extern "C" fn graphics_pop_context() {
    CONTEXTS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.last().is_some_and(|c| !c.own) {
            stack.pop();
        }
    });
}

/// An autoreleased UIImage of what the current image context holds (as on iOS).
extern "C" fn graphics_get_image_from_current_image_context() -> Id {
    let Some((context, scale)) = CONTEXTS.with(|stack| stack.borrow().last().filter(|c| c.own).map(|c| (c.context, c.scale))) else {
        return 0;
    };
    let picture = bitmap_context_create_image(context);
    if picture == 0 {
        return 0;
    }
    let made = uikit_image_from_cg(picture, scale, 0);
    counted::release(picture);
    counted::autorelease(made)
}

extern "C" fn graphics_end_image_context() {
    let ended = CONTEXTS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.last().is_some_and(|c| c.own) {
            stack.pop()
        } else {
            None
        }
    });
    match ended {
        Some(ended) => counted::release(ended.context),
        None => crate::log!("UIGraphicsEndImageContext: no image context is current"),
    }
}

// ----- UIImage <-> CGImage -----

/// A new UIImage (one reference, the caller's) holding a reference to `picture`.
fn uikit_image_from_cg(picture: Id, scale: f64, orientation: i64) -> Id {
    let Some(bitmap) = image(picture) else { return 0 };
    counted::retain(picture);
    let info = uikit::ImageInfo {
        path: String::new(),
        pixels: (bitmap.width as u32, bitmap.height as u32),
        scale: if scale > 0.0 { scale } else { 1.0 },
        orientation,
        cg: OnceLock::from(picture),
        ..Default::default()
    };
    uikit::make_image(info)
}

/// The UIImage's last reference went: it lets go of its CGImage (the pixels
/// go once nothing else holds them) and of the file made to show it.
pub fn ui_image_clean_up(picture: Id) {
    let Some(info) = uikit::image_info(picture) else { return };
    // get_or_init waits for a CGImage being read right now, and stops later
    // reads (the UIImage is then empty), so nothing is left behind.
    counted::release(*info.cg.get_or_init(|| 0));
    if let Some(file) = info.picture_file.get_or_init(|| None) {
        let _ = std::fs::remove_file(file);
    }
    for file in info.part_files.get_or_init(Vec::new).iter().flatten() {
        let _ = std::fs::remove_file(file);
    }
    let darker = DARKER.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|made| made.remove(&picture));
    if let Some(darker) = darker {
        counted::release(darker);
    }
}

/// A one-pixel PNG of a colour (straight red, green, blue, alpha), for a
/// view's background colour (fw/pictures.rs): made once per colour.
pub fn color_picture(rgba: [f64; 4]) -> Option<String> {
    static MADE: Mutex<Option<std::collections::HashMap<[u8; 4], Option<String>>>> = Mutex::new(None);
    let pixel = rgba.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    made.get_or_insert_with(Default::default)
        .entry(pixel)
        .or_insert_with(|| encode_png(1, 1, &pixel, true).and_then(|bytes| write_png_file(&format!("colour {pixel:?}"), &bytes)))
        .clone()
}

/// Darker copies made of UIImages (pressed buttons), by the image they copy.
static DARKER: Mutex<Option<std::collections::HashMap<Id, Id>>> = Mutex::new(None);

/// A CGImage's pixels as straight RGBA rows from the top.
pub fn straight_pixels(picture: Id) -> Option<Vec<u8>> {
    image(picture)?.straight_rgba()
}

/// A copy of a UIImage drawn darker, as UIKit draws a pressed button's
/// pictures (adjustsImageWhenHighlighted): made once, upright, with the
/// same scale and caps; it goes when the image does. 0 if it has no pixels.
pub fn darker_image(picture: Id) -> Id {
    if let Some(&made) = DARKER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|made| made.get(&picture)) {
        return made;
    }
    let Some(info) = uikit::image_info(picture) else { return 0 };
    let Some((width, height, rgba, _)) = upright_rgba(picture) else { return 0 };
    // About half as bright; stored premultiplied, as the bitmap says.
    let data: Vec<u8> = rgba
        .chunks(4)
        .flat_map(|p| {
            let alpha = u32::from(p[3]);
            let shade = |c: u8| (u32::from(c) * 11 / 20 * alpha / 255) as u8;
            [shade(p[0]), shade(p[1]), shade(p[2]), p[3]]
        })
        .collect();
    let bitmap = Bitmap { width, height, bits_per_pixel: 32, bytes_per_row: width * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data };
    let copy = uikit::ImageInfo {
        pixels: (width as u32, height as u32),
        scale: info.scale,
        caps: info.caps,
        cg: OnceLock::from(make_image(bitmap)),
        ..Default::default()
    };
    let made = uikit::make_image(copy);
    DARKER.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(picture, made);
    made
}

/// The UIImage's CGImage (pictures from files are read the first time it's asked for).
pub fn image_cg(picture: Id) -> Id {
    let Some(info) = uikit::image_info(picture) else { return 0 };
    *info.cg.get_or_init(|| match std::fs::read(&info.path).ok().and_then(|bytes| decode(&bytes)) {
        Some((bitmap, _)) => make_image(bitmap),
        None => {
            crate::log!("UIImage {}: can't read its pixels", info.path);
            0
        }
    })
}

/// A file the phone app can show for this UIImage as UIKit would draw it: the
/// picture's own file when it is upright, else an upright PNG made once
/// (pictures made in memory, turned photos). None if it can't be made.
///
/// The PNG is made on its own thread (not the game's main thread); until it
/// is ready this gives None and the picture shows from the next update.
pub fn picture_file(picture: Id) -> Option<String> {
    let info = uikit::image_info(picture)?;
    if info.orientation == 0 && !info.path.is_empty() {
        return Some(info.path.clone());
    }
    if let Some(made) = info.picture_file.get() {
        return made.clone();
    }
    if !info.picture_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
        // Held while the file is made, so the image can't go meanwhile.
        counted::retain(picture);
        std::thread::spawn(move || {
            let made = write_picture_file(picture);
            if let Some(info) = uikit::image_info(picture) {
                if let Err(Some(file)) = info.picture_file.set(made) {
                    let _ = std::fs::remove_file(file);
                }
            }
            counted::release(picture);
        });
    }
    None
}

/// A file for one of a stretchable UIImage's nine parts (0-8, rows from
/// the top: corners, edges and the middle, cut at its caps), made with the
/// others on their own thread as `picture_file` does; None until they are
/// ready, for an empty part, or if it can't be made.
pub fn picture_part_file(picture: Id, part: usize) -> Option<String> {
    let info = uikit::image_info(picture)?;
    if let Some(parts) = info.part_files.get() {
        return parts.get(part).cloned().flatten();
    }
    if !info.parts_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
        // Held while the files are made, so the image can't go meanwhile.
        counted::retain(picture);
        std::thread::spawn(move || {
            let made = write_part_files(picture);
            if let Some(info) = uikit::image_info(picture) {
                if let Err(files) = info.part_files.set(made) {
                    for file in files.into_iter().flatten() {
                        let _ = std::fs::remove_file(file);
                    }
                }
            }
            counted::release(picture);
        });
    }
    None
}

/// Where the three parts of a side `length` pixels long start and how long
/// they are, with caps `first` and `last` (too long caps get what's there).
fn cuts(length: usize, first: usize, last: usize) -> [(usize, usize); 3] {
    let first = first.min(length);
    let last = last.min(length - first);
    [(0, first), (first, length - first - last), (length - last, last)]
}

fn write_part_files(picture: Id) -> Vec<Option<String>> {
    let Some(info) = uikit::image_info(picture) else { return Vec::new() };
    let Some((width, height, rgba, alpha)) = upright_rgba(picture) else { return Vec::new() };
    let [top, left, bottom, right] = info.caps.unwrap_or_default().map(|cap| (cap * info.scale).round().max(0.0) as usize);
    let (columns, rows) = (cuts(width, left, right), cuts(height, top, bottom));
    let mut files = Vec::new();
    for &(y, h) in &rows {
        for &(x, w) in &columns {
            if w == 0 || h == 0 {
                files.push(None);
                continue;
            }
            let mut part = Vec::with_capacity(w * h * 4);
            for row in y..y + h {
                part.extend_from_slice(&rgba[(row * width + x) * 4..(row * width + x + w) * 4]);
            }
            files.push(encode_png(w, h, &part, alpha).and_then(|bytes| write_png_file(&format!("UIImage {picture:#x}"), &bytes)));
        }
    }
    files
}

/// The folder for picture files, emptied the first time it's used in a run.
fn picture_folder() -> &'static str {
    static FOLDER: OnceLock<String> = OnceLock::new();
    FOLDER.get_or_init(|| {
        // The game's tmp folder; with no game (unit tests) the system's.
        let folder = match crate::loader::try_app_dir() {
            Some(_) => format!("{}/tmp/.gamebridge-pictures", crate::fw::foundation::home()),
            None => format!("{}/gamebridge-picture-files-{}", std::env::temp_dir().display(), std::process::id()),
        };
        let _ = std::fs::remove_dir_all(&folder);
        let _ = std::fs::create_dir_all(&folder);
        folder
    })
}

fn write_picture_file(picture: Id) -> Option<String> {
    let (width, height, rgba, alpha) = upright_rgba(picture)?;
    write_png_file(&format!("UIImage {picture:#x}"), &encode_png(width, height, &rgba, alpha)?)
}

/// A new file in the picture folder holding `bytes` (a PNG made to show `what`).
fn write_png_file(what: &str, bytes: &[u8]) -> Option<String> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = format!("{}/{}-{n}.png", picture_folder(), std::process::id());
    match std::fs::write(&file, bytes) {
        Ok(()) => Some(file),
        Err(e) => {
            crate::log!("{what}: can't write a picture file to show it: {e}");
            None
        }
    }
}

extern "C" fn ui_image_cg(this: Id, _: Sel) -> Id {
    image_cg(this)
}

extern "C" fn ui_image_orientation(this: Id, _: Sel) -> i64 {
    uikit::image_info(this).map_or(0, |i| i.orientation)
}

extern "C" fn ui_image_with_cg(_: Class, _: Sel, picture: Id) -> Id {
    counted::autorelease(uikit_image_from_cg(picture, 1.0, 0))
}
extern "C" fn ui_image_with_cg_scale_orientation(_: Class, _: Sel, picture: Id, scale: f64, orientation: i64) -> Id {
    counted::autorelease(uikit_image_from_cg(picture, scale, orientation))
}
extern "C" fn ui_image_init_with_cg(_this: Id, _: Sel, picture: Id) -> Id {
    uikit_image_from_cg(picture, 1.0, 0)
}
extern "C" fn ui_image_init_with_cg_scale_orientation(_this: Id, _: Sel, picture: Id, scale: f64, orientation: i64) -> Id {
    uikit_image_from_cg(picture, scale, orientation)
}

/// A new UIImage (one reference) from PNG or JPEG bytes, turned as its EXIF tag says.
fn ui_image_from_data(bytes: Id, scale: f64) -> Id {
    match decode(&data::bytes(bytes)) {
        Some((bitmap, orientation)) => {
            let picture = make_image(bitmap);
            let made = uikit_image_from_cg(picture, scale, orientation);
            counted::release(picture);
            made
        }
        None => 0,
    }
}
extern "C" fn ui_image_with_data(_: Class, _: Sel, bytes: Id) -> Id {
    counted::autorelease(ui_image_from_data(bytes, 1.0))
}
extern "C" fn ui_image_with_data_scale(_: Class, _: Sel, bytes: Id, scale: f64) -> Id {
    counted::autorelease(ui_image_from_data(bytes, scale))
}
extern "C" fn ui_image_init_with_data(_this: Id, _: Sel, bytes: Id) -> Id {
    ui_image_from_data(bytes, 1.0)
}
extern "C" fn ui_image_init_with_data_scale(_this: Id, _: Sel, bytes: Id, scale: f64) -> Id {
    ui_image_from_data(bytes, scale)
}

/// Where a point (u, v from the top left, 0..1) of the image as shown falls
/// in its stored picture, for each UIImageOrientation.
pub fn orient(orientation: i64, u: f64, v: f64) -> (f64, f64) {
    match orientation {
        1 => (1.0 - u, 1.0 - v), // down
        2 => (1.0 - v, u),       // left: turned a quarter counter-clockwise
        3 => (v, 1.0 - u),       // right: turned a quarter clockwise
        4 => (1.0 - u, v),       // up, mirrored
        5 => (u, 1.0 - v),       // down, mirrored
        6 => (v, u),             // left, mirrored: rows become columns (EXIF 5)
        7 => (1.0 - v, 1.0 - u), // right, mirrored (EXIF 7)
        _ => (u, v),
    }
}

/// -[UIImage drawInRect:]: upright in UIKit's coordinates (top at the
/// rectangle's min y), turned by the image's orientation.
fn ui_image_draw(picture: Id, rect: Rect) {
    let Some(bitmap) = image(image_cg(picture)) else { return };
    let orientation = ui_image_orientation(picture, 0);
    if let Some(mut c) = canvas("-[UIImage drawInRect:]", current_context()) {
        c.draw_bitmap(rect, &bitmap, &|u, v| orient(orientation, u, 1.0 - v));
    }
}

extern "C" fn ui_image_draw_in_rect(this: Id, _: Sel, rect: Rect) {
    ui_image_draw(this, rect);
}

extern "C" fn ui_image_draw_at_point(this: Id, _: Sel, point: Point) {
    ui_image_draw(this, Rect { origin: point, size: uikit::image_points(this) });
}

/// The picture upright (orientation applied), as straight RGBA rows.
fn upright_rgba(picture: Id) -> Option<(usize, usize, Vec<u8>, bool)> {
    let bitmap = image(image_cg(picture))?;
    let layout = bitmap.layout()?;
    let orientation = ui_image_orientation(picture, 0);
    let rgba = bitmap.straight_rgba()?;
    if orientation == 0 {
        return Some((bitmap.width, bitmap.height, rgba, layout.has_alpha()));
    }
    let turned = matches!(orientation, 2 | 3 | 6 | 7);
    let (w, h) = if turned { (bitmap.height, bitmap.width) } else { (bitmap.width, bitmap.height) };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (s, t) = orient(orientation, (x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            let sx = ((s * bitmap.width as f64) as usize).min(bitmap.width - 1);
            let sy = ((t * bitmap.height as f64) as usize).min(bitmap.height - 1);
            let from = (sy * bitmap.width + sx) * 4;
            out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    Some((w, h, out, layout.has_alpha()))
}

pub fn encode_png(width: usize, height: usize, rgba: &[u8], alpha: bool) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    if alpha {
        encoder.write_image(rgba, width as u32, height as u32, image::ExtendedColorType::Rgba8).ok()?;
    } else {
        let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        encoder.write_image(&rgb, width as u32, height as u32, image::ExtendedColorType::Rgb8).ok()?;
    }
    Some(out)
}

extern "C" fn ui_image_png_representation(picture: Id) -> Id {
    // PNG keeps no orientation: the stored pixels are written as they are (as on iOS).
    let Some(bitmap) = image(image_cg(picture)) else { return 0 };
    let Some(layout) = bitmap.layout() else { return 0 };
    match bitmap.straight_rgba().and_then(|rgba| encode_png(bitmap.width, bitmap.height, &rgba, layout.has_alpha())) {
        Some(bytes) => data::make(&bytes),
        None => 0,
    }
}

extern "C" fn ui_image_jpeg_representation(picture: Id, quality: f64) -> Id {
    let Some((width, height, rgba, _)) = upright_rgba(picture) else { return 0 };
    // JPEG has no alpha: colours are written as if over black (premultiplied).
    let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [0, 1, 2].map(|i| ((p[i] as u32 * p[3] as u32 + 127) / 255) as u8)).collect();
    let mut out = Vec::new();
    let quality = (quality.clamp(0.0, 1.0) * 100.0).round().max(1.0) as u8;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    match encoder.encode(&rgb, width as u32, height as u32, image::ExtendedColorType::Rgb8) {
        Ok(()) => data::make(&out),
        Err(e) => {
            crate::log!("UIImageJPEGRepresentation: {e}");
            0
        }
    }
}

/// There is no photo library here: answer like an iPhone where the user
/// denied the game access to Photos (the completion gets an error).
extern "C" fn ui_image_write_to_saved_photos_album(picture: Id, target: Id, selector: Sel, context: usize) {
    crate::log!("UIImageWriteToSavedPhotosAlbum: no photo library here; telling the game access was denied");
    if target == 0 || selector == 0 {
        return;
    }
    // The picture is held until the game has been told (as on iOS).
    counted::retain(picture);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let error = crate::fw::misc::error("ALAssetsLibraryErrorDomain", -3311, "User denied access");
        let name = lock().sel_name(selector);
        if runtime::responds(target, &name) {
            runtime::send(target, &name, &[picture, error, context]);
        }
        counted::release(picture);
    })));
}

/// CFRetain on a data provider counts here (CF objects count with CFRetain);
/// false for every other object.
pub fn cf_retain_provider(object: Id) -> bool {
    shapes::retain_provider(object)
}

/// CFRelease on a data provider: its bytes go back at the last reference
/// (CF objects count with CFRelease); false for every other object.
pub fn cf_release_provider(object: Id) -> bool {
    shapes::release_provider(object)
}

pub fn register(r: &mut Registry) {
    let image_class = define_class("CGImage", "NSObject", 16);
    let context = define_class("CGContext", "NSObject", 16);
    let space = define_class("CGColorSpace", "NSObject", 16);
    let gradient = define_class("CGGradient", "NSObject", 16);
    let path = define_class("CGPath", "NSObject", 16);
    let provider = define_class("CGDataProvider", "NSObject", 16);
    let _ = CLASSES.set(Classes { image: image_class, context, space, gradient, path, provider });
    extern "C" fn retain_method(this: Id, _: Sel) -> Id {
        counted::retain(this)
    }
    extern "C" fn release_method(this: Id, _: Sel) {
        counted::release(this)
    }
    extern "C" fn autorelease_method(this: Id, _: Sel) -> Id {
        counted::autorelease(this)
    }
    extern "C" fn retain_count_method(this: Id, _: Sel) -> usize {
        counted::count(this).unwrap_or(1)
    }
    let ui_image = runtime::class_named("UIImage");
    counted::count_class(provider);
    for class in [image_class, context, ui_image] {
        counted::count_class(class);
        methods(
            class,
            &[
                ("retain", retain_method as usize),
                ("release", release_method as usize),
                ("autorelease", autorelease_method as usize),
                ("retainCount", retain_count_method as usize),
            ],
        );
    }

    class_methods(
        ui_image,
        &[
            ("imageWithCGImage:", ui_image_with_cg as usize),
            ("imageWithCGImage:scale:orientation:", ui_image_with_cg_scale_orientation as usize),
            ("imageWithData:", ui_image_with_data as usize),
            ("imageWithData:scale:", ui_image_with_data_scale as usize),
        ],
    );
    methods(
        ui_image,
        &[
            ("CGImage", ui_image_cg as usize),
            ("imageOrientation", ui_image_orientation as usize),
            ("initWithCGImage:", ui_image_init_with_cg as usize),
            ("initWithCGImage:scale:orientation:", ui_image_init_with_cg_scale_orientation as usize),
            ("initWithData:", ui_image_init_with_data as usize),
            ("initWithData:scale:", ui_image_init_with_data_scale as usize),
            ("drawInRect:", ui_image_draw_in_rect as usize),
            ("drawAtPoint:", ui_image_draw_at_point as usize),
        ],
    );

    r.function("CGImageGetWidth", image_get_width as usize);
    r.function("CGImageGetHeight", image_get_height as usize);
    r.function("CGImageGetBitsPerComponent", image_get_bits_per_component as usize);
    r.function("CGImageGetBitsPerPixel", image_get_bits_per_pixel as usize);
    r.function("CGImageGetBytesPerRow", image_get_bytes_per_row as usize);
    r.function("CGImageGetBitmapInfo", image_get_bitmap_info as usize);
    r.function("CGImageGetAlphaInfo", image_get_alpha_info as usize);
    r.function("CGImageGetColorSpace", image_get_color_space as usize);
    r.function("CGImageRetain", cf_style_retain as usize);
    r.function("CGImageRelease", cf_style_release as usize);
    r.function("CGContextRetain", cf_style_retain as usize);
    r.function("CGContextRelease", cf_style_release as usize);
    // Colour spaces are shared; colours and gradients are small and kept.
    r.function("CGColorSpaceCreateDeviceRGB", space_create_rgb as usize);
    r.function("CGColorSpaceCreateDeviceGray", space_create_gray as usize);
    r.function("CGColorSpaceGetNumberOfComponents", space_get_number_of_components as usize);
    r.function("CGColorSpaceRelease", nothing as usize);
    r.function("CGColorCreate", color_create as usize);
    r.function("CGColorRelease", nothing as usize);
    r.function("CGGradientCreateWithColorComponents", gradient_create_with_color_components as usize);
    r.function("CGGradientRelease", nothing as usize);
    r.function("CGBitmapContextCreate", bitmap_context_create as usize);
    r.function("CGBitmapContextCreateImage", bitmap_context_create_image as usize);
    r.function("CGBitmapContextGetData", bitmap_context_get_data as usize);
    r.function("CGBitmapContextGetWidth", bitmap_context_get_width as usize);
    r.function("CGBitmapContextGetHeight", bitmap_context_get_height as usize);
    r.function("CGBitmapContextGetBytesPerRow", bitmap_context_get_bytes_per_row as usize);
    r.function("CGBitmapContextGetBitsPerComponent", bitmap_context_get_bits_per_component as usize);
    r.function("CGBitmapContextGetBitsPerPixel", bitmap_context_get_bits_per_pixel as usize);
    r.function("CGBitmapContextGetBitmapInfo", bitmap_context_get_bitmap_info as usize);
    r.function("CGBitmapContextGetColorSpace", bitmap_context_get_color_space as usize);
    r.function("CGContextConcatCTM", context_concat_ctm as usize);
    r.function("CGContextTranslateCTM", context_translate_ctm as usize);
    r.function("CGContextScaleCTM", context_scale_ctm as usize);
    r.function("CGContextRotateCTM", context_rotate_ctm as usize);
    r.function("CGContextGetCTM", context_get_ctm as usize);
    r.function("CGContextSaveGState", context_save_gstate as usize);
    r.function("CGContextRestoreGState", context_restore_gstate as usize);
    r.function("CGContextSetFillColorWithColor", context_set_fill_color_with_color as usize);
    r.function("CGContextSetRGBFillColor", context_set_rgb_fill_color as usize);
    r.function("CGContextSetAlpha", context_set_alpha as usize);
    r.function("CGContextFillRect", context_fill_rect as usize);
    r.function("CGContextClearRect", context_clear_rect as usize);
    r.function("CGContextDrawImage", context_draw_image as usize);
    r.function("CGContextDrawLinearGradient", context_draw_linear_gradient as usize);
    r.function("UIGraphicsBeginImageContext", graphics_begin_image_context as usize);
    r.function("UIGraphicsBeginImageContextWithOptions", graphics_begin_image_context_with_options as usize);
    r.function("UIGraphicsGetCurrentContext", graphics_get_current_context as usize);
    r.function("UIGraphicsPushContext", graphics_push_context as usize);
    r.function("UIGraphicsPopContext", graphics_pop_context as usize);
    r.function("UIGraphicsGetImageFromCurrentImageContext", graphics_get_image_from_current_image_context as usize);
    r.function("UIGraphicsEndImageContext", graphics_end_image_context as usize);
    r.function("UIImagePNGRepresentation", ui_image_png_representation as usize);
    r.function("UIImageJPEGRepresentation", ui_image_jpeg_representation as usize);
    r.function("UIImageWriteToSavedPhotosAlbum", ui_image_write_to_saved_photos_album as usize);
    shapes::register(r);
    text::register();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(width: usize, height: usize, info: u32) -> Canvas {
        Canvas::new(std::ptr::null_mut(), width, height, 8, 0, Space::Rgb, info).unwrap()
    }

    fn bytes_at(c: &mut Canvas, x: usize, row: usize) -> Vec<u8> {
        let (at, n) = (row * c.bytes_per_row + x * c.layout.bytes, c.layout.bytes);
        c.memory()[at..at + n].to_vec()
    }

    #[test]
    fn formats_put_components_where_apple_does() {
        let premultiplied_first_little = layout(Space::Rgb, 8, 32, PREMULTIPLIED_FIRST | ORDER_32_LITTLE).unwrap();
        let mut px = [0u8; 4];
        premultiplied_first_little.write(&mut px, [0.5, 0.25, 0.0, 0.5]);
        assert_eq!(px, [0, 64, 128, 128]); // B G R A in memory
        let last = layout(Space::Rgb, 8, 32, PREMULTIPLIED_LAST).unwrap();
        last.write(&mut px, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px, [255, 0, 0, 255]);
        let straight = layout(Space::Rgb, 8, 32, ALPHA_LAST).unwrap();
        assert_eq!(straight.read(&[255, 0, 0, 128]).map(to_byte), [128, 0, 0, 128]);
        assert_eq!(layout(Space::Rgb, 8, 32, NONE_SKIP_FIRST | ORDER_32_LITTLE).unwrap().read(&[1, 2, 3, 0]).map(to_byte), [3, 2, 1, 255]);
        // Formats Core Graphics refuses for drawing.
        assert!(!context_supported(Space::Rgb, 32, ALPHA_LAST));
        assert!(Canvas::new(std::ptr::null_mut(), 4, 4, 8, 0, Space::Rgb, ALPHA_FIRST).is_err());
        assert!(Canvas::new(std::ptr::null_mut(), 4, 4, 8, 8, Space::Rgb, PREMULTIPLIED_LAST).is_err());
        assert_eq!(canvas(5, 1, PREMULTIPLIED_LAST).bytes_per_row, 32);
    }

    #[test]
    fn origin_is_bottom_left_and_uikit_flip_puts_it_top_left() {
        let mut c = canvas(4, 4, PREMULTIPLIED_LAST);
        c.state.fill = [1.0, 0.0, 0.0, 1.0];
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(bytes_at(&mut c, 0, 3), [255, 0, 0, 255]); // last row in memory
        assert_eq!(bytes_at(&mut c, 0, 0), [0, 0, 0, 0]);
        // Half a pixel covered: half the colour.
        c.fill_rect(Rect::new(3.0, 0.0, 0.5, 1.0));
        assert_eq!(bytes_at(&mut c, 3, 3), [128, 0, 0, 128]);
        let mut flipped = canvas(4, 4, PREMULTIPLIED_LAST);
        flipped.state.ctm = Transform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 0.0, ty: 4.0 };
        flipped.state.fill = [0.0, 0.0, 1.0, 0.5];
        flipped.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(bytes_at(&mut flipped, 0, 0), [0, 0, 128, 128]); // first row in memory
    }

    #[test]
    fn images_draw_upright_and_blend_over() {
        // 1x2 picture: red on top, green below.
        let picture = Bitmap {
            width: 1,
            height: 2,
            bits_per_pixel: 32,
            bytes_per_row: 4,
            info: PREMULTIPLIED_LAST,
            space: Space::Rgb,
            data: vec![255, 0, 0, 255, 0, 255, 0, 255],
        };
        let mut c = canvas(1, 2, PREMULTIPLIED_FIRST | ORDER_32_LITTLE);
        c.draw_bitmap(Rect::new(0.0, 0.0, 1.0, 2.0), &picture, &|u, v| (u, v));
        assert_eq!(bytes_at(&mut c, 0, 0), [0, 0, 255, 255]); // red stays on top
        assert_eq!(bytes_at(&mut c, 0, 1), [0, 255, 0, 255]);
        // Half-transparent white over it.
        c.state.fill = [1.0, 1.0, 1.0, 0.5];
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 2.0));
        assert_eq!(bytes_at(&mut c, 0, 0), [128, 128, 255, 255]);
        // Turned a quarter clockwise (UIImageOrientationRight): the stored bottom left shows at the top left.
        assert_eq!(orient(3, 0.0, 0.0), (0.0, 1.0));
        assert_eq!(orient(2, 0.0, 0.0), (1.0, 0.0));
        // Left mirrored: the stored first row runs down the left side.
        assert_eq!(orient(6, 0.0, 1.0), (1.0, 0.0));
        assert_eq!(orient(7, 0.0, 0.0), (1.0, 1.0));
    }

    #[test]
    fn pixel_exact_draws_and_clears_match_the_general_path() {
        // 7x5 picture with every kind of alpha (premultiplied, so colour <= alpha).
        let data: Vec<u8> = (0..35u32).flat_map(|i| { let a = [255u8, 0, 128, 37][i as usize % 4]; [((i * 53) % 256) as u8 & a, ((i * 31) % 256) as u8 & a, (i * 7) as u8 & a, a] }).collect();
        let picture = Bitmap { width: 7, height: 5, bits_per_pixel: 32, bytes_per_row: 28, info: PREMULTIPLIED_LAST, space: Space::Rgb, data };
        let start = |c: &mut Canvas, flip: bool| {
            if flip {
                c.state.ctm = Transform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 0.0, ty: 6.0 };
            }
            c.state.fill = [0.2, 0.6, 0.9, 0.7];
            c.fill_rect(Rect::new(0.0, 0.0, 9.0, 6.0));
        };
        for (flip, info) in [(false, PREMULTIPLIED_LAST), (true, PREMULTIPLIED_FIRST | ORDER_32_LITTLE), (true, NONE_SKIP_LAST)] {
            // Partly off the canvas, so the edges are cut too.
            for at in [Rect::new(1.0, 0.0, 7.0, 5.0), Rect::new(4.0, 3.0, 7.0, 5.0)] {
                let (mut quick, mut general) = (canvas(9, 6, info), canvas(9, 6, info));
                start(&mut quick, flip);
                start(&mut general, flip);
                quick.draw_image(at, &picture);
                general.draw_bitmap(at, &picture, &|u, v| (u, v));
                let (q, g) = (quick.memory().to_vec(), general.memory().to_vec());
                assert!(q.iter().zip(&g).all(|(a, b)| a.abs_diff(*b) <= 1), "draw flip={flip} info={info:#x} at {at:?}:\n{q:?}\n{g:?}");
                quick.clear_rect(Rect::new(2.0, 1.0, 3.0, 2.0));
                general.paint(Rect::new(2.0, 1.0, 3.0, 2.0), Blend::Clear, &|_, _| [0.0, 0.0, 0.0, 1.0]);
                let (q, g) = (quick.memory().to_vec(), general.memory().to_vec());
                assert!(q.iter().zip(&g).all(|(a, b)| a.abs_diff(*b) <= 1), "clear flip={flip} info={info:#x}");
            }
        }
    }

    #[test]
    fn gradients_follow_their_stops() {
        let gradient = Gradient { stops: vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])] };
        let mut c = canvas(4, 1, NONE_SKIP_LAST);
        c.draw_linear_gradient(&gradient, Point { x: 0.0, y: 0.0 }, Point { x: 4.0, y: 0.0 }, 0);
        let row: Vec<u8> = (0..4).map(|x| bytes_at(&mut c, x, 0)[0]).collect();
        assert_eq!(row, [32, 96, 159, 223]);
    }

    #[test]
    fn crushed_png_rows_unfilter() {
        // Two rows of one RGBA pixel: "sub" can't apply, "up" adds the row above.
        let raw = [0, 10, 20, 30, 40, 2, 1, 1, 1, 1];
        assert_eq!(unfilter(&raw, 1, 2, 4).unwrap(), [10, 20, 30, 40, 11, 21, 31, 41]);
        // A broken crushed PNG (header chunk too short) is nil, not a crash.
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in [(b"CgBI", [0u8; 4]), (b"IHDR", [0, 0, 0, 1])] {
            broken.extend_from_slice(&4u32.to_be_bytes());
            broken.extend_from_slice(kind);
            broken.extend_from_slice(&body);
            broken.extend_from_slice(&[0; 4]);
        }
        assert!(decode(&broken).is_none());
    }

    #[test]
    fn jpeg_exif_orientation_turns_the_image_like_uikit() {
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode(&[255, 0, 0, 0, 0, 255], 2, 1, image::ExtendedColorType::Rgb8).unwrap();
        // An EXIF block saying orientation 6 (turned a quarter clockwise), right after the start marker.
        let exif = [b"Exif\0\0MM\0*\0\0\0\x08\0\x01".as_slice(), &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]].concat();
        let segment = [&[0xFF, 0xE1][..], &((exif.len() + 2) as u16).to_be_bytes(), &exif].concat();
        jpeg.splice(2..2, segment);
        let (bitmap, orientation) = decode(&jpeg).unwrap();
        // UIImageOrientationRight; the CGImage keeps the stored pixels (2 wide, 1 high).
        assert_eq!((orientation, bitmap.width, bitmap.height), (3, 2, 1));
        assert_eq!(orientation_from_exif(8), 2);
    }
}
