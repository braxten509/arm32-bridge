//! Pictures made for the screen (fw/pictures.rs) that are not picture
//! files: layers' backgrounds and borders with round corners, gradient and
//! shape layers, CGImage contents, and groups of pictures drawn together as
//! Core Animation composites a layer: its shadow under what it shows, all of
//! it cut to its round bounds when it masks to them, and through its mask
//! layer. Each is made once into a PNG at the screen's pixels and kept while
//! it is used.

use super::*;
use crate::fw::pictures::{meet, screen_box, Corner, Group, Picture, Shadow};
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
///
/// Tests keep everything: they run side by side, each making screen checks
/// of its own, so the shared count would let one test's checks clean up
/// files another test is about to read.
const KEPT_CHECKS: u64 = if cfg!(test) { u64::MAX } else { 40 };

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
            super::remove_picture_file(file);
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
pub fn shape_picture(area: Rect, figures: &[Vec<Point>], fill: Option<[f64; 4]>, even_odd: bool, stroke: Option<([f64; 4], StrokeStyle)>, scale: f64) -> Option<String> {
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
        if let Some((color, style)) = &stroke {
            c.state.stroke = *color;
            c.set_stroke_style(style);
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
    // An indexed picture is turned into colours once, not at every draw.
    let bitmap = Arc::new(bitmap.unindexed().unwrap_or(bitmap));
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
        .map(|p| meet(picture_box(p, Rect::new(0.0, 0.0, p.area.0, p.area.1)), p.clip))
        .filter(|r| r.size.width > 0.0 && r.size.height > 0.0)
        .reduce(union)
}

/// A picture's place with its perspective (fw/pictures.rs `Proj`): rows
/// (a, b, p0), (c, d, p1), (tx, ty, w); a point is a row on the left.
fn projective(p: &Picture) -> [f64; 9] {
    let (t, [p0, p1, w]) = (p.place, p.perspective.unwrap_or([0.0, 0.0, 1.0]));
    [t.a, t.b, p0, t.c, t.d, p1, t.tx, t.ty, w]
}

/// Where `m` puts (x, y); None behind the eye.
fn project(m: &[f64; 9], x: f64, y: f64) -> Option<(f64, f64)> {
    let w = x * m[2] + y * m[5] + m[8];
    (w > 1e-9).then(|| ((x * m[0] + y * m[3] + m[6]) / w, (x * m[1] + y * m[4] + m[7]) / w))
}

/// The inverse of a 3x3 map (None when it flattens the plane).
fn invert_3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let cofactor = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0 * 3 + c0] * m[r1 * 3 + c1] - m[r0 * 3 + c1] * m[r1 * 3 + c0]
    };
    let determinant: f64 = (0..3).map(|c| m[c] * cofactor(0, c)).sum();
    (determinant.abs() > 1e-12).then(|| std::array::from_fn(|i| cofactor(i % 3, i / 3) / determinant))
}

/// The screen area `rect` (in a picture's own points) covers, its perspective too.
fn picture_box(p: &Picture, rect: Rect) -> Rect {
    match p.perspective {
        None => screen_box(p.place, rect),
        Some(_) => projected_box(&projective(p), rect),
    }
}

/// The screen area `rect` covers through the projective map `m`.
fn projected_box(m: &[f64; 9], rect: Rect) -> Rect {
    let corners: Option<Vec<(f64, f64)>> = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
        .iter()
        .map(|(u, v)| project(m, rect.origin.x + u * rect.size.width, rect.origin.y + v * rect.size.height))
        .collect();
    let Some(corners) = corners else { return Rect::default() };
    let (left, right) = corners.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.0), hi.max(c.0)));
    let (top, bottom) = corners.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.1), hi.max(c.1)));
    Rect::new(left, top, right - left, bottom - top)
}

/// A group's place with its perspective (as `projective`).
fn group_projective(group: &Group) -> [f64; 9] {
    let (t, [p0, p1, w]) = (group.place, group.perspective.unwrap_or([0.0, 0.0, 1.0]));
    [t.a, t.b, p0, t.c, t.d, p1, t.tx, t.ty, w]
}

/// A group's nearest affine place (its own when it has no perspective).
fn group_flat(group: &Group) -> Transform {
    let t = group.place;
    match group.perspective {
        Some([_, _, w]) if w.abs() > 1e-12 => Transform { a: t.a / w, b: t.b / w, c: t.c / w, d: t.d / w, tx: t.tx / w, ty: t.ty / w },
        _ => t,
    }
}

/// A point in a group's own points on the canvas (`screen`: screen points to its pixels).
fn group_point(group: &Group, screen: Transform, x: f64, y: f64) -> (f64, f64) {
    let (sx, sy) = project(&group_projective(group), x, y).unwrap_or((x, y));
    geometry::apply(screen, sx, sy)
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.origin.x.min(b.origin.x), a.origin.y.min(b.origin.y));
    let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect::new(left, top, right - left, bottom - top)
}

