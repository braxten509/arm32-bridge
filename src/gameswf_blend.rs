//! Smooth Flash menus for games built on Gameloft's gameswf (Monster Life).
//! Their menus step 30 times a second (the movie's frame rate), however
//! often the screen refreshes. While a menu is drawn, each piece the last
//! step moved or recolored is shown part of the way from where the step
//! before put it to where the last step put it, by how far the menu's clock
//! is toward its next step: the in-between pictures a 120 Hz screen wants.
//! Menus show one step (1/30 s) late; their scripts, timing and touches are
//! untouched (touches are tested outside drawing, on the real places).
//!
//! How (offsets of this gameswf build, found by reading the game's code):
//! a display object's own matrix and color are pointers (+0x54, +0x50);
//! drawing rebuilds world placements from them when an object's "stale"
//! flags (+0xa1, +0xa2) are set. So before `Root::display` a blended piece's
//! pointers are turned to blended copies (flags set), and after it they are
//! turned back (flags set again, so later lookups rebuild the real places).
//!
//! debug.arm32bridge.flashblend=0 turns it off.

use crate::runtime::{mem, Args, ArgSource, Ret, Uc};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where a piece is: its own matrix (a, b, tx, c, d, ty) and color (multiply, add for r, g, b, a).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    pub matrix: [f32; 6],
    pub color: [f32; 8],
}

/// A piece seen at a step: what it is (vtable, id, depth) and where it was.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Seen {
    pub identity: (u32, u32, u32),
    pub placement: Placement,
}

/// How many steps a `Root::advance` call took: the clock (`before`) plus
/// the time passed reaches a step every `period`. Without its flag
/// (`every_step`) advance takes one step and drops the time left over.
pub fn steps_taken(before: f32, passed: f32, period: f32, every_step: bool) -> u32 {
    if !(period > 0.0) || !(passed >= 0.0) {
        return 0;
    }
    let total = before + passed;
    if total < period {
        0
    } else if every_step {
        (total / period).floor().max(1.0) as u32
    } else {
        1
    }
}

/// The furthest a piece moves in one step and is still slid there, in menu
/// units (a drag or fling of the shop's cards moves 10-30; letting go puts
/// a row of them, 276, back where it started while it shows the next cards).
const MAX_STEP_MOVE: f32 = 200.0;

/// What to show for a piece `t` (0..1) of the way to the next step: None
/// keeps its own placement (nothing moved at the last step, it is new, or
/// something other than the step has moved it since: `live` differs).
pub fn blended(previous: &Seen, current: &Seen, live: &Seen, t: f32) -> Option<Placement> {
    if previous.identity != current.identity || live != current || previous.placement == current.placement {
        return None;
    }
    let (from, to) = (&previous.placement.matrix, &current.placement.matrix);
    if (to[2] - from[2]).abs() > MAX_STEP_MOVE || (to[5] - from[5]).abs() > MAX_STEP_MOVE {
        return None;
    }
    let t = t.clamp(0.0, 1.0);
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let (from, to) = (&previous.placement, &current.placement);
    Some(Placement {
        matrix: std::array::from_fn(|i| mix(from.matrix[i], to.matrix[i])),
        color: std::array::from_fn(|i| mix(from.color[i], to.color[i])),
    })
}

/// The pieces of one menu at its last two steps.
#[derive(Default)]
pub struct Frames {
    pub previous: HashMap<u32, Seen>,
    pub current: HashMap<u32, Seen>,
}

impl Frames {
    /// After a call that took `steps` steps, with the pieces as they are now:
    /// one step makes the last step's the previous; more (the game fell
    /// behind) leave nothing to blend from.
    pub fn stepped(&mut self, steps: u32, now: HashMap<u32, Seen>) {
        match steps {
            0 => {}
            1 => self.previous = std::mem::replace(&mut self.current, now),
            _ => {
                self.previous = now.clone();
                self.current = now;
            }
        }
    }
}

