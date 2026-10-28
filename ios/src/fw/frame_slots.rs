//! When a shown OpenGL frame reaches the screen. An iPhone shows the newest
//! frame at each screen refresh: two frames made within one refresh show
//! only the second. A lone drawer's frame goes to the screen as soon as it
//! is drawn, as an iPhone's presentRenderbuffer: (Android shows it at the
//! next refresh; held back, it often missed that refresh). Once frames from
//! the main thread have met within half a refresh in two refreshes running
//! (two drawers taking turns: SpellCraft's old and new screens while they
//! cross-fade), each one waits in the window for half a refresh (eagl.rs
//! swaps it then, from the run loop); a newer one made meanwhile is drawn
//! over it and waits in its place, so the one that follows the other closely
//! always wins. Frames from other threads reach the screen at once, as
//! before (no run loop of ours swaps for them).

/// The frame waiting in the window, by when it was drawn (seconds).
pub struct FrameSlots {
    waiting: Option<f64>,
    /// How long a frame waits for a newer one: half a refresh.
    hold: f64,
    /// When the last main-thread frame was drawn, when two last met, and
    /// until when frames wait (two drawers share the window).
    last_drawn: Option<f64>,
    last_met: Option<f64>,
    crowded_until: f64,
}

impl FrameSlots {
    /// For a screen refreshing every `refresh` seconds.
    pub fn new(refresh: f64) -> FrameSlots {
        FrameSlots { waiting: None, hold: refresh / 2.0, last_drawn: None, last_met: None, crowded_until: f64::NEG_INFINITY }
    }

    /// Before a frame made at `now` is drawn into the window: whether the
    /// waiting one must reach the screen first (it has waited its time). A
    /// waiting frame still within its time is drawn over.
    pub fn before_draw(&mut self, now: f64) -> bool {
        match self.due_at() {
            Some(due) if now >= due => {
                self.waiting = None;
                true
            }
            _ => false,
        }
    }

    /// After it is drawn: whether it goes to the screen now (not from the
    /// main thread, or no other drawer shares the window), else it waits.
    pub fn after_draw(&mut self, now: f64, main_thread: bool) -> bool {
        if main_thread {
            if self.last_drawn.is_some_and(|last| now - last < self.hold) {
                // Met in this refresh and the one before: two drawers. One
                // late frame meeting the next on-time one is no reason to wait.
                if self.last_met.is_some_and(|met| now - met <= 3.0 * self.hold) {
                    self.crowded_until = now + IDLE;
                }
                self.last_met = Some(now);
            }
            self.last_drawn = Some(now);
        }
        // Shown now, it takes along anything drawn under it.
        let wait = main_thread && now < self.crowded_until;
        self.waiting = wait.then_some(now);
        !wait
    }

    /// At `now` (from the run loop): whether the waiting frame is due on the
    /// screen.
    pub fn due(&mut self, now: f64) -> bool {
        self.before_draw(now)
    }

    /// When the waiting frame is due, if one waits.
    pub fn due_at(&self) -> Option<f64> {
        self.waiting.map(|drawn| drawn + self.hold)
    }

    /// The window went away: nothing waits for it any more.
    pub fn forget(&mut self) {
        self.waiting = None;
    }
}

/// Which of the game's picture buffers reaches the window. On an iPhone each
/// buffer belongs to its own layer, and a screen switch cross-fades the old
/// screen's layer out and the new one's in; here every buffer's frames go to
/// the one window, so while both screens draw they would take turns in it.
/// The buffer that began showing frames most recently has the window; one
/// that has shown nothing for a while (`IDLE`) takes it back when it starts
/// again.
pub struct FrameSources {
    /// The source (its layer, or context and renderbuffer) that has the
    /// window, and when each source last showed a frame.
    front: Option<(usize, u32)>,
    last: Vec<((usize, u32), f64)>,
}

/// How long a buffer that shows nothing counts as stopped (seconds).
const IDLE: f64 = 0.25;

impl FrameSources {
    pub const fn new() -> FrameSources {
        FrameSources { front: None, last: Vec::new() }
    }

    /// A frame from `buffer` at `now`: whether it reaches the window.
    /// `on_screen`: its layer is in a window (an iPhone shows nothing of a
    /// layer that isn't; such a buffer is not counted as drawing).
    pub fn shows(&mut self, buffer: (usize, u32), now: f64, on_screen: bool) -> bool {
        if !on_screen {
            return false;
        }
        let before = self.last.iter().find(|(b, _)| *b == buffer).map(|&(_, at)| at);
        match self.last.iter_mut().find(|(b, _)| *b == buffer) {
            Some(entry) => entry.1 = now,
            None => self.last.push((buffer, now)),
        }
        self.last.retain(|&(_, at)| now - at <= IDLE);
        let starting = before.is_none_or(|at| now - at > IDLE);
        let front_drawing = self.front.is_some_and(|front| self.last.iter().any(|(b, _)| *b == front));
        if starting || !front_drawing || self.front == Some(buffer) {
            self.front = Some(buffer);
            return true;
        }
        false
    }

    /// The source whose frames the window shows last.
    pub fn front(&self) -> Option<(usize, u32)> {
        self.front
    }