/// The shadow's offset in screen points.
fn screen_offset(group: &Group, offset: (f64, f64)) -> (f64, f64) {
    let t = group_flat(group);
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
                (x0 < x1 && y0 < y1).then(|| projected_box(&group_projective(group), Rect::new(x0, y0, x1 - x0, y1 - y0)))
            }
            None => area,
        };
        if let Some(source) = source {
            let (dx, dy) = screen_offset(group, shadow.offset);
            let margin = 3.0 * shadow.radius * scale_of(group_flat(group)) + 1.0;
            let moved = Rect::new(source.origin.x + dx - margin, source.origin.y + dy - margin, source.size.width + 2.0 * margin, source.size.height + 2.0 * margin);
            area = Some(area.map_or(moved, |a| union(a, moved)));
        }
    }
    let mut area = area?;
    if group.clips {
        area = meet(area, projected_box(&group_projective(group), group.bounds));
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
    // A clip alone that cuts nothing: the pictures are shown as they are.
    if group.clips && !group.masked && group.shadow.is_none() && group.opacity >= 1.0 && content.iter().all(|p| inside_bounds(group, p)) {
        return content.to_vec();
    }
    let only_shadow = !group.clips && !group.masked && group.opacity >= 1.0;
    let fallback = if only_shadow { content.to_vec() } else { Vec::new() };
    if only_shadow && group.shadow.is_none() {
        return fallback;
    }
    // A shadow alone is drawn at full strength and faded as a picture, so
    // one whose strength changes (a pulsing glow) is drawn once.
    let mut fade = if only_shadow { group.shadow.as_ref().map_or(1.0, |shadow| shadow.color[3].clamp(0.0, 1.0)) } else { 1.0 };
    // So is the shadow of one fading picture (a damage number floating
    // away): it is just as much fainter. One cast from a set path doesn't
    // change however faded the pictures are.
    let whole: Vec<Picture>;
    let content_shown = content;
    let set_path = group.shadow.as_ref().is_some_and(|s| s.path.is_some());
    let content = match content {
        _ if only_shadow && set_path && content.iter().any(|p| p.alpha != 1.0) => {
            whole = content.iter().map(|p| Picture { alpha: 1.0, ..p.clone() }).collect();
            &whole[..]
        }
        [p] if only_shadow && !set_path && p.alpha > 0.0 && p.alpha < 1.0 => {
            fade *= p.alpha;
            whole = vec![Picture { alpha: 1.0, ..p.clone() }];
            &whole[..]
        }
        _ => content,
    };
    if fade <= 0.0 {
        return fallback;
    }
    let full;
    let group = if fade < 1.0 {
        full = Group { shadow: group.shadow.clone().map(|shadow| Shadow { color: [shadow.color[0], shadow.color[1], shadow.color[2], 1.0], ..shadow }), ..group.clone() };
        &full
    } else {
        group
    };
    let Some(area) = region(group, start.clip, content, mask) else { return fallback };
    let k = capped(scale, area.size.width, area.size.height);
    let (key, log) = group_key(k, area, group, content, mask);
    let Some(file) = made(key, || {
        let began = std::time::Instant::now();
        let file = composite(group, area, k, content, mask, only_shadow, start.corner);
        if let Some(key) = &log {
            note_group_made(key, area, k, group, content.len(), began.elapsed());
        }
        file
    }) else {
        return fallback;
    };
    let size = (pixels(area.size.width, k) as f64 / k, pixels(area.size.height, k) as f64 / k);
    let place = Transform { tx: area.origin.x, ty: area.origin.y, ..IDENTITY };
    // A rasterized layer's opacity fades the whole picture.
    let picture = Picture { path: file, place, area: size, alpha: group.opacity * fade, mode: 0, size, text: None, clip: start.clip, group: None, corner: start.corner, perspective: None };
    if only_shadow {
        std::iter::once(picture).chain(content_shown.iter().cloned()).collect()
    } else {
        vec![picture]
    }
}

