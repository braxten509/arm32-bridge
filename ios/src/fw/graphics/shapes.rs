//! Paths and what is drawn with them: filled and stroked shapes, clipping,
//! radial gradients, plus colours by components, data providers and
//! CGImageCreate.
//!
//! A context's path is kept in device space (points pass through the CTM
//! as they are added, as in Core Graphics); a CGPath keeps the points it
//! was given (through its own optional transform). Curves are flattened
//! into short lines. Shapes are filled by sampling each pixel 4x4 times
//! (nonzero or even-odd); a stroke is the union of a band around each line
//! and a round joint at each corner. Shadows aren't drawn; transparency
//! layers draw straight into the context.

use super::*;

pub const BLEND_CLEAR: i32 = 16;

/// Figures (subpaths): points joined by lines, maybe closed.
#[derive(Clone, Default)]
pub struct Outline {
    figures: Vec<Figure>,
}

#[derive(Clone, Default)]
struct Figure {
    points: Vec<(f64, f64)>,
    closed: bool,
}

impl Outline {
    fn current(&self) -> Option<(f64, f64)> {
        let figure = self.figures.last()?;
        if figure.closed {
            figure.points.first().copied()
        } else {
            figure.points.last().copied()
        }
    }

    pub(super) fn move_to(&mut self, p: (f64, f64)) {
        match self.figures.last_mut() {
            Some(figure) if figure.points.len() == 1 && !figure.closed => figure.points[0] = p,
            _ => self.figures.push(Figure { points: vec![p], closed: false }),
        }
    }

    pub(super) fn line_to(&mut self, p: (f64, f64)) {
        match self.figures.last_mut() {
            Some(figure) if !figure.closed => figure.points.push(p),
            Some(figure) => {
                let start = figure.points[0];
                self.figures.push(Figure { points: vec![start, p], closed: false });
            }
            None => self.move_to(p),
        }
    }

    pub(super) fn close(&mut self) {
        if let Some(figure) = self.figures.last_mut() {
            figure.closed = true;
        }
    }

    fn append(&mut self, other: &Outline, map: &dyn Fn((f64, f64)) -> (f64, f64)) {
        for figure in &other.figures {
            self.figures.push(Figure { points: figure.points.iter().map(|&p| map(p)).collect(), closed: figure.closed });
        }
    }

    /// A circle's arc around (cx, cy) from angle `start` to `end` in user
    /// space (angles grow from x towards y); `clockwise` follows Core
    /// Graphics: angles shrinking. Joined to the current point by a line.
    fn arc(&mut self, center: (f64, f64), radius: f64, start: f64, end: f64, clockwise: bool, map: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let tau = std::f64::consts::TAU;
        let mut sweep = end - start;
        if clockwise {
            while sweep > 0.0 {
                sweep -= tau;
            }
            sweep = sweep.max(-tau);
        } else {
            while sweep < 0.0 {
                sweep += tau;
            }
            sweep = sweep.min(tau);
        }
        let steps = ((sweep.abs() * 16.0).ceil() as usize).clamp(1, 256);
        for i in 0..=steps {
            let angle = start + sweep * i as f64 / steps as f64;
            let p = map((center.0 + radius * angle.cos(), center.1 + radius * angle.sin()));
            if i == 0 && self.current().is_none() {
                self.move_to(p);
            } else {
                self.line_to(p);
            }
        }
    }

    /// The arc of `radius` touching the line from the current point to p1
    /// and the line from p1 to p2 (CGContextAddArcToPoint), in user space:
    /// `unmap` turns the current point back into it.
    fn arc_to_point(&mut self, p1: (f64, f64), p2: (f64, f64), radius: f64, map: &dyn Fn((f64, f64)) -> (f64, f64), unmap: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let Some(p0) = self.current().map(unmap) else {
            self.move_to(map(p1));
            return;
        };
        let unit = |(x, y): (f64, f64)| {
            let length = (x * x + y * y).sqrt();
            (length > 1e-12).then(|| (x / length, y / length))
        };
        let (Some(a), Some(b)) = (unit((p0.0 - p1.0, p0.1 - p1.1)), unit((p2.0 - p1.0, p2.1 - p1.1))) else {
            self.line_to(map(p1));
            return;
        };
        let cos = (a.0 * b.0 + a.1 * b.1).clamp(-1.0, 1.0);
        let angle = cos.acos();
        if radius <= 0.0 || angle < 1e-9 || (std::f64::consts::PI - angle) < 1e-9 {
            self.line_to(map(p1));
            return;
        }
        let along = radius / (angle / 2.0).tan();
        let t1 = (p1.0 + a.0 * along, p1.1 + a.1 * along);
        let t2 = (p1.0 + b.0 * along, p1.1 + b.1 * along);
        let Some(bisector) = unit((a.0 + b.0, a.1 + b.1)) else { return };
        let away = radius / (angle / 2.0).sin();
        let center = (p1.0 + bisector.0 * away, p1.1 + bisector.1 * away);
        let start = (t1.1 - center.1).atan2(t1.0 - center.0);
        let end = (t2.1 - center.1).atan2(t2.0 - center.0);
        // The short way round: clockwise when the turn from a to b is.
        let cross = (p1.0 - p0.0) * (p2.1 - p1.1) - (p1.1 - p0.1) * (p2.0 - p1.0);
        self.line_to(map(t1));
        self.arc(center, radius, start, end, cross < 0.0, map);
    }

