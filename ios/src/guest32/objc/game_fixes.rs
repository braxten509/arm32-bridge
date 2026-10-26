//! A game's own bugs that an iPhone has too, fixed where the owner asked:
//! each fix follows one game method (`follows`, then `after_message`).

use super::bridge::{Bridge, GuestSide};
use super::functions::Ctx;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// Whether `-[class name]` is followed by a fix (the class name only made
/// for a selector a fix follows).
pub(super) fn follows(name: &str, class: impl FnOnce() -> String) -> bool {
    match name {
        "dialogViewDidClose:" => class() == "SCAlertController",
        "beginStageInteraction" => class() == "SCTutorialController",
        _ => false,
    }
}

/// After the game method a fix follows (`follows`): `receiver` got it, with
/// `first` its first argument.
pub(super) fn after_message(bridge: &Mutex<Bridge>, g: &dyn GuestSide, receiver: u32, first: u32) -> Result<(), String> {
    let name = {
        let b = bridge.lock().unwrap_or_else(|e| e.into_inner());
        b.class_display(super::bridge::u32_at(g, receiver)).0
    };
    match name.as_str() {
        "SCAlertController" => after_dialog_closed(bridge, g, receiver, first),
        "SCTutorialController" => after_stage_interaction(bridge, g, receiver),
        _ => Ok(()),
    }
}

/// SpellCraft (owner asked): a tutorial step that waits for an event (the
/// store tab, opened by the game itself) locks the screen to its one control
/// only 2 s later (-[SCTutorialController beginStageInteraction] performs
/// -lockControlsAndPlacePointer after a delay), and opening the tab turned
/// the other tabs back on: a tap on one in those seconds left the step
/// waiting for a hat that was no longer on screen. While that lock waits,
/// every control is off, as -lockOutControlsForTransition: does between
/// steps; the waiting lock turns the step's control on as before.
fn after_stage_interaction(bridge: &Mutex<Bridge>, g: &dyn GuestSide, tutorial: u32) -> Result<(), String> {
    let c = Ctx { bridge, g };
    let host = bridge.lock().unwrap_or_else(|e| e.into_inner()).host_object_for(g, tutorial)?;
    if crate::fw::runloop::perform_pending(host, crate::objc::runtime::sel("lockControlsAndPlacePointer")) {
        c.send(tutorial, "lockOutControlsForTransition:", &[1])?;
    }
    Ok(())
}

/// SpellCraft: a story box can line up a next box that shows only once. When
/// that next box was seen before (a quit in the middle of the tutorial),
/// -[SCAlertController processQueue] skips it without telling anyone, and the
/// tutorial, which waits for the next box to close before it goes on
/// (-[SCTutorialController dialogViewDidClose:]), kept the map locked. A box
/// with a next box closed and none is showing or lined up: the tutorial goes
/// on, as it would have when the skipped box closed.
fn after_dialog_closed(bridge: &Mutex<Bridge>, g: &dyn GuestSide, alert: u32, dialog: u32) -> Result<(), String> {
    let c = Ctx { bridge, g };
    let key = {
        let host = crate::fw::string::make("nextDialogName");
        bridge.lock().unwrap_or_else(|e| e.into_inner()).guest_object(g, host)?
    };
    let next = c.send(c.send(dialog, "spec", &[])?, "stringValueForKey:", &[key])?;
    if c.send(next, "length", &[])? == 0 || c.send(alert, "currentAlert", &[])? != 0 || c.send(c.send(alert, "alertQueue", &[])?, "count", &[])? != 0 {
        return Ok(());
    }
    let Some(class) = bridge.lock().unwrap_or_else(|e| e.into_inner()).classes.class_named("SCTutorialController").map(|c| c.address) else {
        return Ok(());
    };
    let tutorial = c.send(class, "tutorialController", &[])?;
    if c.send(tutorial, "tutorialUnderway", &[])? & 0xff != 0 {
        c.send(tutorial, "beginStageInteraction", &[])?;
    }
    Ok(())
}

/// The host function standing in for SpellCraft's -[SCRenderer renderLoop].
pub const RENDER_LOOP: &str = "gamebridge_spellcraft_render_loop";