/// What a group shows, wherever it is: its places counted from its area's
/// corner, so a group that only moves (a floating, shadowed piece) keeps
/// the picture made for it and is put where it now is. None for turned-in-3D
/// pictures (their key stays as it was).
fn moved(area: Rect, group: &Group, content: &[Picture], mask: &[Picture]) -> Option<Moved> {
    if group.perspective.is_some() || content.iter().chain(mask).any(|p| p.perspective.is_some()) {
        return None;
    }
    let (ox, oy) = (area.origin.x, area.origin.y);
    // Rounded so the noise left after taking the corner off doesn't count.
    let r = |v: f64| (v * 1000.0).round() / 1000.0;
    let shift = |t: Transform| Transform { tx: r(t.tx - ox), ty: r(t.ty - oy), ..t };
    let shift_rect = |q: Rect| Rect::new(r(q.origin.x - ox), r(q.origin.y - oy), q.size.width, q.size.height);
    // A clip counts only where it cuts the area (a screen-wide one stays put while the group moves).
    let picture = |p: &Picture| Picture { place: shift(p.place), clip: shift_rect(meet(p.clip, area)), corner: p.corner.map(|c| Corner { rect: shift_rect(c.rect), ..c }), ..p.clone() };
    let group = Group { place: shift(group.place), ..group.clone() };
    let content: Vec<Picture> = content.iter().map(picture).collect();
    let mask: Vec<Picture> = mask.iter().map(picture).collect();
    Some(((r(area.size.width), r(area.size.height)), group, content, mask))
}

/// A moved group's size and its contents counted from its area's corner.
type Moved = ((f64, f64), Group, Vec<Picture>, Vec<Picture>);

/// The key a group's made picture is kept under, and with IOS_GROUP_LOG
/// the same as text (to show what changed when one is drawn anew).
fn group_key(k: f64, area: Rect, group: &Group, content: &[Picture], mask: &[Picture]) -> (String, Option<String>) {
    match moved(area, group, content, mask) {
        Some((size, group, content, mask)) => (
            format!("group moved {:032x}", fingerprint(&(k, size, &group, &content, &mask))),
            group_log().then(|| format!("group moved {k} {size:?} {group:?} {content:?} {mask:?}")),
        ),
        None => (
            format!("group {:032x}", fingerprint(&(k, area, group, content, mask))),
            group_log().then(|| format!("group {k} {area:?} {group:?} {content:?} {mask:?}")),
        ),
    }
}

/// 128 bits of a value's exact contents (numbers by their bits): two
/// differently started hashes of the same bytes.
pub(super) fn fingerprint(value: &impl Print) -> u128 {
    use std::hash::Hasher;
    struct Both(std::collections::hash_map::DefaultHasher, std::collections::hash_map::DefaultHasher);
    impl Hasher for Both {
        fn write(&mut self, bytes: &[u8]) {
            self.0.write(bytes);
            self.1.write(bytes);
        }
        fn finish(&self) -> u64 {
            self.0.finish()
        }
    }
    let mut both = Both(Default::default(), Default::default());
    both.1.write_u8(0x5a);
    value.print(&mut both);
    (u128::from(both.0.finish()) << 64) | u128::from(both.1.finish())
}

/// Feeds everything a value holds to a hasher, floats by their bits (as
/// exact as their text, without the time it takes to write it). Structs
/// name every field, so a new field can't be left out.
pub(super) trait Print {
    fn print(&self, h: &mut impl std::hash::Hasher);
}

impl Print for f64 {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_u64(self.to_bits());
    }
}
impl Print for bool {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_u8(*self as u8);
    }
}
impl Print for i64 {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_i64(*self);
    }
}
impl Print for isize {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_isize(*self);
    }
}
impl Print for String {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_usize(self.len());
        h.write(self.as_bytes());
    }
}
impl<T: Print + ?Sized> Print for &T {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        (**self).print(h);
    }
}
impl<T: Print> Print for Box<T> {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        (**self).print(h);
    }
}
impl<T: Print> Print for [T] {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        h.write_usize(self.len());
        self.iter().for_each(|v| v.print(h));
    }
}
impl<T: Print> Print for Vec<T> {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        self[..].print(h);
    }
}
impl<T: Print, const N: usize> Print for [T; N] {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        self[..].print(h);
    }
}
impl<T: Print> Print for Option<T> {
    fn print(&self, h: &mut impl std::hash::Hasher) {
        match self {
            None => h.write_u8(0),
            Some(v) => {
                h.write_u8(1);
                v.print(h);
            }
        }
    }
}
macro_rules! print_tuple {
    ($($name:ident)+) => {
        impl<$($name: Print),+> Print for ($($name,)+) {
            #[allow(non_snake_case)]
            fn print(&self, h: &mut impl std::hash::Hasher) {
                let ($($name,)+) = self;
                $($name.print(h);)+
            }
        }
    };
}
print_tuple!(A B);
print_tuple!(A B C D E);
macro_rules! print_struct {
    ($ty:path { $($field:ident),+ }) => {
        impl Print for $ty {
            fn print(&self, h: &mut impl std::hash::Hasher) {
                let $ty { $($field),+ } = self;
                $($field.print(h);)+
            }
        }
    };
}
print_struct!(Point { x, y });
print_struct!(geometry::Size { width, height });
print_struct!(Rect { origin, size });
print_struct!(Transform { a, b, c, d, tx, ty });
print_struct!(crate::fw::pictures::Corner { rect, radius });
print_struct!(crate::fw::pictures::Shadow { color, radius, offset, path });
print_struct!(Group { place, perspective, bounds, clips, radius, masked, shadow, opacity });
print_struct!(super::text::TextLook { text, font, color, shadow, alignment, line_break, lines, size, minimum_font_size });
print_struct!(Picture { path, place, area, alpha, mode, size, text, clip, group, corner, perspective });

