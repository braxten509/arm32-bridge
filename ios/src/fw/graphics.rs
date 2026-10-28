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

mod compose;
mod render;
mod shapes;
mod text;

pub use compose::{box_picture, cg_picture, cg_size, gradient_picture, group_pictures, new_screen_check, only_made, shape_picture};
pub use render::render_layer;
pub use shapes::{
    clip_path_in, fill_path_in, new_path, path_append, path_apply, path_arc, path_bounds, path_close, path_contains, path_copy, path_curve, path_current,
    path_is_empty, path_line, path_move, path_oval, path_points, path_quad, path_rect, path_rounded_rect, stroke_path_in, StrokeStyle,
};

/// The current UIKit context (UIGraphicsGetCurrentContext), 0 when none.
pub fn current_ui_context() -> Id {
    current_context()
}

pub fn save_state(context: Id) {
    context_save_gstate(context);
}

pub fn restore_state(context: Id) {
    context_restore_gstate(context);
}

/// Draws later things in a context `alpha` times as see-through.
pub fn multiply_alpha(context: Id, alpha: f64) {
    if let Some(mut c) = canvas("-[UIBezierPath fillWithBlendMode:alpha:]", context) {
        c.state.alpha = (c.state.alpha * alpha).clamp(0.0, 1.0);
    }
}

/// Sets a context's alpha and CGBlendMode (CGContextSetAlpha,
/// CGContextSetBlendMode), as -[UIBezierPath fillWithBlendMode:alpha:] does.
pub fn set_alpha_and_blend(context: Id, alpha: f64, blend_mode: i32) {
    if let Some(mut c) = canvas("-[UIBezierPath fillWithBlendMode:alpha:]", context) {
        (c.state.alpha, c.state.blend_mode) = (alpha.clamp(0.0, 1.0), blend_mode);
    }
}

/// Sets a context's fill colour (-[UIColor setFill]).
pub fn set_fill_in(context: Id, rgba: [f64; 4]) {
    if let Some(mut c) = canvas("-[UIColor setFill]", context) {
        c.state.fill = rgba;
    }
}

/// Sets a context's stroke colour (-[UIColor setStroke]).
pub fn set_stroke_in(context: Id, rgba: [f64; 4]) {
    if let Some(mut c) = canvas("-[UIColor setStroke]", context) {
        c.state.stroke = rgba;
    }
}

pub use text::{text_box, text_pictures, TextLook};

// ----- pixel formats -----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Space {
    Rgb,
    Gray,
    /// No colour space: alpha-only bitmaps.
    None,
    /// Palette indices into these RGB colours (CGColorSpaceCreateIndexed;
    /// ImageIO's for palette PNGs without transparency).
    Indexed(&'static Palette),
}

/// An indexed colour space's colours, by index.
pub type Palette = [[u8; 3]; 256];

/// The one kept copy of a palette (pictures share them; the few distinct
/// palettes a game's files have stay for the run).
fn palette_ref(colours: &[[u8; 3]]) -> &'static Palette {
    static KEPT: Mutex<Vec<&'static Palette>> = Mutex::new(Vec::new());
    let mut palette = [[0u8; 3]; 256];
    for (slot, colour) in palette.iter_mut().zip(colours) {
        *slot = *colour;
    }
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&found) = kept.iter().find(|&&p| *p == palette) {
        return found;
    }
    let made: &'static Palette = Box::leak(Box::new(palette));
    kept.push(made);
    made
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
    /// A palette index (the layout's palette gives its colour).
    Index,
}

/// Where each 8-bit component of one pixel sits in memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    slots: [Slot; 4],
    pub bytes: usize,
    premultiplied: bool,
    /// Four-byte RGB pixels: the byte places of red, green, blue and the
    /// fourth component, read and written without going slot by slot.
    fast: Option<Fast>,
    /// An indexed format's colours.
    palette: Option<&'static Palette>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fast {
    red: usize,
    green: usize,
    blue: usize,
    fourth: usize,
    /// The fourth byte is alpha (else skipped: read as opaque, written 255).
    alpha: bool,
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
        (Space::Indexed(_), ALPHA_NONE) => &[Index],
        _ => return None,
    };
    let palette = match space {
        Space::Indexed(palette) => Some(palette),
        _ => None,
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
    let at = |slot: Slot| slots.iter().position(|&s| s == slot);
    let fast = match (list.len(), at(Red), at(Green), at(Blue), at(Alpha).or(at(Skip))) {
        (4, Some(red), Some(green), Some(blue), Some(fourth)) => Some(Fast { red, green, blue, fourth, alpha: slots[fourth] == Alpha }),
        _ => None,
    };
    Some(Layout { slots, bytes: list.len(), premultiplied: matches!(alpha, PREMULTIPLIED_LAST | PREMULTIPLIED_FIRST), fast, palette })
}