/// Each renderer's steps (by its address).
static STEPS: Mutex<Vec<(u32, Steps)>> = Mutex::new(Vec::new());

/// Puts in place the fixes that replace a game method (from
/// UIApplicationMain, once the game's memory is set up).
pub(super) fn install(uc: &mut arm32_bridge::runtime::Uc, bridge: &Mutex<Bridge>) {
    let mut b = bridge.lock().unwrap_or_else(|e| e.into_inner());
    let Some(renderer) = b.classes.class_named("SCRenderer").filter(|c| c.methods.contains_key("renderLoop")).map(|c| c.address) else {
        return;
    };
    // IOS_SPELLCRAFT_HZ=60: the game's own loop (60 Hz).
    if std::env::var("IOS_SPELLCRAFT_HZ").as_deref() == Ok("60") {
        crate::log!("SpellCraft: its own 60 Hz render loop (IOS_SPELLCRAFT_HZ=60)");
        return;
    }
    b.classes.replace_method(renderer, "renderLoop", arm32_bridge::runtime::stub_for(RENDER_LOOP));
    crate::log!("SpellCraft: render loop at the screen's rate ({:.0} Hz), game steps at 60 a second", 1.0 / crate::fw::runloop::refresh_seconds());
    // Draws between steps move animations on part of a step
    // (IOS_SPELLCRAFT_HALF=0: they draw nothing new, as at 60 Hz).
    let render = b.classes.class_named("AEAnimationPlayer").and_then(|c| c.methods.get("render:").map(|m| m.imp));
    drop(b);
    if std::env::var("IOS_SPELLCRAFT_HALF").as_deref() == Ok("0") {
        return;
    }
    match render.map(|imp| install_half_steps(uc, imp)) {
        Some(Ok(())) => {}
        Some(Err(error)) => crate::log!("SpellCraft: animations only move with game steps: {error}"),
        None => crate::log!("SpellCraft: animations only move with game steps: no -[AEAnimationPlayer render:]"),
    }
}

/// SpellCraft's -[SCRenderer renderLoop] at the screen's rate. The game's
/// own (0x34f10) skips while the app is in the background or inactive, sets
/// its heartbeat timer to 1/60 s (1/30 s while scrolling), counts the whole
/// 1/60 s steps since its last tick (one after a stall of more than four),
/// draws the GL view, then runs that many steps (updateContent with
/// frameCount). Here the timer runs at the screen's rate and `Steps` carries
/// the time between ticks, so the game keeps its speed; the steps run first
/// and the view is drawn after them (one tick less behind). A tick with no
/// step to run draws again with the animations moved on part of a step
/// (`node_transform`) once that is in place, and draws nothing otherwise.
pub(super) fn render_loop(c: &Ctx, renderer: u32) -> Result<(), String> {
    if c.send(renderer, "applicationInBackground", &[])? & 0xff != 0 || c.send(renderer, "applicationResignActive", &[])? & 0xff != 0 {
        return Ok(());
    }
    let timer = c.send(renderer, "heartbeatTimer", &[])?;
    c.send(timer, "setInterval:", &[(crate::fw::runloop::refresh_seconds() as f32).to_bits()])?;
    let now = crate::fw::runloop::media_time();
    let (steps, fraction) = {
        let mut all = STEPS.lock().unwrap_or_else(|e| e.into_inner());
        if !all.iter().any(|(r, _)| *r == renderer) {
            all.push((renderer, Steps::default()));
        }
        let (_, steps) = all.iter_mut().find(|(r, _)| *r == renderer).expect("just added");
        (steps.tick(now), steps.fraction())
    };
    if steps == 0 {
        // frameCount stays at the last steps' (1 or more): at 0 renderView
        // would run a step itself.
        if HALF.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            draw(c, renderer, 0, fraction)?;
        }
        return Ok(());
    }
    // NSDate's reference time (2001-01-01), as the game keeps it.
    let since_2001 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) - 978_307_200.0;
    let bits = since_2001.to_bits();
    c.send(renderer, "setLastFrameTimeInSeconds:", &[bits as u32, (bits >> 32) as u32])?;
    c.send(renderer, "setFrameCount:", &[steps])?;
    c.send(renderer, "setDeltaT:", &[(steps as f32 / 60.0).to_bits()])?;
    c.send(renderer, "updateContent", &[])?;
    draw(c, renderer, steps, fraction)
}