/// Whether all `p` draws lies inside the group's bounds (their corners
/// rounded by its radius), so clipping to them changes nothing.
fn inside_bounds(group: &Group, p: &Picture) -> bool {
    // A little past the edge is still inside: places carry rounding noise.
    const SLACK: f64 = 0.01;
    if group.perspective.is_some() || p.perspective.is_some() || p.path.starts_with(crate::fw::pictures::TRANSITION_MARK) {
        return false;
    }
    let Some(back) = geometry::invert(group.place) else { return false };
    let area = Rect::new(0.0, 0.0, p.area.0, p.area.1);
    // Mode 2 is cropped to its area; the others draw in their content rect.
    let drawn = if p.mode == 2 { area } else { super::render::content_rect(area, p.size, p.mode) };
    let b = group.bounds;
    let (rx, ry) = (group.radius.min(b.size.width / 2.0), group.radius.min(b.size.height / 2.0));
    let inside = |(x, y): (f64, f64)| {
        let (left, top) = (b.origin.x - SLACK, b.origin.y - SLACK);
        let (right, bottom) = (b.origin.x + b.size.width + SLACK, b.origin.y + b.size.height + SLACK);
        if !(left..=right).contains(&x) || !(top..=bottom).contains(&y) {
            return false;
        }
        // Inside a rounded corner's square: within its ellipse.
        let cx = if x < b.origin.x + rx { b.origin.x + rx } else if x > b.origin.x + b.size.width - rx { b.origin.x + b.size.width - rx } else { return true };
        let cy = if y < b.origin.y + ry { b.origin.y + ry } else if y > b.origin.y + b.size.height - ry { b.origin.y + b.size.height - ry } else { return true };
        let (dx, dy) = ((x - cx) / (rx + SLACK), (y - cy) / (ry + SLACK));
        dx * dx + dy * dy <= 1.0
    };
    let x = [drawn.origin.x, drawn.origin.x + drawn.size.width];
    let y = [drawn.origin.y, drawn.origin.y + drawn.size.height];
    // The bounds (round or not) are convex: corners inside means all inside.
    x.iter().all(|&x| y.iter().all(|&y| {
        let (sx, sy) = geometry::apply(p.place, x, y);
        inside(geometry::apply(back, sx, sy))
    }))
}

/// IOS_GROUP_LOG=1: each group drawn anew is logged with what changed
/// since the last one of its size (finding groups redrawn every frame).
fn group_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("IOS_GROUP_LOG").is_ok_and(|v| v == "1"))
}

fn note_group_made(key: &str, area: Rect, k: f64, group: &Group, pictures: usize, took: std::time::Duration) {
    static LAST: Mutex<Option<HashMap<(i64, i64), String>>> = Mutex::new(None);
    let size = ((area.size.width * k) as i64, (area.size.height * k) as i64);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let last = last.get_or_insert_with(HashMap::new);
    let changed = match last.get(&size) {
        None => "new".to_string(),
        Some(before) => {
            let at = before.bytes().zip(key.bytes()).position(|(a, b)| a != b).unwrap_or(before.len().min(key.len()));
            let from = key[..at].rfind("Picture").unwrap_or(at.saturating_sub(120));
            let to = (at + 160).min(key.len());
            let was_to = (at + 160).min(before.len());
            let part = |text: &str, from: usize, to: usize| String::from_utf8_lossy(&text.as_bytes()[from.min(to)..to]).into_owned();
            format!("at {at}: now {:?} was {:?}", part(key, from, to), part(before, from, was_to))
        }
    };
    crate::log!(
        "group made {}x{} at {:.0},{:.0} in {:.1} ms (shadow {}, clips {}, masked {}, opacity {:.2}, {pictures} pictures): {changed}",
        size.0,
        size.1,
        area.origin.x,
        area.origin.y,
        took.as_secs_f64() * 1000.0,
        group.shadow.is_some(),
        group.clips,
        group.masked,
        group.opacity
    );
    last.insert(size, key.to_string());
}