    /// `buffer`'s frame could not reach the window after all (another
    /// thread holds it): it lets go, so the one before shows again.
    pub fn let_go(&mut self, buffer: (usize, u32)) {
        self.last.retain(|(b, _)| *b != buffer);
        if self.front == Some(buffer) {
            self.front = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two drawers taking turns at 100 frames a second, the second 2 ms
    /// after the first: once they have met in two refreshes running, only
    /// the second's frames reach the screen, each once; frames from other
    /// threads go at once.
    #[test]
    fn only_the_newest_of_close_frames_reaches_the_screen() {
        let mut slots = FrameSlots::new(0.010);
        let mut shown = Vec::new();
        let mut waiting = "";
        for tick in 0..6 {
            let start = f64::from(tick) * 0.010;
            for (name, at) in [("old", start), ("new", start + 0.002)] {
                // The run loop looks in before each drawer runs.
                if slots.due(at) || slots.before_draw(at) {
                    shown.push((tick, waiting));
                }
                if slots.after_draw(at, true) {
                    shown.push((tick, name));
                } else {
                    waiting = name;
                }
                if slots.due(at + 0.001) {
                    shown.push((tick, waiting));
                }
            }
            if tick >= 2 {
                assert!((slots.due_at().unwrap() - (start + 0.007)).abs() < 1e-9);
            }
            if slots.due(start + 0.0071) {
                shown.push((tick, waiting));
            }
        }
        let after_meeting: Vec<_> = shown.into_iter().filter(|&(tick, _)| tick >= 2).collect();
        assert_eq!(after_meeting, [(2, "new"), (3, "new"), (4, "new"), (5, "new")]);
        // Another thread's frame goes at once and leaves nothing waiting.
        assert!(slots.after_draw(1.011, false));
        assert_eq!(slots.due_at(), None);
        assert!(!slots.due(2.0));
    }

    /// One drawer at the screen's rate (Dungeon Plunder's walk): each frame
    /// goes to the screen as soon as it is drawn. Held half a refresh, it
    /// reached Android so late that about 10 a second missed their refresh.
    /// Now and then a frame comes 6 ms late and meets the next on-time one:
    /// still one drawer.
    #[test]
    fn a_lone_drawers_frames_are_not_held() {
        let mut slots = FrameSlots::new(0.010);
        for tick in 0..30 {
            let at = 5.0 + f64::from(tick) * 0.010 + if tick % 6 == 0 { 0.006 } else { 0.0 };
            assert!(!slots.before_draw(at));
            assert!(slots.after_draw(at, true), "frame {tick} goes at once");
            assert_eq!(slots.due_at(), None);
        }
    }

    /// SpellCraft's screen switch: the old screen's buffer keeps drawing
    /// through the 0.5 s cross-fade while the new one starts; from its first
    /// frame only the new one is shown. Going back, the first buffer (idle
    /// meanwhile) starts again and has the window from then on.
    #[test]
    fn the_newest_drawing_buffer_has_the_window() {
        let mut sources = FrameSources::new();
        let mut shown = Vec::new();
        let draw = |sources: &mut FrameSources, shown: &mut Vec<(u32, i32)>, buffer: u32, at: f64| {
            if sources.shows((0, buffer), at, true) {
                shown.push((buffer, (at * 1000.0).round() as i32));
            }
        };
        // Hall (1) alone, then the garden (2) starts at 100 ms; the two take
        // turns until the hall stops at 600 ms; the garden goes on.
        for i in 0..125 {
            let at = f64::from(i) * 0.008;
            if at < 0.6 {
                draw(&mut sources, &mut shown, 1, at);
            }
            if at >= 0.1 {
                draw(&mut sources, &mut shown, 2, at + 0.004);
            }
        }
        let garden_from = shown.iter().find(|(b, _)| *b == 2).map(|&(_, ms)| ms).unwrap_or(i32::MAX);
        assert!(shown.iter().filter(|(b, _)| *b == 1).all(|&(_, ms)| ms < garden_from), "{shown:?}");
        assert!(shown.iter().any(|&(b, ms)| b == 2 && ms > 590));
        // Back to the hall at 1 s: both draw for a while, then the garden stops.
        shown.clear();
        for i in 0..75 {
            let at = 1.0 + f64::from(i) * 0.008;
            draw(&mut sources, &mut shown, 1, at);
            if at < 1.5 {
                draw(&mut sources, &mut shown, 2, at + 0.004);
            }
        }
        assert!(shown.iter().all(|(b, _)| *b == 1), "{shown:?}");
        assert_eq!(shown.len(), 75);
    }

    /// A buffer whose layer left the window (an old screen that keeps
    /// drawing) is never shown and can't take the window back when the
    /// shown one pauses; a buffer that could not take the window lets go.
    #[test]
    fn only_buffers_on_screen_and_shown_keep_the_window() {
        let mut sources = FrameSources::new();
        assert!(sources.shows((0, 1), 0.0, true));
        assert!(sources.shows((0, 2), 0.004, true));
        // The old screen's layer is out of the window; the new one pauses.
        assert!(!sources.shows((0, 1), 0.008, false));
        assert!(!sources.shows((0, 1), 0.5, false));
        assert!(sources.shows((0, 2), 0.6, true));
        // A newcomer that could not take the window lets go of it.
        assert!(sources.shows((0, 3), 0.604, true));
        sources.let_go((0, 3));
        assert!(sources.shows((0, 2), 0.608, true), "the shown one goes on");
    }
}