    pub(super) fn rect(&mut self, r: Rect, map: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let (x0, y0, x1, y1) = (r.origin.x, r.origin.y, r.origin.x + r.size.width, r.origin.y + r.size.height);
        self.figures.push(Figure { points: [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(map).to_vec(), closed: true });
    }

    fn ellipse(&mut self, r: Rect, map: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let r = standard(r);
        let (rx, ry) = (r.size.width / 2.0, r.size.height / 2.0);
        let (cx, cy) = (r.origin.x + rx, r.origin.y + ry);
        let points = (0..64)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / 64.0;
                map((cx + rx * t.cos(), cy + ry * t.sin()))
            })
            .collect();
        self.figures.push(Figure { points, closed: true });
    }

    /// Edges of the filled shape (every figure closed).
    fn edges(&self) -> Vec<((f64, f64), (f64, f64))> {
        let mut edges = Vec::new();
        for figure in &self.figures {
            let n = figure.points.len();
            for i in 0..n {
                edges.push((figure.points[i], figure.points[(i + 1) % n]));
            }
        }
        edges
    }

    /// Lines drawn by a stroke (closing lines only for closed figures) and the corners between them.
    fn lines(&self) -> (Vec<((f64, f64), (f64, f64))>, Vec<(f64, f64)>) {
        let (mut lines, mut corners) = (Vec::new(), Vec::new());
        for figure in &self.figures {
            let n = figure.points.len();
            let count = if figure.closed { n } else { n.saturating_sub(1) };
            for i in 0..count {
                lines.push((figure.points[i], figure.points[(i + 1) % n]));
            }
            let inner = if figure.closed { 0..n } else { 1..n.saturating_sub(1) };
            corners.extend(inner.map(|i| figure.points[i]));
        }
        (lines, corners)
    }
}