// Offsets in this gameswf build (see the module notes).
const ROOT_MOVIE: u32 = 0x10;
const ROOT_CLOCK: u32 = 0xe4;
const ROOT_PERIOD: u32 = 0xe8;
const ID: u32 = 0x40;
const COLOR: u32 = 0x50;
const MATRIX: u32 = 0x54;
const EFFECT: u32 = 0x58;
const EXTRA: u32 = 0x5c;
const EXTRA_REFLECTION: u32 = 0x62;
const DEPTH: u32 = 0x9c;
const MATRIX_STALE: u32 = 0xa1;
const COLOR_STALE: u32 = 0xa2;
const CACHED_AS_BITMAP: u32 = 0xa8;
const CHILDREN: u32 = 0xe0;
const CHILD_COUNT: u32 = 0xe4;
/// A container's world-matrix update (vtable slot 0x124) is this function.
const CONTAINER_UPDATE: &str = "_ZN7gameswf24ASDisplayObjectContainer17updateWorldMatrixEv";
const UPDATE_SLOT: u32 = 0x124;
const BUTTON_VTABLE: &str = "_ZTVN7gameswf25button_character_instanceE";
/// Every object's effect pointer starts at this "no effect" (not zero).
const NO_EFFECT: &str = "_ZN7gameswf6Effect8identityE";
const MAX_PIECES: usize = 20_000;
const MAX_DEPTH: u32 = 64;

/// One piece's pointers turned to blended copies during a draw, and what it was.
struct Swap {
    object: u32,
    identity: (u32, u32, u32),
    matrix: Option<(u32, u32)>,
    color: Option<(u32, u32)>,
}

/// What one draw changed: its swaps and its own memory for the copies.
#[derive(Default)]
struct Draw {
    swaps: Vec<Swap>,
    scratch: u32,
}

/// What blending did lately, logged every few seconds (to see it work on the phone).
#[derive(Default)]
struct Counts {
    advances: u32,
    steps: u32,
    pieces: usize,
    roots: usize,
    displays: u32,
    blended: u32,
    clock_sum: f32,
    since: Option<std::time::Instant>,
}

#[derive(Default)]
struct State {
    counts: Counts,
    menus: HashMap<u32, Frames>,
    /// Button parts (address, vtable) seen at the last steps: drawing doesn't
    /// mark them stale when a parent moves, so they are marked by hand around a blended draw.
    button_parts: HashMap<u32, Vec<(u32, u32)>>,
    known: Option<Known>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = crate::dynarec::property("debug.arm32bridge.flashblend").as_deref() != Some("0");
        crate::log::line(&format!("[bridge] Flash menus {}", if on { "blend between steps" } else { "step as made (flashblend=0)" }));
        on
    })
}

fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    work(guard.get_or_insert_with(State::default))
}

/// Addresses in the game the walk compares with.
#[derive(Clone, Copy)]
struct Known {
    container_update: u32,
    /// As stored in objects (the vtable symbol + 8).
    button_vtable: u32,
    no_effect: u32,
}

fn known_addresses(state: &mut State) -> Option<Known> {
    if state.known.is_none() {
        let shared = crate::runtime::shared();
        let exports = &shared.guest_exports;
        state.known = Some(Known {
            container_update: *exports.get(CONTAINER_UPDATE)?,
            button_vtable: *exports.get(BUTTON_VTABLE)? + 8,
            no_effect: *exports.get(NO_EFFECT)?,
        });
    }
    state.known
}

/// Whether guest memory [address, address + size) can be read.
fn readable(address: u32, size: u32) -> bool {
    address >= 0x1000 && crate::mem::layout().mapped(address, u64::from(address) + u64::from(size))
}

fn read_f32(address: u32) -> f32 {
    f32::from_bits(mem().read_u32(address))
}

/// A piece's identity and placement, if its pointers lead somewhere readable.
fn look_at(object: u32) -> Option<Seen> {
    if !readable(object, 0xb0) {
        return None;
    }
    let memory = mem();
    let (matrix, color) = (memory.read_u32(object + MATRIX), memory.read_u32(object + COLOR));
    if !readable(matrix, 24) || !readable(color, 32) {
        return None;
    }
    Some(Seen {
        identity: (memory.read_u32(object), memory.read_u32(object + ID), u32::from(memory.read_u16(object + DEPTH))),
        placement: Placement {
            matrix: std::array::from_fn(|i| read_f32(matrix + i as u32 * 4)),
            color: std::array::from_fn(|i| read_f32(color + i as u32 * 4)),
        },
    })
}