/// The formats a bitmap context can draw into (Apple's list for 8 bits per
/// component on iPhone: RGB with premultiplied or skipped alpha, plain gray,
/// alpha only; never indexed).
fn context_supported(space: Space, bits_per_pixel: usize, info: u32) -> bool {
    let alpha = info & ALPHA_MASK;
    match space {
        Space::Rgb => bits_per_pixel == 32 && matches!(alpha, PREMULTIPLIED_LAST | PREMULTIPLIED_FIRST | NONE_SKIP_LAST | NONE_SKIP_FIRST),
        Space::Gray => bits_per_pixel == 8 && alpha == ALPHA_NONE,
        Space::None => bits_per_pixel == 8 && alpha == ALPHA_ONLY,
        Space::Indexed(_) => false,
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
        match self.fast {
            Some(f) => {
                let v = |at: usize| pixel[at] as f32 / 255.0;
                [v(f.red), v(f.green), v(f.blue), if f.alpha { v(f.fourth) } else { 1.0 }]
            }
            None => self.unpack_slots(pixel),
        }
    }

    fn unpack_slots(&self, pixel: &[u8]) -> [f32; 4] {
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
                Slot::Index => {
                    let colour = self.palette.map_or([0; 3], |p| p[byte as usize]);
                    out[..3].copy_from_slice(&colour.map(|c| c as f32 / 255.0));
                }
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
        let Some(f) = self.fast else { return self.write_slots(pixel, c) };
        let mut c = c;
        let a = c[3];
        if !self.premultiplied && f.alpha {
            for v in &mut c[..3] {
                *v = if a > 0.0 { *v / a } else { 0.0 };
            }
        }
        pixel[f.red] = to_byte(c[0]);
        pixel[f.green] = to_byte(c[1]);
        pixel[f.blue] = to_byte(c[2]);
        pixel[f.fourth] = if f.alpha { to_byte(a) } else { 255 };
    }

    fn write_slots(&self, pixel: &mut [u8], c: [f32; 4]) {
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
                // Never drawn into (no indexed contexts).
                Slot::Index => 0,
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

/// Fixed-point scale for the fast blend: a byte value is kept as itself times
/// 128, so the premultiply and bilinear roundings stay under 1/128 of a byte
/// and only the final write rounds to a whole byte.
const FIX: u32 = 128;

/// `255 * FIX * FIX`: the blend's shared denominator (a byte and the source
/// alpha both at `FIX` scale, times the 255 of alpha).
const DENOM: u32 = 255 * FIX * FIX;
const HALF: u32 = DENOM / 2;

/// One pixel read from bytes as premultiplied red, green, blue, alpha at
/// `FIX` scale. Straight-alpha pixels are premultiplied when `premul`; a
/// no-alpha layout gives alpha 255.
#[inline(always)]
fn premul_fix(data: &[u8], f: Fast, premul: bool, at: usize) -> (u32, u32, u32, u32) {
    let r = data[at + f.red] as u32;
    let g = data[at + f.green] as u32;
    let b = data[at + f.blue] as u32;
    let a = if f.alpha { data[at + f.fourth] as u32 } else { 255 };
    if premul {
        ((r * a * FIX + 127) / 255, (g * a * FIX + 127) / 255, (b * a * FIX + 127) / 255, a * FIX)
    } else {
        (r * FIX, g * FIX, b * FIX, a * FIX)
    }
}

/// `a` mixed toward `b` by `w`/`FIX` (both at `FIX` scale, rounded).
#[inline(always)]
fn mix_fix(a: u32, b: u32, w: u32) -> u32 {
    (a * (FIX - w) + b * w + (FIX >> 1)) >> 7
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

    /// An indexed picture's colours (a skipped fourth byte), for drawing it
    /// by the fast ways; None for any other picture.
    fn unindexed(&self) -> Option<Bitmap> {
        let Space::Indexed(palette) = self.space else { return None };
        let mut data = Vec::with_capacity(self.width * self.height * 4);
        for row in 0..self.height {
            for &index in &self.data[row * self.bytes_per_row..row * self.bytes_per_row + self.width] {
                let [r, g, b] = palette[index as usize];
                data.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Some(Bitmap { width: self.width, height: self.height, bits_per_pixel: 32, bytes_per_row: self.width * 4, info: NONE_SKIP_LAST, space: Space::Rgb, data })
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

/// A PNG's chunks (kind, body) before IEND; None when one runs past the end.
fn png_chunks(bytes: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    let mut at = 8usize;
    let mut chunks = Vec::new();
    while at.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body = bytes.get(at + 8..at.checked_add(8 + length)?)?;
        if kind == b"IEND" {
            break;
        }
        chunks.push((kind, body));
        at += 12 + length;
    }
    Some(chunks)
}

/// What a PNG's header says: width, height, bit depth, colour type, interlaced.
fn png_header(chunks: &[(&[u8], &[u8])]) -> Option<(usize, usize, u8, u8, bool)> {
    let body = chunks.iter().find(|(kind, _)| *kind == b"IHDR")?.1;
    if body.len() < 13 {
        return None;
    }
    let width = u32::from_be_bytes(body[0..4].try_into().ok()?) as usize;
    let height = u32::from_be_bytes(body[4..8].try_into().ok()?) as usize;
    let pixels = width.checked_mul(height)?.checked_mul(4)?;
    (width > 0 && height > 0 && pixels <= MOST_PIXEL_BYTES).then_some((width, height, body[8], body[9], body[12] != 0))
}

fn png_data(chunks: &[(&[u8], &[u8])]) -> Vec<u8> {
    chunks.iter().filter(|(kind, _)| *kind == b"IDAT").flat_map(|(_, body)| body.iter().copied()).collect()
}

/// A PNG made by Apple's pngcrush ("CgBI"): raw deflate data, blue before
/// red, alpha premultiplied. iOS reads it as that format directly: BGRA, or
/// BGR with a skipped fourth byte. Broken files give None (nil), never a crash.
fn decode_cgbi(bytes: &[u8]) -> Option<Bitmap> {
    let chunks = png_chunks(bytes)?;
    let (width, height, depth, kind, interlaced) = png_header(&chunks)?;
    let channels = match (depth, kind) {
        (8, 6) => 4,
        (8, 2) => 3,
        _ => {
            crate::log!("crushed PNG: bit depth {depth} colour type {kind} not read");
            return None;
        }
    };
    let stride = width * channels;
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&png_data(&chunks), (stride + 8) * height + 64).ok()?;
    let rows = png_rows(&raw, width, height, channels * 8, interlaced)?;
    let (data, info) = match channels {
        4 => (rows, PREMULTIPLIED_FIRST | ORDER_32_LITTLE),
        _ => (rows.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(), NONE_SKIP_FIRST | ORDER_32_LITTLE),
    };
    Some(Bitmap { width, height, bits_per_pixel: 32, bytes_per_row: width * 4, info, space: Space::Rgb, data })
}

/// A palette PNG without transparency as ImageIO reads it: one byte of
/// palette index per pixel (1, 2 and 4-bit indices widened to a byte) in an
/// indexed colour space. None for any other PNG (read the usual way).
fn decode_palette(bytes: &[u8]) -> Option<Bitmap> {
    let chunks = png_chunks(bytes)?;
    let (width, height, depth, kind, interlaced) = png_header(&chunks)?;
    if kind != 3 || !matches!(depth, 1 | 2 | 4 | 8) || chunks.iter().any(|(kind, _)| *kind == b"tRNS") {
        return None;
    }
    let colours: Vec<[u8; 3]> = chunks.iter().find(|(kind, _)| *kind == b"PLTE")?.1.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let bits = depth as usize;
    let stride = (width * bits).div_ceil(8);
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&png_data(&chunks), (stride + 8) * height + 64).ok()?;
    let rows = png_rows(&raw, width, height, bits, interlaced)?;
    let data = match bits {
        8 => rows,
        _ => (0..height).flat_map(|y| (0..width).map(move |x| (y, x))).map(|(y, x)| (rows[y * stride + x * bits / 8] >> (8 - bits - x * bits % 8)) & ((1u8 << bits) - 1)).collect(),
    };
    Some(Bitmap { width, height, bits_per_pixel: 8, bytes_per_row: width, info: ALPHA_NONE, space: Space::Indexed(palette_ref(&colours)), data })
}

/// A PNG's pixel rows (filters undone, interlaced passes put in place):
/// `bits` per pixel, each row `width * bits` bits rounded up to bytes.
fn png_rows(raw: &[u8], width: usize, height: usize, bits: usize, interlaced: bool) -> Option<Vec<u8>> {
    let distance = (bits / 8).max(1);
    let stride = width.checked_mul(bits)?.div_ceil(8);
    if !interlaced {
        return unfilter_rows(raw, stride, height, distance);
    }
    let mut out = vec![0u8; stride.checked_mul(height)?];
    let mut at = 0;
    // Adam7: each pass's first pixel and its steps across and down.
    for (x0, y0, dx, dy) in [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)] {
        let (w, h) = (width.saturating_sub(x0).div_ceil(dx), height.saturating_sub(y0).div_ceil(dy));
        if w == 0 || h == 0 {
            continue;
        }
        let pass_stride = (w * bits).div_ceil(8);
        let size = (pass_stride + 1) * h;
        let pass = unfilter_rows(raw.get(at..at + size)?, pass_stride, h, distance)?;
        at += size;
        for py in 0..h {
            for px in 0..w {
                copy_pixel(&pass[py * pass_stride..], px, &mut out[(y0 + py * dy) * stride..], x0 + px * dx, bits);
            }
        }
    }
    Some(out)
}

/// Pixel `from_x` of one row into pixel `to_x` of another (`bits` per pixel).
fn copy_pixel(from: &[u8], from_x: usize, to: &mut [u8], to_x: usize, bits: usize) {
    if bits >= 8 {
        let n = bits / 8;
        to[to_x * n..to_x * n + n].copy_from_slice(&from[from_x * n..from_x * n + n]);
        return;
    }
    let mask = (1u8 << bits) - 1;
    let value = (from[from_x * bits / 8] >> (8 - bits - from_x * bits % 8)) & mask;
    let shift = 8 - bits - to_x * bits % 8;
    let byte = &mut to[to_x * bits / 8];
    *byte = (*byte & !(mask << shift)) | (value << shift);
}

/// Undoes PNG's per-row filters over rows of `stride` bytes, each byte
/// predicted from the one `bpp` bytes before it.
fn unfilter_rows(raw: &[u8], stride: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    if stride == 0 || height == 0 || raw.len() < height.checked_mul(stride + 1)? {
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
/// RGBA with straight alpha last, RGB with a skipped fourth byte, plain gray,
/// palette indices (palette PNGs without transparency).
/// Also gives the file's orientation (UIImageOrientation) from its EXIF tag;
/// the pixels stay as stored, as in the CGImage on iOS.
pub fn decode(bytes: &[u8]) -> Option<(Bitmap, i64)> {
    if bytes.len() > 16 && &bytes[1..4] == b"PNG" && &bytes[12..16] == b"CgBI" {
        return decode_cgbi(bytes).map(|b| (b, 0));
    }
    // The header's colour type (byte 25) says whether it has a palette.
    if bytes.len() > 26 && &bytes[1..4] == b"PNG" && bytes[25] == 3 {
        if let Some(bitmap) = decode_palette(bytes) {
            return Some((bitmap, 0));
        }
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

/// A picture's orientation (UIImageOrientation) from its EXIF tag, read from
/// its bytes (which were read once to make the picture).
pub fn file_orientation(bytes: &[u8]) -> i64 {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .and_then(|mut d| d.orientation().ok())
        .map_or(0, |o| orientation_from_exif(o.to_exif()))
}

/// A CGImage: its pixels never change; they go (None) when the last reference does.
struct ImageState {
    bitmap: RwLock<Option<Arc<Bitmap>>>,
    /// An image mask (CGImageMaskCreate): its pixels are the samples decoded
    /// to 8-bit gray for drawing (1 hides, 0 paints); this is the format it
    /// was made with, which its getters report.
    mask: Option<MaskFormat>,
    /// CGImageGetDataProvider's provider: the one a mask was made with, else
    /// one of the pixels made when first asked for; released with the image.
    provider: OnceLock<Id>,
    /// An indexed picture's colours, made the first time it is drawn.
    colours: Mutex<Option<Arc<Bitmap>>>,
}

/// An image mask as the game made it.
struct MaskFormat {
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    decode: Option<[f64; 2]>,
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
    make_picture(ImageState { bitmap: RwLock::new(Some(Arc::new(bitmap))), mask: None, provider: OnceLock::new(), colours: Mutex::new(None) })
}

/// A new image mask with one reference: `samples` decoded to 8-bit gray,
/// `format` and `provider` as the game gave them (the mask holds the provider).
fn make_mask(samples: Bitmap, format: MaskFormat, provider: Id) -> Id {
    counted::retain(provider);
    make_picture(ImageState { bitmap: RwLock::new(Some(Arc::new(samples))), mask: Some(format), provider: OnceLock::from(provider), colours: Mutex::new(None) })
}

fn make_picture(picture: ImageState) -> Id {
    let object = wrap(classes().image, picture);
    counted::track(object, free_image);
    object
}

fn mask_format(object: Id) -> Option<&'static MaskFormat> {
    state::<ImageState>(object, classes().image)?.mask.as_ref()
}

fn free_image(object: Id) {
    if let Some(s) = state::<ImageState>(object, classes().image) {
        *s.bitmap.write().unwrap_or_else(|e| e.into_inner()) = None;
        *s.colours.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(&provider) = s.provider.get() {
            counted::release(provider);
        }
    }
}

/// The picture's pixels (kept alive while in use, even if released meanwhile).
fn image(object: Id) -> Option<Arc<Bitmap>> {
    state::<ImageState>(object, classes().image)?.bitmap.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The pixels to draw a picture with: an indexed one's colours, made once.
fn drawable(object: Id) -> Option<Arc<Bitmap>> {
    let bitmap = image(object)?;
    if !matches!(bitmap.space, Space::Indexed(_)) {
        return Some(bitmap);
    }
    let s = state::<ImageState>(object, classes().image)?;
    let mut colours = s.colours.lock().unwrap_or_else(|e| e.into_inner());
    Some(colours.get_or_insert_with(|| Arc::new(bitmap.unindexed().expect("indexed"))).clone())
}

fn is_mask(object: Id) -> bool {
    mask_format(object).is_some()
}

/// The image's own provider (not the caller's to release, as on Apple):
/// the one a mask was made with, or the pixels as they are stored.
extern "C" fn image_get_data_provider(object: Id) -> Id {
    let Some(s) = state::<ImageState>(object, classes().image) else {
        complain("CGImageGetDataProvider", "image", object);
        return 0;
    };
    if let Some(&provider) = s.provider.get() {
        return provider;
    }
    let Some(bitmap) = image(object) else { return 0 };
    *s.provider.get_or_init(|| shapes::picture_provider(bitmap))
}

extern "C" fn image_is_mask(object: Id) -> bool {
    is_mask(object)
}

/// A mask's decode array (two values), or NULL: other images keep none here.
extern "C" fn image_get_decode(object: Id) -> *const f64 {
    mask_format(object).and_then(|f| f.decode.as_ref()).map_or(std::ptr::null(), |d| d.as_ptr())
}

/// How much (0..1) a mask lets through at (s, t) (0..1, t down from its
/// first row): an image mask's samples hide (1 - S), a gray picture's
/// samples are the alpha (S). Nearest sample: Quartz stretches the mask
/// over the image.
fn mask_alpha(mask: &Bitmap, layout: &Layout, image_mask: bool, s: f64, t: f64) -> f32 {
    let x = ((s * mask.width as f64) as usize).min(mask.width - 1);
    let y = ((t * mask.height as f64) as usize).min(mask.height - 1);
    let at = y * mask.bytes_per_row + x * layout.bytes;
    let c = layout.read_straight(&mask.data[at..at + layout.bytes]);
    let sample = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    if image_mask {
        1.0 - sample
    } else {
        sample
    }
}

/// CGImageCreateWithMask: a new picture of `picture` with the mask's alpha
/// (premultiplied RGBA). The picture can't be a mask itself, and a picture
/// used as the mask must be DeviceGray without alpha (Apple's rules: nil
/// otherwise).
extern "C" fn image_create_with_mask(picture: Id, mask: Id) -> Id {
    let (Some(source), Some(cover)) = (image(picture), image(mask)) else {
        complain("CGImageCreateWithMask", "image", if image(picture).is_none() { picture } else { mask });
        return 0;
    };
    let image_mask = is_mask(mask);
    let (Some(from), Some(through)) = (source.layout(), cover.layout()) else { return 0 };
    let gray_picture = cover.space == Space::Gray && !through.has_alpha();
    if is_mask(picture) || !(image_mask || gray_picture) || cover.width == 0 || cover.height == 0 {
        crate::log!("CGImageCreateWithMask: this image and mask can't be combined");
        return 0;
    }
    let out = layout(Space::Rgb, 8, 32, PREMULTIPLIED_LAST).expect("RGBA layout");
    let (width, height) = (source.width, source.height);
    let mut data = vec![0u8; width * height * 4];
    for row in 0..height {
        for x in 0..width {
            let (s, t) = ((x as f64 + 0.5) / width as f64, (row as f64 + 0.5) / height as f64);
            let alpha = mask_alpha(&cover, &through, image_mask, s, t);
            let at = (row * width + x) * 4;
            out.write(&mut data[at..at + 4], scaled(source.pixel(&from, x, row), alpha));
        }
    }
    make_image(Bitmap { width, height, bits_per_pixel: 32, bytes_per_row: width * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data })
}

/// An image mask painted in `fill` (straight RGBA): a premultiplied RGBA picture.
fn mask_in_color(mask: &Bitmap, fill: [f64; 4]) -> Option<Bitmap> {
    let through = mask.layout()?;
    let color = premultiply(fill);
    let out = layout(Space::Rgb, 8, 32, PREMULTIPLIED_LAST)?;
    let mut data = vec![0u8; mask.width * mask.height * 4];
    for row in 0..mask.height {
        for x in 0..mask.width {
            let (s, t) = ((x as f64 + 0.5) / mask.width as f64, (row as f64 + 0.5) / mask.height as f64);
            let at = (row * mask.width + x) * 4;
            out.write(&mut data[at..at + 4], scaled(color, mask_alpha(mask, &through, true, s, t)));
        }
    }
    Some(Bitmap { width: mask.width, height: mask.height, bits_per_pixel: 32, bytes_per_row: mask.width * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data })
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
// A mask reports the format it was made with (its pixels are only for drawing).
extern "C" fn image_get_bits_per_component(object: Id) -> usize {
    match mask_format(object) {
        Some(f) => f.bits_per_component,
        None => image(object).map_or(0, |_| 8),
    }
}
extern "C" fn image_get_bits_per_pixel(object: Id) -> usize {
    match mask_format(object) {
        Some(f) => f.bits_per_pixel,
        None => image(object).map_or(0, |b| b.bits_per_pixel),
    }
}
extern "C" fn image_get_bytes_per_row(object: Id) -> usize {
    match mask_format(object) {
        Some(f) => f.bytes_per_row,
        None => image(object).map_or(0, |b| b.bytes_per_row),
    }
}
extern "C" fn image_get_bitmap_info(object: Id) -> u32 {
    image(object).map_or(0, |b| b.info)
}
extern "C" fn image_get_alpha_info(object: Id) -> u32 {
    image(object).map_or(0, |b| b.info & ALPHA_MASK)
}
/// An image mask has no colour space (NULL, as on Apple).
extern "C" fn image_get_color_space(object: Id) -> Id {
    if is_mask(object) {
        return 0;
    }
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
    // One shared object per palette, like the device spaces.
    static INDEXED: Mutex<Vec<(usize, Id)>> = Mutex::new(Vec::new());
    let objects = OBJECTS.get_or_init(|| [wrap(classes().space, Space::Rgb), wrap(classes().space, Space::Gray)]);
    match space {
        Space::Rgb => objects[0],
        Space::Gray => objects[1],
        Space::None => 0,
        Space::Indexed(palette) => {
            let key = palette as *const Palette as usize;
            let mut indexed = INDEXED.lock().unwrap_or_else(|e| e.into_inner());
            match indexed.iter().find(|(k, _)| *k == key) {
                Some(&(_, object)) => object,
                None => {
                    let object = wrap(classes().space, space);
                    indexed.push((key, object));
                    object
                }
            }
        }
    }
}

fn space_of(object: Id) -> Space {
    state::<Space>(object, classes().space).map_or(Space::None, |s| *s)
}

fn space_components(space: Space) -> usize {
    match space {
        Space::Rgb => 3,
        Space::Gray | Space::Indexed(_) => 1,
        Space::None => 0,
    }
}

extern "C" fn space_create_rgb() -> Id {
    space_object(Space::Rgb)
}
extern "C" fn space_create_gray() -> Id {
    space_object(Space::Gray)
}
/// CGColorSpaceCreateIndexed over an RGB base: `last` + 1 colours of
/// three bytes in `table`; NULL for other bases.
extern "C" fn space_create_indexed(base: Id, last: usize, table: *const u8) -> Id {
    if space_of(base) != Space::Rgb || table.is_null() || last > 255 {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(table, 3 * (last + 1)) };
    let colours: Vec<[u8; 3]> = bytes.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    space_object(Space::Indexed(palette_ref(&colours)))
}
extern "C" fn space_get_number_of_components(space: Id) -> usize {
    space_components(space_of(space))
}

/// Components in a colour space (then alpha) as red, green, blue, alpha.
fn to_rgba(space: Space, values: &[f64]) -> [f64; 4] {
    match space {
        Space::Gray => [values[0], values[0], values[0], values[1]],
        Space::Indexed(palette) => {
            let [r, g, b] = palette[values[0].round().clamp(0.0, 255.0) as usize].map(|c| c as f64 / 255.0);
            [r, g, b, values[1]]
        }
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

/// CGGradientCreateWithColors(space, CFArray of CGColors, locations or NULL
/// for evenly spread ones). The colours are RGBA here whatever the space.
extern "C" fn gradient_create_with_colors(_space: Id, colors: Id, locations: *const f64) -> Id {
    if !crate::fw::collections::is_array(colors) {
        complain("CGGradientCreateWithColors", "colour array", colors);
        return 0;
    }
    let colors: Vec<[f64; 4]> = crate::fw::collections::array_items(colors).into_iter().filter_map(color_rgba).collect();
    if colors.is_empty() {
        return 0;
    }
    let count = colors.len();
    let mut stops: Vec<(f64, [f64; 4])> = colors
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let location = if locations.is_null() {
                if count == 1 { 0.0 } else { i as f64 / (count - 1) as f64 }
            } else {
                unsafe { *locations.add(i) }
            };
            (location, c)
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
    /// CGBlendMode (shapes::composite).
    blend_mode: i32,
    /// How much of each device pixel (rows from the bottom) drawing may touch.
    clip: Option<Arc<Vec<f32>>>,
    /// CGTextDrawingMode: fill, stroke or both (fw/graphics/text.rs).
    text_mode: i32,
    /// CGContextSetShadow: offset (device pixels, y up), blur and colour
    /// (straight); drawing casts it.
    shadow: Option<Shadow>,
    /// CGLineCap (butt 0, round 1, square 2) and CGLineJoin (miter 0, round
    /// 1, bevel 2), the miter limit, and the dash (phase, lengths).
    line_cap: i32,
    line_join: i32,
    miter_limit: f64,
    dash: Option<Arc<(f64, Vec<f64>)>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Shadow {
    offset: (f64, f64),
    blur: f64,
    color: [f64; 4],
}

impl Shadow {
    /// How far its blur spreads, in pixels (three box passes of about the
    /// deviation each).
    fn reach(&self) -> i64 {
        (1.5 * self.blur).ceil() as i64 + 2
    }

    /// Its offset in whole device pixels (y up).
    fn step(&self) -> (i64, i64) {
        (self.offset.0.round() as i64, self.offset.1.round() as i64)
    }
}

impl GraphicsState {
    fn new() -> GraphicsState {
        GraphicsState {
            ctm: IDENTITY,
            fill: [0.0, 0.0, 0.0, 1.0],
            alpha: 1.0,
            stroke: [0.0, 0.0, 0.0, 1.0],
            line_width: 1.0,
            // A new context's colour spaces are DeviceGray (UIKit sets
            // DeviceRGB for drawRect:, `draw_picture`).
            fill_space: Space::Gray,
            stroke_space: Space::Gray,
            blend_mode: 0,
            clip: None,
            text_mode: 0,
            shadow: None,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash: None,
        }
    }
}

/// What a transparency layer (CGContextBeginTransparencyLayer) is drawn
/// over: the context's own pixels and state, put back when it ends.
struct Under {
    data: *mut u8,
    owned: Option<Vec<u8>>,
    layout: Layout,
    bytes_per_row: usize,
    width: usize,
    height: usize,
    state: GraphicsState,
    /// Where the layer's own pixels lie on what it was begun on (they may
    /// reach past its edges, to what casts a shadow onto it).
    area: Area,
    /// Past the pixel budget: no pixels of its own; the drawing went straight
    /// on with the alpha, blend and clip it was begun with (no shadow).
    direct: bool,
    /// The coverage kept for what it was begun on (`Canvas::cover`).
    cover: Option<Vec<f32>>,
}

/// Device pixels x0..x1, y0..y1 (y up), past a canvas's edges too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Area {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Area {
    pub(super) const NONE: Area = Area { x0: 0, y0: 0, x1: 0, y1: 0 };

    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    fn pixels(self) -> usize {
        if self.is_empty() { 0 } else { ((self.x1 - self.x0) * (self.y1 - self.y0)) as usize }
    }

    fn meet(self, other: Area) -> Area {
        Area { x0: self.x0.max(other.x0), y0: self.y0.max(other.y0), x1: self.x1.min(other.x1), y1: self.y1.min(other.y1) }
    }

    /// The box around both.
    fn join(self, other: Area) -> Area {
        Area { x0: self.x0.min(other.x0), y0: self.y0.min(other.y0), x1: self.x1.max(other.x1), y1: self.y1.max(other.y1) }
    }

    fn grown(self, by: i64) -> Area {
        Area { x0: self.x0 - by, y0: self.y0 - by, x1: self.x1 + by, y1: self.y1 + by }
    }

    fn moved(self, (dx, dy): (i64, i64)) -> Area {
        Area { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }
}

/// The most pixels open transparency layers may hold at once (four of
/// compose.rs's biggest pictures).
const LAYER_PIXELS: usize = 16_000_000;

#[cfg(test)]
thread_local! {
    /// The pixels the last transparency layer begun on this thread covers
    /// (tests check how big the layers drawings make are).
    pub(super) static LAST_LAYER: std::cell::Cell<Area> = const { std::cell::Cell::new(Area::NONE) };
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
    /// Open transparency layers, innermost last: while one is open, drawing
    /// goes into its own premultiplied RGBA pixels.
    layers: Vec<Under>,
    /// How much of each pixel (rows from the bottom) the drawing covered:
    /// kept only in a shadowed drawing's layer that is put down in a mode
    /// where its clear parts count (`shapes::clear_source_counts`), so a
    /// clear picture clears what it covers and nothing past it.
    cover: Option<Vec<f32>>,
}

// The memory is the game's or the canvas's own; it's only touched under the context's lock.
unsafe impl Send for Canvas {}

/// A CGContext: drawn into by one thread at a time.
struct ContextState(Mutex<Canvas>);

enum Blend {
    Over,
    Clear,
    /// The source replaces what is there (kCGBlendModeCopy), as far as it covers the pixel.
    Copy,
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
            (Space::None | Space::Indexed(_), _) => 8,
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
            // Apple picks the row size itself: rounded up to 32 bytes for its
            // own memory (CGBitmapGetAlignedBytesPerRow).
            0 if data.is_null() => (least + 31) & !31,
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
            layers: Vec::new(),
            cover: None,
        })
    }

    fn close(&mut self) {
        self.owned = None;
        // A clip mask is as big as the pixels: nothing of the context's stays.
        self.state = GraphicsState::new();
        self.saved = Vec::new();
        self.path = shapes::Outline::default();
        self.layers = Vec::new();
        self.cover = None;
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

    /// Device pixel (x, y from the bottom) composited with `src`
    /// (premultiplied, the alpha applied) as far as `cover` (how much of the
    /// pixel the drawing covers) and the clip reach. `Over` uses the state's
    /// blend mode: what it makes of the pixel is mixed with what is there by
    /// the cover, so a Porter-Duff mode leaves what the drawing misses.
    fn blend(&mut self, x: usize, y: usize, src: [f32; 4], cover: f32, blend: &Blend) {
        let k = match &self.state.clip {
            Some(clip) => cover * clip.get(y * self.width + x).copied().unwrap_or(0.0),
            None => cover,
        };
        let width = self.width;
        if let Some(covered) = self.cover.as_mut().and_then(|c| c.get_mut(y * width + x)) {
            *covered += k - *covered * k;
        }
        let mode = match blend {
            Blend::Over => self.state.blend_mode,
            Blend::Clear => shapes::BLEND_CLEAR,
            Blend::Copy => shapes::BLEND_COPY,
        };
        let (layout, at) = (self.layout, (self.height - 1 - y) * self.bytes_per_row + x * self.layout.bytes);
        let pixel = &mut self.memory()[at..at + layout.bytes];
        let out = match mode {
            shapes::BLEND_NORMAL => {
                let src = scaled(src, k);
                // An opaque source over what is there is the source (dst * 0 = 0).
                if src[3] == 1.0 {
                    return layout.write(pixel, src);
                }
                over(src, layout.read(pixel))
            }
            // The source's alpha says how much is cleared.
            shapes::BLEND_CLEAR => scaled(layout.read(pixel), 1.0 - src[3] * k),
            mode => {
                let dst = layout.read(pixel);
                let made = shapes::composite(mode, src, dst);
                [0, 1, 2, 3].map(|i| dst[i] + (made[i] - dst[i]) * k)
            }
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
        let alpha = self.state.alpha as f32;
        if straight {
            // A straight transform keeps x and y apart: each column's and
            // row's coverage and user coordinate are worked out once, not
            // once per pixel.
            let (x_start, x_end) = (x0.floor() as usize, x1.ceil() as usize);
            let (y_start, y_end) = (y0.floor() as usize, y1.ceil() as usize);
            let swapped = ctm.a == 0.0 && ctm.d == 0.0;
            let columns: Vec<(f64, (f64, f64))> = (x_start..x_end)
                .map(|x| {
                    let fx = x as f64;
                    let coverage = ((fx + 1.0).min(x1) - fx.max(x0)).max(0.0);
                    (coverage, geometry::apply(inverse, fx + 0.5, 0.0))
                })
                .collect();
            let rows: Vec<(f64, (f64, f64))> = (y_start..y_end)
                .map(|y| {
                    let fy = y as f64;
                    let coverage = ((fy + 1.0).min(y1) - fy.max(y0)).max(0.0);
                    (coverage, geometry::apply(inverse, 0.0, fy + 0.5))
                })
                .collect();
            for (y, &(coverage_y, row)) in rows.iter().enumerate() {
                for (x, &(coverage_x, column)) in columns.iter().enumerate() {
                    let coverage = coverage_x * coverage_y;
                    if coverage <= 0.0 {
                        continue;
                    }
                    let (ux, uy) = if swapped { (row.0, column.1) } else { (column.0, row.1) };
                    let color = shade(ux.clamp(min_x, max_x), uy.clamp(min_y, max_y));
                    self.blend(x + x_start, y + y_start, scaled(color, alpha), coverage as f32, &blend);
                }
            }
            return;
        }
        let inside = |dx: f64, dy: f64| {
            let (ux, uy) = geometry::apply(inverse, dx, dy);
            ux >= min_x && ux <= max_x && uy >= min_y && uy <= max_y
        };
        for y in y0.floor() as usize..y1.ceil() as usize {
            for x in x0.floor() as usize..x1.ceil() as usize {
                let (fx, fy) = (x as f64, y as f64);
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        hits += inside(fx + (sx as f64 + 0.5) / 4.0, fy + (sy as f64 + 0.5) / 4.0) as u32;
                    }
                }
                let coverage = hits as f64 / 16.0;
                if coverage <= 0.0 {
                    continue;
                }
                let (ux, uy) = geometry::apply(inverse, fx + 0.5, fy + 0.5);
                let color = shade(ux.clamp(min_x, max_x), uy.clamp(min_y, max_y));
                self.blend(x, y, scaled(color, alpha), coverage as f32, &blend);
            }
        }
    }

    fn fill_rect(&mut self, rect: Rect) {
        let color = premultiply(self.state.fill);
        self.paint(rect, Blend::Over, &|_, _| color);
    }

    /// The fill colour replacing what is there (UIRectFill).
    fn copy_rect(&mut self, rect: Rect) {
        let color = premultiply(self.state.fill);
        self.paint(rect, Blend::Copy, &|_, _| color);
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
        if self.state.clip.is_some() || self.cover.is_some() {
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
        if let Some(colours) = bitmap.unindexed() {
            return self.draw_bitmap(rect, &colours, map);
        }
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
        if !self.draw_bitmap_fast(rect, bitmap, &source, map) {
            self.draw_bitmap_paint(rect, bitmap, &source, map);
        }
    }

    /// The slow, general path: a per-pixel bilinear sample through `paint`.
    fn draw_bitmap_paint(&mut self, rect: Rect, bitmap: &Bitmap, source: &Layout, map: &dyn Fn(f64, f64) -> (f64, f64)) {
        self.paint(rect, Blend::Over, &|ux, uy| {
            let u = (ux - rect.origin.x) / rect.size.width;
            let v = (rect.origin.y + rect.size.height - uy) / rect.size.height;
            let (s, t) = map(u, v);
            bitmap.sample(source, s, t)
        });
    }

    /// The common fast path: an axis-aligned (not turned) transform, a map
    /// where s depends on u alone and t on v alone, and 4-byte pixel layouts
    /// on both sides. Each column's source x index and weight and each row's
    /// source y index and weight are worked out once, source pixels are read
    /// straight from their bytes, and the blend is fixed-point integer math
    /// rounded once — no per-pixel closures or array maps. False falls back
    /// to the general path (turned transforms, odd layouts and maps).
    fn draw_bitmap_fast(&mut self, rect: Rect, bitmap: &Bitmap, source: &Layout, map: &dyn Fn(f64, f64) -> (f64, f64)) -> bool {
        let ctm = self.state.ctm;
        // Axis-aligned only: no rotation, no 90-degree swap, so source x is a
        // function of device x alone and source y of device y alone.
        if ctm.b != 0.0 || ctm.c != 0.0 || ctm.a == 0.0 || ctm.d == 0.0 {
            return false;
        }
        // Only the normal and clear blends are worked out here, without
        // keeping coverage (`cover`).
        if !matches!(self.state.blend_mode, shapes::BLEND_NORMAL | shapes::BLEND_CLEAR) || self.cover.is_some() {
            return false;
        }
        let (Some(sf), Some(df)) = (source.fast, self.layout.fast) else {
            return false;
        };
        // A separable map: s(u, v) == s(u, 0) and t(u, v) == t(0, v), checked
        // at the corners (exact for the affine maps used here, and it also
        // turns away any u*v cross terms).
        let (s00, t00) = map(0.0, 0.0);
        let (s10, t10) = map(1.0, 0.0);
        let (s01, t01) = map(0.0, 1.0);
        let (s11, t11) = map(1.0, 1.0);
        if (s01 - s00).abs() > 1e-12 || (s11 - s10).abs() > 1e-12 || (t10 - t00).abs() > 1e-12 || (t11 - t01).abs() > 1e-12 {
            return false;
        }
        let Some(inverse) = geometry::invert(ctm) else {
            return false;
        };

        // The device area the rectangle covers, as paint works it out.
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .map(|(fx, fy)| geometry::apply(ctm, rect.origin.x + fx * rect.size.width, rect.origin.y + fy * rect.size.height));
        let x0 = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min).max(0.0);
        let x1 = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max).min(self.width as f64);
        let y0 = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min).max(0.0);
        let y1 = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max).min(self.height as f64);
        if !(x0 < x1 && y0 < y1) {
            return true; // nothing to draw
        }
        let (min_x, max_x) = (rect.origin.x, rect.origin.x + rect.size.width);
        let (min_y, max_y) = (rect.origin.y, rect.origin.y + rect.size.height);

        let (x_start, x_end) = (x0.floor() as usize, x1.ceil() as usize);
        let (y_start, y_end) = (y0.floor() as usize, y1.ceil() as usize);

        let source_bytes = source.bytes;
        let mut columns: Vec<(f32, usize, usize, u32)> = Vec::with_capacity(x_end - x_start);
        for x in x_start..x_end {
            let fx = x as f64;
            let coverage_x = (((fx + 1.0).min(x1) - fx.max(x0)).max(0.0)) as f32;
            let ux = geometry::apply(inverse, fx + 0.5, 0.0).0;
            let u = (ux.clamp(min_x, max_x) - min_x) / rect.size.width;
            let s = map(u, 0.0).0;
            let sx = (s * bitmap.width as f64 - 0.5).clamp(0.0, (bitmap.width - 1) as f64);
            let x0i = sx.floor() as usize;
            let x1i = (x0i + 1).min(bitmap.width - 1);
            let wfx = ((sx - x0i as f64) as f32 * FIX as f32).round() as u32;
            columns.push((coverage_x, x0i * source_bytes, x1i * source_bytes, wfx));
        }
        let mut rows: Vec<(f32, usize, usize, u32)> = Vec::with_capacity(y_end - y_start);
        for y in y_start..y_end {
            let fy = y as f64;
            let coverage_y = (((fy + 1.0).min(y1) - fy.max(y0)).max(0.0)) as f32;
            let uy = geometry::apply(inverse, 0.0, fy + 0.5).1;
            let v = (max_y - uy.clamp(min_y, max_y)) / rect.size.height;
            let t = map(0.0, v).1;
            let sy = (t * bitmap.height as f64 - 0.5).clamp(0.0, (bitmap.height - 1) as f64);
            let y0i = sy.floor() as usize;
            let y1i = (y0i + 1).min(bitmap.height - 1);
            let wfy = ((sy - y0i as f64) as f32 * FIX as f32).round() as u32;
            rows.push((coverage_y, y0i * bitmap.bytes_per_row, y1i * bitmap.bytes_per_row, wfy));
        }

        let alpha = self.state.alpha as f32;
        let clear = self.state.blend_mode == shapes::BLEND_CLEAR;
        let clip = self.state.clip.clone();
        let premul = sf.alpha && !source.premultiplied;
        let data = bitmap.data.as_slice();
        let (width, height) = (self.width, self.height);
        let (bytes_per_row, dest_bytes) = (self.bytes_per_row, self.layout.bytes);
        let memory = self.memory();

        for (yi, &(coverage_y, y0_base, y1_base, wfy)) in rows.iter().enumerate() {
            if coverage_y <= 0.0 {
                continue;
            }
            let dev_y = yi + y_start;
            let dst_row = (height - 1 - dev_y) * bytes_per_row;
            for (xi, &(coverage_x, x0_off, x1_off, wfx)) in columns.iter().enumerate() {
                let cov = coverage_x * coverage_y;
                if cov <= 0.0 {
                    continue;
                }
                let dev_x = xi + x_start;
                let mut k = cov * alpha;
                if let Some(c) = &clip {
                    k *= c[dev_y * width + dev_x];
                }
                let k_fix = (k * FIX as f32).round().clamp(0.0, FIX as f32) as u32;
                if k_fix == 0 {
                    // Fully transparent: nothing blends, but a no-alpha
                    // destination's skipped fourth byte is still written 255,
                    // as the general path's blend does.
                    if !df.alpha {
                        memory[dst_row + dev_x * dest_bytes + df.fourth] = 255;
                    }
                    continue;
                }
                let (addr00, addr10) = (y0_base + x0_off, y0_base + x1_off);
                let (addr01, addr11) = (y1_base + x0_off, y1_base + x1_off);
                let (r00, g00, b00, al00) = premul_fix(data, sf, premul, addr00);
                let (r10, g10, b10, al10) = premul_fix(data, sf, premul, addr10);
                let (r01, g01, b01, al01) = premul_fix(data, sf, premul, addr01);
                let (r11, g11, b11, al11) = premul_fix(data, sf, premul, addr11);
                let r = mix_fix(mix_fix(r00, r10, wfx), mix_fix(r01, r11, wfx), wfy);
                let g = mix_fix(mix_fix(g00, g10, wfx), mix_fix(g01, g11, wfx), wfy);
                let b = mix_fix(mix_fix(b00, b10, wfx), mix_fix(b01, b11, wfx), wfy);
                let a = mix_fix(mix_fix(al00, al10, wfx), mix_fix(al01, al11, wfx), wfy);
                // out = src*k + dst*(1 - alpha*k), premultiplied; a clear
                // blend drops the src term.
                let base = dst_row + dev_x * dest_bytes;
                let dr = memory[base + df.red] as u32;
                let dg = memory[base + df.green] as u32;
                let db = memory[base + df.blue] as u32;
                let da = if df.alpha { memory[base + df.fourth] as u32 } else { 255 };
                let keep = DENOM - a * k_fix;
                let (out_r, out_g, out_b, out_a) = if clear {
                    (
                        (dr * keep + HALF) / DENOM,
                        (dg * keep + HALF) / DENOM,
                        (db * keep + HALF) / DENOM,
                        (da * keep + HALF) / DENOM,
                    )
                } else {
                    (
                        (r * k_fix * 255 + dr * keep + HALF) / DENOM,
                        (g * k_fix * 255 + dg * keep + HALF) / DENOM,
                        (b * k_fix * 255 + db * keep + HALF) / DENOM,
                        (a * k_fix * 255 + da * keep + HALF) / DENOM,
                    )
                };
                memory[base + df.red] = out_r as u8;
                memory[base + df.green] = out_g as u8;
                memory[base + df.blue] = out_b as u8;
                memory[base + df.fourth] = if df.alpha { out_a as u8 } else { 255 };
            }
        }
        true
    }

    /// CGContextDrawImage: a picture landing pixel for pixel (one picture
    /// pixel per device pixel, on pixel edges, no turn) is read straight
    /// from its pixels; anything else is sampled by `draw_bitmap`.
    fn draw_image(&mut self, rect: Rect, bitmap: &Bitmap) {
        if let Some(colours) = bitmap.unindexed() {
            return self.draw_image(rect, &colours);
        }
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
        // Clear and opaque pixels are left or copied byte for byte (the
        // float blend handles the in-between ones) when nothing clips or
        // fades, the blend is normal and both layouts have fixed byte places.
        let straight = self.state.clip.is_none()
            && self.state.blend_mode == shapes::BLEND_NORMAL
            && self.cover.is_none()
            && self.state.alpha == 1.0
            && source.fast.is_some()
            && self.layout.fast.is_some();
        self.draw_whole_pixels_loop(x0, x1, y0, y1, bitmap, &source, straight)
    }

    /// The whole-pixel loop, with or without the byte fast path (`straight`).
    fn draw_whole_pixels_loop(&mut self, x0: i64, x1: i64, y0: i64, y1: i64, bitmap: &Bitmap, source: &Layout, straight: bool) -> bool {
        // The picture's first row is the rectangle's top (user max y): the
        // device top when y goes up, the device bottom when flipped.
        let (mirrored, upright) = (self.state.ctm.a < 0.0, self.state.ctm.d > 0.0);
        let alpha = self.state.alpha as f32;
        let (xs, ys) = self.cut((x0, x1, y0, y1));
        for y in ys {
            let row = (if upright { y1 - 1 - y as i64 } else { y as i64 - y0 }) as usize;
            for x in xs.clone() {
                let column = (if mirrored { x1 - 1 - x as i64 } else { x as i64 - x0 }) as usize;
                let at = row * bitmap.bytes_per_row + column * source.bytes;
                let pixel = &bitmap.data[at..at + source.bytes];
                if straight {
                    self.blend_straight(x, y, pixel, source);
                } else {
                    self.blend(x, y, scaled(source.read(pixel), alpha), 1.0, &Blend::Over);
                }
            }
        }
        true
    }

    /// A whole-pixel source straight into (x, y): an opaque one is copied
    /// byte for byte (into this layout's byte places); any other goes through
    /// the float blend (even a clear one writes bytes, e.g. a no-alpha
    /// destination's fourth byte).
    fn blend_straight(&mut self, x: usize, y: usize, pixel: &[u8], source: &Layout) {
        let (Some(sf), Some(df)) = (source.fast, self.layout.fast) else { return };
        let alpha = if sf.alpha { pixel[sf.fourth] } else { 255 };
        let at = (self.height - 1 - y) * self.bytes_per_row + x * self.layout.bytes;
        if alpha == 255 {
            let memory = self.memory();
            memory[at + df.red] = pixel[sf.red];
            memory[at + df.green] = pixel[sf.green];
            memory[at + df.blue] = pixel[sf.blue];
            memory[at + df.fourth] = 255;
        } else {
            self.blend(x, y, source.read(pixel), 1.0, &Blend::Over);
        }
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
                self.blend(x, y, scaled(premultiply(gradient.at(t)), alpha), 1.0, &Blend::Over);
            }
        }
    }

    /// CGContextBeginTransparencyLayer: later drawing goes into clear pixels
    /// of its own, with alpha 1, the normal blend, no shadow and no clip (the
    /// context's are applied when the layer is put down).
    ///
    /// The layer's pixels cover only what can show: the canvas and, with a
    /// shadow, what casts one onto it (past the edges too), cut to `area`
    /// when the drawing can reach no further. Drawing goes in through a
    /// CTM (and path) moved to the layer's corner. Past the budget
    /// (LAYER_PIXELS) no layer is made: the drawing goes straight on with
    /// the context's alpha, blend and clip, and no shadow.
    ///
    /// `drawing`: the layer holds one shadowed drawing (`shadowed`), whose
    /// clear parts outside what it covers must not count when it is put down.
    pub(super) fn begin_layer(&mut self, area: Option<Area>, drawing: bool) {
        let canvas = self.whole();
        let wanted = match self.state.shadow {
            Some(shadow) => canvas.join(canvas.moved((-shadow.step().0, -shadow.step().1)).grown(shadow.reach())),
            None => canvas,
        };
        let mut region = area.map_or(wanted, |area| area.meet(wanted));
        if region.is_empty() {
            // Nothing of it can show: drawn into one pixel and dropped.
            region = Area { x0: canvas.x1, y0: canvas.y1, x1: canvas.x1 + 1, y1: canvas.y1 + 1 };
        }
        #[cfg(test)]
        LAST_LAYER.with(|last| last.set(region));
        let held: usize = self.layers.iter().filter(|under| !under.direct).map(|under| under.area.pixels()).sum();
        let (width, height) = (self.width, self.height);
        if held + region.pixels() > LAYER_PIXELS {
            crate::log!("CGContextBeginTransparencyLayer: {}x{} is past the layer budget; drawn straight on", region.x1 - region.x0, region.y1 - region.y0);
            let under = Under { data: self.data, owned: None, layout: self.layout, bytes_per_row: self.bytes_per_row, width, height, state: self.state.clone(), area: canvas, direct: true, cover: None };
            self.state.shadow = None;
            self.layers.push(under);
            return;
        }
        let (w, h) = ((region.x1 - region.x0) as usize, (region.y1 - region.y0) as usize);
        let mut pixels = vec![0u8; w * h * 4];
        let keeps_cover = drawing && shapes::clear_source_counts(self.state.blend_mode);
        let cover = std::mem::replace(&mut self.cover, keeps_cover.then(|| vec![0.0; w * h]));
        let under = Under { data: self.data, owned: self.owned.take(), layout: self.layout, bytes_per_row: self.bytes_per_row, width, height, state: self.state.clone(), area: region, direct: false, cover };
        self.data = pixels.as_mut_ptr();
        self.owned = Some(pixels);
        self.layout = layout(Space::Rgb, 8, 32, PREMULTIPLIED_LAST).expect("RGBA layout");
        (self.width, self.height, self.bytes_per_row) = (w, h, w * 4);
        let by = (-region.x0 as f64, -region.y0 as f64);
        (self.state.ctm.tx, self.state.ctm.ty) = (self.state.ctm.tx + by.0, self.state.ctm.ty + by.1);
        self.path.shift(by);
        self.state.alpha = 1.0;
        self.state.blend_mode = 0;
        self.state.shadow = None;
        self.state.clip = None;
        self.layers.push(under);
    }

    /// CGContextEndTransparencyLayer: the layer's pixels are put down as one
    /// picture over what it was begun on, with the state from then (alpha,
    /// blend, shadow, clip), which comes back.
    pub(super) fn end_layer(&mut self) {
        let Some(under) = self.layers.pop() else { return };
        if under.direct {
            self.state = under.state;
            return;
        }
        let pixels = self.owned.take().unwrap_or_default();
        let (w, h) = (self.width, self.height);
        self.data = under.data;
        self.owned = under.owned;
        self.layout = under.layout;
        (self.width, self.height, self.bytes_per_row) = (under.width, under.height, under.bytes_per_row);
        self.state = under.state;
        self.path.shift((under.area.x0 as f64, under.area.y0 as f64));
        let cover = std::mem::replace(&mut self.cover, under.cover);
        if pixels.len() == w * h * 4 {
            self.put_down(&pixels, w, h, under.area, cover.as_deref());
        }
    }

    /// A layer's premultiplied RGBA pixels (`w` x `h`, rows from the top,
    /// lying at `at`) drawn over the canvas with the alpha, blend, clip and
    /// shadow; what falls past the canvas's edges is left out. Its clear
    /// pixels count only in modes where they change things, and then, with
    /// `cover` (how much of each pixel the drawing covered, rows from the
    /// bottom), only as far as the drawing covered them.
    fn put_down(&mut self, pixels: &[u8], w: usize, h: usize, at: Area, cover: Option<&[f32]>) {
        let (width, height) = (self.width as i64, self.height as i64);
        // A layer pixel's place on the canvas (device x, y up).
        let place = |u: i64, row: i64| (at.x0 + u, at.y0 + h as i64 - 1 - row);
        let inside = |(x, y): (i64, i64)| (0..width).contains(&x) && (0..height).contains(&y);
        let source = layout(Space::Rgb, 8, 32, PREMULTIPLIED_LAST).expect("RGBA layout");
        let alpha = self.state.alpha as f32;
        if let Some(shadow) = self.state.shadow {
            // The cover with room round it for the blur to spread into.
            let reach = shadow.reach() as usize;
            let (bw, bh) = (w + 2 * reach, h + 2 * reach);
            let mut cover = vec![0f32; bw * bh];
            for row in 0..h {
                for u in 0..w {
                    cover[(row + reach) * bw + u + reach] = pixels[(row * w + u) * 4 + 3] as f32 / 255.0;
                }
            }
            // Quartz's blur is about twice the Gaussian's deviation.
            let cover = compose::blur(cover, bw, bh, shadow.blur / 2.0);
            let (dx, dy) = shadow.step();
            let color = scaled(premultiply(shadow.color), alpha);
            for brow in 0..bh {
                for bx in 0..bw {
                    let k = cover[brow * bw + bx];
                    let (x, y) = place(bx as i64 - reach as i64, brow as i64 - reach as i64);
                    let (x, y) = (x + dx, y + dy);
                    if k > 0.0 && inside((x, y)) {
                        self.blend(x as usize, y as usize, scaled(color, k.min(1.0)), 1.0, &Blend::Over);
                    }
                }
            }
        }
        let clear_counts = shapes::clear_source_counts(self.state.blend_mode);
        for row in 0..h {
            for u in 0..w {
                let from = (row * w + u) * 4;
                let (x, y) = place(u as i64, row as i64);
                if (pixels[from + 3] == 0 && !clear_counts) || !inside((x, y)) {
                    continue;
                }
                let color = source.read(&pixels[from..from + 4]);
                // The drawing's own colour where it covered part of a pixel.
                let (color, k) = match cover {
                    Some(cover) => match cover[(h - 1 - row) * w + u] {
                        k if k <= 0.0 => continue,
                        k if k < 1.0 => (scaled(color, 1.0 / k).map(|v| v.min(1.0)), k),
                        _ => (color, 1.0),
                    },
                    None => (color, 1.0),
                };
                self.blend(x as usize, y as usize, scaled(color, alpha), k, &Blend::Over);
            }
        }
    }

    /// Runs `draw` so that it casts the context's shadow (if it has one), as
    /// Quartz draws every shape: drawn on its own, then put down with it.
    /// `area` gives the pixels the drawing can land in (past the canvas's
    /// edges too). `draw` is told how far device-space outlines made before
    /// it must move to land in the layer (0 without a shadow).
    fn shadowed(&mut self, area: impl FnOnce(&Canvas) -> Area, draw: impl FnOnce(&mut Canvas, (f64, f64))) {
        if self.state.shadow.is_none() {
            return draw(self, (0.0, 0.0));
        }
        let area = area(self);
        self.begin_layer(Some(area), true);
        let by = match self.layers.last() {
            Some(under) if !under.direct => (-under.area.x0 as f64, -under.area.y0 as f64),
            _ => (0.0, 0.0),
        };
        draw(self, by);
        self.end_layer();
    }

    /// Every pixel.
    fn whole(&self) -> Area {
        Area { x0: 0, y0: 0, x1: self.width as i64, y1: self.height as i64 }
    }

    /// The pixels a device-space box (left, bottom, right, top; y up)
    /// touches, `margin` pixels more all round (past the edges too).
    pub(super) fn area_around(&self, (left, bottom, right, top): (f64, f64, f64, f64), margin: f64) -> Area {
        // Not-a-number goes to 0, endless to far away.
        let at = |v: f64| v.clamp(-1e12, 1e12) as i64;
        Area { x0: at((left - margin).floor()), y0: at((bottom - margin).floor()), x1: at((right + margin).ceil()), y1: at((top + margin).ceil()) }
    }

    /// The pixels a user-space rectangle covers.
    fn rect_area(&self, rect: Rect) -> Area {
        let r = standard(rect);
        let corners = [(r.origin.x, r.origin.y), (r.origin.x + r.size.width, r.origin.y), (r.origin.x, r.origin.y + r.size.height), (r.origin.x + r.size.width, r.origin.y + r.size.height)]
            .map(|(x, y)| geometry::apply(self.state.ctm, x, y));
        let (xs, ys) = (corners.map(|p| p.0), corners.map(|p| p.1));
        let low = |v: [f64; 4]| v.into_iter().fold(f64::INFINITY, f64::min);
        let high = |v: [f64; 4]| v.into_iter().fold(f64::NEG_INFINITY, f64::max);
        self.area_around((low(xs), low(ys), high(xs), high(ys)), 1.0)
    }

    /// The picture in it now (a copy, as Core Graphics gives); the context's
    /// own pixels while a transparency layer is open.
    fn snapshot(&mut self) -> Bitmap {
        let (width, height, bytes_per_row, data) = self.base();
        let data = if data.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(data, bytes_per_row * height) }.to_vec() };
        Bitmap { width, height, bits_per_pixel: self.bits_per_pixel, bytes_per_row, info: self.info, space: self.space, data }
    }

    /// The context's own pixels (width, height, bytes per row, memory),
    /// under any open transparency layers.
    fn base(&self) -> (usize, usize, usize, *mut u8) {
        match self.layers.first() {
            Some(under) => (under.width, under.height, under.bytes_per_row, under.data),
            None => (self.width, self.height, self.bytes_per_row, self.data),
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

/// The context's own pixels (not an open transparency layer's).
extern "C" fn bitmap_context_get_data(context: Id) -> *mut u8 {
    locked(context).map_or(std::ptr::null_mut(), |c| c.base().3)
}
extern "C" fn bitmap_context_get_width(context: Id) -> usize {
    locked(context).map_or(0, |c| c.base().0)
}
extern "C" fn bitmap_context_get_height(context: Id) -> usize {
    locked(context).map_or(0, |c| c.base().1)
}
extern "C" fn bitmap_context_get_bytes_per_row(context: Id) -> usize {
    locked(context).map_or(0, |c| c.base().2)
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
        c.shadowed(|c| c.rect_area(rect), |c, _| c.fill_rect(rect));
    }
}
extern "C" fn context_clear_rect(context: Id, rect: Rect) {
    if let Some(mut c) = canvas("CGContextClearRect", context) {
        c.clear_rect(rect);
    }
}
extern "C" fn context_draw_image(context: Id, rect: Rect, picture: Id) {
    // The picture first: it may be this same context's picture or need the runtime.
    let Some(bitmap) = drawable(picture) else {
        complain("CGContextDrawImage", "image", picture);
        return;
    };
    let mask = is_mask(picture);
    if let Some(mut c) = canvas("CGContextDrawImage", context) {
        if !mask {
            c.shadowed(|c| c.rect_area(rect), |c, _| c.draw_image(rect, &bitmap));
        } else if let Some(painted) = mask_in_color(&bitmap, c.state.fill) {
            // An image mask paints the fill colour where it lets through.
            c.shadowed(|c| c.rect_area(rect), |c, _| c.draw_image(rect, &painted));
        }
    }
}
extern "C" fn context_draw_linear_gradient(context: Id, gradient: Id, start: Point, end: Point, options: u32) {
    let Some(g) = state::<Gradient>(gradient, classes().gradient) else {
        complain("CGContextDrawLinearGradient", "gradient", gradient);
        return;
    };
    if let Some(mut c) = canvas("CGContextDrawLinearGradient", context) {
        c.shadowed(Canvas::whole, |c, _| c.draw_linear_gradient(g, start, end, options));
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

/// UIRectFill: the current UIKit context's fill colour replaces what is
/// there (UIKit fills with kCGBlendModeCopy).
extern "C" fn ui_rect_fill(rect: Rect) {
    let context = current_context();
    if context == 0 {
        crate::log!("UIRectFill: no current context");
        return;
    }
    if let Some(mut c) = canvas("UIRectFill", context) {
        c.copy_rect(rect);
    }
}

/// UIGraphicsPushContext: the stack holds the context while it is on it.
extern "C" fn graphics_push_context(context: Id) {
    counted::retain(context);
    CONTEXTS.with(|stack| stack.borrow_mut().push(ImageContext { context, scale: 1.0, own: false }));
}

/// UIGraphicsPopContext: whatever is on top goes (an image context too)
/// and the stack lets go of it.
extern "C" fn graphics_pop_context() {
    let popped = CONTEXTS.with(|stack| stack.borrow_mut().pop());
    if let Some(popped) = popped {
        counted::release(popped.context);
    }
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
    // The file's bytes, if it was never decoded.
    drop(info.bytes.lock().unwrap_or_else(|e| e.into_inner()).take());
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

/// A dot picture's width and height in pixels.
pub const DOT_PIXELS: usize = 28;

/// The pixels of a round dot of a colour filling DOT_PIXELS square, its edge smoothed.
fn dot_pixels(rgba: [f64; 4]) -> Vec<u8> {
    let n = DOT_PIXELS;
    let radius = n as f64 / 2.0;
    let mut pixels = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let distance = (x as f64 + 0.5 - radius).hypot(y as f64 + 0.5 - radius);
            let cover = (radius - distance + 0.5).clamp(0.0, 1.0);
            let [r, g, b, a] = rgba.map(|c| c.clamp(0.0, 1.0));
            pixels.extend([r, g, b, a * cover].map(|c| (c * 255.0).round() as u8));
        }
    }
    pixels
}

/// A PNG of a round dot of a colour (page control dots, fw/pickers.rs): made once per colour.
pub fn dot_picture(rgba: [f64; 4]) -> Option<String> {
    static MADE: Mutex<Option<std::collections::HashMap<[u8; 4], Option<String>>>> = Mutex::new(None);
    let key = rgba.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    made.get_or_insert_with(Default::default)
        .entry(key)
        .or_insert_with(|| encode_png(DOT_PIXELS, DOT_PIXELS, &dot_pixels(rgba), true).and_then(|bytes| write_png_file(&format!("dot {key:?}"), &bytes)))
        .clone()
}

/// A pressed button's glow (showsTouchWhenHighlighted, fw/controls.rs):
/// our own drawing of a soft white spot, GLOW_POINTS across at 2x, solid in
/// the middle and fading to nothing at its edge. Made once, kept.
pub fn glow_image() -> Id {
    const GLOW_POINTS: usize = 20;
    static MADE: OnceLock<usize> = OnceLock::new();
    *MADE.get_or_init(|| {
        let n = GLOW_POINTS * 2;
        let radius = n as f64 / 2.0;
        let mut data = Vec::with_capacity(n * n * 4);
        for y in 0..n {
            for x in 0..n {
                let distance = (x as f64 + 0.5 - radius).hypot(y as f64 + 0.5 - radius) / radius;
                // Solid to a third of the way out, then a smooth fall to the edge.
                let t = ((distance - 0.33) / 0.67).clamp(0.0, 1.0);
                let alpha = ((1.0 - t * t * (3.0 - 2.0 * t)) * 255.0).round() as u8;
                // Premultiplied white.
                data.extend([alpha, alpha, alpha, alpha]);
            }
        }
        let bitmap = Bitmap { width: n, height: n, bits_per_pixel: 32, bytes_per_row: n * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data };
        let info = uikit::ImageInfo { pixels: (n as u32, n as u32), scale: 2.0, cg: OnceLock::from(make_image(bitmap)), ..Default::default() };
        uikit::make_image(info)
    })
}

/// How an iOS 6 style glossy box looks (alerts, action sheets and their
/// buttons, fw/controls.rs): a top-to-bottom gradient, a white shine over
/// the upper part, rounded corners and an edge line. Colours straight RGBA.
#[derive(Clone, Copy)]
pub struct Gloss {
    pub top: [f64; 4],
    pub bottom: [f64; 4],
    /// The white shine's strength at the very top (0: none).
    pub shine: f64,
    /// How far down the shine reaches, as a part of the height.
    pub shine_depth: f64,
    pub edge: [f64; 4],
    pub edge_width: f64,
    pub radius: f64,
}

/// A UIImage of `width` x `height` points at 2x of a glossy box: our own
/// drawing of iOS 6's look, with smoothed corners.
pub fn glossy_image(width: f64, height: f64, look: &Gloss) -> Id {
    let (w, h) = ((width * 2.0).round().max(1.0) as usize, (height * 2.0).round().max(1.0) as usize);
    let (radius, edge) = (look.radius * 2.0, look.edge_width * 2.0);
    let mix = |a: [f64; 4], b: [f64; 4], t: f64| -> [f64; 4] { std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t) };
    // Over: `top` (straight) laid on `under` (straight).
    let over = |under: [f64; 4], top: [f64; 4]| -> [f64; 4] {
        let alpha = top[3] + under[3] * (1.0 - top[3]);
        if alpha <= 0.0 {
            return [0.0; 4];
        }
        let channel = |i: usize| (top[i] * top[3] + under[i] * under[3] * (1.0 - top[3])) / alpha;
        [channel(0), channel(1), channel(2), alpha]
    };
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            // Distance inside the rounded rectangle's edge (negative outside).
            let (cx, cy) = (px.clamp(radius, w as f64 - radius), py.clamp(radius, h as f64 - radius));
            let inside = if (px - cx).abs() > 0.0 || (py - cy).abs() > 0.0 {
                radius - (px - cx).hypot(py - cy)
            } else {
                px.min(py).min(w as f64 - px).min(h as f64 - py).max(radius)
            };
            let cover = (inside + 0.5).clamp(0.0, 1.0);
            let t = py / h as f64;
            let mut colour = mix(look.top, look.bottom, t);
            if look.shine > 0.0 && t < look.shine_depth {
                // Strongest at the top, a clear lower edge where it stops.
                let shine = look.shine * (0.55 + 0.45 * (1.0 - t / look.shine_depth));
                colour = over(colour, [1.0, 1.0, 1.0, shine]);
            }
            if inside < edge {
                colour = over(colour, look.edge);
            }
            let alpha = colour[3] * cover;
            data.extend([colour[0] * alpha, colour[1] * alpha, colour[2] * alpha, alpha].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    let bitmap = Bitmap { width: w, height: h, bits_per_pixel: 32, bytes_per_row: w * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data };
    let info = uikit::ImageInfo { pixels: (w as u32, h as u32), scale: 2.0, cg: OnceLock::from(make_image(bitmap)), ..Default::default() };
    uikit::make_image(info)
}

/// Darker copies made of UIImages (pressed buttons), by the image they copy.
static DARKER: Mutex<Option<std::collections::HashMap<Id, Id>>> = Mutex::new(None);

/// A view's drawing (-drawRect:, UIKit's display): a see-through UIKit
/// context of `size` points at `scale` pixels per point, its origin moved to
/// the bounds' `origin`, made current while `draw` runs. The CGImage it
/// made, one reference (the caller's); 0 for an empty size.
pub fn draw_picture(size: Size, scale: f64, origin: Point, draw: impl FnOnce()) -> Id {
    graphics_begin_image_context_with_options(size, false, scale);
    let context = CONTEXTS.with(|stack| stack.borrow().last().filter(|c| c.own).map(|c| c.context));
    let Some(context) = context else { return 0 };
    context_translate_ctm(context, -origin.x, -origin.y);
    if let Some(mut c) = canvas("-[UIView drawRect:]", context) {
        // UIKit's -drawLayer:inContext: draws in DeviceRGB.
        (c.state.fill_space, c.state.stroke_space) = (Space::Rgb, Space::Rgb);
    }
    draw();
    // drawRect: may leave contexts it pushed (or pop this one): they go
    // down to this one, which is ended if still there.
    let picture = bitmap_context_create_image(context);
    let left = CONTEXTS.with(|stack| {
        let mut stack = stack.borrow_mut();
        match stack.iter().rposition(|c| c.context == context) {
            Some(at) => stack.split_off(at),
            None => Vec::new(),
        }
    });
    for ended in left {
        counted::release(ended.context);
    }
    picture
}

/// `look` drawn over `rect` of `context` at the context's own pixels per
/// point, so text is sharp.
fn draw_text_look(function: &str, context: Id, look: &TextLook, rect: Rect) {
    let scale = canvas(function, context).map_or(1.0, |c| (c.state.ctm.a * c.state.ctm.d - c.state.ctm.b * c.state.ctm.c).abs().sqrt()).clamp(1.0, 4.0);
    if let Some(bitmap) = text::look_bitmap(look, scale) {
        if let Some(mut c) = canvas(function, context) {
            c.draw_bitmap(rect, &bitmap, &|u, v| (u, 1.0 - v));
        }
    }
}

/// `look` drawn over `rect` of the current UIKit context (UILabel's
/// drawTextInRect:); nothing with no context.
pub fn draw_text_here(look: &TextLook, rect: Rect) {
    let context = current_context();
    if context != 0 {
        draw_text_look("-[UILabel drawTextInRect:]", context, look, rect);
    }
}

/// For tests: opens a UIKit image context of `width` x `height` points at
/// 1x and returns it.
#[cfg(test)]
pub fn test_begin_context(width: f64, height: f64) -> Id {
    graphics_begin_image_context_with_options(Size { width, height }, false, 1.0);
    current_context()
}

/// For tests: the current image context's pixels (straight RGBA rows from
/// the top), closing it.
#[cfg(test)]
pub fn test_end_context() -> Vec<u8> {
    let picture = bitmap_context_create_image(current_context());
    let pixels = straight_pixels(picture).unwrap_or_default();
    counted::release(picture);
    graphics_end_image_context();
    pixels
}

/// For tests: a 1x UIImage of `width` x `height` points all `rgba` (kept).
#[cfg(test)]
pub fn test_solid_image(width: f64, height: f64, rgba: [f64; 4]) -> Id {
    test_begin_context(width, height);
    test_fill(Rect::new(0.0, 0.0, width, height), rgba);
    let image = counted::retain(graphics_get_image_from_current_image_context());
    graphics_end_image_context();
    image
}

/// For tests: fills `rect` of the current UIKit context with `rgba` (UIRectFill).
#[cfg(test)]
pub fn test_fill(rect: Rect, rgba: [f64; 4]) {
    set_fill_in(current_context(), rgba);
    ui_rect_fill(rect);
}

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
    *info.cg.get_or_init(|| {
        // The bytes kept from making the picture are used once, then dropped
        // so the decoded pixels aren't held alongside them.
        let bytes = info.bytes.lock().unwrap_or_else(|e| e.into_inner()).take();
        let bytes = bytes.or_else(|| std::fs::read(&info.path).ok().map(std::sync::Arc::new));
        match bytes.and_then(|bytes| decode(bytes.as_slice())) {
            Some((bitmap, _)) => make_image(bitmap),
            None => {
                crate::log!("UIImage {}: can't read its pixels", info.path);
                0
            }
        }
    })
}

/// `make`, and whether a picture file it asked for was still being written
/// on its own thread (see `picture_file`).
pub fn noting_unready<T>(make: impl FnOnce() -> T) -> (T, bool) {
    let before = UNREADY.with(|u| u.replace(false));
    let made = make();
    // (An outer `noting_unready` still sees it.)
    let unready = UNREADY.with(|u| u.replace(before || u.get()));
    (made, unready)
}

thread_local! {
    /// A picture file asked for on this thread was still being written.
    static UNREADY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
static HELD_WRITERS: (Mutex<Vec<Id>>, std::sync::Condvar) = (Mutex::new(Vec::new()), std::sync::Condvar::new());

/// For tests: holds (or lets go) the thread writing `picture`'s files.
#[cfg(test)]
pub fn test_hold_writing(picture: Id, held: bool) {
    let mut list = HELD_WRITERS.0.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|&p| p != picture);
    if held {
        list.push(picture);
    }
    HELD_WRITERS.1.notify_all();
}

/// A writer thread waits here while a test holds its picture.
fn writer_may_start(picture: Id) {
    #[cfg(test)]
    {
        let mut list = HELD_WRITERS.0.lock().unwrap_or_else(|e| e.into_inner());
        while list.contains(&picture) {
            list = HELD_WRITERS.1.wait(list).unwrap_or_else(|e| e.into_inner());
        }
    }
    let _ = picture;
}

/// A file the phone app can show for this UIImage as UIKit would draw it: the
/// picture's own file when it is upright, else an upright PNG made once
/// (pictures made in memory, turned photos). None if it can't be made.
///
/// The PNG is made on its own thread (not the game's main thread); until it
/// is ready this gives None (noted for `noting_unready`, so the list can
/// wait for it) and the picture shows from the next update.
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
            writer_may_start(picture);
            let made = write_picture_file(picture);
            if let Some(info) = uikit::image_info(picture) {
                let ready = made.is_some();
                if let Err(Some(file)) = info.picture_file.set(made) {
                    let _ = std::fs::remove_file(file);
                } else if ready {
                    crate::fw::changes::touch();
                }
            }
            counted::release(picture);
        });
    }
    UNREADY.with(|u| u.set(true));
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
            writer_may_start(picture);
            let made = write_part_files(picture);
            if let Some(info) = uikit::image_info(picture) {
                let ready = made.iter().any(Option::is_some);
                if let Err(files) = info.part_files.set(made) {
                    for file in files.into_iter().flatten() {
                        let _ = std::fs::remove_file(file);
                    }
                } else if ready {
                    crate::fw::changes::touch();
                }
            }
            counted::release(picture);
        });
    }
    UNREADY.with(|u| u.set(true));
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