/// Coverage (0..1) of each device pixel (rows from the bottom) by the shape
/// with these edges, 4x4 samples per pixel.
fn coverage(edges: &[((f64, f64), (f64, f64))], even_odd: bool, width: usize, height: usize) -> Vec<f32> {
    let mut mask = vec![0f32; width * height];
    if edges.is_empty() || width == 0 || height == 0 {
        return mask;
    }
    let min_y = edges.iter().map(|e| e.0 .1.min(e.1 .1)).fold(f64::INFINITY, f64::min).max(0.0);
    let max_y = edges.iter().map(|e| e.0 .1.max(e.1 .1)).fold(f64::NEG_INFINITY, f64::max).min(height as f64);
    if !(min_y < max_y) {
        return mask;
    }
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for row in (min_y * 4.0).floor() as usize..((max_y * 4.0).ceil() as usize).min(height * 4) {
        let y = (row as f64 + 0.5) / 4.0;
        crossings.clear();
        for &((x0, y0), (x1, y1)) in edges {
            if (y0 <= y && y < y1) || (y1 <= y && y < y0) {
                crossings.push((x0 + (y - y0) * (x1 - x0) / (y1 - y0), if y1 > y0 { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0;
        for i in 0..crossings.len().saturating_sub(1) {
            winding += crossings[i].1;
            let inside = if even_odd { winding % 2 != 0 } else { winding != 0 };
            if !inside {
                continue;
            }
            // Samples at (s + 0.5) / 4 between the two crossings.
            let first = ((crossings[i].0 * 4.0 - 0.5).ceil().max(0.0)) as usize;
            let end = ((crossings[i + 1].0 * 4.0 - 0.5).ceil().max(0.0) as usize).min(width * 4);
            let line = &mut mask[(row / 4) * width..(row / 4 + 1) * width];
            for sample in first..end {
                line[sample / 4] += 1.0 / 16.0;
            }
        }
    }
    mask
}

/// The band around a line and the joint at a corner, all turning the same way.
fn stroke_edges(lines: &[((f64, f64), (f64, f64))], corners: &[(f64, f64)], half: f64) -> Vec<((f64, f64), (f64, f64))> {
    let mut edges = Vec::new();
    let mut polygon = |points: &[(f64, f64)]| {
        let n = points.len();
        let area: f64 = (0..n).map(|i| points[i].0 * points[(i + 1) % n].1 - points[(i + 1) % n].0 * points[i].1).sum();
        for i in 0..n {
            let (a, b) = (points[i], points[(i + 1) % n]);
            edges.push(if area >= 0.0 { (a, b) } else { (b, a) });
        }
    };
    for &(a, b) in lines {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1e-12 {
            continue;
        }
        let (nx, ny) = (-dy / length * half, dx / length * half);
        polygon(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]);
    }
    for &(cx, cy) in corners {
        let joint: Vec<(f64, f64)> = (0..12)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / 12.0;
                (cx + half * t.cos(), cy + half * t.sin())
            })
            .collect();
        polygon(&joint);
    }
    edges
}

impl Canvas {
    fn to_device(&self) -> impl Fn((f64, f64)) -> (f64, f64) {
        let ctm = self.state.ctm;
        move |(x, y)| geometry::apply(ctm, x, y)
    }

    /// Paints `color` (straight) where `mask` covers.
    fn paint_mask(&mut self, mask: &[f32], color: [f64; 4]) {
        let color = scaled(premultiply(color), self.state.alpha as f32);
        for y in 0..self.height {
            for x in 0..self.width {
                let cover = mask[y * self.width + x];
                if cover > 0.0 {
                    self.blend(x, y, scaled(color, cover.min(1.0)), &Blend::Over);
                }
            }
        }
    }

    pub(super) fn fill_outline(&mut self, outline: &Outline, even_odd: bool) {
        let mask = coverage(&outline.edges(), even_odd, self.width, self.height);
        self.paint_mask(&mask, self.state.fill);
    }

    pub(super) fn stroke_outline(&mut self, outline: &Outline) {
        let ctm = self.state.ctm;
        let half = self.state.line_width / 2.0 * (ctm.a * ctm.d - ctm.b * ctm.c).abs().sqrt();
        let (lines, corners) = outline.lines();
        let mask = coverage(&stroke_edges(&lines, &corners, half), false, self.width, self.height);
        self.paint_mask(&mask, self.state.stroke);
    }

    pub(super) fn clip_to(&mut self, outline: &Outline, even_odd: bool) {
        let mut mask = coverage(&outline.edges(), even_odd, self.width, self.height);
        for value in mask.iter_mut() {
            *value = value.min(1.0);
        }
        if let Some(old) = &self.state.clip {
            for (value, old) in mask.iter_mut().zip(old.iter()) {
                *value *= old;
            }
        }
        self.state.clip = Some(Arc::new(mask));
    }

    fn draw_radial_gradient(&mut self, gradient: &Gradient, c0: Point, r0: f64, c1: Point, r1: f64, options: u32) {
        let Some(inverse) = geometry::invert(self.state.ctm) else { return };
        let (cdx, cdy, dr) = (c1.x - c0.x, c1.y - c0.y, r1 - r0);
        let a = cdx * cdx + cdy * cdy - dr * dr;
        let alpha = self.state.alpha as f32;
        for y in 0..self.height {
            for x in 0..self.width {
                let (ux, uy) = geometry::apply(inverse, x as f64 + 0.5, y as f64 + 0.5);
                let (px, py) = (ux - c0.x, uy - c0.y);
                let b = px * cdx + py * cdy + r0 * dr;
                let c = px * px + py * py - r0 * r0;
                // The biggest t whose circle passes through the point with a radius >= 0.
                let t = if a.abs() < 1e-12 {
                    if b.abs() < 1e-12 {
                        continue;
                    }
                    c / (2.0 * b)
                } else {
                    let disc = b * b - a * c;
                    if disc < 0.0 {
                        continue;
                    }
                    let root = disc.sqrt();
                    let (big, small) = ((b + root) / a, (b - root) / a);
                    let (big, small) = if big >= small { (big, small) } else { (small, big) };
                    if r0 + big * dr >= 0.0 {
                        big
                    } else if r0 + small * dr >= 0.0 {
                        small
                    } else {
                        continue;
                    }
                };
                let t = if t < 0.0 {
                    if options & 1 == 0 {
                        continue;
                    }
                    0.0
                } else if t > 1.0 {
                    if options & 2 == 0 {
                        continue;
                    }
                    1.0
                } else {
                    t
                };
                self.blend(x, y, scaled(premultiply(gradient.at(t)), alpha), &Blend::Over);
            }
        }
    }
}

/// The context's path, edited in place.
fn with_path(function: &str, context: Id, work: impl FnOnce(&mut Canvas, &mut Outline)) {
    if let Some(mut c) = canvas(function, context) {
        let mut path = std::mem::take(&mut c.path);
        work(&mut c, &mut path);
        c.path = path;
    }
}

extern "C" fn begin_path(context: Id) {
    with_path("CGContextBeginPath", context, |_, path| *path = Outline::default());
}
extern "C" fn move_to_point(context: Id, x: f64, y: f64) {
    with_path("CGContextMoveToPoint", context, |c, path| path.move_to(c.to_device()((x, y))));
}
extern "C" fn add_line_to_point(context: Id, x: f64, y: f64) {
    with_path("CGContextAddLineToPoint", context, |c, path| path.line_to(c.to_device()((x, y))));
}
extern "C" fn close_path(context: Id) {
    with_path("CGContextClosePath", context, |_, path| path.close());
}
extern "C" fn add_rect(context: Id, rect: Rect) {
    with_path("CGContextAddRect", context, |c, path| path.rect(rect, &c.to_device()));
}
extern "C" fn add_ellipse_in_rect(context: Id, rect: Rect) {
    with_path("CGContextAddEllipseInRect", context, |c, path| path.ellipse(rect, &c.to_device()));
}
extern "C" fn add_arc(context: Id, x: f64, y: f64, radius: f64, start: f64, end: f64, clockwise: i32) {
    with_path("CGContextAddArc", context, |c, path| path.arc((x, y), radius, start, end, clockwise != 0, &c.to_device()));
}
extern "C" fn add_arc_to_point(context: Id, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64) {
    with_path("CGContextAddArcToPoint", context, |c, path| {
        let Some(inverse) = geometry::invert(c.state.ctm) else { return };
        path.arc_to_point((x1, y1), (x2, y2), radius, &c.to_device(), &|(x, y)| geometry::apply(inverse, x, y));
    });
}
extern "C" fn add_path(context: Id, other: Id) {
    let Some(other) = path_state(other) else {
        complain("CGContextAddPath", "path", other);
        return;
    };
    let other = other.lock().unwrap_or_else(|e| e.into_inner()).clone();
    with_path("CGContextAddPath", context, |c, path| path.append(&other, &c.to_device()));
}

const PATH_FILL: i32 = 0;
const PATH_EO_FILL: i32 = 1;
const PATH_STROKE: i32 = 2;
const PATH_FILL_STROKE: i32 = 3;
const PATH_EO_FILL_STROKE: i32 = 4;

/// Draws the context's path and starts a new one (CGContextDrawPath).
extern "C" fn draw_path(context: Id, mode: i32) {
    with_path("CGContextDrawPath", context, |c, path| {
        if matches!(mode, PATH_FILL | PATH_FILL_STROKE) {
            c.fill_outline(path, false);
        }
        if matches!(mode, PATH_EO_FILL | PATH_EO_FILL_STROKE) {
            c.fill_outline(path, true);
        }
        if matches!(mode, PATH_STROKE | PATH_FILL_STROKE | PATH_EO_FILL_STROKE) {
            c.stroke_outline(path);
        }
        *path = Outline::default();
    });
}
extern "C" fn fill_path(context: Id) {
    draw_path(context, PATH_FILL);
}
extern "C" fn eo_fill_path(context: Id) {
    draw_path(context, PATH_EO_FILL);
}
extern "C" fn stroke_path(context: Id) {
    draw_path(context, PATH_STROKE);
}
extern "C" fn clip(context: Id) {
    with_path("CGContextClip", context, |c, path| {
        c.clip_to(path, false);
        *path = Outline::default();
    });
}
extern "C" fn eo_clip(context: Id) {
    with_path("CGContextEOClip", context, |c, path| {
        c.clip_to(path, true);
        *path = Outline::default();
    });
}
extern "C" fn clip_to_rect(context: Id, rect: Rect) {
    with_path("CGContextClipToRect", context, |c, _| {
        let mut outline = Outline::default();
        outline.rect(rect, &c.to_device());
        c.clip_to(&outline, false);
    });
}

/// Draws a shape of its own without touching the context's path.
fn draw_shape(function: &str, context: Id, mode: i32, build: impl FnOnce(&mut Outline, &dyn Fn((f64, f64)) -> (f64, f64))) {
    if let Some(mut c) = canvas(function, context) {
        let mut outline = Outline::default();
        build(&mut outline, &c.to_device());
        if mode == PATH_FILL {
            c.fill_outline(&outline, false);
        } else {
            c.stroke_outline(&outline);
        }
    }
}
extern "C" fn fill_ellipse_in_rect(context: Id, rect: Rect) {
    draw_shape("CGContextFillEllipseInRect", context, PATH_FILL, |o, map| o.ellipse(rect, map));
}
extern "C" fn stroke_ellipse_in_rect(context: Id, rect: Rect) {
    draw_shape("CGContextStrokeEllipseInRect", context, PATH_STROKE, |o, map| o.ellipse(rect, map));
}
extern "C" fn stroke_rect(context: Id, rect: Rect) {
    draw_shape("CGContextStrokeRect", context, PATH_STROKE, |o, map| o.rect(rect, map));
}
/// Pairs of points, each pair its own line.
extern "C" fn stroke_line_segments(context: Id, points: *const Point, count: usize) {
    if points.is_null() {
        return;
    }
    let points = unsafe { std::slice::from_raw_parts(points, count) }.to_vec();
    draw_shape("CGContextStrokeLineSegments", context, PATH_STROKE, |o, map| {
        for pair in points.chunks_exact(2) {
            o.move_to(map((pair[0].x, pair[0].y)));
            o.line_to(map((pair[1].x, pair[1].y)));
        }
    });
}

extern "C" fn draw_radial_gradient(context: Id, gradient: Id, start: Point, start_radius: f64, end: Point, end_radius: f64, options: u32) {
    let Some(g) = state::<Gradient>(gradient, classes().gradient) else {
        complain("CGContextDrawRadialGradient", "gradient", gradient);
        return;
    };
    if let Some(mut c) = canvas("CGContextDrawRadialGradient", context) {
        c.draw_radial_gradient(g, start, start_radius, end, end_radius, options);
    }
}

// ----- colours and drawing settings -----

fn components(space: Space, values: *const f64) -> Option<[f64; 4]> {
    if values.is_null() || space == Space::None {
        return None;
    }
    let values = unsafe { std::slice::from_raw_parts(values, space_components(space) + 1) };
    Some(to_rgba(space, values))
}

extern "C" fn set_fill_color(context: Id, values: *const f64) {
    if let Some(mut c) = canvas("CGContextSetFillColor", context) {
        if let Some(rgba) = components(c.state.fill_space, values) {
            c.state.fill = rgba;
        }
    }
}
extern "C" fn set_stroke_color(context: Id, values: *const f64) {
    if let Some(mut c) = canvas("CGContextSetStrokeColor", context) {
        if let Some(rgba) = components(c.state.stroke_space, values) {
            c.state.stroke = rgba;
        }
    }
}
extern "C" fn set_fill_color_space(context: Id, space: Id) {
    if let Some(mut c) = canvas("CGContextSetFillColorSpace", context) {
        c.state.fill_space = space_of(space);
        c.state.fill = [0.0, 0.0, 0.0, 1.0];
    }
}
extern "C" fn set_stroke_color_space(context: Id, space: Id) {
    if let Some(mut c) = canvas("CGContextSetStrokeColorSpace", context) {
        c.state.stroke_space = space_of(space);
        c.state.stroke = [0.0, 0.0, 0.0, 1.0];
    }
}
extern "C" fn set_gray_fill_color(context: Id, gray: f64, alpha: f64) {
    if let Some(mut c) = canvas("CGContextSetGrayFillColor", context) {
        c.state.fill = [gray, gray, gray, alpha];
    }
}
extern "C" fn set_gray_stroke_color(context: Id, gray: f64, alpha: f64) {
    if let Some(mut c) = canvas("CGContextSetGrayStrokeColor", context) {
        c.state.stroke = [gray, gray, gray, alpha];
    }
}
extern "C" fn set_rgb_stroke_color(context: Id, r: f64, g: f64, b: f64, a: f64) {
    if let Some(mut c) = canvas("CGContextSetRGBStrokeColor", context) {
        c.state.stroke = [r, g, b, a];
    }
}
extern "C" fn set_stroke_color_with_color(context: Id, color: Id) {
    let Some(rgba) = color_rgba(color) else { return };
    if let Some(mut c) = canvas("CGContextSetStrokeColorWithColor", context) {
        c.state.stroke = rgba;
    }
}
extern "C" fn set_line_width(context: Id, width: f64) {
    if let Some(mut c) = canvas("CGContextSetLineWidth", context) {
        c.state.line_width = width.max(0.0);
    }
}
extern "C" fn set_blend_mode(context: Id, mode: i32) {
    if mode != 0 && mode != BLEND_CLEAR {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| crate::log!("CGContextSetBlendMode: mode {mode} draws as normal here"));
    }
    if let Some(mut c) = canvas("CGContextSetBlendMode", context) {
        c.state.blend_mode = mode;
    }
}
fn shadows_not_drawn() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| crate::log!("CGContextSetShadow: shadows are not drawn here"));
}
extern "C" fn set_shadow(_context: Id, _offset: Size, _blur: f64) {
    shadows_not_drawn();
}
extern "C" fn set_shadow_with_color(_context: Id, _offset: Size, _blur: f64, _color: Id) {
    shadows_not_drawn();
}
extern "C" fn set_text_drawing_mode(context: Id, mode: i32) {
    if let Some(mut c) = canvas("CGContextSetTextDrawingMode", context) {
        c.state.text_mode = mode;
    }
}
/// A transparency layer draws straight into the context (same result
/// without shadows or group alpha); its settings end with it.
extern "C" fn begin_transparency_layer(context: Id, _auxiliary: Id) {
    if let Some(mut c) = canvas("CGContextBeginTransparencyLayer", context) {
        let state = c.state.clone();
        c.saved.push(state);
    }
}
extern "C" fn begin_transparency_layer_with_rect(context: Id, _rect: Rect, auxiliary: Id) {
    begin_transparency_layer(context, auxiliary);
}
extern "C" fn end_transparency_layer(context: Id) {
    if let Some(mut c) = canvas("CGContextEndTransparencyLayer", context) {
        if let Some(saved) = c.saved.pop() {
            c.state = saved;
        }
    }
}