/// Draws the GL view after `steps` game steps, animations `fraction` of a
/// step on (`HalfSteps`) only during this draw.
fn draw(c: &Ctx, renderer: u32, steps: u32, fraction: f64) -> Result<(), String> {
    let half = |work: &dyn Fn(&mut HalfSteps)| {
        if let Some((_, _, half)) = HALF.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            work(half);
        }
    };
    half(&|h| h.begin(steps, fraction));
    let view = c.send(renderer, "glView", &[]);
    let drawn = view.and_then(|view| c.send(view, "renderView", &[]));
    half(&|h| h.end());
    drawn.map(|_| ())
}

/// SpellCraft's game steps for its render loop at the screen's rate
/// (`render_loop`): each tick makes the whole 1/60 s steps that time has
/// covered, carrying the rest to the next tick, so the game keeps its speed
/// at any tick rate. After a stall of more than four steps it makes one, as
/// the game's own loop does (it slows down rather than catching up).
#[derive(Default)]
pub struct Steps {
    last: Option<f64>,
    left: f64,
}

impl Steps {
    /// One tick at media time `now`: the game steps to make.
    pub fn tick(&mut self, now: f64) -> u32 {
        let Some(last) = self.last.replace(now) else { return 1 };
        self.left += (now - last).max(0.0);
        if self.left * 60.0 > 4.2 {
            self.left = 0.0;
            return 1;
        }
        let steps = (self.left * 60.0 + 1e-6).floor();
        self.left = (self.left - steps / 60.0).max(0.0);
        steps as u32
    }

    /// How far into the next step the time is (0 to 1).
    pub fn fraction(&self) -> f64 {
        (self.left * 60.0).clamp(0.0, 1.0)
    }
}

/// A Thumb-2 call at `from` to `to`: BLX when `to` is ARM code, else BL.
pub fn thumb_call(from: u32, to: u32, arm: bool) -> [u8; 4] {
    // BL adds to the call's address + 4; BLX to it rounded down to 4.
    let base = if arm { (from + 4) & !3 } else { from + 4 };
    let offset = to.wrapping_sub(base);
    let s = (offset >> 24) & 1;
    let (i1, i2) = ((offset >> 23) & 1, (offset >> 22) & 1);
    let (j1, j2) = ((!(i1 ^ s)) & 1, (!(i2 ^ s)) & 1);
    let first = 0xf000 | (s << 10) | ((offset >> 12) & 0x3ff);
    // 11 J1 1 J2 imm11 (BL); 11 J1 0 J2 imm10L 0 (BLX).
    let low = if arm { 0xc000 | ((offset >> 1) & 0x7fe) } else { 0xd000 | ((offset >> 1) & 0x7ff) };
    let second = low | (j1 << 13) | (j2 << 11);
    ((second << 16) | first).to_le_bytes()
}

/// Where the Thumb-2 BL or BLX in `bytes` (at `from`) calls, and whether
/// it switches to ARM code (BLX); None for another instruction.
pub fn thumb_call_target(bytes: [u8; 4], from: u32) -> Option<(u32, bool)> {
    let first = u32::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let second = u32::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    let arm = match (first >> 11, second >> 14, (second >> 12) & 1) {
        (0b11110, 0b11, 1) => false,
        (0b11110, 0b11, 0) if second & 1 == 0 => true,
        _ => return None,
    };
    let s = (first >> 10) & 1;
    let (j1, j2) = ((second >> 13) & 1, (second >> 11) & 1);
    let (i1, i2) = ((!(j1 ^ s)) & 1, (!(j2 ^ s)) & 1);
    let offset = (s << 24) | (i1 << 23) | (i2 << 22) | ((first & 0x3ff) << 12) | ((second & 0x7ff) << 1);
    // Sign-extend the 25-bit offset.
    let offset = ((offset << 7) as i32 >> 7) as u32;
    let base = if arm { (from + 4) & !3 } else { from + 4 };
    Some((base.wrapping_add(offset), arm))
}

