//! Frame limiters and the frame rate games are raised to.
//!
//! The same rule as the 32-bit bridge (src/imports/threads.rs, frame_limit):
//! many games sleep after each frame until a fixed frame time has passed. A
//! sleep of at most 100 ms on the thread that shows frames, within 100 ms of
//! its last frame, ends instead one screen refresh after that frame (minus the
//! work the game still does after its sleep), so the limit becomes the
//! screen's. The game's own sleep is never made longer. Kept as a copy because
//! the bridge's version lives inside the translator's state.
//!
//! The one per-game setting is the bridge's: `debug.arm32bridge.unlock.<package>`
//! = 0 keeps the game's own limits, = <fps> caps it there. No setting: 120.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The refresh rate asked for, and the frame rate games are raised to.
pub const TARGET_REFRESH_RATE: f32 = 120.0;

/// Frames further apart than this are not one running game loop.
const NEAR: Duration = Duration::from_millis(100);

struct Pacing {
    /// When the last frame was shown.
    last: Option<Instant>,
    /// When this frame's limiter sleep is due to end.
    sleep_end: Option<Instant>,
    /// Game work after the limiter sleep until the frame was shown, last 8 frames.
    after: VecDeque<Duration>,
}

thread_local! {
    static PACING: RefCell<Pacing> = const { RefCell::new(Pacing { last: None, sleep_end: None, after: VecDeque::new() }) };
}

/// The per-game setting, read once.
pub struct Setting {
    /// The frame rate to raise the game to; None keeps the game's own limits.
    pub fps: Option<f32>,
    /// The frame time limiter sleeps end at (whole screen refreshes).
    frame: Option<Duration>,
}

pub fn setting() -> &'static Setting {
    static SETTING: OnceLock<Setting> = OnceLock::new();
    SETTING.get_or_init(|| {
        let name = format!("debug.arm32bridge.unlock.{}", crate::package());
        let value = crate::property(&name).filter(|value| !value.trim().is_empty());
        let setting = parse_setting(value.as_deref());
        crate::log(&match setting.fps {
            None => format!("frame limits kept ({name}=0)"),
            Some(fps) => format!("frame rate raised to {fps:.0} fps ({name}=0 keeps the game's limits, =<fps> caps)"),
        });
        setting
    })
}

fn parse_setting(value: Option<&str>) -> Setting {
    let refresh = Duration::from_secs_f32(1.0 / TARGET_REFRESH_RATE);
    match value.map(str::trim) {
        Some("0") => Setting { fps: None, frame: None },
        Some(text) => match text.parse::<f64>() {
            Ok(fps) if fps > 1.0 => {
                let refreshes = (1.0 / fps / refresh.as_secs_f64() - 0.01).ceil().max(1.0);
                let frame = refresh.mul_f64(refreshes);
                Setting { fps: Some((1.0 / frame.as_secs_f64()) as f32), frame: Some(frame) }
            }
            _ => Setting { fps: Some(TARGET_REFRESH_RATE), frame: Some(refresh) },
        },
        None => Setting { fps: Some(TARGET_REFRESH_RATE), frame: Some(refresh) },
    }
}

/// Called when this thread has shown a frame (eglSwapBuffers, vkQueuePresentKHR).
pub fn frame_shown() {
    let now = Instant::now();
    PACING.with(|pacing| {
        let pacing = &mut *pacing.borrow_mut();
        if pacing.last.is_some_and(|last| now - last < NEAR) {
            if let Some(end) = pacing.sleep_end {
                if pacing.after.len() == 8 {
                    pacing.after.pop_front();
                }
                pacing.after.push_back(now.saturating_duration_since(end));
            }
        }
        pacing.last = Some(now);
        pacing.sleep_end = None;
    });
}

/// How long a sleep of `wait` on this thread should really last.
pub fn frame_limit(wait: Duration) -> Duration {
    if wait > NEAR {
        return wait;
    }
    let Some(frame) = setting().frame else { return wait };
    PACING.with(|pacing| {
        let pacing = &mut *pacing.borrow_mut();
        let Some(last) = pacing.last else { return wait };
        let now = Instant::now();
        if now - last > NEAR {
            return wait;
        }
        let ahead = after_estimate(&pacing.after).min(frame / 2);
        let sleep = (last + frame - ahead).saturating_duration_since(now).min(wait);
        pacing.sleep_end = Some(now + sleep);
        sleep
    })
}

/// A high estimate of the game's work after its limiter sleep (zero until 3 frames are known).
fn after_estimate(after: &VecDeque<Duration>) -> Duration {
    if after.len() < 3 {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = after.iter().copied().collect();
    sorted.sort_unstable();
    sorted[sorted.len() * 3 / 4]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings() {
        assert_eq!(parse_setting(None).fps, Some(120.0));
        assert_eq!(parse_setting(Some("0")).fps, None);
        // 60 rounds to two refreshes of 120 Hz.
        let sixty = parse_setting(Some("60"));
        assert!((sixty.fps.unwrap() - 60.0).abs() < 0.01);
        assert_eq!(parse_setting(Some("junk")).fps, Some(120.0));
    }

    #[test]
    fn sleeps_off_the_frame_thread_are_kept() {
        // No frame shown on this thread yet.
        assert_eq!(frame_limit(Duration::from_millis(30)), Duration::from_millis(30));
    }

    #[test]
    fn limiter_sleep_ends_one_refresh_after_the_frame() {
        frame_shown();
        let sleep = frame_limit(Duration::from_millis(33));
        assert!(sleep <= Duration::from_secs_f32(1.0 / 120.0), "{sleep:?}");
        // Long sleeps are not limiters.
        assert_eq!(frame_limit(Duration::from_millis(500)), Duration::from_millis(500));
    }
}