/// -[UIImage drawInRect:blendMode:alpha:]: upright in UIKit's coordinates
/// (top at the rectangle's min y), turned by the image's orientation, a
/// stretchable one in nine parts; with this blend mode and alpha (not on
/// top of the context's), casting the context's shadow.
fn ui_image_draw(picture: Id, rect: Rect, blend_mode: i32, alpha: f64) {
    // The picture first: it may need the runtime.
    let Some(shown) = shown_image(picture) else { return };
    if let Some(mut c) = canvas("-[UIImage drawInRect:]", current_context()) {
        let saved = (c.state.alpha, c.state.blend_mode);
        (c.state.alpha, c.state.blend_mode) = (alpha.clamp(0.0, 1.0), blend_mode);
        c.shadowed(|c| c.rect_area(rect), |c, _| draw_shown(c, &shown, rect));
        (c.state.alpha, c.state.blend_mode) = saved;
    }
}

/// What drawing a UIImage takes: its pixels, orientation, cap insets
/// (points: top, left, bottom, right), scale and whether its middle tiles.
pub(super) struct ShownImage {
    bitmap: Arc<Bitmap>,
    orientation: i64,
    caps: Option<[f64; 4]>,
    scale: f64,
    tile: bool,
}

pub(super) fn shown_image(picture: Id) -> Option<ShownImage> {
    let bitmap = drawable(image_cg(picture))?;
    let info = uikit::image_info(picture)?;
    let caps = info.caps.filter(|caps| caps.iter().any(|&cap| cap > 0.0));
    Some(ShownImage { bitmap, orientation: info.orientation, caps, scale: if info.scale > 0.0 { info.scale } else { 1.0 }, tile: info.tile })
}