/// Every piece of a menu that may be blended, and the button parts.
fn walk(movie: u32, known: Known) -> (HashMap<u32, Seen>, Vec<(u32, u32)>) {
    let Known { container_update, button_vtable, no_effect } = known;
    let memory = mem();
    let mut pieces = HashMap::new();
    let mut parts = Vec::new();
    let mut stack = vec![(movie, 0u32)];
    while let Some((object, depth)) = stack.pop() {
        if pieces.len() >= MAX_PIECES || depth > MAX_DEPTH || !readable(object, 0xf0) {
            continue;
        }
        // Pictures drawn once and reused (bitmap caches, filters, reflections) don't follow a pointer swap.
        let extra = memory.read_u32(object + EXTRA);
        if memory.read_u8(object + CACHED_AS_BITMAP) != 0
            || ![0, no_effect].contains(&memory.read_u32(object + EFFECT))
            || (extra != 0 && readable(extra, 0x64) && memory.read_u8(extra + EXTRA_REFLECTION) != 0)
        {
            continue;
        }
        // A piece that can't be read isn't blended, but the pieces inside it still are.
        if let Some(seen) = look_at(object) {
            pieces.insert(object, seen);
        }
        let vtable = memory.read_u32(object);
        let children = if readable(vtable + UPDATE_SLOT, 4) && memory.read_u32(vtable + UPDATE_SLOT) == container_update {
            Some((memory.read_u32(object + CHILDREN), memory.read_u32(object + CHILD_COUNT)))
        } else if vtable == button_vtable {
            let definition = memory.read_u32(object + CHILDREN);
            let count = if readable(definition + 0x24, 4) { memory.read_u32(definition + 0x24) } else { 0 };
            let list = memory.read_u32(object + CHILD_COUNT);
            if readable(list, count.saturating_mul(4)) {
                let found = (0..count.min(256)).map(|i| memory.read_u32(list + i * 4)).filter(|&part| readable(part, 0xb0));
                parts.extend(found.map(|part| (part, memory.read_u32(part))));
            }
            Some((list, count))
        } else {
            None
        };
        if let Some((list, count)) = children {
            if count <= 4096 && readable(list, count * 4) {
                stack.extend((0..count).map(|i| (memory.read_u32(list + i * 4), depth + 1)).filter(|&(child, _)| child != 0));
            }
        }
    }
    (pieces, parts)
}

pub(crate) fn call_original(uc: &mut Uc, symbol: &str, args: &[u32]) -> Ret {
    match crate::runtime::call_guest(uc, crate::wraps::original(symbol), args) {
        Ok(result) => Ret::U32(result as u32),
        Err(_) => Ret::U32(0),
    }
}

pub const ADVANCE: &str = "_ZN7gameswf4Root7advanceEfb";
pub const DISPLAY: &str = "_ZN7gameswf4Root7displayEv";

/// `gameswf::Root::advance(float dt, bool)`: notes the pieces after each step.
pub fn advance(uc: &mut Uc, args: &mut Args) -> Ret {
    let (root, passed, flag) = (args.u32(), args.u32(), args.u32());
    if !enabled() || !readable(root, 0x100) {
        return call_original(uc, ADVANCE, &[root, passed, flag]);
    }
    let (before, period) = (read_f32(root + ROOT_CLOCK), read_f32(root + ROOT_PERIOD));
    let result = call_original(uc, ADVANCE, &[root, passed, flag]);
    let steps = steps_taken(before, f32::from_bits(passed), period, flag & 0xFF != 0);
    with_state(|state| {
        state.counts.advances += 1;
        state.counts.steps += steps;
    });
    if steps > 0 {
        with_state(|state| {
            let Some(known) = known_addresses(state) else { return };
            let (pieces, parts) = walk(mem().read_u32(root + ROOT_MOVIE), known);
            state.counts.pieces = pieces.len();
            state.menus.entry(root).or_default().stepped(steps, pieces);
            state.counts.roots = state.menus.len();
            state.button_parts.insert(root, parts);
        });
    }
    result
}

