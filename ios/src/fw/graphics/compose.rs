//! Pictures made for the screen (fw/pictures.rs) that are not picture
//! files: layers' backgrounds and borders with round corners, gradient and
//! shape layers, CGImage contents, and groups of pictures drawn together as
//! Core Animation composites a layer: its shadow under what it shows, all of
//! it cut to its round bounds when it masks to them, and through its mask
//! layer. Each is made once into a PNG at the screen's pixels and kept while
//! it is used.

use super::*;
use crate::fw::pictures::{meet, screen_box, Group, Picture};
use std::collections::HashMap;

/// Most pixels a made picture has (it is made coarser past this), so a
/// layer with absurd bounds can't ask for more memory than there is.
const MOST_PIXELS: f64 = 4.0e6;

/// `scale` pixels per point, or fewer so `width` x `height` points stay
/// within MOST_PIXELS.
fn capped(scale: f64, width: f64, height: f64) -> f64 {
    scale.min((MOST_PIXELS / (width * height)).sqrt())
}

/// Screen checks a made picture is kept unused. The screen asks for what
/// it shows on every check (20 a second), so what shows stays; what an
/// animation made a moment ago goes, file and all, about 2 s later. Counted
/// in checks, not time, so a check that takes long to make its pictures
/// never loses its own first ones.
const KEPT_CHECKS: u64 = 40;

