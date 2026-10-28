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

pub const BLEND_NORMAL: i32 = 0;
pub const BLEND_CLEAR: i32 = 16;
pub const BLEND_COPY: i32 = 17;

/// `s` drawn on `d` (both premultiplied RGBA) with CGBlendMode `mode`: the
/// PDF blend formulas for the separable (1-11) and non-separable (12-15)
/// modes, composited source-over, and Quartz's documented Porter-Duff ones
/// (16-27). An unknown mode is normal.
pub(super) fn composite(mode: i32, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let porter_duff = |fs: f32, fd: f32| [0, 1, 2, 3].map(|i| s[i] * fs + d[i] * fd);
    match mode {
        16 => [0.0; 4],
        17 => s,
        18 => porter_duff(da, 0.0),
        19 => porter_duff(1.0 - da, 0.0),
        20 => porter_duff(da, 1.0 - sa),
        21 => porter_duff(1.0 - da, 1.0),
        22 => porter_duff(0.0, sa),
        23 => porter_duff(0.0, 1.0 - sa),
        24 => porter_duff(1.0 - da, sa),
        25 => porter_duff(1.0 - da, 1.0 - sa),
        // Plus darker: MAX(0, 1 - ((1 - D) + (1 - S))); plus lighter: MIN(1, S + D).
        26 => [0, 1, 2, 3].map(|i| (s[i] + d[i] - 1.0).max(0.0)),
        27 => [0, 1, 2, 3].map(|i| (s[i] + d[i]).min(1.0)),
        1..=15 => {
            let straight = |c: [f32; 4]| if c[3] > 0.0 { [c[0] / c[3], c[1] / c[3], c[2] / c[3]] } else { [0.0; 3] };
            let mixed = blend_colours(mode, straight(d), straight(s));
            let rgb = [0, 1, 2].map(|i| (1.0 - da) * s[i] + (1.0 - sa) * d[i] + sa * da * mixed[i]);
            [rgb[0], rgb[1], rgb[2], sa + da - sa * da]
        }
        _ => over(s, d),
    }
}

/// Whether a clear source pixel changes what is under it in `mode` (copy,
/// source in/out, destination in/atop, plus darker): then clear parts of a
/// drawing count, not just its coloured ones.
pub(super) fn clear_source_counts(mode: i32) -> bool {
    matches!(mode, 17 | 18 | 19 | 22 | 24 | 26)
}