/// `gameswf::Root::display()`: draws the menu at its in-between placements.
pub fn display(uc: &mut Uc, args: &mut Args) -> Ret {
    let root = args.u32();
    if !enabled() || !readable(root, 0x100) {
        return call_original(uc, DISPLAY, &[root]);
    }
    let period = read_f32(root + ROOT_PERIOD);
    let t = if period > 0.0 { read_f32(root + ROOT_CLOCK) / period } else { 1.0 };
    // Each draw keeps its own changes, so a draw inside it (another menu) leaves them alone;
    // pieces it already swapped don't match their last step any more, so they aren't swapped twice.
    let draw = with_state(|state| {
        let draw = blend_in(state, root, t);
        let counts = &mut state.counts;
        counts.displays += 1;
        counts.clock_sum += t;
        counts.blended += draw.as_ref().map_or(0, |draw| draw.swaps.len() as u32);
        let since = *counts.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed().as_secs() >= 3 {
            crate::log::line(&format!(
                "[flash] {} advances, {} steps, {} menus, {} pieces at the last step; {} draws, {} blended pieces, average clock {:.2}",
                counts.advances, counts.steps, counts.roots, counts.pieces, counts.displays, counts.blended,
                counts.clock_sum / counts.displays.max(1) as f32
            ));
            *counts = Counts { since: Some(std::time::Instant::now()), ..Counts::default() };
        }
        draw
    });
    let result = call_original(uc, DISPLAY, &[root]);
    if let Some(draw) = draw {
        with_state(|state| blend_out(state, root, draw));
    }
    result
}

fn blend_in(state: &mut State, root: u32, t: f32) -> Option<Draw> {
    let memory = mem();
    let frames = state.menus.get(&root)?;
    let mut wanted: Vec<(u32, Seen, Placement)> = Vec::new();
    for (&object, current) in &frames.current {
        let Some(previous) = frames.previous.get(&object) else { continue };
        let Some(live) = look_at(object) else { continue };
        if let Some(placement) = blended(previous, current, &live, t) {
            wanted.push((object, *current, placement));
        }
    }
    if wanted.is_empty() {
        return None;
    }
    // 24 bytes of matrix and 32 of color per piece.
    let scratch = crate::runtime::heap().memalign(8, wanted.len() as u32 * 56);
    if scratch == 0 {
        return None;
    }
    let mut draw = Draw { swaps: Vec::with_capacity(wanted.len()), scratch };
    let mut at = scratch;
    for (object, current, placement) in wanted {
        let mut swap = Swap { object, identity: current.identity, matrix: None, color: None };
        if placement.matrix != current.placement.matrix {
            for (i, value) in placement.matrix.iter().enumerate() {
                memory.write_u32(at + i as u32 * 4, value.to_bits());
            }
            swap.matrix = Some((memory.read_u32(object + MATRIX), at));
            memory.write_u32(object + MATRIX, at);
            memory.write_u8(object + MATRIX_STALE, 1);
        }
        if placement.color != current.placement.color {
            for (i, value) in placement.color.iter().enumerate() {
                memory.write_u32(at + 24 + i as u32 * 4, value.to_bits());
            }
            swap.color = Some((memory.read_u32(object + COLOR), at + 24));
            memory.write_u32(object + COLOR, at + 24);
            memory.write_u8(object + COLOR_STALE, 1);
        }
        at += 56;
        draw.swaps.push(swap);
    }
    mark_button_parts(state, root);
    Some(draw)
}