/// Draws resolved pictures as the phone app does: each over its area by its
/// content mode, with its alpha, inside the screen area it may draw in.
/// `screen`: screen points to the canvas's pixels. `outer`: the group's own
/// round corners, which cut its finished picture (after its shadow is made).
fn draw_pictures(c: &mut Canvas, pictures: &[Picture], screen: Transform, outer: Option<Corner>) {
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
        if let Some(corner) = p.corner.filter(|&corner| Some(corner) != outer) {
            let b = screen_box(screen, corner.rect);
            let (rx, ry) = (corner.radius.0 * screen.a.abs(), corner.radius.1 * screen.d.abs());
            // Corners wider than tall: drawn round with x squeezed by ry / rx, then stretched back.
            let s = if rx > 0.0 && ry > 0.0 { ry / rx } else { 1.0 };
            let mut outline = shapes::Outline::default();
            outline.rounded_rect(Rect::new(b.origin.x * s, b.origin.y, b.size.width * s, b.size.height), ry, &|(x, y)| (x / s, y));
            c.clip_to(&outline, false);
        }
        c.state.alpha = p.alpha.clamp(0.0, 1.0);
        let ((aw, ah), (sw, sh)) = (p.area, p.size);
        let rect = super::render::content_rect(Rect::new(0.0, 0.0, aw, ah), (sw, sh), p.mode);
        if p.perspective.is_some() {
            draw_in_perspective(c, p, &bitmap, rect, screen);
            continue;
        }
        c.state.ctm = geometry::concat(p.place, screen);
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

/// A picture inside a layer turned in 3D (`Picture::perspective`), as the
/// phone draws one: its content `rect` (its own points) cut to where the
/// projective place puts it, each pixel taken from the point it shows.
fn draw_in_perspective(c: &mut Canvas, p: &Picture, bitmap: &Bitmap, rect: Rect, screen: Transform) {
    let m = projective(p);
    let Some(back) = invert_3(&m) else { return };
    let shown = if p.mode == 2 { meet(rect, Rect::new(0.0, 0.0, p.area.0, p.area.1)) } else { rect };
    let corners: Option<Vec<Point>> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]
        .iter()
        .map(|(u, v)| project(&m, shown.origin.x + u * shown.size.width, shown.origin.y + v * shown.size.height).map(|(x, y)| Point { x, y }))
        .collect();
    let Some(corners) = corners else { return };
    c.clip_to(&outline_of(&[corners], &|(x, y)| geometry::apply(screen, x, y)), false);
    let area = picture_box(p, shown);
    if area.size.width <= 0.0 || area.size.height <= 0.0 || rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    c.state.ctm = screen;
    c.draw_bitmap(area, bitmap, &|u, v| {
        // (u, v) across the screen area (v up from its bottom) back to the picture's own points.
        let (x, y) = (area.origin.x + u * area.size.width, area.origin.y + (1.0 - v) * area.size.height);
        let w = x * back[2] + y * back[5] + back[8];
        let (px, py) = ((x * back[0] + y * back[3] + back[6]) / w, (x * back[1] + y * back[4] + back[7]) / w);
        ((px - rect.origin.x) / rect.size.width, (py - rect.origin.y) / rect.size.height)
    });
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
pub(super) fn blur(values: Vec<f32>, width: usize, height: usize, sigma: f64) -> Vec<f32> {
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
fn composite(group: &Group, area: Rect, k: f64, content: &[Picture], mask: &[Picture], only_shadow: bool, outer: Option<Corner>) -> Option<String> {
    let (width, height) = (pixels(area.size.width, k), pixels(area.size.height, k));
    let mut c = canvas_at(area.origin, width, height, k)?;
    let screen = c.state.ctm;
    let layer = |(x, y): (f64, f64)| group_point(group, screen, x, y);
    draw_pictures(&mut c, content, screen, outer);
    let mut out = premultiplied(&mut c);
    if let Some(shadow) = &group.shadow {
        let alpha = match &shadow.path {
            Some(figures) => from_top(outline_of(figures, &layer).cover(false, width, height), width, height),
            None => out.iter().map(|p| p[3]).collect(),
        };
        // Core Animation's blur: a Gaussian whose deviation is shadowRadius
        // (renderInContext: gives Quartz a blur of twice the radius).
        let alpha = blur(alpha, width, height, shadow.radius * scale_of(group_flat(group)) * k);
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
        outline.rounded_rect(group.bounds, group.radius, &layer);
        keep(&from_top(outline.cover(false, width, height), width, height));
    }
    if group.masked {
        let mut m = canvas_at(area.origin, width, height, k)?;
        draw_pictures(&mut m, mask, screen, outer);
        let alpha: Vec<f32> = premultiplied(&mut m).iter().map(|p| p[3]).collect();
        keep(&alpha);
    }
    if out.iter().all(|p| p[3] <= 0.0) {
        return None;
    }
    write_png_file("a layer drawn with its corners, mask or shadow", &encode_png(width, height, &straight_bytes(&out), true)?)
}

/// Premultiplied colours as a picture file's straight RGBA bytes: each
/// rounded to a byte, then divided by the rounded alpha, as a premultiplied
/// canvas holding them reads back (by table: one pass, no canvas).
fn straight_bytes(colours: &[[f32; 4]]) -> Vec<u8> {
    static TABLE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = vec![0u8; 256 * 256];
        for alpha in 1..256 {
            let a = alpha as f32 / 255.0;
            for v in 0..256 {
                table[alpha * 256 + v] = to_byte((v as f32 / 255.0) / a);
            }
        }
        table
    });
    let mut out = Vec::with_capacity(colours.len() * 4);
    for c in colours {
        let alpha = to_byte(c[3]);
        let row = &table[alpha as usize * 256..][..256];
        out.extend([row[to_byte(c[0]) as usize], row[to_byte(c[1]) as usize], row[to_byte(c[2]) as usize], to_byte(alpha as f32 / 255.0)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turned layer that clips to its bounds, holding one picture: the
    /// clip changes nothing when the picture lies inside the bounds, so the
    /// picture is shown as it is (no group drawn on the CPU each time the
    /// layer turns, as fight icons do); one reaching past them is still cut.
    #[test]
    fn a_turned_clip_that_cuts_nothing_draws_no_group() {
        let (sin, cos) = 0.3f64.sin_cos();
        let place = Transform { a: cos, b: sin, c: -sin, d: cos, tx: 200.0, ty: 100.0 };
        let group = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 100.0, 50.0), clips: true, radius: 0.0, masked: false, shadow: None, opacity: 1.0 };
        let start = Picture { group: Some(Box::new(group)), ..crate::fw::pictures::mark_picture(Rect::new(0.0, 0.0, 568.0, 320.0)) };
        let picture = |area: (f64, f64), dx: f64| Picture {
            path: "/nowhere/icon.png".into(),
            place: geometry::concat(Transform { tx: dx, ..IDENTITY }, place),
            area,
            alpha: 1.0,
            mode: 0,
            size: area,
            text: None,
            clip: Rect::new(0.0, 0.0, 568.0, 320.0),
            group: None,
            corner: None,
            perspective: None,
        };
        let inside = vec![picture((80.0, 50.0), 10.0)];
        assert_eq!(group_pictures(&start, &inside, &[], 2.0), inside);
        let past = vec![picture((80.0, 50.0), 30.0)];
        assert_ne!(group_pictures(&start, &past, &[], 2.0), past);
    }

    /// A shadowed layer that only moves (a floating fight piece) keeps its
    /// one drawn shadow, placed where the layer now is, instead of drawing
    /// it again for every step of the move.
    #[test]
    fn a_moved_shadow_is_drawn_once() {
        let file = box_picture((40.0, 30.0), 0.0, [1.0, 0.0, 0.0, 1.0], ([0.0; 4], 0.0), 2.0).unwrap();
        let shown = |dx: f64, dy: f64| {
            let place = Transform { tx: 100.0 + dx, ty: 100.0 + dy, ..IDENTITY };
            let shadow = crate::fw::pictures::Shadow { color: [0.0, 0.0, 0.0, 0.5], radius: 3.0, offset: (0.0, 4.0), path: None };
            let group = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 40.0, 30.0), clips: false, radius: 0.0, masked: false, shadow: Some(shadow), opacity: 1.0 };
            let start = Picture { group: Some(Box::new(group)), ..crate::fw::pictures::mark_picture(Rect::new(0.0, 0.0, 568.0, 320.0)) };
            let content = vec![Picture { path: file.clone(), place, area: (40.0, 30.0), alpha: 1.0, mode: 0, size: (40.0, 30.0), text: None, clip: Rect::new(0.0, 0.0, 568.0, 320.0), group: None, corner: None, perspective: None }];
            group_pictures(&start, &content, &[], 2.0)
        };
        let (before, after) = (shown(0.0, 0.0), shown(3.25, 7.483));
        assert_eq!(before.len(), 2, "the shadow under the layer's own picture");
        assert_eq!(after[0].path, before[0].path);
        assert!((after[0].place.tx - before[0].place.tx - 3.25).abs() < 1e-6 && (after[0].place.ty - before[0].place.ty - 7.483).abs() < 1e-6);
    }

    /// A shadow whose strength pulses (a glow under a fight piece) is drawn
    /// once at full strength and faded as a picture, the way the phone
    /// fades any picture, instead of being drawn again for each strength.
    #[test]
    fn a_pulsing_shadow_is_drawn_once() {
        let file = box_picture((40.0, 30.0), 0.0, [0.0, 0.0, 1.0, 1.0], ([0.0; 4], 0.0), 2.0).unwrap();
        let shown = |strength: f64| {
            let place = Transform { tx: 100.0, ty: 100.0, ..IDENTITY };
            let shadow = crate::fw::pictures::Shadow { color: [0.0, 0.0, 0.0, strength], radius: 3.0, offset: (0.0, 4.0), path: None };
            let group = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 40.0, 30.0), clips: false, radius: 0.0, masked: false, shadow: Some(shadow), opacity: 1.0 };
            let start = Picture { group: Some(Box::new(group)), ..crate::fw::pictures::mark_picture(Rect::new(0.0, 0.0, 568.0, 320.0)) };
            let content = vec![Picture { path: file.clone(), place, area: (40.0, 30.0), alpha: 1.0, mode: 0, size: (40.0, 30.0), text: None, clip: Rect::new(0.0, 0.0, 568.0, 320.0), group: None, corner: None, perspective: None }];
            group_pictures(&start, &content, &[], 2.0)
        };
        let (strong, weak) = (shown(0.99), shown(0.5));
        assert_eq!(weak[0].path, strong[0].path);
        assert!((strong[0].alpha - 0.99).abs() < 1e-9 && (weak[0].alpha - 0.5).abs() < 1e-9);
        // Drawn at full strength: its darkest pixel is the shadow colour at (nearly) full cover.
        let (_, _, rgba) = pixels_of(&strong[0].path);
        assert!(rgba.chunks(4).map(|p| p[3]).max().unwrap() > 250);
    }

    /// A fading shadowed picture (a damage number floating away): its shadow
    /// is drawn once and faded with it, as strong as drawing it faded; a
    /// shadow cast from a set path doesn't fade with the picture.
    #[test]
    fn a_fading_shadowed_picture_draws_its_shadow_once() {
        let file = box_picture((40.0, 30.0), 0.0, [0.0, 0.0, 1.0, 1.0], ([0.0; 4], 0.0), 2.0).unwrap();
        let place = Transform { tx: 100.0, ty: 100.0, ..IDENTITY };
        let shown = |alpha: f64, path: Option<Vec<Vec<Point>>>| {
            let shadow = crate::fw::pictures::Shadow { color: [0.0, 0.0, 0.0, 0.8], radius: 3.0, offset: (0.0, 4.0), path };
            let group = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 40.0, 30.0), clips: false, radius: 0.0, masked: false, shadow: Some(shadow), opacity: 1.0 };
            let start = Picture { group: Some(Box::new(group)), ..crate::fw::pictures::mark_picture(Rect::new(0.0, 0.0, 568.0, 320.0)) };
            let content = vec![Picture { path: file.clone(), place, area: (40.0, 30.0), alpha, mode: 0, size: (40.0, 30.0), text: None, clip: Rect::new(0.0, 0.0, 568.0, 320.0), group: None, corner: None, perspective: None }];
            group_pictures(&start, &content, &[], 2.0)
        };
        let (near, far) = (shown(0.9, None), shown(0.4, None));
        assert_eq!(far[0].path, near[0].path);
        assert!((near[0].alpha - 0.72).abs() < 1e-9 && (far[0].alpha - 0.32).abs() < 1e-9, "{} {}", near[0].alpha, far[0].alpha);
        assert!((far[1].alpha - 0.4).abs() < 1e-9);
        // As strong as the shadow of the faded picture drawn as it is.
        let strongest = |path: &str| pixels_of(path).2.chunks(4).map(|p| p[3]).max().unwrap() as f64;
        let faded = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 40.0, 30.0), clips: false, radius: 0.0, masked: false, shadow: Some(crate::fw::pictures::Shadow { color: [0.0, 0.0, 0.0, 1.0], radius: 3.0, offset: (0.0, 4.0), path: None }), opacity: 1.0 };
        let content = vec![Picture { path: file.clone(), place, area: (40.0, 30.0), alpha: 0.4, mode: 0, size: (40.0, 30.0), text: None, clip: Rect::new(0.0, 0.0, 568.0, 320.0), group: None, corner: None, perspective: None }];
        let area = region(&faded, Rect::new(0.0, 0.0, 568.0, 320.0), &content, &[]).unwrap();
        let direct = composite(&faded, area, 2.0, &content, &[], true, None).unwrap();
        assert!((strongest(&far[0].path) * 0.4 - strongest(&direct)).abs() <= 2.0);
        // A set path's shadow is the same however faded the picture is.
        let square = Some(vec![vec![Point { x: 0.0, y: 0.0 }, Point { x: 40.0, y: 0.0 }, Point { x: 40.0, y: 30.0 }, Point { x: 0.0, y: 30.0 }, Point { x: 0.0, y: 0.0 }]]);
        let set = shown(0.4, square.clone());
        assert!((set[0].alpha - 0.8).abs() < 1e-9, "{}", set[0].alpha);
        // Nor is it drawn again as the picture fades.
        assert_eq!(shown(0.9, square)[0].path, set[0].path);
    }

    /// A group's key is short (every number formatted as text took most of
    /// a busy fight's group time), the same for the same group anywhere,
    /// and different when anything it shows changes.
    #[test]
    fn group_keys_are_short_and_tell_groups_apart() {
        let screen = Rect::new(0.0, 0.0, 568.0, 320.0);
        let place = Transform { tx: 100.0, ty: 100.0, ..IDENTITY };
        let shadow = crate::fw::pictures::Shadow { color: [0.0, 0.0, 0.0, 0.8], radius: 3.0, offset: (0.0, 4.0), path: None };
        let group = Group { place, perspective: None, bounds: Rect::new(0.0, 0.0, 40.0, 30.0), clips: false, radius: 0.0, masked: false, shadow: Some(shadow), opacity: 1.0 };
        let picture = |n: usize| Picture { path: format!("/p/{n}.png"), place: Transform { tx: 100.0 + n as f64, ..place }, area: (40.0, 30.0), alpha: 1.0, mode: 0, size: (40.0, 30.0), text: None, clip: screen, group: None, corner: None, perspective: None };
        let content: Vec<Picture> = (0..30).map(picture).collect();
        let area = region(&group, screen, &content, &[]).unwrap();
        let key = |group: &Group, content: &[Picture], area: Rect| group_key(2.0, area, group, content, &[]).0;
        let first = key(&group, &content, area);
        assert!(first.len() <= 64, "{} bytes", first.len());
        assert_eq!(key(&group, &content, area), first);
        // Moved as a whole: the same picture, put elsewhere.
        let by = |t: Transform| Transform { tx: t.tx + 7.5, ty: t.ty - 3.25, ..t };
        let moved_group = Group { place: by(group.place), ..group.clone() };
        let moved: Vec<Picture> = content.iter().map(|p| Picture { place: by(p.place), ..p.clone() }).collect();
        let moved_area = Rect::new(area.origin.x + 7.5, area.origin.y - 3.25, area.size.width, area.size.height);
        assert_eq!(key(&moved_group, &moved, moved_area), first);
        // Anything else changed: another key.
        let mut changed = Vec::new();
        let with = |f: &dyn Fn(&mut Picture)| { let mut c = content.clone(); f(&mut c[29]); c };
        changed.push(key(&group, &with(&|p| p.alpha = 0.5), area));
        changed.push(key(&group, &with(&|p| p.path.push('x')), area));
        changed.push(key(&group, &with(&|p| p.place.a = 1.0 + 1e-12), area));
        changed.push(key(&group, &with(&|p| p.text = Some(crate::fw::graphics::text::TextLook { text: "a".into(), font: ("F".into(), 12.0), color: [1.0; 4], shadow: None, alignment: 0, line_break: 0, lines: 1, size: (40.0, 30.0), minimum_font_size: None })), area));
        changed.push(key(&group, &content[..29], area));
        changed.push(key(&Group { opacity: 0.5, ..group.clone() }, &content, area));
        changed.push(key(&Group { shadow: Some(crate::fw::pictures::Shadow { radius: 4.0, ..group.shadow.clone().unwrap() }), ..group.clone() }, &content, area));
        changed.push(key(&Group { shadow: Some(crate::fw::pictures::Shadow { path: Some(vec![vec![Point { x: 0.0, y: 0.0 }]]), ..group.shadow.clone().unwrap() }), ..group.clone() }, &content, area));
        changed.push(group_key(3.0, area, &group, &content, &[]).0);
        changed.push(group_key(2.0, area, &group, &content, &content[..1]).0);
        for (n, other) in changed.iter().enumerate() {
            assert_ne!(other, &first, "change {n} kept the key");
        }
        let distinct: std::collections::HashSet<&String> = changed.iter().collect();
        assert_eq!(distinct.len(), changed.len());
    }

    /// A group's colours become the file's bytes directly, the same bytes
    /// as written into a premultiplied canvas and read back straight.
    #[test]
    fn group_colours_become_the_same_straight_bytes() {
        let mut colours = Vec::new();
        let mut state = 9u32;
        for _ in 0..4000 {
            let mut next = || {
                state = state.wrapping_mul(1103515245).wrapping_add(12345);
                (state >> 8) as f32 / (1u32 << 24) as f32
            };
            let a = match colours.len() % 4 { 0 => 0.0, 1 => next() * 0.02, _ => next() };
            colours.push([next() * a, next() * a, next() * a, a]);
        }
        colours.push([0.5, 0.5, 0.5, 1.0]);
        colours.push([1.2, -0.1, 0.3, 0.9]);
        let (width, height) = (colours.len(), 1);
        let mut canvas = Canvas::new(std::ptr::null_mut(), width, height, 8, width * 4, Space::Rgb, PREMULTIPLIED_LAST).unwrap();
        let layout = canvas.layout;
        for (pixel, colour) in canvas.memory().chunks_exact_mut(4).zip(&colours) {
            layout.write(pixel, *colour);
        }
        let want = canvas.snapshot().straight_rgba().unwrap();
        assert!(straight_bytes(&colours) == want);
    }

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