/// A UIImage drawn over `rect` of a canvas as UIKit draws it: turned by its
/// orientation; with cap insets, cut at them into nine parts whose corners
/// keep their size while the edges and middle stretch (or tile, for
/// UIImageResizingModeTile) between them (-[UIImage drawInRect:]'s
/// _Draw3PartSlice), each part drawn from its own pixels only.
pub(super) fn draw_shown(c: &mut Canvas, shown: &ShownImage, rect: Rect) {
    let (bitmap, orientation) = (&shown.bitmap, shown.orientation);
    let Some(caps) = shown.caps else {
        c.draw_bitmap(rect, bitmap, &|u, v| orient(orientation, u, 1.0 - v));
        return;
    };
    let (scale, tile) = (shown.scale, shown.tile);
    let turned = matches!(orientation, 2 | 3 | 6 | 7);
    let (width, height) = if turned { (bitmap.height, bitmap.width) } else { (bitmap.width, bitmap.height) };
    let [top, left, bottom, right] = caps.map(|cap| (cap * scale).round().max(0.0) as usize);
    let rect = standard(rect);
    // Where the three parts of a side go: the caps keep their size (shrunk
    // alike when they don't fit), the middle takes the rest.
    let places = |parts: [(usize, usize); 3], start: f64, length: f64| {
        let (first, last) = (parts[0].1 as f64 / scale, parts[2].1 as f64 / scale);
        let k = if first + last > length && first + last > 0.0 { length / (first + last) } else { 1.0 };
        let (first, last) = (first * k, last * k);
        [(start, first), (start + first, length - first - last), (start + length - last, last)]
    };
    let (columns, rows) = (cuts(width, left, right), cuts(height, top, bottom));
    let (across, down) = (places(columns, rect.origin.x, rect.size.width), places(rows, rect.origin.y, rect.size.height));
    for (j, (&(y, h), &(dy, dh))) in rows.iter().zip(&down).enumerate() {
        for (i, (&(x, w), &(dx, dw))) in columns.iter().zip(&across).enumerate() {
            if w == 0 || h == 0 || dw <= 0.0 || dh <= 0.0 {
                continue;
            }
            let Some(part) = crop_upright(bitmap, orientation, (width, height), (x, y, w, h)) else { continue };
            let map = |u: f64, v: f64| orient(orientation, u, 1.0 - v);
            let target = Rect::new(dx, dy, dw, dh);
            if tile && (i == 1 || j == 1) {
                // Tiled at the part's own size from its top left, cut to its place.
                let (tw, th) = (if i == 1 { w as f64 / scale } else { dw }, if j == 1 { h as f64 / scale } else { dh });
                let saved = c.state.clip.clone();
                let mut outline = shapes::Outline::default();
                outline.rect(target, &c.to_device());
                c.clip_to(&outline, false);
                let mut ty = dy;
                while ty < dy + dh - 1e-9 {
                    let mut tx = dx;
                    while tx < dx + dw - 1e-9 {
                        c.draw_bitmap(Rect::new(tx, ty, tw, th), &part, &map);
                        tx += tw;
                    }
                    ty += th;
                }
                c.state.clip = saved;
            } else {
                c.draw_bitmap(target, &part, &map);
            }
        }
    }
}