/// Colours are RGBA here (UIColor's layout): four components.
extern "C" fn color_get_number_of_components(color: Id) -> usize {
    if color == 0 { 0 } else { 4 }
}
extern "C" fn color_get_components(color: Id) -> *const f64 {
    if color == 0 { std::ptr::null() } else { (color + 8) as *const f64 }
}
extern "C" fn color_get_alpha(color: Id) -> f64 {
    color_rgba(color).map_or(0.0, |c| c[3])
}

// ----- CGPath -----

fn path_state(path: Id) -> Option<&'static Mutex<Outline>> {
    state::<Mutex<Outline>>(path, classes().path)
}

/// Paths aren't counted (fw/counted.rs keeps its classes few): a released
/// path stays usable, which is harmless as nothing here keeps one.
extern "C" fn path_create_mutable() -> Id {
    wrap(classes().path, Mutex::new(Outline::default()))
}
extern "C" fn path_retain(path: Id) -> Id {
    path
}
extern "C" fn path_release(_path: Id) {}

/// Edits a CGPath; points pass through its optional transform `m`.
fn edit_path(function: &str, path: Id, m: *const Transform, work: impl FnOnce(&mut Outline, &dyn Fn((f64, f64)) -> (f64, f64))) {
    let Some(p) = path_state(path) else {
        complain(function, "path", path);
        return;
    };
    let t = if m.is_null() { IDENTITY } else { unsafe { *m } };
    work(&mut p.lock().unwrap_or_else(|e| e.into_inner()), &|(x, y)| geometry::apply(t, x, y));
}