fn blend_out(state: &mut State, root: u32, draw: Draw) {
    let memory = mem();
    for swap in draw.swaps {
        // A piece the drawing removed (its memory reused or freed: not the same thing any more) is left alone.
        let still = readable(swap.object, 0xb0)
            && (memory.read_u32(swap.object), memory.read_u32(swap.object + ID), u32::from(memory.read_u16(swap.object + DEPTH))) == swap.identity;
        if !still {
            continue;
        }
        // A pointer the game changed while drawing stays as the game left it.
        if let Some((original, copy)) = swap.matrix {
            if memory.read_u32(swap.object + MATRIX) == copy {
                memory.write_u32(swap.object + MATRIX, original);
            }
        }
        if let Some((original, copy)) = swap.color {
            if memory.read_u32(swap.object + COLOR) == copy {
                memory.write_u32(swap.object + COLOR, original);
            }
        }
        memory.write_u8(swap.object + MATRIX_STALE, 1);
        memory.write_u8(swap.object + COLOR_STALE, 1);
    }
    crate::runtime::heap().free(draw.scratch);
    mark_button_parts(state, root);
}

fn mark_button_parts(state: &State, root: u32) {
    let memory = mem();
    for &(part, vtable) in state.button_parts.get(&root).into_iter().flatten() {
        if readable(part, 0xb0) && memory.read_u32(part) == vtable {
            memory.write_u8(part + MATRIX_STALE, 1);
            memory.write_u8(part + COLOR_STALE, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(x: f32, alpha: f32) -> Seen {
        Seen { identity: (1, 2, 3), placement: Placement { matrix: [1.0, 0.0, x, 0.0, 1.0, 0.0], color: [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, alpha, 0.0] } }
    }

    /// A piece the last step moved is shown part of the way; one that
    /// didn't move, changed what it is, or was moved since by something else is left alone.
    #[test]
    fn moved_pieces_are_shown_between_steps() {
        let (previous, current) = (seen(0.0, 0.0), seen(40.0, 1.0));
        let middle = blended(&previous, &current, &current, 0.25).unwrap();
        assert_eq!(middle.matrix[2], 10.0);
        assert_eq!(middle.color[6], 0.25);
        assert_eq!(blended(&previous, &current, &current, 7.0).unwrap(), current.placement);
        assert_eq!(blended(&current, &current, &current, 0.5), None);
        assert_eq!(blended(&previous, &current, &seen(55.0, 1.0), 0.5), None);
        let other = Seen { identity: (9, 2, 3), ..previous };
        assert_eq!(blended(&other, &current, &current, 0.5), None);
    }

    /// A piece that jumped further in one step than anything moves (a
    /// scrolled list putting a row back at its start and showing the next
    /// card in it) is shown where it is, not slid across the jump.
    #[test]
    fn jumps_are_not_slid_across() {
        let fling = (seen(0.0, 1.0), seen(MAX_STEP_MOVE, 1.0));
        assert!(blended(&fling.0, &fling.1, &fling.1, 0.5).is_some());
        let row_back = (seen(0.0, 1.0), seen(-276.0, 1.0));
        assert_eq!(blended(&row_back.0, &row_back.1, &row_back.1, 0.5), None);
        let mut down = seen(0.0, 1.0);
        down.placement.matrix[5] = 276.0;
        assert_eq!(blended(&seen(0.0, 1.0), &down, &down, 0.5), None);
    }

    /// Steps counted from the clock; after one step the last step's pieces
    /// become the previous; after several there is nothing to blend from.
    #[test]
    fn steps_move_the_frames_along() {
        let period = 1.0 / 30.0;
        assert_eq!(steps_taken(0.0, 0.008, period, true), 0);
        assert_eq!(steps_taken(0.030, 0.008, period, true), 1);
        assert_eq!(steps_taken(0.030, 0.040, period, true), 2);
        assert_eq!(steps_taken(0.0, 0.008, 0.0, true), 0);
        // Without its flag, advance takes one step and drops the time left over.
        assert_eq!(steps_taken(0.030, 0.040, period, false), 1);
        let mut frames = Frames::default();
        let one = HashMap::from([(7, seen(0.0, 1.0))]);
        let two = HashMap::from([(7, seen(5.0, 1.0))]);
        frames.stepped(1, one.clone());
        frames.stepped(0, HashMap::new());
        frames.stepped(1, two.clone());
        assert_eq!((&frames.previous, &frames.current), (&one, &two));
        frames.stepped(3, one.clone());
        assert_eq!((&frames.previous, &frames.current), (&one, &one));
    }
}