/// Which of SpellCraft's animations are drawn part of a step ahead
/// (`node_transform`), by their animation data and frame: only ones whose
/// frame moved on with the last steps, so a still one stays still.
#[derive(Default)]
pub struct HalfSteps {
    steps: u32,
    fraction: f64,
    /// The (animation, frame) pairs drawn at the last draw after steps, and at this one.
    last: HashSet<(u32, u32)>,
    seen: HashSet<(u32, u32)>,
    /// The pairs at this draw that moved on with the last steps.
    moving: HashSet<(u32, u32)>,
    /// Whether the render loop is drawing (other draws are left as they are).
    drawing: bool,
}

impl HalfSteps {
    /// A draw begins, after `steps` game steps (0: a draw between steps),
    /// `fraction` of a step past the last one.
    pub fn begin(&mut self, steps: u32, fraction: f64) {
        (self.steps, self.fraction, self.drawing) = (steps, fraction.clamp(0.0, 1.0), true);
        if steps > 0 {
            self.last = std::mem::take(&mut self.seen);
            self.moving.clear();
        }
    }

    /// The render loop's draw ended.
    pub fn end(&mut self) {
        self.drawing = false;
    }

    /// How far past its frame an animation at `frame` is drawn (0 to 1).
    pub fn ahead(&mut self, animation: u32, frame: u32) -> f64 {
        if !self.drawing {
            return 0.0;
        }
        if self.steps > 0 {
            self.seen.insert((animation, frame));
            if frame >= self.steps && self.last.contains(&(animation, frame - self.steps)) {
                self.moving.insert((animation, frame));
            }
        }
        if self.moving.contains(&(animation, frame)) {
            self.fraction
        } else {
            0.0
        }
    }
}

/// The host function SpellCraft's -[AEAnimationPlayer render:] calls in
/// place of aecAnimation::nodeTransform (`install_half_steps`).
pub const NODE_TRANSFORM: &str = "gamebridge_spellcraft_node_transform";

/// The call to aecAnimation::nodeTransform(node, frame, out) in
/// -[AEAnimationPlayer render:] (its only one), from the method's start.
const NODE_TRANSFORM_CALL: u32 = 0xbe81e - 0xbe5b4;

/// Where nodeTransform is, and a 24-byte place for a second answer, once
/// the call goes through `node_transform`.
static HALF: Mutex<Option<(u32, u32, HalfSteps)>> = Mutex::new(None);

/// Points SpellCraft's nodeTransform call in -[AEAnimationPlayer render:]
/// at `node_transform`, so the render loop can draw between game steps.
/// The stand-ins are too far for a Thumb call, so the call goes to a jump
/// put in the unused room after the program's load commands. Checks every
/// byte it relies on first and changes nothing if one is not as expected.
fn install_half_steps(uc: &mut arm32_bridge::runtime::Uc, render: u32) -> Result<(), String> {
    let memory = arm32_bridge::runtime::mem();
    let call = (render & !1) + NODE_TRANSFORM_CALL;
    let bytes: [u8; 4] = memory.bytes(call, 4).try_into().expect("4 bytes");
    let Some((target, false)) = thumb_call_target(bytes, call) else {
        return Err(format!("no Thumb call at {call:#x}: {bytes:02x?}"));
    };
    if target.wrapping_sub(call) != 0xca12c - 0xbe81e {
        return Err(format!("the call at {call:#x} goes to {target:#x}, not nodeTransform"));
    }
    let header = crate::guest32::main_image().ok_or("no program loaded")?.0;
    // mach_header: sizeofcmds at 20, load commands from 28. The room after
    // them up to the first section is zero padding.
    let commands_end = header + 28 + memory.read_u32(header + 20);
    let first_section = text_sections(header).into_iter().min().ok_or("no __TEXT sections")?;
    let jump = (commands_end + 15) & !7;
    if jump + 12 > first_section || memory.bytes(jump, 12).iter().any(|&b| b != 0) {
        return Err(format!("no free room for the jump at {jump:#x} (sections from {first_section:#x})"));
    }
    if call.abs_diff(jump) >= 1 << 24 {
        return Err(format!("the jump at {jump:#x} is out of reach of {call:#x}"));
    }
    let scratch = arm32_bridge::runtime::shared().misc_alloc(24, 4);
    *HALF.lock().unwrap_or_else(|e| e.into_inner()) = Some((target, scratch, HalfSteps::default()));
    // ldr.w ip, [pc, #4]; bx ip; nop; .word stand-in (ARM code).
    let stub = arm32_bridge::runtime::stub_for(NODE_TRANSFORM);
    let mut code = Vec::with_capacity(12);
    for half in [0xf8df_u16, 0xc004, 0x4760, 0xbf00] {
        code.extend_from_slice(&half.to_le_bytes());
    }
    code.extend_from_slice(&stub.to_le_bytes());
    memory.write_bytes(jump, &code);
    memory.write_bytes(call, &thumb_call(call, jump, false));
    for (start, end) in [(jump, jump + 12), (call, call + 4)] {
        arm32_bridge::runtime::note_code_change(start, end);
        arm32_bridge::dynarec::invalidate(uc, start, end);
    }
    crate::log!("SpellCraft: animations drawn between game steps (call at {call:#x} through {jump:#x})");
    Ok(())
}