extern "C" fn path_move_to_point(path: Id, m: *const Transform, x: f64, y: f64) {
    edit_path("CGPathMoveToPoint", path, m, |o, map| o.move_to(map((x, y))));
}
extern "C" fn path_add_line_to_point(path: Id, m: *const Transform, x: f64, y: f64) {
    edit_path("CGPathAddLineToPoint", path, m, |o, map| o.line_to(map((x, y))));
}
extern "C" fn path_add_arc_to_point(path: Id, m: *const Transform, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64) {
    let inverse = if m.is_null() { Some(IDENTITY) } else { geometry::invert(unsafe { *m }) };
    let Some(inverse) = inverse else { return };
    edit_path("CGPathAddArcToPoint", path, m, |o, map| {
        o.arc_to_point((x1, y1), (x2, y2), radius, map, &|(x, y)| geometry::apply(inverse, x, y))
    });
}
extern "C" fn path_add_arc(path: Id, m: *const Transform, x: f64, y: f64, radius: f64, start: f64, end: f64, clockwise: bool) {
    edit_path("CGPathAddArc", path, m, |o, map| o.arc((x, y), radius, start, end, clockwise, map));
}
extern "C" fn path_add_rect(path: Id, m: *const Transform, rect: Rect) {
    edit_path("CGPathAddRect", path, m, |o, map| o.rect(rect, map));
}
extern "C" fn path_add_ellipse_in_rect(path: Id, m: *const Transform, rect: Rect) {
    edit_path("CGPathAddEllipseInRect", path, m, |o, map| o.ellipse(rect, map));
}
extern "C" fn path_close_subpath(path: Id) {
    edit_path("CGPathCloseSubpath", path, std::ptr::null(), |o, _| o.close());
}