/// Screen checks so far (fw/pictures.rs counts them).
static CHECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A new screen check starts: what goes unused from now on counts toward going.
pub fn new_screen_check() {
    CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

struct Made {
    file: Option<String>,
    /// The screen check it was last used in.
    used: u64,
}

/// What was made, by what it shows.
static MADE: Mutex<Option<HashMap<String, Made>>> = Mutex::new(None);

thread_local! {
    /// Coming frames (fw/pictures.rs Sampling) only use what was made: a
    /// new composite each frame would cost too much. `MISSED` notes a miss.
    static ONLY_MADE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MISSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `compose` using only pictures made already: None if it needed a new one.
pub fn only_made<T>(compose: impl FnOnce() -> T) -> Option<T> {
    ONLY_MADE.with(|o| o.set(true));
    MISSED.with(|m| m.set(false));
    let out = compose();
    ONLY_MADE.with(|o| o.set(false));
    (!MISSED.with(|m| m.get())).then_some(out)
}

/// The file made for `key`: made by `make` the first time it is asked for.
fn made(key: String, make: impl FnOnce() -> Option<String>) -> Option<String> {
    let check = CHECKS.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(found) = MADE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&key) {
        found.used = check;
        return found.file.clone();
    }
    if ONLY_MADE.with(|o| o.get()) {
        MISSED.with(|m| m.set(true));
        return None;
    }
    let file = make();
    let mut all = MADE.lock().unwrap_or_else(|e| e.into_inner());
    let kept = all.get_or_insert_with(HashMap::new);
    kept.retain(|_, m| {
        let keep = check.saturating_sub(m.used) < KEPT_CHECKS;
        if let (false, Some(file)) = (keep, &m.file) {
            let _ = std::fs::remove_file(file);
        }
        keep
    });
    kept.insert(key, Made { file: file.clone(), used: check });
    file
}

fn pixels(points: f64, scale: f64) -> usize {
    (points * scale).ceil().max(1.0) as usize
}

/// A size worth drawing: some area, and finite.
fn drawable(width: f64, height: f64) -> bool {
    width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite()
}

/// A clear canvas of `width` x `height` pixels whose user space is points
/// from `origin`, y going down, `scale` pixels per point.
fn canvas_at(origin: Point, width: usize, height: usize, scale: f64) -> Option<Canvas> {
    let mut c = Canvas::new(std::ptr::null_mut(), width, height, 8, 0, Space::Rgb, PREMULTIPLIED_LAST).ok()?;
    c.state.ctm = Transform { a: scale, b: 0.0, c: 0.0, d: -scale, tx: -origin.x * scale, ty: height as f64 + origin.y * scale };
    Some(c)
}

fn save(c: &mut Canvas, what: &str) -> Option<String> {
    let rgba = c.snapshot().straight_rgba()?;
    write_png_file(what, &encode_png(c.width, c.height, &rgba, true)?)
}

/// A layer's background or border: a box of `size` points, its corners
/// rounded by `radius`, filled with `fill`; the border (colour, width) lies
/// inside its edge, following the corners (Core Animation's way).
pub fn box_picture(size: (f64, f64), radius: f64, fill: [f64; 4], border: ([f64; 4], f64), scale: f64) -> Option<String> {
    if !drawable(size.0, size.1) {
        return None;
    }
    let scale = capped(scale, size.0, size.1);
    made(format!("box {size:?} {radius} {fill:?} {border:?} {scale}"), || {
        let mut c = canvas_at(Point::default(), pixels(size.0, scale), pixels(size.1, scale), scale)?;
        let to_device = c.to_device();
        let bounds = Rect::new(0.0, 0.0, size.0, size.1);
        if fill[3] > 0.0 {
            let mut outline = shapes::Outline::default();
            outline.rounded_rect(bounds, radius, &to_device);
            c.state.fill = fill;
            c.fill_outline(&outline, false);
        }
        let (color, width) = border;
        if width > 0.0 && color[3] > 0.0 {
            // Between the bounds and the bounds made smaller by the width.
            let mut outline = shapes::Outline::default();
            outline.rounded_rect(bounds, radius, &to_device);
            let inner = Rect::new(width, width, size.0 - 2.0 * width, size.1 - 2.0 * width);
            if inner.size.width > 0.0 && inner.size.height > 0.0 {
                outline.rounded_rect(inner, (radius - width).max(0.0), &to_device);
            }
            c.state.fill = color;
            c.fill_outline(&outline, true);
        }
        save(&mut c, "a layer's background or border")
    })
}

/// A gradient layer's colours over `size` points, from `start` to `end` (unit
/// points of the layer), the stops (straight colour, location) taken in
/// location order, as renderInContext: takes them.
pub fn gradient_picture(size: (f64, f64), start: Point, end: Point, stops: &[([f64; 4], f64)], scale: f64) -> Option<String> {
    if !drawable(size.0, size.1) || stops.is_empty() {
        return None;
    }
    let scale = capped(scale, size.0, size.1);
    let mut stops: Vec<(f64, [f64; 4])> = stops.iter().map(|&(color, at)| (at, color)).collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    made(format!("gradient {size:?} {start:?} {end:?} {stops:?} {scale}"), || {
        let mut c = canvas_at(Point::default(), pixels(size.0, scale), pixels(size.1, scale), scale)?;
        let at = |p: Point| Point { x: p.x * size.0, y: p.y * size.1 };
        let gradient = Gradient { stops };
        c.draw_linear_gradient(&gradient, at(start), at(end), 3);
        save(&mut c, "a gradient layer")
    })
}

/// A shape layer's path (figures in the layer's points, a closed one ending
/// at its start) filled and stroked, over `area` of the layer's points.
pub fn shape_picture(area: Rect, figures: &[Vec<Point>], fill: Option<[f64; 4]>, even_odd: bool, stroke: Option<([f64; 4], f64)>, scale: f64) -> Option<String> {
    if !drawable(area.size.width, area.size.height) {
        return None;
    }
    let scale = capped(scale, area.size.width, area.size.height);
    made(format!("shape {area:?} {figures:?} {fill:?} {even_odd} {stroke:?} {scale}"), || {
        let mut c = canvas_at(area.origin, pixels(area.size.width, scale), pixels(area.size.height, scale), scale)?;
        let outline = outline_of(figures, &c.to_device());
        if let Some(color) = fill {
            c.state.fill = color;
            c.fill_outline(&outline, even_odd);
        }
        if let Some((color, width)) = stroke {
            (c.state.stroke, c.state.line_width) = (color, width);
            c.stroke_outline(&outline);
        }
        save(&mut c, "a shape layer")
    })
}

fn outline_of(figures: &[Vec<Point>], map: &dyn Fn((f64, f64)) -> (f64, f64)) -> shapes::Outline {
    let mut outline = shapes::Outline::default();
    for figure in figures {
        let Some(first) = figure.first() else { continue };
        outline.move_to(map((first.x, first.y)));
        for p in &figure[1..] {
            outline.line_to(map((p.x, p.y)));
        }
        if figure.len() > 2 && figure.last() == Some(first) {
            outline.close();
        }
    }
    outline
}

/// A CGImage's size in pixels.
pub fn cg_size(picture: Id) -> Option<(usize, usize)> {
    image(picture).map(|b| (b.width, b.height))
}

/// A PNG of a CGImage (a layer's contents), made once.
pub fn cg_picture(picture: Id) -> Option<String> {
    let bitmap = image(picture)?;
    made(format!("cgimage {picture:x} {:p}", Arc::as_ptr(&bitmap)), || {
        let rgba = bitmap.straight_rgba()?;
        write_png_file(&format!("CGImage {picture:#x}"), &encode_png(bitmap.width, bitmap.height, &rgba, true)?)
    })
}

/// A picture file's pixels, kept for the next draws.
fn decoded(path: &str) -> Option<Arc<Bitmap>> {
    static FILES: Mutex<Option<HashMap<String, Arc<Bitmap>>>> = Mutex::new(None);
    if let Some(found) = FILES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|files| files.get(path)) {
        return Some(found.clone());
    }
    let (bitmap, _) = decode(&std::fs::read(path).ok()?)?;
    let bitmap = Arc::new(bitmap);
    let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
    let files = files.get_or_insert_with(HashMap::new);
    if files.len() >= 64 {
        files.clear();
    }
    files.insert(path.to_string(), bitmap.clone());
    Some(bitmap)
}