/// The addresses of the program's __TEXT sections, from its header in memory.
fn text_sections(header: u32) -> Vec<u32> {
    let memory = arm32_bridge::runtime::mem();
    let (count, mut command) = (memory.read_u32(header + 16), header + 28);
    let mut sections = Vec::new();
    for _ in 0..count.min(256) {
        let (kind, size) = (memory.read_u32(command), memory.read_u32(command + 4));
        // LC_SEGMENT: segname at 8, nsects at 48, 68-byte sections from 56 (addr at 32).
        if kind == 1 && memory.bytes(command + 8, 7) == b"__TEXT\0" {
            let slide = crate::guest32::main_image().map_or(0, |image| image.1);
            for i in 0..memory.read_u32(command + 48).min(64) {
                sections.push(memory.read_u32(command + 56 + i * 68 + 32).wrapping_add(slide));
            }
        }
        if size < 8 {
            break;
        }
        command += size;
    }
    sections
}

/// SpellCraft's aecAnimation::nodeTransform(animation, node, frame, out):
/// the node's place at its frame, or, in a draw between game steps, that
/// far toward its next frame (the game moves between frames in a straight
/// line too). Its six numbers are x, y, x and y scale, turn (degrees) and
/// opacity (aecSceneRenderer::pushTransform). A jump of more than 64
/// points, 1 in scale or 90 degrees in one frame (a cut, or a loop starting
/// over) is not drawn between.
pub(super) fn node_transform(uc: &mut arm32_bridge::runtime::Uc, animation: u32, node: u32, frame: u32, out: u32) -> Result<(), String> {
    let (function, scratch, ahead) = {
        let mut half = HALF.lock().unwrap_or_else(|e| e.into_inner());
        let (function, scratch, steps) = half.as_mut().ok_or("nodeTransform called before it was moved")?;
        (*function, *scratch, steps.ahead(animation, frame))
    };
    arm32_bridge::runtime::call_guest(uc, function | 1, &[animation, node, frame, out])?;
    // Log lines at the first call and after 5000: how many were drawn ahead.
    static CALLS: AtomicU32 = AtomicU32::new(0);
    static AHEAD: AtomicU32 = AtomicU32::new(0);
    match CALLS.fetch_add(1, Ordering::Relaxed) {
        0 => crate::log!("SpellCraft: first animation placed through the in-between drawing"),
        5000 => crate::log!("SpellCraft: {} of the first 5000 animation places drawn part of a step ahead", AHEAD.load(Ordering::Relaxed)),
        _ => {}
    }
    if ahead <= 0.0 {
        return Ok(());
    }
    AHEAD.fetch_add(1, Ordering::Relaxed);
    arm32_bridge::runtime::call_guest(uc, function | 1, &[animation, node, frame + 1, scratch])?;
    let memory = arm32_bridge::runtime::mem();
    let read = |at: u32| -> [f32; 6] { std::array::from_fn(|i| f32::from_bits(memory.read_u32(at + i as u32 * 4))) };
    let (now, next) = (read(out), read(scratch));
    let near = now.iter().zip(&next).enumerate().all(|(i, (a, b))| a.is_finite() && b.is_finite() && (b - a).abs() <= [64.0, 64.0, 1.0, 1.0, 90.0, 1.0][i]);
    if near {
        for i in 0..6 {
            let value = now[i] + (next[i] - now[i]) * ahead as f32;
            memory.write_u32(out + i as u32 * 4, value.to_bits());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{thumb_call, thumb_call_target, HalfSteps, Steps};

    #[test]
    fn thumb_calls_read_and_write_like_the_listing() {
        // The game's own call at 0xbe81e: bl 0xca12c (nodeTransform).
        assert_eq!(thumb_call_target(0xfc85f00bu32.to_le_bytes(), 0xbe81e), Some((0xca12c, false)));
        assert_eq!(thumb_call(0xbe81e, 0xca12c, false), 0xfc85f00bu32.to_le_bytes());
        // To an ARM stub, backwards, from a 2-aligned place: BLX.
        let blx = thumb_call(0xbe81e, 0x10_0a8, true);
        assert_eq!(thumb_call_target(blx, 0xbe81e), Some((0x10_0a8, true)));
        assert_eq!(thumb_call_target([0, 0xbf, 0, 0xbf], 0x100), None, "two nops are no call");
    }

    #[test]
    fn only_moving_animations_are_drawn_ahead() {
        let mut half = HalfSteps::default();
        half.begin(1, 0.0);
        assert_eq!((half.ahead(0xa, 10), half.ahead(0xb, 3)), (0.0, 0.0), "first seen: not known to move");
        half.begin(0, 0.5);
        assert_eq!(half.ahead(0xa, 10), 0.0);
        half.begin(1, 0.0);
        assert_eq!((half.ahead(0xa, 11), half.ahead(0xb, 3)), (0.0, 0.0), "a step drawn on time");
        half.begin(0, 0.5);
        assert_eq!((half.ahead(0xa, 11), half.ahead(0xb, 3)), (0.5, 0.0), "the moving one half a step ahead, the still one not");
        half.begin(2, 0.25);
        assert_eq!(half.ahead(0xa, 13), 0.25, "two steps at once");
        half.end();
        assert_eq!(half.ahead(0xa, 13), 0.0, "drawn outside the render loop (AEGLAnimationView): as it is");
    }

    fn run(steps: &mut Steps, from: f64, every: f64, ticks: usize) -> Vec<u32> {
        (0..ticks).map(|i| steps.tick(from + i as f64 * every)).collect()
    }

    #[test]
    fn spellcraft_keeps_sixty_steps_a_second_at_any_tick_rate() {
        let mut steps = Steps::default();
        let made = run(&mut steps, 10.0, 1.0 / 120.0, 241);
        assert_eq!(made[0], 1, "the first tick makes one step, as the game's loop");
        assert_eq!(made[1..].iter().sum::<u32>(), 120, "two seconds at 120 Hz: 120 steps");
        assert!(made[1..].iter().all(|&n| n <= 1), "never two steps in one 120 Hz tick: {made:?}");
        let mut steps = Steps::default();
        let made = run(&mut steps, 10.0, 1.0 / 60.0, 121);
        assert_eq!(made[1..].iter().sum::<u32>(), 120, "at 60 Hz the same");
        // Uneven ticks still add up.
        let mut steps = Steps::default();
        let times = [0.0, 0.010, 0.013, 0.031, 0.040, 0.058, 0.0666667];
        let made: u32 = times.iter().map(|t| steps.tick(5.0 + t)).sum::<u32>() - 1;
        assert_eq!(made, 4, "{times:?}");
        // A half-second stall: one step, no catching up after.
        let n = steps.tick(5.6);
        assert_eq!((n, steps.tick(5.6 + 1.0 / 60.0)), (1, 1));
    }
}