// ----- data providers and CGImageCreate -----

/// Bytes the game owns, given back (`release`: info, data, size) when the
/// last reference goes, or bytes this layer holds (`held`). Providers count
/// with the other CG objects (fw/counted.rs): CFRetain/CFRelease,
/// retain/release messages, ARC calls and autorelease pools all reach the
/// same count. CGImageCreate copies the bytes, so pictures don't hold a
/// reference.
struct Provider {
    info: usize,
    data: usize,
    size: usize,
    release: usize,
    held: RwLock<Option<Held>>,
    /// The last reference went: the game's bytes may be gone too.
    released: std::sync::atomic::AtomicBool,
}

/// What a provider holds itself.
enum Held {
    /// A CFData's bytes (CGDataProviderCreateWithCFData).
    Bytes(Vec<u8>),
    /// A picture's pixels (CGImageGetDataProvider).
    Picture(Arc<Bitmap>),
}

impl Provider {
    /// Runs `work` on the provider's bytes (none once its last reference went).
    fn with_bytes<R>(&self, work: impl FnOnce(&[u8]) -> R) -> R {
        let held = self.held.read().unwrap_or_else(|e| e.into_inner());
        match held.as_ref() {
            Some(Held::Bytes(bytes)) => work(bytes),
            Some(Held::Picture(bitmap)) => work(&bitmap.data),
            None if self.data != 0 && !self.released.load(std::sync::atomic::Ordering::Acquire) => {
                work(unsafe { std::slice::from_raw_parts(self.data as *const u8, self.size) })
            }
            None => work(&[]),
        }
    }
}

fn provider_state(provider: Id) -> Option<&'static Provider> {
    state::<Provider>(provider, classes().provider)
}

/// The clean-up counted runs at the last reference.
fn free_provider(provider: Id) {
    let Some(p) = provider_state(provider) else { return };
    p.released.store(true, std::sync::atomic::Ordering::Release);
    *p.held.write().unwrap_or_else(|e| e.into_inner()) = None;
    if p.release != 0 {
        let release: extern "C" fn(usize, usize, usize) = unsafe { std::mem::transmute(p.release) };
        release(p.info, p.data, p.size);
    }
}

fn make_provider(provider: Provider) -> Id {
    let provider = wrap(classes().provider, provider);
    counted::track(provider, free_provider);
    provider
}

fn holding(held: Held) -> Provider {
    Provider { info: 0, data: 0, size: 0, release: 0, held: RwLock::new(Some(held)), released: Default::default() }
}

extern "C" fn data_provider_create_with_data(info: usize, data: *const u8, size: usize, release: usize) -> Id {
    make_provider(Provider { info, data: data as usize, size, release, held: RwLock::new(None), released: Default::default() })
}

/// A copy of the CFData's bytes: Apple's provider keeps the CFData itself,
/// which only differs if the game changes a mutable one afterwards.
extern "C" fn data_provider_create_with_cf_data(bytes: Id) -> Id {
    if !data::is_data(bytes) {
        complain("CGDataProviderCreateWithCFData", "data", bytes);
        return 0;
    }
    make_provider(holding(Held::Bytes(data::bytes(bytes))))
}

/// A provider of a picture's pixels, sharing them (CGImageGetDataProvider).
pub(super) fn picture_provider(bitmap: Arc<Bitmap>) -> Id {
    make_provider(holding(Held::Picture(bitmap)))
}

/// A new CFData of the provider's bytes.
extern "C" fn data_provider_copy_data(provider: Id) -> Id {
    match provider_state(provider) {
        Some(p) => p.with_bytes(data::make),
        None => {
            complain("CGDataProviderCopyData", "data provider", provider);
            0
        }
    }
}
/// +1 on a data provider; false for anything else.
pub fn retain_provider(provider: Id) -> bool {
    if provider_state(provider).is_none() {
        return false;
    }
    counted::retain(provider);
    true
}
/// -1 on a data provider, running the release callback at zero; false for
/// anything else.
pub fn release_provider(provider: Id) -> bool {
    if provider_state(provider).is_none() {
        return false;
    }
    counted::release(provider);
    true
}
extern "C" fn data_provider_retain(provider: Id) -> Id {
    counted::retain(provider);
    provider
}
extern "C" fn data_provider_release(provider: Id) {
    counted::release(provider);
}
extern "C" fn provider_retain_method(this: Id, _: Sel) -> Id {
    counted::retain(this)
}
extern "C" fn provider_release_method(this: Id, _: Sel) {
    counted::release(this)
}
extern "C" fn provider_retain_count_method(this: Id, _: Sel) -> usize {
    counted::count(this).unwrap_or(0)
}