/// The pixels of a picture under part (x, y, w, h) of it as shown upright
/// (`upright`: its shown width and height), in its stored orientation.
fn crop_upright(bitmap: &Bitmap, orientation: i64, upright: (usize, usize), (x, y, w, h): (usize, usize, usize, usize)) -> Option<Bitmap> {
    let source = bitmap.layout()?;
    let corner = |u: usize, v: usize| orient(orientation, u as f64 / upright.0 as f64, v as f64 / upright.1 as f64);
    let ((s0, t0), (s1, t1)) = (corner(x, y), corner(x + w, y + h));
    let column = |s: f64| (s * bitmap.width as f64).round() as usize;
    let row = |t: f64| (t * bitmap.height as f64).round() as usize;
    let (x0, x1) = (column(s0.min(s1)), column(s0.max(s1)).min(bitmap.width));
    let (y0, y1) = (row(t0.min(t1)), row(t0.max(t1)).min(bitmap.height));
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let mut data = Vec::with_capacity((x1 - x0) * (y1 - y0) * source.bytes);
    for r in y0..y1 {
        let at = r * bitmap.bytes_per_row;
        data.extend_from_slice(&bitmap.data[at + x0 * source.bytes..at + x1 * source.bytes]);
    }
    let bytes_per_row = (x1 - x0) * source.bytes;
    Some(Bitmap { width: x1 - x0, height: y1 - y0, bits_per_pixel: bitmap.bits_per_pixel, bytes_per_row, info: bitmap.info, space: bitmap.space, data })
}