/// How much a transform scales lengths.
fn scale_of(t: Transform) -> f64 {
    (t.a * t.d - t.b * t.c).abs().sqrt()
}

/// The screen area holding all the pictures (as far as each may draw).
fn covered(pictures: &[Picture]) -> Option<Rect> {
    pictures
        .iter()
        .map(|p| meet(screen_box(p.place, Rect::new(0.0, 0.0, p.area.0, p.area.1)), p.clip))
        .filter(|r| r.size.width > 0.0 && r.size.height > 0.0)
        .reduce(union)
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.origin.x.min(b.origin.x), a.origin.y.min(b.origin.y));
    let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect::new(left, top, right - left, bottom - top)
}

/// The shadow's offset in screen points.
fn screen_offset(group: &Group, offset: (f64, f64)) -> (f64, f64) {
    let t = group.place;
    (t.a * offset.0 + t.c * offset.1, t.b * offset.0 + t.d * offset.1)
}

/// The screen area a group's picture covers: what it shows and its shadow,
/// cut to its bounds, its mask and the area it may draw in.
fn region(group: &Group, clip: Rect, content: &[Picture], mask: &[Picture]) -> Option<Rect> {
    let mut area = covered(content);
    if let Some(shadow) = &group.shadow {
        let source = match &shadow.path {
            Some(figures) => {
                let points: Vec<Point> = figures.iter().flatten().copied().collect();
                let (x0, x1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
                let (y0, y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
                (x0 < x1 && y0 < y1).then(|| screen_box(group.place, Rect::new(x0, y0, x1 - x0, y1 - y0)))
            }
            None => area,
        };
        if let Some(source) = source {
            let (dx, dy) = screen_offset(group, shadow.offset);
            let margin = 1.5 * shadow.radius * scale_of(group.place) + 1.0;
            let moved = Rect::new(source.origin.x + dx - margin, source.origin.y + dy - margin, source.size.width + 2.0 * margin, source.size.height + 2.0 * margin);
            area = Some(area.map_or(moved, |a| union(a, moved)));
        }
    }
    let mut area = area?;
    if group.clips {
        area = meet(area, screen_box(group.place, group.bounds));
    }
    if group.masked {
        area = meet(area, covered(mask)?);
    }
    let area = meet(area, clip);
    (area.size.width > 0.0 && area.size.height > 0.0).then_some(area)
}

/// Draws a group (fw/pictures.rs `Group`; its pictures' files made) as Core
/// Animation composites the layer, giving the pictures to show instead: one
/// picture of it all, or, when it only has a shadow, the shadow's picture
/// and then the pictures as they were.
pub fn group_pictures(start: &Picture, content: &[Picture], mask: &[Picture], scale: f64) -> Vec<Picture> {
    let Some(group) = start.group.as_deref() else { return content.to_vec() };
    let only_shadow = !group.clips && !group.masked;
    let fallback = if only_shadow { content.to_vec() } else { Vec::new() };
    if only_shadow && group.shadow.is_none() {
        return fallback;
    }
    let Some(area) = region(group, start.clip, content, mask) else { return fallback };
    let k = capped(scale, area.size.width, area.size.height);
    let key = format!("group {k} {area:?} {group:?} {content:?} {mask:?}");
    let Some(file) = made(key, || {
        // TEMP: which groups are composited, and what they cost.
        let t = std::time::Instant::now();
        let out = composite(group, area, k, content, mask, only_shadow);
        crate::log!("TEMP group {:.0}x{:.0} at ({:.0},{:.0}) k {k}: {} pictures, clips {} radius {} masked {} shadow {:?}: {} ms", area.size.width, area.size.height, area.origin.x, area.origin.y, content.len(), group.clips, group.radius, group.masked, group.shadow.as_ref().map(|s| (s.radius, s.offset, s.path.is_some())), t.elapsed().as_millis());
        out
    }) else { return fallback };
    let size = (pixels(area.size.width, k) as f64 / k, pixels(area.size.height, k) as f64 / k);
    let place = Transform { tx: area.origin.x, ty: area.origin.y, ..IDENTITY };
    let picture = Picture { path: file, place, area: size, alpha: 1.0, mode: 0, size, text: None, clip: start.clip, group: None, corner: start.corner };
    if only_shadow {
        std::iter::once(picture).chain(content.iter().cloned()).collect()
    } else {
        vec![picture]
    }
}

/// Draws resolved pictures as the phone app does: each over its area by its
/// content mode, with its alpha, inside the screen area it may draw in.
/// `screen`: screen points to the canvas's pixels.
fn draw_pictures(c: &mut Canvas, pictures: &[Picture], screen: Transform) {
    let whole = Rect::new(0.0, 0.0, c.width as f64, c.height as f64);
    for p in pictures {
        let Some(bitmap) = decoded(&p.path) else { continue };
        c.state.clip = None;
        let clip = screen_box(screen, p.clip);
        if meet(clip, whole) != whole {
            let mut outline = shapes::Outline::default();
            outline.rect(clip, &|p| p);
            c.clip_to(&outline, false);
        }
        // Round corners of a layer inside the group (fw/pictures.rs `Corner`).
        if let Some(corner) = p.corner {
            let ((rx, ry), k) = (corner.radius, screen.a.abs());
            let mut outline = shapes::Outline::default();
            outline.rounded_rect(screen_box(screen, corner.rect), rx.min(ry) * k, &|p| p);
            c.clip_to(&outline, false);
        }
        c.state.ctm = geometry::concat(p.place, screen);
        c.state.alpha = p.alpha.clamp(0.0, 1.0);
        let ((aw, ah), (sw, sh)) = (p.area, p.size);
        let rect = match p.mode {
            _ if sw <= 0.0 || sh <= 0.0 => Rect::new(0.0, 0.0, aw, ah),
            0 => Rect::new(0.0, 0.0, aw, ah),
            1 | 2 => {
                let k = if p.mode == 1 { (aw / sw).min(ah / sh) } else { (aw / sw).max(ah / sh) };
                Rect::new((aw - sw * k) / 2.0, (ah - sh * k) / 2.0, sw * k, sh * k)
            }
            _ => Rect::new((aw - sw) / 2.0, (ah - sh) / 2.0, sw, sh),
        };
        if p.mode == 2 {
            // Filling and cropped to its area.
            let mut outline = shapes::Outline::default();
            outline.rect(Rect::new(0.0, 0.0, aw, ah), &c.to_device());
            c.clip_to(&outline, false);
        }
        // The picture's first row at the top of its rect (UIKit's way up).
        c.draw_bitmap(rect, &bitmap, &|u, v| (u, 1.0 - v));
    }
    c.state.clip = None;
}

/// The canvas's pixels, premultiplied, rows from the top.
fn premultiplied(c: &mut Canvas) -> Vec<[f32; 4]> {
    let (layout, width, height, bytes_per_row) = (c.layout, c.width, c.height, c.bytes_per_row);
    let memory = c.memory();
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        for x in 0..width {
            let at = row * bytes_per_row + x * layout.bytes;
            out.push(layout.read(&memory[at..at + layout.bytes]));
        }
    }
    out
}

/// A coverage mask (rows from the bottom, as the canvas keeps them) with rows from the top.
fn from_top(cover: Vec<f32>, width: usize, height: usize) -> Vec<f32> {
    cover.chunks(width).rev().flatten().copied().take(width * height).collect()
}

/// A Gaussian blur of `sigma` pixels, as three box blurs each way.
fn blur(values: Vec<f32>, width: usize, height: usize, sigma: f64) -> Vec<f32> {
    if sigma < 0.5 {
        return values;
    }
    let reach = (((12.0 * sigma * sigma / 3.0 + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
    let mut values = values;
    let mut other = vec![0f32; values.len()];
    for _ in 0..3 {
        box_blur(&values, &mut other, width, height, reach, (1, width));
        box_blur(&other, &mut values, height, width, reach, (width, 1));
    }
    values
}

/// One box blur along lines of `length` (`count` of them): `step` is (along
/// a line, between lines) in the buffer.
fn box_blur(from: &[f32], to: &mut [f32], length: usize, count: usize, reach: usize, (along, between): (usize, usize)) {
    let share = 1.0 / (2 * reach + 1) as f32;
    for line in 0..count {
        let at = |i: usize| from[line * between + i * along];
        let mut sum: f32 = (0..reach.min(length)).map(at).sum();
        for i in 0..length {
            if i + reach < length {
                sum += at(i + reach);
            }
            to[line * between + i * along] = sum * share;
            if i >= reach {
                sum -= at(i - reach);
            }
        }
    }
}

/// The group's picture over `area` (screen points) at `k` pixels per point.
fn composite(group: &Group, area: Rect, k: f64, content: &[Picture], mask: &[Picture], only_shadow: bool) -> Option<String> {
    let (width, height) = (pixels(area.size.width, k), pixels(area.size.height, k));
    let mut c = canvas_at(area.origin, width, height, k)?;
    let screen = c.state.ctm;
    let layer = geometry::concat(group.place, screen);
    draw_pictures(&mut c, content, screen);
    let mut out = premultiplied(&mut c);
    if let Some(shadow) = &group.shadow {
        let alpha = match &shadow.path {
            Some(figures) => from_top(outline_of(figures, &|(x, y)| geometry::apply(layer, x, y)).cover(false, width, height), width, height),
            None => out.iter().map(|p| p[3]).collect(),
        };
        let alpha = blur(alpha, width, height, shadow.radius / 2.0 * scale_of(group.place) * k);
        let (dx, dy) = screen_offset(group, shadow.offset);
        let (dx, dy) = ((dx * k).round() as isize, (dy * k).round() as isize);
        let color = premultiply(shadow.color);
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = (x as isize - dx, y as isize - dy);
                let cover = if sx >= 0 && sy >= 0 && (sx as usize) < width && (sy as usize) < height { alpha[sy as usize * width + sx as usize] } else { 0.0 };
                let under = scaled(color, cover.min(1.0));
                let pixel = &mut out[y * width + x];
                *pixel = if only_shadow { under } else { over(*pixel, under) };
            }
        }
    }
    let mut keep = |cover: &[f32]| out.iter_mut().zip(cover).for_each(|(pixel, &k)| *pixel = scaled(*pixel, k));
    if group.clips {
        let mut outline = shapes::Outline::default();
        outline.rounded_rect(group.bounds, group.radius, &|(x, y)| geometry::apply(layer, x, y));
        keep(&from_top(outline.cover(false, width, height), width, height));
    }
    if group.masked {
        let mut m = canvas_at(area.origin, width, height, k)?;
        draw_pictures(&mut m, mask, screen);
        let alpha: Vec<f32> = premultiplied(&mut m).iter().map(|p| p[3]).collect();
        keep(&alpha);
    }
    if out.iter().all(|p| p[3] <= 0.0) {
        return None;
    }
    let mut picture = Canvas::new(std::ptr::null_mut(), width, height, 8, width * 4, Space::Rgb, PREMULTIPLIED_LAST).ok()?;
    let layout = picture.layout;
    for (pixel, color) in picture.memory().chunks_exact_mut(4).zip(&out) {
        layout.write(pixel, *color);
    }
    save(&mut picture, "a layer drawn with its corners, mask or shadow")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made picture file's pixels: width, height, straight RGBA rows.
    fn pixels_of(file: &str) -> (usize, usize, Vec<u8>) {
        let (bitmap, _) = decode(&std::fs::read(file).unwrap()).unwrap();
        (bitmap.width, bitmap.height, bitmap.straight_rgba().unwrap())
    }

    /// However big a layer is, its drawn pictures stay within the pixels a
    /// group may have (made coarser past that), so huge bounds can't use up
    /// the memory.
    #[test]
    fn drawn_pictures_have_at_most_so_many_pixels() {
        let big = (3000.0, 3000.0);
        let red = [1.0, 0.0, 0.0, 1.0];
        let files = [
            box_picture(big, 10.0, red, ([0.0; 4], 0.0), 1.0),
            gradient_picture(big, Point { x: 0.0, y: 0.0 }, Point { x: 1.0, y: 1.0 }, &[(red, 0.0), ([0.0, 0.0, 1.0, 1.0], 1.0)], 1.0),
            shape_picture(Rect::new(0.0, 0.0, big.0, big.1), &[vec![Point { x: 0.0, y: 0.0 }, Point { x: 3000.0, y: 0.0 }, Point { x: 0.0, y: 3000.0 }, Point { x: 0.0, y: 0.0 }]], Some(red), false, None, 1.0),
        ];
        for file in files {
            let (width, height, _) = pixels_of(&file.expect("a picture"));
            assert!((width * height) as f64 <= MOST_PIXELS * 1.01, "{width}x{height}");
        }
    }

    /// A gradient's stops are taken in location order, whatever order they
    /// were given in (as renderInContext: draws them).
    #[test]
    fn gradient_stops_go_by_location() {
        let (red, blue) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]);
        let file = gradient_picture((10.0, 1.0), Point { x: 0.0, y: 0.5 }, Point { x: 1.0, y: 0.5 }, &[(blue, 1.0), (red, 0.0)], 1.0).expect("a picture");
        let (width, _, rgba) = pixels_of(&file);
        assert!(rgba[0] > 200 && rgba[2] < 50, "red at the start: {:?}", &rgba[..4]);
        let last = (width - 1) * 4;
        assert!(rgba[last + 2] > 200 && rgba[last] < 50, "blue at the end: {:?}", &rgba[last..last + 4]);
    }
}