/// A picture from a provider's bytes (copied: the picture never changes).
#[allow(clippy::too_many_arguments)]
extern "C" fn image_create(width: usize, height: usize, bits_per_component: usize, bits_per_pixel: usize, bytes_per_row: usize, space: Id, info: u32, provider: Id, _decode: *const f64, _interpolate: bool, _intent: i32) -> Id {
    let Some(p) = provider_state(provider) else {
        complain("CGImageCreate", "data provider", provider);
        return 0;
    };
    let bitmap = Bitmap { width, height, bits_per_pixel, bytes_per_row, info, space: space_of(space), data: Vec::new() };
    p.with_bytes(|bytes| {
        let needed = bytes_per_row.checked_mul(height).filter(|&n| n <= bytes.len() && n <= MOST_PIXEL_BYTES);
        let (Some(needed), true, Some(_)) = (needed, bits_per_component == 8, bitmap.layout()) else {
            crate::log!("CGImageCreate: {width}x{height}, {bits_per_component} bits/component, {bits_per_pixel} bits/pixel, {bytes_per_row} bytes/row, info {info:#x} from {} bytes: not supported", bytes.len());
            return 0;
        };
        make_image(Bitmap { data: bytes[..needed].to_vec(), ..bitmap })
    })
}

/// CGImageMaskCreate: samples of 1, 2, 4 or 8 bits (rows from the top, the
/// first pixel in a byte's top bits), through the decode range [Dmin Dmax]
/// (default [0 1]); kept as 8-bit samples, where 1 hides and 0 paints.
#[allow(clippy::too_many_arguments)]
extern "C" fn image_mask_create(width: usize, height: usize, bits_per_component: usize, bits_per_pixel: usize, bytes_per_row: usize, provider: Id, decode: *const f64, _interpolate: bool) -> Id {
    let Some(p) = provider_state(provider) else {
        complain("CGImageMaskCreate", "data provider", provider);
        return 0;
    };
    let (low, high) = if decode.is_null() { (0.0, 1.0) } else { unsafe { (*decode, *decode.add(1)) } };
    let bits = bits_per_component;
    let usable = matches!(bits, 1 | 2 | 4 | 8) && bits_per_pixel == bits && width > 0 && height > 0;
    let least = width.saturating_mul(bits).div_ceil(8);
    let pixels = width.checked_mul(height).filter(|&n| n <= MOST_PIXEL_BYTES);
    let needed = bytes_per_row.checked_mul(height).filter(|&n| usable && pixels.is_some() && bytes_per_row >= least && n <= MOST_PIXEL_BYTES);
    p.with_bytes(|bytes| {
        let Some(_) = needed.filter(|&n| n <= bytes.len()) else {
            crate::log!("CGImageMaskCreate: {width}x{height}, {bits} bits/component, {bits_per_pixel} bits/pixel, {bytes_per_row} bytes/row from {} bytes: not supported", bytes.len());
            return 0;
        };
        let most = ((1u32 << bits) - 1) as f64;
        let mut data = Vec::with_capacity(width * height);
        for row in 0..height {
            for x in 0..width {
                let bit = x * bits;
                let byte = bytes[row * bytes_per_row + bit / 8];
                let sample = (byte >> (8 - bits - bit % 8)) & ((1u16 << bits) - 1) as u8;
                let value = low + sample as f64 / most * (high - low);
                data.push(to_byte(value as f32));
            }
        }
        let bitmap = Bitmap { width, height, bits_per_pixel: 8, bytes_per_row: width, info: ALPHA_NONE, space: Space::Gray, data };
        make_mask(bitmap)
    })
}