extern "C" fn ui_image_draw_in_rect(this: Id, _: Sel, rect: Rect) {
    ui_image_draw(this, rect, 0, 1.0);
}

extern "C" fn ui_image_draw_in_rect_blend(this: Id, _: Sel, rect: Rect, blend_mode: i32, alpha: f64) {
    ui_image_draw(this, rect, blend_mode, alpha);
}

extern "C" fn ui_image_draw_at_point(this: Id, _: Sel, point: Point) {
    ui_image_draw(this, Rect { origin: point, size: uikit::image_points(this) }, 0, 1.0);
}

extern "C" fn ui_image_draw_at_point_blend(this: Id, _: Sel, point: Point, blend_mode: i32, alpha: f64) {
    ui_image_draw(this, Rect { origin: point, size: uikit::image_points(this) }, blend_mode, alpha);
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
        Some(bytes) => counted::autorelease(data::make(&bytes)),
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
        Ok(()) => counted::autorelease(data::make(&out)),
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
            ("drawInRect:blendMode:alpha:", ui_image_draw_in_rect_blend as usize),
            ("drawAtPoint:", ui_image_draw_at_point as usize),
            ("drawAtPoint:blendMode:alpha:", ui_image_draw_at_point_blend as usize),
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
    r.function("CGImageGetDataProvider", image_get_data_provider as usize);
    r.function("CGImageIsMask", image_is_mask as usize);
    r.function("CGImageGetDecode", image_get_decode as usize);
    r.function("CGImageCreateWithMask", image_create_with_mask as usize);
    r.function("CGImageRetain", cf_style_retain as usize);
    r.function("CGImageRelease", cf_style_release as usize);
    r.function("CGContextRetain", cf_style_retain as usize);
    r.function("CGContextRelease", cf_style_release as usize);
    // Colour spaces are shared; colours and gradients are small and kept.
    r.function("CGColorSpaceCreateDeviceRGB", space_create_rgb as usize);
    r.function("CGColorSpaceCreateDeviceGray", space_create_gray as usize);
    r.function("CGColorSpaceCreateIndexed", space_create_indexed as usize);
    r.function("CGColorSpaceGetNumberOfComponents", space_get_number_of_components as usize);
    r.function("CGColorSpaceRelease", nothing as usize);
    r.function("CGColorCreate", color_create as usize);
    r.function("CGColorRelease", nothing as usize);
    r.function("CGGradientCreateWithColorComponents", gradient_create_with_color_components as usize);
    r.function("CGGradientCreateWithColors", gradient_create_with_colors as usize);
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
    r.function("UIRectFill", ui_rect_fill as usize);
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

    #[test]
    fn four_byte_pixels_read_and_write_like_the_slot_by_slot_way() {
        // SpellCraft froze 80-120 ms per cast drawing a card pixel by pixel;
        // RGB pixels are read and written at fixed byte places, which must
        // give the same bytes and colours as going slot by slot.
        let infos = [PREMULTIPLIED_LAST, PREMULTIPLIED_FIRST, ALPHA_LAST, ALPHA_FIRST, NONE_SKIP_LAST, NONE_SKIP_FIRST];
        let orders = [0, ORDER_32_BIG, ORDER_32_LITTLE];
        let values = [0u8, 1, 37, 128, 200, 254, 255];
        for info in infos.into_iter().flat_map(|alpha| orders.map(|order| alpha | order)) {
            let layout = layout(Space::Rgb, 8, 32, info).unwrap();
            assert!(layout.fast.is_some(), "{info:#x} has fixed byte places");
            for (i, &a) in values.iter().enumerate() {
                for &b in &values {
                    let pixel = [b, a, values[(i + 3) % values.len()], b ^ a];
                    assert_eq!(layout.unpack(&pixel), layout.unpack_slots(&pixel), "{info:#x} {pixel:?}");
                    let color = layout.read(&pixel);
                    let (mut fast, mut slots) = ([9u8; 4], [9u8; 4]);
                    layout.write(&mut fast, color);
                    layout.write_slots(&mut slots, color);
                    assert_eq!(fast, slots, "{info:#x} {color:?}");
                }
            }
        }
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

    /// The whole-pixel byte fast path (clear pixels skipped, opaque pixels
    /// copied) gives the same bytes as the float blend for every source and
    /// destination layout with fixed byte places.
    #[test]
    fn whole_pixel_byte_path_matches_the_float_path() {
        let source_infos = [PREMULTIPLIED_LAST, ALPHA_LAST, NONE_SKIP_LAST, PREMULTIPLIED_FIRST | ORDER_32_LITTLE];
        let dst_infos = [PREMULTIPLIED_LAST, PREMULTIPLIED_FIRST | ORDER_32_LITTLE, NONE_SKIP_LAST, NONE_SKIP_FIRST | ORDER_32_LITTLE];
        for src_info in source_infos {
            for &dst_info in &dst_infos {
                let source = layout(Space::Rgb, 8, 32, src_info).unwrap();
                // One 5x1 picture with clear, opaque and in-between pixels.
                let mut data = vec![0u8; 5 * 4];
                for (i, &a) in [0u8, 255, 128, 37, 255].iter().enumerate() {
                    let (r, g, b) = ((i * 53 % 256) as u8, (i * 31 % 256) as u8, (i * 7 % 256) as u8);
                    let af = a as f32 / 255.0;
                    let color = [r as f32 / 255.0 * af, g as f32 / 255.0 * af, b as f32 / 255.0 * af, af];
                    source.write(&mut data[i * 4..i * 4 + 4], color);
                }
                let picture = Bitmap { width: 5, height: 1, bits_per_pixel: 32, bytes_per_row: 20, info: src_info, space: Space::Rgb, data };
                let background = |c: &mut Canvas| {
                    c.state.fill = [0.2, 0.6, 0.9, 0.7];
                    c.fill_rect(Rect::new(0.0, 0.0, 5.0, 1.0));
                };
                // Over a painted background, and over a fresh all-zero one.
                for painted in [true, false] {
                    let (mut fast, mut float) = (canvas(5, 1, dst_info), canvas(5, 1, dst_info));
                    if painted {
                        background(&mut fast);
                        background(&mut float);
                    }
                    fast.draw_whole_pixels(Rect::new(0.0, 0.0, 5.0, 1.0), &picture);
                    float.draw_whole_pixels_loop(0, 5, 0, 1, &picture, &source, false);
                    assert_eq!(fast.memory().to_vec(), float.memory().to_vec(), "source {src_info:#x} destination {dst_info:#x} painted {painted}");
                }
            }
        }
    }

    #[test]
    fn fast_bitmap_path_matches_the_general_path() {
        // A scaled, alpha-faded, clipped draw of a picture: the integer fast
        // path and the float general path give the same pixels (at most one
        // step per channel, 1/255), whatever the source and destination
        // layouts.
        let source_infos = [PREMULTIPLIED_LAST, ALPHA_LAST, NONE_SKIP_LAST];
        let dst_infos = [PREMULTIPLIED_LAST, PREMULTIPLIED_FIRST | ORDER_32_LITTLE, NONE_SKIP_LAST];
        for &src_info in &source_infos {
            let src_layout = layout(Space::Rgb, 8, 32, src_info).unwrap();
            // A 5x3 picture with varied straight colours and alphas, stored
            // the way the source layout stores them.
            let mut data = vec![0u8; 5 * 3 * 4];
            for i in 0..15u32 {
                let a = [255u8, 0, 128, 37, 200][i as usize % 5] as f32 / 255.0;
                let color = [
                    ((i * 53) % 256) as f32 / 255.0 * a,
                    ((i * 31) % 256) as f32 / 255.0 * a,
                    ((i * 17) % 256) as f32 / 255.0 * a,
                    a,
                ];
                src_layout.write(&mut data[i as usize * 4..i as usize * 4 + 4], color);
            }
            let picture = Bitmap { width: 5, height: 3, bits_per_pixel: 32, bytes_per_row: 20, info: src_info, space: Space::Rgb, data };
            let source = picture.layout().unwrap();
            for flip in [false, true] {
                for &dst_info in &dst_infos {
                    let (mut fast, mut general) = (canvas(9, 7, dst_info), canvas(9, 7, dst_info));
                    let setup = |c: &mut Canvas| {
                        if flip {
                            c.state.ctm = Transform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 0.0, ty: 7.0 };
                        }
                        c.state.alpha = 0.7;
                        let mut outline = shapes::Outline::default();
                        outline.rect(Rect::new(1.0, 1.0, 6.0, 4.0), &c.to_device());
                        c.clip_to(&outline, false);
                    };
                    setup(&mut fast);
                    setup(&mut general);
                    fast.draw_bitmap(Rect::new(0.0, 0.0, 9.0, 7.0), &picture, &|u, v| (u, 1.0 - v));
                    general.draw_bitmap_paint(Rect::new(0.0, 0.0, 9.0, 7.0), &picture, &source, &|u, v| (u, 1.0 - v));
                    let (f, g) = (fast.memory().to_vec(), general.memory().to_vec());
                    assert!(f.iter().zip(&g).all(|(a, b)| a.abs_diff(*b) <= 1), "fast vs general src={src_info:#x} dst={dst_info:#x} flip={flip}:\n{f:?}\n{g:?}");
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn timing_scaled_bitmap_with_rectangle_clip() {
        // A 512x512 RGBA picture scaled into a 710x300 canvas under a
        // rectangle clip, 14 times: the SpellCraft picture-draw shape.
        let (w, h) = (512usize, 512usize);
        let data: Vec<u8> = (0..(w * h) as u32).flat_map(|i| {
            let (x, y) = ((i as usize % w) as u8, (i as usize / w) as u8);
            [x, y, x ^ y, 255]
        }).collect();
        let bitmap = Bitmap { width: w, height: h, bits_per_pixel: 32, bytes_per_row: w * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data };
        let source = bitmap.layout().unwrap();
        let map = |u: f64, v: f64| (u, 1.0 - v);

        // The whole scenario (fast clip + fast draw).
        let start = std::time::Instant::now();
        for _ in 0..14 {
            let mut c = canvas(710, 300, PREMULTIPLIED_LAST);
            let mut outline = shapes::Outline::default();
            outline.rect(Rect::new(10.0, 10.0, 690.0, 280.0), &|p| p);
            c.clip_to(&outline, false);
            c.draw_bitmap(Rect::new(0.0, 0.0, 710.0, 300.0), &bitmap, &map);
        }
        eprintln!("TIMING whole scenario (fast clip + fast draw) 14x: {:?}", start.elapsed());

        // Clip alone.
        let start = std::time::Instant::now();
        for _ in 0..14 {
            let mut c = canvas(710, 300, PREMULTIPLIED_LAST);
            let mut outline = shapes::Outline::default();
            outline.rect(Rect::new(10.0, 10.0, 690.0, 280.0), &|p| p);
            c.clip_to(&outline, false);
        }
        eprintln!("TIMING clip_to alone 14x: {:?}", start.elapsed());

        // Fast draw alone.
        let start = std::time::Instant::now();
        for _ in 0..14 {
            let mut c = canvas(710, 300, PREMULTIPLIED_LAST);
            c.draw_bitmap(Rect::new(0.0, 0.0, 710.0, 300.0), &bitmap, &map);
        }
        eprintln!("TIMING draw_bitmap (fast) alone 14x: {:?}", start.elapsed());

        // General draw alone.
        let start = std::time::Instant::now();
        for _ in 0..14 {
            let mut c = canvas(710, 300, PREMULTIPLIED_LAST);
            c.draw_bitmap_paint(Rect::new(0.0, 0.0, 710.0, 300.0), &bitmap, &source, &map);
        }
        eprintln!("TIMING draw_bitmap_paint (general) alone 14x: {:?}", start.elapsed());
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
        assert_eq!(unfilter_rows(&raw, 4, 2, 4).unwrap(), [10, 20, 30, 40, 11, 21, 31, 41]);
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

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    /// A PNG file of these chunks (signature first, each with its CRC).
    pub(super) fn png_of(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let crc = |bytes: &[u8]| {
            let mut c = !0u32;
            for &b in bytes {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in chunks {
            png.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = png.len();
            png.extend_from_slice(*kind);
            png.extend_from_slice(body);
            let sum = crc(&png[start..]);
            png.extend_from_slice(&sum.to_be_bytes());
        }
        png
    }

    pub(super) fn header(width: u32, height: u32, depth: u8, kind: u8) -> Vec<u8> {
        [&width.to_be_bytes()[..], &height.to_be_bytes(), &[depth, kind, 0, 0, 0]].concat()
    }

    fn pixel_at(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        pixels[(y * width + x) * 4..(y * width + x) * 4 + 4].try_into().unwrap()
    }

    /// Crushed PNGs of other colour types read too (ImageIO reads them):
    /// a crushed RGB file keeps its pixels blue, green, red.
    #[test]
    fn crushed_rgb_pngs_are_read() {
        let idat = miniz_oxide::deflate::compress_to_vec(&[0, 3, 2, 1, 6, 5, 4], 6);
        let png = png_of(&[(b"CgBI", vec![0x50, 0, 0x20, 6]), (b"IHDR", header(2, 1, 8, 2)), (b"IDAT", idat), (b"IEND", Vec::new())]);
        let (bitmap, _) = decode(&png).expect("read");
        assert_eq!(bitmap.straight_rgba().unwrap(), [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    /// A stretchable image is drawn in nine parts cut at its caps: the
    /// corners keep their size and the middle stretches. -drawInRect: draws
    /// at full alpha whatever the context's (drawInRect:blendMode:alpha:
    /// sets its own).
    #[test]
    fn stretchable_images_draw_in_nine_parts_at_their_own_alpha() {
        setup();
        let (red, blue) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]);
        test_begin_context(3.0, 3.0);
        test_fill(Rect::new(0.0, 0.0, 3.0, 3.0), blue);
        for (x, y) in [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (2.0, 2.0)] {
            test_fill(Rect::new(x, y, 1.0, 1.0), red);
        }
        let corners = counted::retain(graphics_get_image_from_current_image_context());
        graphics_end_image_context();
        let stretchy = runtime::send(corners, "stretchableImageWithLeftCapWidth:topCapHeight:", &[1, 1]);
        let context = test_begin_context(9.0, 10.0);
        context_set_alpha(context, 0.5);
        uikit::send_rect(stretchy, "drawInRect:", Rect::new(0.0, 0.0, 9.0, 9.0));
        let s = runtime::sel("drawInRect:blendMode:alpha:");
        let imp = runtime::imp_for(corners, s).expect("drawInRect:blendMode:alpha:");
        let draw: extern "C" fn(Id, Sel, Rect, i32, f64) = unsafe { std::mem::transmute(imp) };
        draw(corners, s, Rect::new(0.0, 9.0, 3.0, 1.0), 0, 0.5);
        let pixels = test_end_context();
        assert_eq!(pixel_at(&pixels, 9, 0, 0), [255, 0, 0, 255], "a corner, at full alpha");
        assert_eq!(pixel_at(&pixels, 9, 8, 8), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&pixels, 9, 1, 1), [0, 0, 255, 255], "the middle stretched");
        assert_eq!(pixel_at(&pixels, 9, 4, 0), [0, 0, 255, 255], "the top edge stretched");
        assert!(pixel_at(&pixels, 9, 1, 9)[3].abs_diff(128) <= 1, "its own alpha: {:?}", pixel_at(&pixels, 9, 1, 9));
    }

    /// A shadowed drawing's layer has pixels only for the drawing (its
    /// shadow spreads round them when put down); past the pixel budget a
    /// layer is drawn straight on, its alpha applied piece by piece.
    #[test]
    fn layers_cover_only_their_drawing_and_stop_at_a_budget() {
        let mut c = Canvas::new(std::ptr::null_mut(), 2000, 2000, 8, 0, Space::Rgb, PREMULTIPLIED_LAST).unwrap();
        let at = |c: &mut Canvas, x: usize, y: usize| -> [u8; 4] { c.memory()[((1999 - y) * 2000 + x) * 4..][..4].try_into().unwrap() };
        c.state.shadow = Some(Shadow { offset: (0.0, 0.0), blur: 2.0, color: [0.0, 0.0, 0.0, 1.0] });
        let rect = Rect::new(10.0, 20.0, 4.0, 3.0);
        let mut seen = None;
        c.shadowed(|c| c.rect_area(rect), |c, _| {
            seen = c.layers.last().map(|under| (under.area, c.width, c.height));
            c.fill_rect(rect);
        });
        // x 10..14, y 20..23, a pixel more all round.
        assert_eq!(seen, Some((Area { x0: 9, y0: 19, x1: 15, y1: 24 }, 6, 5)));
        assert_eq!(at(&mut c, 11, 21)[3], 255);
        assert!(at(&mut c, 11, 24)[3] > 0, "the shadow reaches past the drawing");
        c.state.shadow = None;
        c.state.fill = [1.0, 0.0, 0.0, 1.0];
        for _ in 0..4 {
            c.begin_layer(None, false);
        }
        assert!(c.layers.iter().all(|under| !under.direct));
        c.state.alpha = 0.5;
        c.begin_layer(None, false);
        assert!(c.layers.last().unwrap().direct, "past the budget");
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        for _ in 0..5 {
            c.end_layer();
        }
        let red = at(&mut c, 0, 0);
        assert!(red[0].abs_diff(128) <= 1 && red[1] == 0 && red[3].abs_diff(128) <= 1, "{red:?}");
    }

    /// A shape just past the canvas's edge still casts its shadow onto it
    /// (Quartz draws the shape where it is, then its shadow moved).
    #[test]
    fn shapes_off_the_canvas_cast_their_shadows_onto_it() {
        let mut c = Canvas::new(std::ptr::null_mut(), 20, 20, 8, 0, Space::Rgb, PREMULTIPLIED_LAST).unwrap();
        c.state.shadow = Some(Shadow { offset: (8.0, -4.0), blur: 0.0, color: [0.0, 0.0, 0.0, 1.0] });
        // Left of and above the canvas: x -6..-2, y 22..25; its shadow x 2..6, y 18..21.
        let rect = Rect::new(-6.0, 22.0, 4.0, 3.0);
        c.shadowed(|c| c.rect_area(rect), |c, _| c.fill_rect(rect));
        let at = |c: &mut Canvas, x: usize, y: usize| {
            let row = (19 - y) * c.bytes_per_row;
            c.memory()[row + x * 4 + 3]
        };
        assert_eq!((at(&mut c, 3, 18), at(&mut c, 5, 19)), (255, 255), "the shadow lands");
        assert_eq!((at(&mut c, 7, 18), at(&mut c, 3, 17)), (0, 0), "nothing past it");
    }

    /// UIKit's image contexts pad rows to 32 bytes (CGBitmapGetAlignedBytesPerRow).
    #[test]
    fn image_context_rows_are_padded_to_32_bytes() {
        setup();
        graphics_begin_image_context_with_options(Size { width: 9.0, height: 1.0 }, false, 1.0);
        assert_eq!(bitmap_context_get_bytes_per_row(current_context()), 64);
        graphics_end_image_context();
    }

    /// UIGraphicsPushContext holds the context; UIGraphicsPopContext pops
    /// whatever is on top (an image context too) and lets go of it.
    #[test]
    fn push_holds_and_pop_takes_any_context() {
        setup();
        let own = bitmap_context_create(std::ptr::null_mut(), 2, 2, 8, 0, space_create_rgb(), PREMULTIPLIED_LAST);
        graphics_push_context(own);
        assert_eq!(counted::count(own), Some(2), "pushing holds it");
        let image_context = counted::retain(test_begin_context(1.0, 1.0));
        graphics_pop_context();
        assert_eq!((current_context(), counted::count(image_context)), (own, Some(1)), "the image context popped and let go of");
        graphics_pop_context();
        assert_eq!((current_context(), counted::count(own)), (0, Some(1)));
        counted::release(image_context);
        counted::release(own);
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