/// B(Cb, Cs): the backdrop and source colours (straight) mixed by a
/// separable or non-separable blend mode (PDF 32000 11.3.5).
fn blend_colours(mode: i32, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    fn hard_light(b: f32, s: f32) -> f32 {
        if s <= 0.5 { b * 2.0 * s } else { let t = 2.0 * s - 1.0; b + t - b * t }
    }
    fn soft_light(b: f32, s: f32) -> f32 {
        if s <= 0.5 {
            b - (1.0 - 2.0 * s) * b * (1.0 - b)
        } else {
            let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
            b + (2.0 * s - 1.0) * (d - b)
        }
    }
    let lum = |c: [f32; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let set_lum = |c: [f32; 3], l: f32| {
        let d = l - lum(c);
        let c = c.map(|v| v + d);
        let (l, n, x) = (lum(c), c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
        c.map(|v| if n < 0.0 { l + (v - l) * l / (l - n) } else if x > 1.0 { l + (v - l) * (1.0 - l) / (x - l) } else { v })
    };
    let sat = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    let set_sat = |c: [f32; 3], s: f32| {
        let mut order = [0, 1, 2];
        order.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
        let [min, mid, max] = order;
        let mut out = [0.0; 3];
        if c[max] > c[min] {
            out[mid] = (c[mid] - c[min]) * s / (c[max] - c[min]);
            out[max] = s;
        }
        out
    };
    let each = |f: fn(f32, f32) -> f32| [0, 1, 2].map(|i| f(cb[i], cs[i]));
    match mode {
        1 => each(|b, s| b * s),
        2 => each(|b, s| b + s - b * s),
        3 => each(|b, s| hard_light(s, b)),
        4 => each(f32::min),
        5 => each(f32::max),
        6 => each(|b, s| if b <= 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        7 => each(|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        8 => each(soft_light),
        9 => each(hard_light),
        10 => each(|b, s| (b - s).abs()),
        11 => each(|b, s| b + s - 2.0 * b * s),
        12 => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        13 => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        14 => set_lum(cs, lum(cb)),
        _ => set_lum(cb, lum(cs)),
    }
}

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
    /// Moves every point by `by` (device pixels; a transparency layer's corner).
    pub(super) fn shift(&mut self, (dx, dy): (f64, f64)) {
        for point in self.figures.iter_mut().flat_map(|f| f.points.iter_mut()) {
            (point.0, point.1) = (point.0 + dx, point.1 + dy);
        }
    }

    /// A copy moved by `by`.
    pub(super) fn moved(&self, by: (f64, f64)) -> Outline {
        let mut moved = self.clone();
        if by != (0.0, 0.0) {
            moved.shift(by);
        }
        moved
    }

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
    /// `unmap` turns the current point back into it. With no current point
    /// it adds nothing (Quartz complains and ignores it).
    fn arc_to_point(&mut self, p1: (f64, f64), p2: (f64, f64), radius: f64, map: &dyn Fn((f64, f64)) -> (f64, f64), unmap: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let Some(p0) = self.current().map(unmap) else {
            crate::log!("AddArcToPoint: no current point.");
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

}

/// The edges of a plain axis-aligned rectangle (two vertical, two horizontal
/// lines), as its bounds in device space; None for any other shape.
fn axis_aligned_rect(edges: &[((f64, f64), (f64, f64))]) -> Option<(f64, f64, f64, f64)> {
    if edges.len() != 4 {
        return None;
    }
    let (mut xs, mut ys) = ([f64::NAN; 2], [f64::NAN; 2]);
    let (mut nx, mut ny) = (0usize, 0usize);
    for &((x0, y0), (x1, y1)) in edges {
        if x0 == x1 && y0 != y1 {
            if nx == 2 {
                return None;
            }
            xs[nx] = x0;
            nx += 1;
        } else if y0 == y1 && x0 != x1 {
            if ny == 2 {
                return None;
            }
            ys[ny] = y0;
            ny += 1;
        } else {
            return None;
        }
    }
    if nx != 2 || ny != 2 || xs[0] == xs[1] || ys[0] == ys[1] {
        return None;
    }
    Some((xs[0].min(xs[1]), ys[0].min(ys[1]), xs[0].max(xs[1]), ys[0].max(ys[1])))
}

/// Coverage of an axis-aligned rectangle: whole pixels inside are 1, outside
/// 0, and an edge pixel the fraction of it the rectangle covers. This is the
/// exact area (not the 4x4 sample count), so the two may differ by fractions
/// at an edge; that is fine, only the interior and outside must be exact.
fn rect_coverage(x0: f64, y0: f64, x1: f64, y1: f64, width: usize, height: usize) -> Vec<f32> {
    let mut mask = vec![0f32; width * height];
    let (min_x, max_x) = (x0.max(0.0).min(width as f64), x1.max(0.0).min(width as f64));
    let (min_y, max_y) = (y0.max(0.0).min(height as f64), y1.max(0.0).min(height as f64));
    if !(min_x < max_x && min_y < max_y) {
        return mask;
    }
    let (x_start, x_end) = (min_x.floor() as usize, (max_x.ceil() as usize).min(width));
    let (y_start, y_end) = (min_y.floor() as usize, (max_y.ceil() as usize).min(height));
    // A column's share of the rectangle, worked out once (only the first and
    // last columns can be a fraction; the rest are 1).
    let column: Vec<f32> = (x_start..x_end)
        .map(|x| (((x as f64 + 1.0).min(max_x) - (x as f64).max(min_x)).clamp(0.0, 1.0)) as f32)
        .collect();
    for y in y_start..y_end {
        let cov_y = (((y as f64 + 1.0).min(max_y) - (y as f64).max(min_y)).clamp(0.0, 1.0)) as f32;
        let row = &mut mask[y * width..(y + 1) * width];
        for (i, &cov_x) in column.iter().enumerate() {
            row[x_start + i] = cov_x * cov_y;
        }
    }
    mask
}

/// Coverage (0..1) of each device pixel (rows from the bottom) by the shape
/// with these edges, 4x4 samples per pixel.
fn coverage(edges: &[((f64, f64), (f64, f64))], even_odd: bool, width: usize, height: usize) -> Vec<f32> {
    let mut mask = vec![0f32; width * height];
    if edges.is_empty() || width == 0 || height == 0 {
        return mask;
    }
    if let Some((x0, y0, x1, y1)) = axis_aligned_rect(edges) {
        return rect_coverage(x0, y0, x1, y1, width, height);
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

/// How a stroke is drawn, in user space: half its width, CGLineCap (butt
/// 0, round 1, square 2), CGLineJoin (miter 0, round 1, bevel 2), the miter
/// limit and the dash (phase, lengths on and off).
pub(super) struct Pen<'a> {
    pub half: f64,
    pub cap: i32,
    pub join: i32,
    pub miter_limit: f64,
    pub dash: Option<&'a (f64, Vec<f64>)>,
}

/// The polygons a stroke of the outline covers: a band along each line, the
/// joins at its corners and the caps at open ends (dashes cut into pieces
/// with caps of their own). Filled nonzero once all turn the same way,
/// they are the stroke.
fn stroke_polygons(outline: &Outline, pen: &Pen) -> Vec<Vec<(f64, f64)>> {
    let mut polygons = Vec::new();
    for figure in &outline.figures {
        let mut points = figure.points.clone();
        points.dedup_by(|a, b| same(*a, *b));
        if figure.closed && points.len() > 1 && same(points[0], points[points.len() - 1]) {
            points.pop();
        }
        let closed = figure.closed && points.len() > 2;
        match pen.dash {
            Some(dash) => dashed(&points, closed, dash).into_iter().for_each(|piece| stroke_line(&piece, false, pen, &mut polygons)),
            None => stroke_line(&points, closed, pen, &mut polygons),
        }
    }
    polygons
}

fn same(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12
}

fn unit(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = (dx * dx + dy * dy).sqrt();
    (length > 1e-12).then(|| (dx / length, dy / length))
}

/// A circle of `radius` around `c` as a polygon.
fn circle(c: (f64, f64), radius: f64) -> Vec<(f64, f64)> {
    (0..16).map(|i| std::f64::consts::TAU * i as f64 / 16.0).map(|t| (c.0 + radius * t.cos(), c.1 + radius * t.sin())).collect()
}

/// One polyline's stroke polygons.
fn stroke_line(points: &[(f64, f64)], closed: bool, pen: &Pen, out: &mut Vec<Vec<(f64, f64)>>) {
    let mut points = points.to_vec();
    points.dedup_by(|a, b| same(*a, *b));
    let (n, h) = (points.len(), pen.half);
    if n < 2 {
        // A lone point: a dot for round and square caps.
        if let (Some(&p), 1 | 2) = (points.first(), pen.cap) {
            out.push(if pen.cap == 1 { circle(p, h) } else { vec![(p.0 - h, p.1 - h), (p.0 + h, p.1 - h), (p.0 + h, p.1 + h), (p.0 - h, p.1 + h)] });
        }
        return;
    }
    let lines = if closed { n } else { n - 1 };
    for i in 0..lines {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let Some((dx, dy)) = unit(a, b) else { continue };
        let (nx, ny) = (-dy * h, dx * h);
        out.push(vec![(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]);
    }
    let corners = if closed { 0..n } else { 1..n - 1 };
    for i in corners {
        let (before, v, after) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
        let (Some(d1), Some(d2)) = (unit(before, v), unit(v, after)) else { continue };
        let cross = d1.0 * d2.1 - d1.1 * d2.0;
        if cross.abs() < 1e-9 && d1.0 * d2.0 + d1.1 * d2.1 > 0.0 {
            continue;
        }
        if pen.join == 1 {
            out.push(circle(v, h));
            continue;
        }
        // The outer side of the turn.
        let s = if cross > 0.0 { -1.0 } else { 1.0 };
        let (n1, n2) = ((-d1.1 * h * s, d1.0 * h * s), (-d2.1 * h * s, d2.0 * h * s));
        let (p1, p2) = ((v.0 + n1.0, v.1 + n1.1), (v.0 + n2.0, v.1 + n2.1));
        let m = (n1.0 + n2.0, n1.1 + n2.1);
        let m2 = m.0 * m.0 + m.1 * m.1;
        // The miter's length over the line width is 1 / sin(half the angle between the lines).
        let ratio = if m2 > 1e-24 { 2.0 * h / m2.sqrt() } else { f64::INFINITY };
        if pen.join == 0 && ratio <= pen.miter_limit {
            let k = 2.0 * h * h / m2;
            out.push(vec![v, p1, (v.0 + m.0 * k, v.1 + m.1 * k), p2]);
        } else {
            out.push(vec![v, p1, p2]);
        }
    }
    if closed {
        return;
    }
    for (end, inner) in [(points[0], points[1]), (points[n - 1], points[n - 2])] {
        let Some((dx, dy)) = unit(inner, end) else { continue };
        match pen.cap {
            1 => out.push(circle(end, h)),
            2 => {
                let (nx, ny) = (-dy * h, dx * h);
                let far = (end.0 + dx * h, end.1 + dy * h);
                out.push(vec![(end.0 + nx, end.1 + ny), (far.0 + nx, far.1 + ny), (far.0 - nx, far.1 - ny), (end.0 - nx, end.1 - ny)]);
            }
            _ => {}
        }
    }
}

/// A polyline cut by a dash pattern into the pieces drawn.
fn dashed(points: &[(f64, f64)], closed: bool, (phase, lengths): &(f64, Vec<f64>)) -> Vec<Vec<(f64, f64)>> {
    let total: f64 = lengths.iter().sum();
    if points.len() < 2 || total <= 0.0 {
        return vec![points.to_vec()];
    }
    let mut path = points.to_vec();
    if closed {
        path.push(points[0]);
    }
    // Where the phase starts in the pattern (dashes on, gaps off, in turn).
    let (mut index, mut on, mut left) = (0, true, lengths[0]);
    let mut skip = phase.rem_euclid(total);
    while skip > 0.0 {
        if skip >= left {
            skip -= left;
            index = (index + 1) % lengths.len();
            (on, left) = (!on, lengths[index]);
        } else {
            left -= skip;
            skip = 0.0;
        }
    }
    let mut pieces = Vec::new();
    let mut piece = if on { vec![path[0]] } else { Vec::new() };
    for pair in path.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let length = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let mut done = 0.0;
        while length - done > left {
            done += left;
            let f = done / length;
            let p = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
            if on {
                piece.push(p);
                pieces.push(std::mem::take(&mut piece));
            } else {
                piece = vec![p];
            }
            index = (index + 1) % lengths.len();
            (on, left) = (!on, lengths[index]);
        }
        left -= length - done;
        if on {
            piece.push(b);
        }
    }
    if on && piece.len() >= 2 {
        pieces.push(piece);
    }
    pieces
}

/// Polygons (points in device space) as edges all turning the same way.
fn oriented_edges(polygons: &[Vec<(f64, f64)>]) -> Vec<((f64, f64), (f64, f64))> {
    let mut edges = Vec::new();
    for points in polygons {
        let n = points.len();
        let area: f64 = (0..n).map(|i| points[i].0 * points[(i + 1) % n].1 - points[(i + 1) % n].0 * points[i].1).sum();
        for i in 0..n {
            let (a, b) = (points[i], points[(i + 1) % n]);
            edges.push(if area >= 0.0 { (a, b) } else { (b, a) });
        }
    }
    edges
}

impl Canvas {
    pub(super) fn to_device(&self) -> impl Fn((f64, f64)) -> (f64, f64) {
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
                    self.blend(x, y, color, cover.min(1.0), &Blend::Over);
                }
            }
        }
    }

    pub(super) fn fill_outline(&mut self, outline: &Outline, even_odd: bool) {
        let mask = coverage(&outline.edges(), even_odd, self.width, self.height);
        self.paint_mask(&mask, self.state.fill);
    }

    /// Strokes a device-space outline as Quartz does: in user space (the
    /// CTM stretches the pen too), with the state's width, cap, join, miter
    /// limit and dash. A width of 0 is the thinnest line (one device pixel).
    /// The pixels filling (or stroking, with the pen's reach) `outline` can touch.
    pub(super) fn outline_area(&self, outline: &Outline, stroked: bool) -> Area {
        let points = outline.figures.iter().flat_map(|f| f.points.iter());
        let (mut left, mut bottom, mut right, mut top) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in points {
            (left, bottom, right, top) = (left.min(x), bottom.min(y), right.max(x), top.max(y));
        }
        if left > right {
            return Area::NONE;
        }
        let mut margin = 1.0;
        if stroked {
            let ctm = self.state.ctm;
            // The pen's half width in device pixels (at most), times how far
            // a miter join or a square cap reaches past it.
            let stretch = (ctm.a * ctm.a + ctm.b * ctm.b + ctm.c * ctm.c + ctm.d * ctm.d).sqrt();
            let half = if self.state.line_width > 0.0 { self.state.line_width / 2.0 * stretch } else { 0.5 };
            let reach = if self.state.line_join == 0 { self.state.miter_limit.max(std::f64::consts::SQRT_2) } else { std::f64::consts::SQRT_2 };
            margin += half * reach;
        }
        self.area_around((left, bottom, right, top), margin)
    }

    pub(super) fn stroke_outline(&mut self, outline: &Outline) {
        let ctm = self.state.ctm;
        let Some(inverse) = geometry::invert(ctm) else { return };
        let mut user = Outline::default();
        user.append(outline, &|(x, y)| geometry::apply(inverse, x, y));
        let width = self.state.line_width;
        let half = if width > 0.0 { width / 2.0 } else { 0.5 / (ctm.a * ctm.d - ctm.b * ctm.c).abs().sqrt() };
        let dash = self.state.dash.clone();
        let pen = Pen { half, cap: self.state.line_cap, join: self.state.line_join, miter_limit: self.state.miter_limit, dash: dash.as_deref() };
        let polygons: Vec<Vec<(f64, f64)>> = stroke_polygons(&user, &pen).into_iter().map(|p| p.into_iter().map(|(x, y)| geometry::apply(ctm, x, y)).collect()).collect();
        let mask = coverage(&oriented_edges(&polygons), false, self.width, self.height);
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
                self.blend(x, y, scaled(premultiply(gradient.at(t)), alpha), 1.0, &Blend::Over);
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
        let stroked = matches!(mode, PATH_STROKE | PATH_FILL_STROKE | PATH_EO_FILL_STROKE);
        c.shadowed(|c| c.outline_area(path, stroked), |c, by| {
            let path = &path.moved(by);
            if matches!(mode, PATH_FILL | PATH_FILL_STROKE) {
                c.fill_outline(path, false);
            }
            if matches!(mode, PATH_EO_FILL | PATH_EO_FILL_STROKE) {
                c.fill_outline(path, true);
            }
            if matches!(mode, PATH_STROKE | PATH_FILL_STROKE | PATH_EO_FILL_STROKE) {
                c.stroke_outline(path);
            }
        });
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
        c.shadowed(|c| c.outline_area(&outline, mode != PATH_FILL), |c, by| {
            let outline = outline.moved(by);
            if mode == PATH_FILL {
                c.fill_outline(&outline, false);
            } else {
                c.stroke_outline(&outline);
            }
        });
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
/// Pairs of points, each pair its own line. The context's path goes first
/// (Quartz strokes them as a path of its own).
extern "C" fn stroke_line_segments(context: Id, points: *const Point, count: usize) {
    with_path("CGContextStrokeLineSegments", context, |_, path| *path = Outline::default());
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
        c.shadowed(Canvas::whole, |c, _| c.draw_radial_gradient(g, start, start_radius, end, end_radius, options));
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
extern "C" fn set_line_cap(context: Id, cap: i32) {
    if let Some(mut c) = canvas("CGContextSetLineCap", context) {
        c.state.line_cap = cap;
    }
}
extern "C" fn set_line_join(context: Id, join: i32) {
    if let Some(mut c) = canvas("CGContextSetLineJoin", context) {
        c.state.line_join = join;
    }
}
extern "C" fn set_miter_limit(context: Id, limit: f64) {
    if let Some(mut c) = canvas("CGContextSetMiterLimit", context) {
        c.state.miter_limit = limit;
    }
}
/// CGContextSetLineDash: lengths on and off along each line from `phase`
/// in; none (or NULL) draws lines whole.
extern "C" fn set_line_dash(context: Id, phase: f64, lengths: *const f64, count: usize) {
    let lengths = if lengths.is_null() || count == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(lengths, count) }.to_vec() };
    if let Some(mut c) = canvas("CGContextSetLineDash", context) {
        c.state.dash = (lengths.iter().any(|&l| l > 0.0) && lengths.iter().all(|&l| l >= 0.0)).then(|| Arc::new((phase, lengths)));
    }
}
extern "C" fn set_blend_mode(context: Id, mode: i32) {
    if !(0..=27).contains(&mode) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| crate::log!("CGContextSetBlendMode: unknown mode {mode} draws as normal"));
    }
    if let Some(mut c) = canvas("CGContextSetBlendMode", context) {
        c.state.blend_mode = mode;
    }
}
/// CGContextSetShadow: black at a third alpha (Quartz's default colour).
extern "C" fn set_shadow(context: Id, offset: Size, blur: f64) {
    set_shadow_to(context, offset, blur, Some([0.0, 0.0, 0.0, 1.0 / 3.0]));
}
/// CGContextSetShadowWithColor: a NULL colour or a clear one draws no shadow.
extern "C" fn set_shadow_with_color(context: Id, offset: Size, blur: f64, color: Id) {
    set_shadow_to(context, offset, blur, color_rgba(color));
}
fn set_shadow_to(context: Id, offset: Size, blur: f64, color: Option<[f64; 4]>) {
    if let Some(mut c) = canvas("CGContextSetShadowWithColor", context) {
        c.state.shadow = color.filter(|c| c[3] > 0.0).map(|color| Shadow { offset: (offset.width, offset.height), blur: blur.max(0.0), color });
    }
}
extern "C" fn set_text_drawing_mode(context: Id, mode: i32) {
    if let Some(mut c) = canvas("CGContextSetTextDrawingMode", context) {
        c.state.text_mode = mode;
    }
}
/// Drawing goes into a layer of its own until it ends; then it is put down
/// as one picture with the context's alpha, blend, clip and shadow.
extern "C" fn begin_transparency_layer(context: Id, _auxiliary: Id) {
    if let Some(mut c) = canvas("CGContextBeginTransparencyLayer", context) {
        c.begin_layer(None, false);
    }
}
/// The layer's drawing is cut to `rect` (as Quartz's is).
extern "C" fn begin_transparency_layer_with_rect(context: Id, rect: Rect, _auxiliary: Id) {
    if let Some(mut c) = canvas("CGContextBeginTransparencyLayerWithRect", context) {
        let area = c.rect_area(rect);
        c.begin_layer(Some(area), false);
    }
}
extern "C" fn end_transparency_layer(context: Id) {
    if let Some(mut c) = canvas("CGContextEndTransparencyLayer", context) {
        c.end_layer();
    }
}

/// Opens a transparency layer on a context (-renderInContext: draws a
/// faded or shadowed layer through one).
pub(super) fn begin_layer_in(context: Id) {
    begin_transparency_layer(context, 0);
}

pub(super) fn end_layer_in(context: Id) {
    end_transparency_layer(context);
}

/// Sets a context's shadow in device pixels (y up) and multiplies its alpha
/// (inside a layer the alpha is 1; a layer drawn straight on keeps the
/// faded layers' around it).
pub(super) fn set_shadow_and_alpha_in(context: Id, offset: (f64, f64), blur: f64, color: Option<[f64; 4]>, alpha: Option<f64>) {
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        if let Some(color) = color {
            c.state.shadow = Some(Shadow { offset, blur, color });
        }
        if let Some(alpha) = alpha {
            c.state.alpha = (c.state.alpha * alpha).clamp(0.0, 1.0);
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

/// Marks the layers drawing `path` as changed (fw/changes.rs).
fn mark_holders(path: Id) {
    for layer in crate::fw::layers::path_holders(path) {
        crate::fw::changes::changed(layer);
    }
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
    // A path installed on a CAShapeLayer (or used by a shadow) that the game
    // edits in place changes what is drawn: mark its layers after the edit.
    mark_holders(path);
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

/// A new CFData of the provider's bytes (+1: its bytes go at the game's CFRelease).
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
/// (default [0 1]); drawn from 8-bit samples, where 1 hides and 0 paints.
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
    let samples = p.with_bytes(|bytes| {
        let Some(_) = needed.filter(|&n| n <= bytes.len()) else {
            crate::log!("CGImageMaskCreate: {width}x{height}, {bits} bits/component, {bits_per_pixel} bits/pixel, {bytes_per_row} bytes/row from {} bytes: not supported", bytes.len());
            return None;
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
        Some(Bitmap { width, height, bits_per_pixel: 8, bytes_per_row: width, info: ALPHA_NONE, space: Space::Gray, data })
    });
    let Some(samples) = samples else { return 0 };
    // The mask keeps the game's own format, decode array and provider (its
    // getters and CGImageGetDataProvider give them back); the decoded
    // samples are only for drawing.
    let decode = (!decode.is_null()).then_some([low, high]);
    make_mask(samples, MaskFormat { bits_per_component, bits_per_pixel, bytes_per_row, decode }, provider)
}

// ----- paths for UIBezierPath and CAShapeLayer (fw/bezier.rs, render.rs) -----

/// A new empty CGPath.
pub fn new_path() -> Id {
    path_create_mutable()
}

fn edit(path: Id, work: impl FnOnce(&mut Outline)) {
    if let Some(p) = path_state(path) {
        work(&mut p.lock().unwrap_or_else(|e| e.into_inner()));
        // A path installed on a CAShapeLayer (or used by a shadow) that the
        // game edits in place changes what is drawn: mark its layers after
        // the edit (a path only built to draw with marks nothing).
        mark_holders(path);
    }
}

pub fn path_move(path: Id, p: Point) {
    edit(path, |o| o.move_to((p.x, p.y)));
}

pub fn path_line(path: Id, p: Point) {
    edit(path, |o| o.line_to((p.x, p.y)));
}

/// A cubic curve from the current point, flattened into short lines.
pub fn path_curve(path: Id, c1: Point, c2: Point, end: Point) {
    edit(path, |o| {
        let Some((x0, y0)) = o.current() else {
            o.move_to((end.x, end.y));
            return;
        };
        let steps = 24;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            o.line_to((a * x0 + b * c1.x + c * c2.x + d * end.x, a * y0 + b * c1.y + c * c2.y + d * end.y));
        }
    });
}

pub fn path_quad(path: Id, control: Point, end: Point) {
    let start = path_current(path).unwrap_or(end);
    // The same curve as a cubic one.
    let c1 = Point { x: start.x + 2.0 / 3.0 * (control.x - start.x), y: start.y + 2.0 / 3.0 * (control.y - start.y) };
    let c2 = Point { x: end.x + 2.0 / 3.0 * (control.x - end.x), y: end.y + 2.0 / 3.0 * (control.y - end.y) };
    path_curve(path, c1, c2, end);
}

pub fn path_arc(path: Id, center: Point, radius: f64, start: f64, end: f64, clockwise: bool) {
    edit(path, |o| o.arc((center.x, center.y), radius, start, end, clockwise, &|p| p));
}

pub fn path_close(path: Id) {
    edit(path, |o| o.close());
}

pub fn path_rect(path: Id, rect: Rect) {
    edit(path, |o| o.rect(rect, &|p| p));
}

pub fn path_oval(path: Id, rect: Rect) {
    edit(path, |o| o.ellipse(rect, &|p| p));
}

/// A rectangle with round corners (the radius at most half the shorter side).
pub fn path_rounded_rect(path: Id, rect: Rect, radius: f64) {
    edit(path, |o| o.rounded_rect(rect, radius, &|p| p));
}

impl Outline {
    /// Adds a rectangle with round corners (the radius at most half the
    /// shorter side), its points passed through `map`.
    pub(super) fn rounded_rect(&mut self, rect: Rect, radius: f64, map: &dyn Fn((f64, f64)) -> (f64, f64)) {
        let rect = standard(rect);
        let r = radius.max(0.0).min(rect.size.width / 2.0).min(rect.size.height / 2.0);
        let (x0, y0, x1, y1) = (rect.origin.x, rect.origin.y, rect.origin.x + rect.size.width, rect.origin.y + rect.size.height);
        let half = std::f64::consts::FRAC_PI_2;
        self.move_to(map((x0 + r, y0)));
        self.line_to(map((x1 - r, y0)));
        self.arc((x1 - r, y0 + r), r, -half, 0.0, false, map);
        self.line_to(map((x1, y1 - r)));
        self.arc((x1 - r, y1 - r), r, 0.0, half, false, map);
        self.line_to(map((x0 + r, y1)));
        self.arc((x0 + r, y1 - r), r, half, 2.0 * half, false, map);
        self.line_to(map((x0, y0 + r)));
        self.arc((x0 + r, y0 + r), r, 2.0 * half, 3.0 * half, false, map);
        self.close();
    }

    /// How much (0..1) of each device pixel (rows from the bottom) the filled shape covers.
    pub(super) fn cover(&self, even_odd: bool, width: usize, height: usize) -> Vec<f32> {
        coverage(&self.edges(), even_odd, width, height).into_iter().map(|v| v.min(1.0)).collect()
    }
}

pub fn path_current(path: Id) -> Option<Point> {
    let p = path_state(path)?;
    let current = p.lock().unwrap_or_else(|e| e.into_inner()).current();
    current.map(|(x, y)| Point { x, y })
}

pub fn path_is_empty(path: Id) -> bool {
    path_state(path).is_none_or(|p| p.lock().unwrap_or_else(|e| e.into_inner()).figures.is_empty())
}

/// The smallest rectangle holding the path's points.
pub fn path_bounds(path: Id) -> Rect {
    let Some(p) = path_state(path) else { return Rect::default() };
    let outline = p.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let points: Vec<(f64, f64)> = outline.figures.iter().flat_map(|f| f.points.iter().copied()).collect();
    if points.is_empty() {
        return Rect::default();
    }
    let (x0, x1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
    let (y0, y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// The path's points, one list per figure (curves already in short lines;
/// a closed figure ends back at its start).
pub fn path_points(path: Id) -> Vec<Vec<Point>> {
    let Some(p) = path_state(path) else { return Vec::new() };
    let outline = p.lock().unwrap_or_else(|e| e.into_inner()).clone();
    outline
        .figures
        .iter()
        .map(|f| {
            let mut points: Vec<Point> = f.points.iter().map(|&(x, y)| Point { x, y }).collect();
            if let (true, Some(&first)) = (f.closed, points.first()) {
                points.push(first);
            }
            points
        })
        .collect()
}

pub fn path_copy(path: Id) -> Id {
    let copy = new_path();
    if let Some(p) = path_state(path) {
        let outline = p.lock().unwrap_or_else(|e| e.into_inner()).clone();
        edit(copy, |o| *o = outline);
    }
    copy
}

/// Moves every point of the path by `t`.
pub fn path_apply(path: Id, t: Transform) {
    edit(path, |o| {
        for figure in &mut o.figures {
            for point in &mut figure.points {
                *point = geometry::apply(t, point.0, point.1);
            }
        }
    });
}

pub fn path_append(path: Id, other: Id) {
    let Some(source) = path_state(other).map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).clone()) else { return };
    edit(path, |o| o.append(&source, &|p| p));
}

/// Whether the point is inside the filled path.
pub fn path_contains(path: Id, point: Point, even_odd: bool) -> bool {
    let Some(p) = path_state(path) else { return false };
    let edges = p.lock().unwrap_or_else(|e| e.into_inner()).edges();
    let mut winding = 0;
    for ((x0, y0), (x1, y1)) in edges {
        if (y0 <= point.y && point.y < y1) || (y1 <= point.y && point.y < y0) {
            let x = x0 + (point.y - y0) * (x1 - x0) / (y1 - y0);
            if x > point.x {
                winding += if y1 > y0 { 1 } else { -1 };
            }
        }
    }
    if even_odd { winding % 2 != 0 } else { winding != 0 }
}

/// Fills and strokes a CGPath (in the context's user space): `fill` and
/// `stroke` colours (straight RGBA) and the line width; None leaves that out.
pub(super) fn draw_path_on(c: &mut Canvas, path: Id, fill: Option<[f64; 4]>, stroke: Option<([f64; 4], &StrokeStyle)>, even_odd: bool) {
    let Some(p) = path_state(path) else { return };
    let source = p.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut outline = Outline::default();
    outline.append(&source, &c.to_device());
    if let Some(color) = fill {
        let saved = c.state.fill;
        c.state.fill = color;
        c.fill_outline(&outline, even_odd);
        c.state.fill = saved;
    }
    if let Some((color, style)) = stroke {
        // A shape layer's own line settings, not the context's.
        let saved = c.state.clone();
        c.state.stroke = color;
        c.set_stroke_style(style);
        c.stroke_outline(&outline);
        c.state = saved;
    }
}

/// The context's path with a CGPath added to it (CGContextAddPath), taken
/// out of the context: what UIBezierPath fills, strokes or clips to.
fn take_path_with(c: &mut Canvas, path: Id) -> Outline {
    let mut outline = std::mem::take(&mut c.path);
    if let Some(p) = path_state(path) {
        let source = p.lock().unwrap_or_else(|e| e.into_inner()).clone();
        outline.append(&source, &c.to_device());
    }
    outline
}

/// -[UIBezierPath fill]: the path added to the context's and filled with
/// its fill colour (CGContextAddPath, CGContextFillPath).
pub fn fill_path_in(context: Id, path: Id, even_odd: bool) {
    if let Some(mut c) = canvas("-[UIBezierPath fill]", context) {
        let outline = take_path_with(&mut c, path);
        c.shadowed(|c| c.outline_area(&outline, false), |c, by| c.fill_outline(&outline.moved(by), even_odd));
    }
}

/// How UIBezierPath strokes: its width, CGLineCap, CGLineJoin, miter limit
/// and dash (phase, lengths).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrokeStyle {
    pub width: f64,
    pub cap: i32,
    pub join: i32,
    pub miter_limit: f64,
    pub dash: Option<(f64, Vec<f64>)>,
}

/// -[UIBezierPath stroke]: the path added to the context's and stroked with
/// its stroke colour and the path's own line settings.
pub fn stroke_path_in(context: Id, path: Id, style: &StrokeStyle) {
    if let Some(mut c) = canvas("-[UIBezierPath stroke]", context) {
        let outline = take_path_with(&mut c, path);
        let saved = c.state.clone();
        c.set_stroke_style(style);
        c.shadowed(|c| c.outline_area(&outline, true), |c, by| c.stroke_outline(&outline.moved(by)));
        c.state = saved;
    }
}

impl Canvas {
    /// The line settings strokes use from now on.
    pub(super) fn set_stroke_style(&mut self, style: &StrokeStyle) {
        self.state.line_width = style.width.max(0.0);
        (self.state.line_cap, self.state.line_join, self.state.miter_limit) = (style.cap, style.join, style.miter_limit);
        self.state.dash = style.dash.clone().filter(|(_, lengths)| lengths.iter().any(|&l| l > 0.0) && lengths.iter().all(|&l| l >= 0.0)).map(Arc::new);
    }
}

/// -[UIBezierPath addClip]: the path added to the context's and clipped to.
pub fn clip_path_in(context: Id, path: Id, even_odd: bool) {
    if let Some(mut c) = canvas("-[UIBezierPath addClip]", context) {
        let outline = take_path_with(&mut c, path);
        c.clip_to(&outline, even_odd);
    }
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
        ("CGContextSetLineCap", set_line_cap as usize),
        ("CGContextSetLineJoin", set_line_join as usize),
        ("CGContextSetMiterLimit", set_miter_limit as usize),
        ("CGContextSetLineDash", set_line_dash as usize),
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
        let pen = Pen { half: 1.0, cap: 0, join: 0, miter_limit: 10.0, dash: None };
        let mask = coverage(&oriented_edges(&stroke_polygons(&line, &pen)), false, 8, 8);
        assert_eq!(mask.iter().sum::<f32>(), 12.0);
        assert_eq!(mask[3 * 8 + 4], 1.0);
        // A clockwise arc a quarter turn round: from angle 0 down to -90 degrees.
        let mut arc = Outline::default();
        arc.arc((0.0, 0.0), 1.0, 0.0, -std::f64::consts::FRAC_PI_2, true, &|p| p);
        let end = arc.current().unwrap();
        assert!(end.0.abs() < 1e-9 && (end.1 + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_rectangle_clip_covers_interior_edge_and_outside() {
        // A clip rectangle from (2, 2) to (6, 5) on an 8x8 canvas: whole
        // pixels inside are 1, outside 0.
        let mut outline = Outline::default();
        outline.rect(Rect::new(2.0, 2.0, 4.0, 3.0), &|p| p);
        let mask = outline.cover(false, 8, 8);
        assert_eq!(mask[3 * 8 + 3], 1.0, "an inside pixel");
        assert_eq!(mask[8 + 1], 0.0, "an outside pixel");
        assert_eq!(mask.iter().sum::<f32>(), 12.0, "4 by 3 fully-covered pixels");
        // A rectangle whose left edge lands mid-pixel: that pixel is a
        // fraction of 1 (partly in), never 0 or 1.
        let mut fractional = Outline::default();
        fractional.rect(Rect::new(2.5, 2.0, 3.5, 3.0), &|p| p);
        let mask = fractional.cover(false, 8, 8);
        let edge = mask[2 * 8 + 2];
        assert!(edge > 0.0 && edge < 1.0, "edge pixel is a fraction, got {edge}");
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

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn context(width: usize, height: usize) -> Id {
        bitmap_context_create(std::ptr::null_mut(), width, height, 8, 0, space_create_rgb(), PREMULTIPLIED_LAST)
    }

    /// Device pixel (x, y from the bottom) as stored (premultiplied RGBA).
    fn pixel(context: Id, x: usize, y: usize) -> [u8; 4] {
        let (height, bytes_per_row) = (bitmap_context_get_height(context), bitmap_context_get_bytes_per_row(context));
        let at = (height - 1 - y) * bytes_per_row + x * 4;
        unsafe { std::slice::from_raw_parts(bitmap_context_get_data(context).add(at), 4) }.try_into().unwrap()
    }

    /// A palette PNG without transparency stays 8 bits of palette indices
    /// per pixel, as ImageIO gives it, so its bytes make an image mask (the
    /// dungeon map's fog of war); with transparency it is RGBA.
    #[test]
    fn palette_pngs_keep_their_indices_and_make_masks() {
        setup();
        use super::super::tests::{header, png_of};
        let palette: Vec<u8> = (0..4u8).flat_map(|i| [i * 80; 3]).collect();
        let idat = miniz_oxide::deflate::compress_to_vec_zlib(&[0, 0, 1, 2, 3], 6);
        let png = png_of(&[(b"IHDR", header(4, 1, 8, 3)), (b"PLTE", palette.clone()), (b"IDAT", idat.clone()), (b"IEND", Vec::new())]);
        let (bitmap, _) = decode(&png).expect("read");
        assert_eq!((bitmap.bits_per_pixel, bitmap.bytes_per_row, &bitmap.data[..]), (8, 4, &[0u8, 1, 2, 3][..]));
        assert_eq!(bitmap.straight_rgba().unwrap()[4..8], [80, 80, 80, 255]);
        let picture = make_image(bitmap);
        assert_eq!((image_get_bits_per_component(picture), image_get_bits_per_pixel(picture)), (8, 8));
        assert_eq!(space_get_number_of_components(image_get_color_space(picture)), 1);
        let mask = image_mask_create(4, 1, 8, 8, 4, image_get_data_provider(picture), std::ptr::null(), false);
        assert_ne!(mask, 0, "its bytes make a mask");
        // Drawn again and again, it is turned into colours only once.
        let (first, again) = (drawable(picture).unwrap(), drawable(picture).unwrap());
        assert!(matches!(first.space, Space::Rgb) && Arc::ptr_eq(&first, &again), "one colour copy");
        let with_alpha = png_of(&[(b"IHDR", header(4, 1, 8, 3)), (b"PLTE", palette), (b"tRNS", vec![0, 255]), (b"IDAT", idat), (b"IEND", Vec::new())]);
        let (bitmap, _) = decode(&with_alpha).expect("read");
        assert_eq!((bitmap.bits_per_pixel, bitmap.straight_rgba().unwrap()[..8].to_vec()), (32, vec![0, 0, 0, 0, 80, 80, 80, 255]));
    }

    /// A bitmap context's fill colour space is DeviceGray until set
    /// (CGContextSetFillColor reads gray and alpha); a drawRect: context's
    /// is DeviceRGB (UIKit sets it).
    #[test]
    fn fill_colours_are_gray_until_a_space_is_set() {
        setup();
        let c = context(1, 1);
        set_fill_color(c, [0.5, 1.0, 0.0, 0.0].as_ptr());
        context_fill_rect(c, Rect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(pixel(c, 0, 0), [128, 128, 128, 255]);
        let picture = draw_picture(Size { width: 1.0, height: 1.0 }, 1.0, Point::default(), || {
            let here = current_context();
            set_fill_color(here, [1.0, 0.0, 0.0, 1.0].as_ptr());
            context_fill_rect(here, Rect::new(0.0, 0.0, 1.0, 1.0));
        });
        assert_eq!(straight_pixels(picture).unwrap(), [255, 0, 0, 255]);
    }

    /// A transparency layer is drawn on its own and put down as one picture
    /// with the context's alpha and shadow; a shadow set on the context
    /// falls under each shape (offset in device space, y up).
    #[test]
    fn transparency_layers_and_shadows_draw_as_one_picture() {
        setup();
        let c = context(1, 1);
        context_set_alpha(c, 0.5);
        begin_transparency_layer(c, 0);
        context_set_rgb_fill_color(c, 1.0, 0.0, 0.0, 1.0);
        context_fill_rect(c, Rect::new(0.0, 0.0, 1.0, 1.0));
        context_set_rgb_fill_color(c, 0.0, 0.0, 1.0, 1.0);
        context_fill_rect(c, Rect::new(0.0, 0.0, 1.0, 1.0));
        end_transparency_layer(c);
        assert!(pixel(c, 0, 0).iter().zip([0, 0, 128, 128]).all(|(&a, b)| a.abs_diff(b) <= 1), "{:?}", pixel(c, 0, 0));
        let c = context(10, 10);
        set_shadow_with_color(c, Size { width: 4.0, height: 4.0 }, 0.0, make_color([1.0, 0.0, 0.0, 1.0]));
        context_set_rgb_fill_color(c, 0.0, 0.0, 1.0, 1.0);
        context_fill_rect(c, Rect::new(0.0, 0.0, 3.0, 3.0));
        assert_eq!((pixel(c, 1, 1), pixel(c, 5, 5), pixel(c, 8, 8)[3]), ([0, 0, 255, 255], [255, 0, 0, 255], 0));
    }

    /// Strokes as Core Graphics draws them: miter joins and butt caps until
    /// set, round caps past the end, dashes, and the width in user space
    /// (stretched by the CTM).
    #[test]
    fn strokes_follow_joins_caps_dashes_and_the_ctm() {
        setup();
        let c = context(16, 16);
        set_line_width(c, 4.0);
        move_to_point(c, 2.0, 2.0);
        add_line_to_point(c, 10.0, 2.0);
        add_line_to_point(c, 10.0, 10.0);
        stroke_path(c);
        assert_eq!(pixel(c, 11, 0)[3], 255, "a miter corner");
        move_to_point(c, 2.0, 13.0);
        add_line_to_point(c, 6.0, 13.0);
        stroke_path(c);
        assert_eq!(pixel(c, 7, 13)[3], 0, "a butt end");
        set_line_cap(c, 1);
        move_to_point(c, 2.0, 13.0);
        add_line_to_point(c, 6.0, 13.0);
        stroke_path(c);
        assert!(pixel(c, 7, 13)[3] > 128, "a round end: {:?}", pixel(c, 7, 13));
        let dashed = context(16, 4);
        set_line_width(dashed, 2.0);
        set_line_dash(dashed, 0.0, [4.0, 4.0].as_ptr(), 2);
        move_to_point(dashed, 0.0, 2.0);
        add_line_to_point(dashed, 16.0, 2.0);
        stroke_path(dashed);
        assert_eq!((pixel(dashed, 2, 2)[3], pixel(dashed, 6, 2)[3]), (255, 0), "on, then off");
        let wide = context(8, 8);
        context_scale_ctm(wide, 2.0, 1.0);
        move_to_point(wide, 2.0, 0.0);
        add_line_to_point(wide, 2.0, 8.0);
        stroke_path(wide);
        assert_eq!((pixel(wide, 3, 4)[3], pixel(wide, 4, 4)[3]), (255, 255), "1 unit wide is 2 pixels across");
    }

    /// Blend modes as Quartz composites them (premultiplied, the PDF
    /// formulas): the separable and non-separable ones mix with what is
    /// there; the Porter-Duff ones replace it where the drawing covers and
    /// leave the rest. Pictures (whole-pixel and scaled) blend the same way.
    #[test]
    fn blend_modes_mix_colours_as_quartz_does() {
        setup();
        let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 1);
        let under = |c: Id, color: [f64; 4]| {
            context_set_rgb_fill_color(c, color[0], color[1], color[2], color[3]);
            context_fill_rect(c, Rect::new(0.0, 0.0, 2.0, 2.0));
        };
        // Grey 0.5 (or the given colour) drawn with `mode` over opaque (0.8, 0.4, 0.2).
        let mixed = |mode: i32, color: [f64; 4]| {
            let c = context(1, 1);
            under(c, [0.8, 0.4, 0.2, 1.0]);
            set_blend_mode(c, mode);
            under(c, color);
            pixel(c, 0, 0)
        };
        let grey = [0.5, 0.5, 0.5, 1.0];
        for (mode, color, want) in [
            (1, grey, [102, 51, 26, 255]),             // multiply
            (2, grey, [230, 179, 153, 255]),           // screen
            (4, grey, [128, 102, 51, 255]),            // darken
            (5, grey, [204, 128, 128, 255]),           // lighten
            (7, grey, [153, 0, 0, 255]),               // colour burn
            (10, grey, [77, 26, 77, 255]),             // difference
            (13, grey, [127, 127, 127, 255]),          // saturation: grey keeps the luminosity
            (15, grey, [205, 103, 52, 255]),           // luminosity
            (17, [0.0, 0.0, 1.0, 0.5], [0, 0, 128, 128]), // copy
            (18, grey, [128, 128, 128, 255]),          // source in
            (23, grey, [0, 0, 0, 0]),                  // destination out
            (25, grey, [0, 0, 0, 0]),                  // xor
            (27, grey, [255, 230, 179, 255]),          // plus lighter
        ] {
            assert!(near(mixed(mode, color), want), "mode {mode}: {:?}, want {want:?}", mixed(mode, color));
        }
        // Over nothing, multiply is the source; source in leaves nothing.
        let c = context(1, 1);
        set_blend_mode(c, 1);
        under(c, grey);
        assert!(near(pixel(c, 0, 0), [128, 128, 128, 255]), "{:?}", pixel(c, 0, 0));
        // Destination in only where the shape lands.
        let c = context(2, 1);
        under(c, [1.0, 0.0, 0.0, 1.0]);
        set_blend_mode(c, 22);
        context_set_rgb_fill_color(c, 0.0, 0.0, 0.0, 0.0);
        context_fill_rect(c, Rect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!((pixel(c, 0, 0), pixel(c, 1, 0)), ([0, 0, 0, 0], [255, 0, 0, 255]));
        // A grey picture multiplied, pixel for pixel and stretched.
        let source = context(1, 1);
        under(source, grey);
        let picture = bitmap_context_create_image(source);
        for size in [1.0, 2.0] {
            let c = context(2, 2);
            under(c, [0.8, 0.4, 0.2, 1.0]);
            set_blend_mode(c, 1);
            context_draw_image(c, Rect::new(0.0, 0.0, size, size), picture);
            assert!(near(pixel(c, 0, 0), [102, 51, 26, 255]), "{size}: {:?}", pixel(c, 0, 0));
        }
    }

    /// Clear source pixels count for the modes they change things in: a
    /// clear picture drawn destination-in (with a shadow, so through a
    /// layer of its own) clears what it covers and nothing past it; an
    /// explicit transparency layer put down destination-in clears where
    /// nothing was drawn into it.
    #[test]
    fn clear_pixels_clear_under_destination_in_layers() {
        setup();
        let red = |c: Id| {
            context_set_rgb_fill_color(c, 1.0, 0.0, 0.0, 1.0);
            context_fill_rect(c, Rect::new(0.0, 0.0, 4.0, 4.0));
        };
        let clear = bitmap_context_create_image(context(2, 2));
        let c = context(4, 4);
        red(c);
        set_shadow_with_color(c, Size { width: 0.0, height: 0.0 }, 0.0, make_color([0.0, 0.0, 0.0, 1.0]));
        set_blend_mode(c, 22);
        context_draw_image(c, Rect::new(0.0, 0.0, 2.0, 2.0), clear);
        assert_eq!((pixel(c, 1, 1), pixel(c, 3, 3)), ([0, 0, 0, 0], [255, 0, 0, 255]), "cleared under the picture only");
        let c = context(4, 4);
        red(c);
        set_blend_mode(c, 22);
        begin_transparency_layer(c, 0);
        set_blend_mode(c, 0);
        context_set_rgb_fill_color(c, 0.0, 0.0, 1.0, 1.0);
        context_fill_rect(c, Rect::new(0.0, 0.0, 1.0, 1.0));
        end_transparency_layer(c);
        assert_eq!((pixel(c, 0, 0), pixel(c, 3, 3)), ([255, 0, 0, 255], [0, 0, 0, 0]), "kept where drawn, cleared elsewhere");
    }

    /// CGContextStrokeLineSegments uses up the context's path; a
    /// UIBezierPath fill draws the context's path with its own and uses it
    /// up; CGContextAddArcToPoint with no current point adds nothing.
    #[test]
    fn line_segments_and_bezier_fills_use_up_the_context_path() {
        setup();
        let c = context(8, 8);
        add_rect(c, Rect::new(0.0, 0.0, 4.0, 4.0));
        let points = [Point { x: 6.0, y: 6.0 }, Point { x: 7.0, y: 7.0 }];
        stroke_line_segments(c, points.as_ptr(), 2);
        fill_path(c);
        assert_eq!(pixel(c, 1, 1)[3], 0);
        add_rect(c, Rect::new(0.0, 0.0, 2.0, 2.0));
        let path = new_path();
        path_rect(path, Rect::new(4.0, 4.0, 2.0, 2.0));
        fill_path_in(c, path, false);
        assert_eq!((pixel(c, 1, 1)[3], pixel(c, 5, 5)[3]), (255, 255));
        assert!(locked(c).unwrap().path.figures.is_empty());
        let mut outline = Outline::default();
        outline.arc_to_point((1.0, 1.0), (2.0, 0.0), 1.0, &|p| p, &|p| p);
        assert!(outline.figures.is_empty());
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