pub fn register(r: &mut Registry) {
    methods(
        classes().provider,
        &[
            ("retain", provider_retain_method as usize),
            ("release", provider_release_method as usize),
            ("retainCount", provider_retain_count_method as usize),
        ],
    );
    for (name, function) in [
        ("CGContextBeginPath", begin_path as usize),
        ("CGContextMoveToPoint", move_to_point as usize),
        ("CGContextAddLineToPoint", add_line_to_point as usize),
        ("CGContextClosePath", close_path as usize),
        ("CGContextAddRect", add_rect as usize),
        ("CGContextAddEllipseInRect", add_ellipse_in_rect as usize),
        ("CGContextAddArc", add_arc as usize),
        ("CGContextAddArcToPoint", add_arc_to_point as usize),
        ("CGContextAddPath", add_path as usize),
        ("CGContextDrawPath", draw_path as usize),
        ("CGContextFillPath", fill_path as usize),
        ("CGContextEOFillPath", eo_fill_path as usize),
        ("CGContextStrokePath", stroke_path as usize),
        ("CGContextClip", clip as usize),
        ("CGContextEOClip", eo_clip as usize),
        ("CGContextClipToRect", clip_to_rect as usize),
        ("CGContextFillEllipseInRect", fill_ellipse_in_rect as usize),
        ("CGContextStrokeEllipseInRect", stroke_ellipse_in_rect as usize),
        ("CGContextStrokeRect", stroke_rect as usize),
        ("CGContextStrokeLineSegments", stroke_line_segments as usize),
        ("CGContextDrawRadialGradient", draw_radial_gradient as usize),
        ("CGContextSetFillColor", set_fill_color as usize),
        ("CGContextSetStrokeColor", set_stroke_color as usize),
        ("CGContextSetFillColorSpace", set_fill_color_space as usize),
        ("CGContextSetStrokeColorSpace", set_stroke_color_space as usize),
        ("CGContextSetGrayFillColor", set_gray_fill_color as usize),
        ("CGContextSetGrayStrokeColor", set_gray_stroke_color as usize),
        ("CGContextSetRGBStrokeColor", set_rgb_stroke_color as usize),
        ("CGContextSetStrokeColorWithColor", set_stroke_color_with_color as usize),
        ("CGContextSetLineWidth", set_line_width as usize),
        ("CGContextSetBlendMode", set_blend_mode as usize),
        ("CGContextSetShadow", set_shadow as usize),
        ("CGContextSetShadowWithColor", set_shadow_with_color as usize),
        ("CGContextSetTextDrawingMode", set_text_drawing_mode as usize),
        ("CGContextBeginTransparencyLayer", begin_transparency_layer as usize),
        ("CGContextBeginTransparencyLayerWithRect", begin_transparency_layer_with_rect as usize),
        ("CGContextEndTransparencyLayer", end_transparency_layer as usize),
        ("CGColorGetNumberOfComponents", color_get_number_of_components as usize),
        ("CGColorGetComponents", color_get_components as usize),
        ("CGColorGetAlpha", color_get_alpha as usize),
        ("CGPathCreateMutable", path_create_mutable as usize),
        ("CGPathRetain", path_retain as usize),
        ("CGPathRelease", path_release as usize),
        ("CGPathMoveToPoint", path_move_to_point as usize),
        ("CGPathAddLineToPoint", path_add_line_to_point as usize),
        ("CGPathAddArcToPoint", path_add_arc_to_point as usize),
        ("CGPathAddArc", path_add_arc as usize),
        ("CGPathAddRect", path_add_rect as usize),
        ("CGPathAddEllipseInRect", path_add_ellipse_in_rect as usize),
        ("CGPathCloseSubpath", path_close_subpath as usize),
        ("CGDataProviderCreateWithData", data_provider_create_with_data as usize),
        ("CGDataProviderRetain", data_provider_retain as usize),
        ("CGDataProviderRelease", data_provider_release as usize),
        ("CGDataProviderCreateWithCFData", data_provider_create_with_cf_data as usize),
        ("CGDataProviderCopyData", data_provider_copy_data as usize),
        ("CGImageCreate", image_create as usize),
        ("CGImageMaskCreate", image_mask_create as usize),
    ] {
        r.function(name, function);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filled_and_stroked_shapes_cover_the_right_pixels() {
        // A unit-free square from (2, 2) to (6, 6): inside fully, outside not at all.
        let mut square = Outline::default();
        square.rect(Rect::new(2.0, 2.0, 4.0, 4.0), &|p| p);
        let mask = coverage(&square.edges(), false, 8, 8);
        assert_eq!(mask[3 * 8 + 3], 1.0);
        assert_eq!(mask[8 + 1], 0.0);
        assert_eq!(mask.iter().sum::<f32>(), 16.0);
        // A horizontal line 2 wide along y = 4 from x = 1 to 7 (butt ends).
        let mut line = Outline::default();
        line.move_to((1.0, 4.0));
        line.line_to((7.0, 4.0));
        let (lines, corners) = line.lines();
        let mask = coverage(&stroke_edges(&lines, &corners, 1.0), false, 8, 8);
        assert_eq!(mask.iter().sum::<f32>(), 12.0);
        assert_eq!(mask[3 * 8 + 4], 1.0);
        // A clockwise arc a quarter turn round: from angle 0 down to -90 degrees.
        let mut arc = Outline::default();
        arc.arc((0.0, 0.0), 1.0, 0.0, -std::f64::consts::FRAC_PI_2, true, &|p| p);
        let end = arc.current().unwrap();
        assert!(end.0.abs() < 1e-9 && (end.1 + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_closed_context_lets_go_of_its_clip_saved_states_and_path() {
        let mut c = Canvas::new(std::ptr::null_mut(), 64, 64, 8, 0, Space::Rgb, PREMULTIPLIED_LAST).unwrap();
        c.state.clip = Some(Arc::new(vec![1.0; 64 * 64]));
        c.saved.push(c.state.clone());
        c.path.move_to((1.0, 1.0));
        c.close();
        assert!(c.state.clip.is_none());
        assert!(c.saved.is_empty());
        assert!(c.path.figures.is_empty());
    }

    #[test]
    fn a_data_provider_gives_its_bytes_back_on_the_last_release_only() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static RELEASED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        extern "C" fn on_release(_info: usize, _data: usize, _size: usize) {
            RELEASED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let bytes = [255u8; 16];
        let provider = data_provider_create_with_data(0, bytes.as_ptr(), 16, on_release as usize);
        assert_eq!(counted::count(provider), Some(1));
        // CFRetain, an ARC retain and a release message share the one count.
        data_provider_retain(provider);
        counted::retain(provider);
        assert_eq!(counted::count(provider), Some(3));
        data_provider_release(provider);
        runtime::send(provider, "release", &[]);
        assert_eq!(counted::count(provider), Some(1));
        let picture = image_create(2, 2, 8, 32, 8, space_create_rgb(), PREMULTIPLIED_LAST, provider, std::ptr::null(), false, 0);
        assert_ne!(picture, 0);
        data_provider_release(provider);
        assert_eq!(RELEASED.load(std::sync::atomic::Ordering::Relaxed), 1);
        data_provider_release(provider);
        assert_eq!(RELEASED.load(std::sync::atomic::Ordering::Relaxed), 1, "once only");
        // An autorelease pool releases it like any other counted object.
        RELEASED.store(0, std::sync::atomic::Ordering::Relaxed);
        let pooled = data_provider_create_with_data(0, bytes.as_ptr(), 16, on_release as usize);
        let mark = counted::pool_mark();
        counted::autorelease(pooled);
        counted::pool_drain(mark);
        assert_eq!(RELEASED.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
