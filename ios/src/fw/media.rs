//! Sound session (C and AVAudioSession) and the video player.
//! Videos play in the phone app's own player, over the game.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::sync::OnceLock;

fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// The interruption listener given to AudioSessionInitialize (function, its
/// data): void listener(void *data, UInt32 state).
static LISTENER: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);

extern "C" fn audio_session_initialize(_run_loop: usize, _mode: usize, listener: usize, data: usize) -> i32 {
    if listener != 0 {
        *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some((listener, data));
    }
    0
}

// ----- interruptions -----

/// The sound session is interrupted (between begin and end).
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From the phone app: Android's audio focus went (a call, another app's
/// sound) or came back. On iOS that is an interruption of the app's sound
/// session; `should_resume` (at the end) says the sound may start again, as
/// after a call. Delivered on the main loop, where iOS tells the app.
pub fn audio_interruption(began: bool, should_resume: bool) {
    crate::log!("sound: interruption {}", if began { "began" } else { "ended" });
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || interrupt_now(began, should_resume))));
}

/// An interruption begins or ends, as iOS 6 tells it: the C listener
/// (kAudioSessionBeginInterruption 1, kAudioSessionEndInterruption 0), the
/// AVAudioSession delegate (beginInterruption; endInterruptionWithFlags:
/// with AVAudioSessionInterruptionFlags_ShouldResume, else endInterruption),
/// then AVAudioSessionInterruptionNotification (type Began 1 / Ended 0;
/// at the end the option ShouldResume 1 or 0). Nothing for a begin while
/// interrupted or an end without one.
fn interrupt_now(began: bool, should_resume: bool) {
    use std::sync::atomic::Ordering;
    if INTERRUPTED.swap(began, Ordering::SeqCst) == began {
        return;
    }
    let listener = *LISTENER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((function, data)) = listener {
        let call: extern "C" fn(usize, u32) = unsafe { std::mem::transmute(function) };
        call(data, began as u32);
    }
    let session = session_shared(0, 0);
    let delegate = property_get(session, "delegate").unwrap_or(0) as Id;
    if delegate != 0 {
        if began {
            if runtime::responds(delegate, "beginInterruption") {
                runtime::send(delegate, "beginInterruption", &[]);
            }
        } else if runtime::responds(delegate, "endInterruptionWithFlags:") {
            runtime::send(delegate, "endInterruptionWithFlags:", &[should_resume as usize]);
        } else if runtime::responds(delegate, "endInterruption") {
            runtime::send(delegate, "endInterruption", &[]);
        }
    }
    let mut info = vec![(string::make("AVAudioSessionInterruptionTypeKey"), collections::number_int(began as i64))];
    if !began {
        info.push((string::make("AVAudioSessionInterruptionOptionKey"), collections::number_int(should_resume as i64)));
    }
    crate::fw::foundation::post("AVAudioSessionInterruptionNotification", session, collections::dictionary_from(&info));
}

/// AudioSessionGetProperty(id, *size, *data).
extern "C" fn audio_session_get_property(id: u32, size: *mut u32, data: *mut u8) -> i32 {
    unsafe {
        let write = |bytes: &[u8]| {
            if !size.is_null() && (*size as usize) >= bytes.len() && !data.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
                *size = bytes.len() as u32;
            }
        };
        // Other audio playing, audio input (microphone) available: no.
        if id == four(b"othr") || id == four(b"aiav") {
            write(&0u32.to_le_bytes());
        } else if id == four(b"chsr") {
            write(&44100.0f64.to_le_bytes());
        } else if id == four(b"cbuf") || id == four(b"chbd") {
            // The preferred and current I/O buffer: 512 frames at 44100 Hz.
            write(&0.0116f32.to_le_bytes());
        } else if id == four(b"choc") {
            write(&2u32.to_le_bytes());
        } else if id == four(b"chic") {
            write(&0u32.to_le_bytes());
        } else if id == four(b"acat") {
            write(&four(b"ambi").to_le_bytes());
        } else if id == four(b"rout") {
            write(&leak_string("Speaker").to_le_bytes());
        } else {
            let text: String = id.to_be_bytes().iter().map(|&b| b as char).collect();
            crate::log!("AudioSessionGetProperty '{text}': not known");
            return four(b"pty?") as i32;
        }
    }
    0
}

fn leak_string(text: &str) -> usize {
    string::make(text)
}

extern "C" fn audio_session_ok_3(_a: usize, _b: usize, _c: usize) -> i32 {
    0
}

extern "C" fn audio_session_set_active(_active: u8) -> i32 {
    0
}

// ----- AVAudioSession -----

static SESSION: OnceLock<Id> = OnceLock::new();

extern "C" fn session_shared(_: Class, _: Sel) -> Id {
    *SESSION.get_or_init(|| runtime::alloc_instance(runtime::class_named("AVAudioSession")))
}

extern "C" fn session_set_category(this: Id, _: Sel, category: Id, _error: usize) -> bool {
    property_set(this, "category", category as u64);
    true
}

extern "C" fn session_set_category_options(this: Id, s: Sel, category: Id, _options: usize, error: usize) -> bool {
    session_set_category(this, s, category, error)
}

extern "C" fn session_category(this: Id, _: Sel) -> Id {
    property_get(this, "category").unwrap_or(0) as Id
}

extern "C" fn yes_with_error(_this: Id, _: Sel, _value: usize, _error: usize) -> bool {
    true
}

extern "C" fn yes_with_options_error(_this: Id, _: Sel, _value: usize, _options: usize, _error: usize) -> bool {
    true
}

extern "C" fn yes_with_double_error(_this: Id, _: Sel, _value: f64, _error: usize) -> bool {
    true
}

extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}

extern "C" fn sample_rate(_this: Id, _: Sel) -> f64 {
    44100.0
}

extern "C" fn io_buffer_duration(_this: Id, _: Sel) -> f64 {
    0.0116
}

extern "C" fn output_volume(_this: Id, _: Sel) -> f32 {
    1.0
}

extern "C" fn two(_this: Id, _: Sel) -> isize {
    2
}

extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}

/// The session's delegate (AVAudioSessionDelegate) hears interruptions; not retained, as Apple's.
extern "C" fn session_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
}

extern "C" fn session_delegate(this: Id, _: Sel) -> Id {
    property_get(this, "delegate").unwrap_or(0) as Id
}

// ----- NSURL-based video: AVURLAsset, AVPlayerItem, AVPlayer, AVPlayerLayer -----

extern "C" fn asset_with_url(class: Class, _: Sel, url: Id, _options: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "url", url as u64);
    object
}

extern "C" fn item_with_asset(class: Class, _: Sel, asset: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "asset", asset as u64);
    object
}

extern "C" fn item_with_url(class: Class, _: Sel, url: Id) -> Id {
    let asset = runtime::alloc_instance(runtime::class_named("AVURLAsset"));
    property_set(asset, "url", url as u64);
    let object = runtime::alloc_instance(class);
    property_set(object, "asset", asset as u64);
    object
}

extern "C" fn item_asset(this: Id, _: Sel) -> Id {
    property_get(this, "asset").unwrap_or(0) as Id
}

/// AVPlayerItemStatusReadyToPlay.
extern "C" fn ready_status(_this: Id, _: Sel) -> isize {
    1
}

extern "C" fn player_with_item(class: Class, _: Sel, item: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "item", item as u64);
    object
}

extern "C" fn player_init_with_item(this: Id, _: Sel, item: Id) -> Id {
    property_set(this, "item", item as u64);
    this
}

extern "C" fn player_item(this: Id, _: Sel) -> Id {
    property_get(this, "item").unwrap_or(0) as Id
}

extern "C" fn player_replace_item(this: Id, _: Sel, item: Id) {
    if player_item(this, 0) != item {
        close_video_of(this);
    }
    property_set(this, "item", item as u64);
}

// Videos are shown by the phone app (over the game, with their own sound);
// without it (PC and phone tests) they are skipped: the game is told at
// once that they ended. Every call carries the video's token, so a late
// call about an older video never touches a newer one.

/// The phone app's video player.
pub struct VideoScreen {
    /// Shows a video from its start; false if it can't.
    pub show: fn(&str, u64) -> bool,
    pub pause: fn(u64),
    pub resume: fn(u64),
    /// Takes the video off the screen.
    pub stop: fn(u64),
}

static VIDEO_SCREEN: std::sync::OnceLock<VideoScreen> = std::sync::OnceLock::new();

pub fn set_video_screen(screen: VideoScreen) {
    let _ = VIDEO_SCREEN.set(screen);
}

#[derive(Clone, Copy)]
struct Showing {
    token: u64,
    player: Id,
    item: Id,
    paused: bool,
}

/// The video on screen. Held while calling the phone app, so its calls
/// arrive in the same order as the changes here.
static SHOWING: std::sync::Mutex<Option<Showing>> = std::sync::Mutex::new(None);

fn showing() -> std::sync::MutexGuard<'static, Option<Showing>> {
    SHOWING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the phone's video player shows a video now (it sits under the
/// iPhone pictures, so pictures.rs leaves it uncovered).
pub fn video_showing() -> bool {
    showing().is_some()
}

/// The phone app: the video with this token played to its end (or failed).
pub fn video_ended(token: u64) {
    let finished = {
        let mut showing = showing();
        match *showing {
            Some(s) if s.token == token => {
                *showing = None;
                Some((s.player, s.item))
            }
            _ => None,
        }
    };
    if let Some((player, item)) = finished {
        tell_ended(player, item);
    }
}

/// Takes `player`'s video off the screen (the game let go of it).
fn close_video_of(player: Id) {
    let mut showing = showing();
    if let Some(s) = *showing {
        if s.player == player {
            *showing = None;
            if let Some(screen) = VIDEO_SCREEN.get() {
                (screen.stop)(s.token);
            }
            crate::log!("video closed");
        }
    }
}

/// Whether `view` shows a video: a movie player's view, or a view whose layer
/// is an AVPlayerLayer with a player. As on iOS it covers what is under it
/// while it is on screen, also before and after the video plays.
pub fn shows_video(view: Id) -> bool {
    if property_get(view, "moviePlayerOf").is_some() {
        return true;
    }
    layer_shows_video(crate::fw::uikit::view_state(view).layer)
}

/// Whether a layer is an AVPlayerLayer with a player (covers what is under it).
pub fn layer_shows_video(layer: Id) -> bool {
    layer != 0
        && property_get(layer, "player").is_some_and(|player| player != 0)
        && runtime::send(layer, "isKindOfClass:", &[runtime::class_named("AVPlayerLayer") as Id]) & 0xFF != 0
}

/// UIView removeFromSuperview: a view showing a player's picture is gone.
pub fn view_removed(view: Id) {
    if showing().is_none() {
        return;
    }
    if let Some(player) = property_get(view, "moviePlayerOf") {
        return close_video_of(player as Id);
    }
    if !runtime::responds(view, "layer") {
        return;
    }
    let layer = runtime::send(view, "layer", &[]);
    if layer != 0 && runtime::send(layer, "isKindOfClass:", &[runtime::class_named("AVPlayerLayer") as Id]) & 0xFF != 0 {
        let player = property_get(layer, "player").unwrap_or(0) as Id;
        if player != 0 {
            close_video_of(player);
        }
    }
}

fn tell_ended(player: Id, item: Id) {
    if property_get(player, "moviePlayer").is_some() {
        return movie_player_ended(player);
    }
    property_set(player, "rate", 0);
    struct Ended(Id);
    let ended = Box::into_raw(Box::new(Ended(item))) as usize;
    let block = make_callback(ended, |data| {
        let item = unsafe { Box::from_raw(data as *mut Ended) }.0;
        crate::fw::foundation::post("AVPlayerItemDidPlayToEndTimeNotification", item, 0);
    });
    crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
}

extern "C" fn player_play(this: Id, _: Sel) {
    let item = player_item(this, 0);
    let asset = property_get(item, "asset").unwrap_or(0) as Id;
    let url = property_get(asset, "url").unwrap_or(0) as Id;
    show_video(this, item, &string::get(runtime::send(url, "path", &[])));
}

/// Shows the video at `path` for `player` (an AVPlayer with its item, or a
/// movie player controller, which is its own item), or carries on with it
/// when it is already this player's; without a video screen the game is
/// told at once that it ended.
fn show_video(player: Id, item: Id, path: &str) {
    if let Some(screen) = VIDEO_SCREEN.get() {
        let mut showing = showing();
        // Already this player's video: carry on where it is.
        if let Some(s) = showing.as_mut() {
            if s.player == player && s.item == item {
                if s.paused {
                    s.paused = false;
                    (screen.resume)(s.token);
                }
                property_set(player, "rate", 1f32.to_bits() as u64);
                return;
            }
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(old) = showing.take() {
            (screen.stop)(old.token);
        }
        if (screen.show)(path, token) {
            *showing = Some(Showing { token, player, item, paused: false });
            property_set(player, "rate", 1f32.to_bits() as u64);
            crate::log!("video {path} playing");
            return;
        }
        drop(showing);
        crate::log!("video {path} could not be shown; telling the game it ended");
        tell_ended(player, item);
        return;
    }
    crate::log!("video {path} skipped (no screen for videos here); telling the game it ended");
    tell_ended(player, item);
}

/// A tiny block (Apple's closure object) that calls a Rust function once.
pub fn make_callback(data: usize, f: fn(usize)) -> usize {
    #[repr(C)]
    struct Callback {
        isa: usize,
        flags: i32,
        reserved: i32,
        invoke: usize,
        descriptor: usize,
        f: fn(usize),
        data: usize,
    }
    extern "C" fn invoke(block: *const Callback) {
        unsafe { ((*block).f)((*block).data) }
    }
    static DESCRIPTOR: [usize; 2] = [0, std::mem::size_of::<Callback>()];
    leak(Callback {
        isa: crate::libsystem::mach::MALLOC_BLOCK_CLASS.load(std::sync::atomic::Ordering::Relaxed),
        flags: 1 << 28,
        reserved: 0,
        invoke: invoke as usize,
        descriptor: DESCRIPTOR.as_ptr() as usize,
        f,
        data,
    })
}

extern "C" fn player_pause(this: Id, _: Sel) {
    property_set(this, "rate", 0);
    let mut showing = showing();
    if let Some(s) = showing.as_mut() {
        if s.player == this && !s.paused {
            s.paused = true;
            if let Some(screen) = VIDEO_SCREEN.get() {
                (screen.pause)(s.token);
            }
        }
    }
}

extern "C" fn player_rate(this: Id, _: Sel) -> f32 {
    f32::from_bits(property_get(this, "rate").unwrap_or(0) as u32)
}

extern "C" fn player_set_rate(this: Id, _: Sel, rate: f32) {
    if rate != 0.0 {
        player_play(this, 0);
    } else {
        player_pause(this, 0);
    }
}

extern "C" fn player_ignore_float(_this: Id, _: Sel, _value: f32) {}
extern "C" fn player_ignore_int(_this: Id, _: Sel, _value: isize) {}

/// CMTime is 24 bytes: returned through memory (x8).
#[repr(C)]
pub struct CmTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

extern "C" fn zero_time(_this: Id, _: Sel) -> CmTime {
    CmTime { value: 0, timescale: 600, flags: 1, epoch: 0 }
}

/// CMTimeGetSeconds: NaN for an invalid or indefinite time, +/- infinity for
/// the infinite ones (flags: 1 valid, 4 +inf, 8 -inf, 16 indefinite).
extern "C" fn cm_time_get_seconds(time: CmTime) -> f64 {
    if time.flags & 1 == 0 || time.flags & 16 != 0 {
        f64::NAN
    } else if time.flags & 4 != 0 {
        f64::INFINITY
    } else if time.flags & 8 != 0 {
        f64::NEG_INFINITY
    } else if time.timescale == 0 {
        f64::NAN
    } else {
        time.value as f64 / time.timescale as f64
    }
}

extern "C" fn layer_set_player(this: Id, _: Sel, player: Id) {
    let old = property_get(this, "player").unwrap_or(0) as Id;
    if old != 0 && old != player {
        close_video_of(old);
    }
    property_set(this, "player", player as u64);
}

extern "C" fn layer_player(this: Id, _: Sel) -> Id {
    property_get(this, "player").unwrap_or(0) as Id
}

extern "C" fn layer_with_player(class: Class, _: Sel, player: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "player", player as u64);
    object
}

extern "C" fn item_add_observer(this: Id, s: Sel, observer: Id, key: Id, options: usize, context: usize) {
    let generic: extern "C" fn(Id, Sel, Id, Id, usize, usize) = unsafe {
        std::mem::transmute(object_method("addObserver:forKeyPath:options:context:"))
    };
    generic(this, s, observer, key, options, context);
    if string::get(key) == "status" {
        let data = this;
        let block = make_callback(data, |item| {
            // AVPlayerItemStatusUnknown (0) to ReadyToPlay (1).
            crate::fw::object::kvo_notify(item, "status", collections::number_int(0), collections::number_int(1));
        });
        crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
    }
}

/// NSObject's own version of an instance method (skipping overrides).
fn object_method(name: &str) -> usize {
    let mut rt = runtime::lock();
    let s = rt.sel(name);
    let object = rt.by_name["NSObject"];
    rt.lookup(object, s).expect("NSObject method")
}

// ----- MPMoviePlayerController (MediaPlayer framework) -----
// The older full-screen movie player: shown through the same video screen
// as AVPlayer. It tells the game a movie finished with
// MPMoviePlayerPlaybackDidFinishNotification, or starts it again when it
// repeats (MPMovieRepeatModeOne).

const MOVIE_STOPPED: u64 = 0;
const MOVIE_PLAYING: u64 = 1;
const MOVIE_PAUSED: u64 = 2;

extern "C" fn movie_init_with_url(this: Id, _: Sel, url: Id) -> Id {
    property_set(this, "moviePlayer", 1);
    property_set(this, "url", url as u64);
    this
}

extern "C" fn movie_url(this: Id, _: Sel) -> Id {
    property_get(this, "url").unwrap_or(0) as Id
}

/// setContentURL: the movie playing stops (said so) and the new one loads
/// again; one that was playing starts the new movie once loaded (a later
/// turn) when shouldAutoplay, as MediaPlayer does.
extern "C" fn movie_set_url(this: Id, _: Sel, url: Id) {
    let was_playing = property_get(this, "playbackState") == Some(MOVIE_PLAYING);
    if property_get(this, "playbackState").unwrap_or(MOVIE_STOPPED) != MOVIE_STOPPED {
        movie_stop(this, 0);
    }
    property_set(this, "url", url as u64);
    property_set(this, "loadTold", 0);
    if was_playing && property_get(this, "shouldAutoplay").unwrap_or(1) != 0 {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            if property_get(this, "playbackState").unwrap_or(MOVIE_STOPPED) == MOVIE_STOPPED {
                movie_play(this, 0);
            }
        })));
    }
}

#[cfg(test)]
thread_local! {
    /// A test takes the notices at once: running the main loop to get them
    /// would run other tests' queued work too.
    static NOTICES_AT_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Posts a movie player notification on a later turn, as MediaPlayer does.
fn movie_notice_later(player: Id, name: &'static str) {
    #[cfg(test)]
    if NOTICES_AT_ONCE.with(|n| n.get()) {
        return crate::fw::foundation::post(name, player, 0);
    }
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || crate::fw::foundation::post(name, player, 0))));
}

/// The movie is loaded (here at once): its load state, duration and
/// preload are announced once per movie.
fn movie_loaded(player: Id) {
    if property_get(player, "loadTold").unwrap_or(0) != 0 {
        return;
    }
    property_set(player, "loadTold", 1);
    for name in ["MPMoviePlayerLoadStateDidChangeNotification", "MPMovieDurationAvailableNotification", "MPMoviePlayerContentPreloadDidFinishNotification"] {
        movie_notice_later(player, name);
    }
}

/// A new playback state, announced when it changed.
fn set_movie_state(player: Id, state: u64) {
    let old = property_get(player, "playbackState").unwrap_or(MOVIE_STOPPED);
    property_set(player, "playbackState", state);
    if old != state {
        movie_notice_later(player, "MPMoviePlayerPlaybackStateDidChangeNotification");
    }
}

fn movie_path(player: Id) -> String {
    let url = movie_url(player, 0);
    if url == 0 { String::new() } else { string::get(runtime::send(url, "path", &[])) }
}

extern "C" fn movie_play(this: Id, _: Sel) {
    property_set(this, "moviePlayer", 1);
    movie_loaded(this);
    set_movie_state(this, MOVIE_PLAYING);
    show_video(this, this, &movie_path(this));
}

extern "C" fn movie_prepare(this: Id, _: Sel) {
    // shouldAutoplay (YES unless the game says no) starts it once ready.
    if property_get(this, "shouldAutoplay").unwrap_or(1) != 0 && property_get(this, "playbackState").unwrap_or(MOVIE_STOPPED) == MOVIE_STOPPED {
        return movie_play(this, 0);
    }
    movie_loaded(this);
}

extern "C" fn movie_pause(this: Id, s: Sel) {
    set_movie_state(this, MOVIE_PAUSED);
    player_pause(this, s);
}

extern "C" fn movie_stop(this: Id, _: Sel) {
    set_movie_state(this, MOVIE_STOPPED);
    property_set(this, "rate", 0);
    close_video_of(this);
}

/// The movie played to its end: again when it repeats, else the game hears
/// MPMoviePlayerPlaybackDidFinishNotification (reason: playback ended).
fn movie_player_ended(player: Id) {
    struct Ended(Id);
    let ended = Box::into_raw(Box::new(Ended(player))) as usize;
    let block = make_callback(ended, |data| {
        let player = unsafe { Box::from_raw(data as *mut Ended) }.0;
        if property_get(player, "playbackState") != Some(MOVIE_PLAYING) {
            return;
        }
        if property_get(player, "repeatMode") == Some(1) {
            return show_video(player, player, &movie_path(player));
        }
        set_movie_state(player, MOVIE_STOPPED);
        let info = crate::fw::collections::dictionary_from(&[(
            string::make("MPMoviePlayerPlaybackDidFinishReasonUserInfoKey"),
            crate::fw::collections::number_int(0),
        )]);
        crate::fw::foundation::post("MPMoviePlayerPlaybackDidFinishNotification", player, info);
    });
    crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
}

/// The movie's view (and background view): a plain view over the screen;
/// taking it off the screen takes the movie off too.
fn movie_view_named(this: Id, key: &'static str) -> Id {
    if let Some(view) = property_get(this, key) {
        return view as Id;
    }
    let screen = runtime::send(runtime::class_named("UIScreen") as Id, "mainScreen", &[]);
    let bounds: crate::fw::geometry::Rect = unsafe {
        let s = runtime::sel("bounds");
        let imp = runtime::imp_for(screen, s).unwrap();
        let f: extern "C" fn(Id, Sel) -> crate::fw::geometry::Rect = std::mem::transmute(imp);
        f(screen, s)
    };
    let view = runtime::send(runtime::class_named("UIView") as Id, "alloc", &[]);
    let view = unsafe {
        let s = runtime::sel("initWithFrame:");
        let imp = runtime::imp_for(view, s).unwrap();
        let f: extern "C" fn(Id, Sel, crate::fw::geometry::Rect) -> Id = std::mem::transmute(imp);
        f(view, s, bounds)
    };
    property_set(view, "moviePlayerOf", this as u64);
    property_set(this, key, view as u64);
    view
}

extern "C" fn movie_view(this: Id, _: Sel) -> Id {
    movie_view_named(this, "view")
}

extern "C" fn movie_background_view(this: Id, _: Sel) -> Id {
    movie_view_named(this, "backgroundView")
}

extern "C" fn movie_playback_state(this: Id, _: Sel) -> isize {
    property_get(this, "playbackState").unwrap_or(MOVIE_STOPPED) as isize
}

extern "C" fn movie_load_state(_this: Id, _: Sel) -> isize {
    // MPMovieLoadStatePlayable | MPMovieLoadStatePlaythroughOK
    3
}

extern "C" fn movie_yes(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn movie_zero_seconds(_this: Id, _: Sel) -> f64 {
    0.0
}

extern "C" fn movie_set_repeat_mode(this: Id, _: Sel, mode: isize) {
    property_set(this, "repeatMode", mode as u64);
}

extern "C" fn movie_repeat_mode(this: Id, _: Sel) -> isize {
    property_get(this, "repeatMode").unwrap_or(0) as isize
}

extern "C" fn movie_set_should_autoplay(this: Id, _: Sel, autoplay: u8) {
    property_set(this, "shouldAutoplay", (autoplay != 0) as u64);
}

extern "C" fn movie_should_autoplay(this: Id, _: Sel) -> bool {
    property_get(this, "shouldAutoplay").unwrap_or(1) != 0
}

extern "C" fn movie_ignore_bool_bool(_this: Id, _: Sel, _a: u8, _b: u8) {}
extern "C" fn movie_ignore_double(_this: Id, _: Sel, _value: f64) {}

fn register_movie_player() {
    let movie = define_class("MPMoviePlayerController", "NSObject", 8);
    methods(
        movie,
        &[
            ("initWithContentURL:", movie_init_with_url as usize),
            ("contentURL", movie_url as usize),
            ("setContentURL:", movie_set_url as usize),
            ("play", movie_play as usize),
            ("prepareToPlay", movie_prepare as usize),
            ("isPreparedToPlay", movie_yes as usize),
            ("pause", movie_pause as usize),
            ("stop", movie_stop as usize),
            ("view", movie_view as usize),
            ("backgroundView", movie_background_view as usize),
            ("playbackState", movie_playback_state as usize),
            ("loadState", movie_load_state as usize),
            ("duration", movie_zero_seconds as usize),
            ("playableDuration", movie_zero_seconds as usize),
            ("currentPlaybackTime", movie_zero_seconds as usize),
            ("setCurrentPlaybackTime:", movie_ignore_double as usize),
            ("setInitialPlaybackTime:", movie_ignore_double as usize),
            ("setEndPlaybackTime:", movie_ignore_double as usize),
            ("repeatMode", movie_repeat_mode as usize),
            ("setRepeatMode:", movie_set_repeat_mode as usize),
            ("shouldAutoplay", movie_should_autoplay as usize),
            ("setShouldAutoplay:", movie_set_should_autoplay as usize),
            ("setControlStyle:", player_ignore_int as usize),
            ("setScalingMode:", player_ignore_int as usize),
            ("setMovieSourceType:", player_ignore_int as usize),
            ("setMovieControlMode:", player_ignore_int as usize),
            ("setUseApplicationAudioSession:", player_ignore_int as usize),
            ("setFullscreen:", player_ignore_int as usize),
            ("setFullscreen:animated:", movie_ignore_bool_bool as usize),
            ("setBackgroundColor:", ignore_id as usize),
        ],
    );
}

pub fn register(r: &mut Registry) {
    register_movie_player();
    // The movie player's notification names and keys are their own text.
    for name in [
        "MPMovieDurationAvailableNotification",
        "MPMoviePlayerContentPreloadDidFinishNotification",
        "MPMoviePlayerLoadStateDidChangeNotification",
        "MPMoviePlayerPlaybackDidFinishNotification",
        "MPMoviePlayerPlaybackDidFinishReasonUserInfoKey",
        "MPMoviePlayerPlaybackStateDidChangeNotification",
        "MPMoviePlayerNowPlayingMovieDidChangeNotification",
        "MPMoviePlayerWillEnterFullscreenNotification",
        "MPMoviePlayerDidExitFullscreenNotification",
        "MPMediaLibraryDidChangeNotification",
        "MPMusicPlayerControllerNowPlayingItemDidChangeNotification",
        "MPMusicPlayerControllerPlaybackStateDidChangeNotification",
    ] {
        r.data(name, leak(string::make(name)));
    }
    r.data("MPMediaItemPropertyPersistentID", leak(string::make("persistentID")));
    r.function("CMTimeGetSeconds", cm_time_get_seconds as usize);
    r.data("kCMTimeZero", leak(CmTime { value: 0, timescale: 1, flags: 1, epoch: 0 }));
    r.function("AudioSessionInitialize", audio_session_initialize as usize);
    r.function("AudioSessionGetProperty", audio_session_get_property as usize);
    r.function("AudioSessionSetProperty", audio_session_ok_3 as usize);
    r.function("AudioSessionAddPropertyListener", audio_session_ok_3 as usize);
    r.function("AudioSessionRemovePropertyListenerWithUserData", audio_session_ok_3 as usize);
    r.function("AudioSessionSetActive", audio_session_set_active as usize);

    let session = define_class("AVAudioSession", "NSObject", 8);
    class_methods(session, &[("sharedInstance", session_shared as usize)]);
    methods(
        session,
        &[
            ("setCategory:error:", session_set_category as usize),
            ("setCategory:withOptions:error:", session_set_category_options as usize),
            ("category", session_category as usize),
            ("setActive:error:", yes_with_error as usize),
            ("setActive:withOptions:error:", yes_with_options_error as usize),
            ("setMode:error:", yes_with_error as usize),
            ("setPreferredSampleRate:error:", yes_with_double_error as usize),
            ("setPreferredHardwareSampleRate:error:", yes_with_double_error as usize),
            ("setPreferredIOBufferDuration:error:", yes_with_double_error as usize),
            ("isOtherAudioPlaying", no as usize),
            ("otherAudioPlaying", no as usize),
            ("secondaryAudioShouldBeSilencedHint", no as usize),
            ("sampleRate", sample_rate as usize),
            ("preferredSampleRate", sample_rate as usize),
            ("currentHardwareSampleRate", sample_rate as usize),
            ("IOBufferDuration", io_buffer_duration as usize),
            ("outputVolume", output_volume as usize),
            ("outputNumberOfChannels", two as usize),
            ("setDelegate:", session_set_delegate as usize),
            ("delegate", session_delegate as usize),
        ],
    );
    for (name, value) in [
        ("AVAudioSessionCategoryAmbient", "AVAudioSessionCategoryAmbient"),
        ("AVAudioSessionCategorySoloAmbient", "AVAudioSessionCategorySoloAmbient"),
        ("AVAudioSessionCategoryPlayback", "AVAudioSessionCategoryPlayback"),
        ("AVAudioSessionCategoryPlayAndRecord", "AVAudioSessionCategoryPlayAndRecord"),
        ("AVAudioSessionInterruptionNotification", "AVAudioSessionInterruptionNotification"),
        ("AVAudioSessionInterruptionTypeKey", "AVAudioSessionInterruptionTypeKey"),
        ("AVAudioSessionInterruptionOptionKey", "AVAudioSessionInterruptionOptionKey"),
        ("AVPlayerItemDidPlayToEndTimeNotification", "AVPlayerItemDidPlayToEndTimeNotification"),
        ("AVLayerVideoGravityResizeAspect", "AVLayerVideoGravityResizeAspect"),
        ("AVLayerVideoGravityResizeAspectFill", "AVLayerVideoGravityResizeAspectFill"),
    ] {
        r.data(name, leak(string::make(value)));
    }

    let asset = define_class("AVAsset", "NSObject", 8);
    let url_asset = define_class("AVURLAsset", "AVAsset", 8);
    class_methods(url_asset, &[("URLAssetWithURL:options:", asset_with_url as usize)]);
    let _ = asset;
    let item = define_class("AVPlayerItem", "NSObject", 8);
    class_methods(
        item,
        &[("playerItemWithAsset:", item_with_asset as usize), ("playerItemWithURL:", item_with_url as usize)],
    );
    methods(
        item,
        &[
            ("asset", item_asset as usize),
            ("status", ready_status as usize),
            ("currentTime", zero_time as usize),
            ("addObserver:forKeyPath:options:context:", item_add_observer as usize),
        ],
    );
    let player = define_class("AVPlayer", "NSObject", 8);
    class_methods(player, &[("playerWithPlayerItem:", player_with_item as usize)]);
    methods(
        player,
        &[
            ("initWithPlayerItem:", player_init_with_item as usize),
            ("currentItem", player_item as usize),
            ("replaceCurrentItemWithPlayerItem:", player_replace_item as usize),
            ("play", player_play as usize),
            ("pause", player_pause as usize),
            ("rate", player_rate as usize),
            ("setRate:", player_set_rate as usize),
            ("setVolume:", player_ignore_float as usize),
            ("setActionAtItemEnd:", player_ignore_int as usize),
            ("status", ready_status as usize),
            ("currentTime", zero_time as usize),
        ],
    );
    let player_layer = define_class("AVPlayerLayer", "CALayer", 8);
    class_methods(player_layer, &[("playerLayerWithPlayer:", layer_with_player as usize)]);
    methods(
        player_layer,
        &[("setVideoGravity:", ignore_id as usize), ("setPlayer:", layer_set_player as usize), ("player", layer_player as usize)],
    );

    let _ = collections::array_from;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sound engines (FMOD) ask whether a microphone is there before starting; the answer is no.
    #[test]
    fn audio_session_says_no_input_is_available() {
        let mut available = 7u32;
        let mut size = 4u32;
        let result = audio_session_get_property(four(b"aiav"), &mut size, &mut available as *mut u32 as *mut u8);
        assert_eq!(result, 0, "kAudioSessionProperty_AudioInputAvailable was refused");
        assert_eq!((available, size), (0, 4));
    }

    static INTERRUPTIONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn c_listener(data: usize, state: u32) {
        INTERRUPTIONS.lock().unwrap_or_else(|e| e.into_inner()).push(format!("listener {data:#x} {state}"));
    }

    extern "C" fn delegate_begin(_this: Id, _: Sel) {
        INTERRUPTIONS.lock().unwrap_or_else(|e| e.into_inner()).push("beginInterruption".into());
    }

    extern "C" fn delegate_end(_this: Id, _: Sel, flags: usize) {
        INTERRUPTIONS.lock().unwrap_or_else(|e| e.into_inner()).push(format!("endInterruptionWithFlags: {flags}"));
    }

    extern "C" fn interruption_heard(_this: Id, _: Sel, notification: Id) {
        let info = runtime::send(notification, "userInfo", &[]);
        let number = |key: &str| {
            let value = collections::dictionary_get(info, string::make(key));
            if value == 0 { -1 } else { runtime::send(value, "integerValue", &[]) as isize }
        };
        let text = format!("notification type {} option {}", number("AVAudioSessionInterruptionTypeKey"), number("AVAudioSessionInterruptionOptionKey"));
        INTERRUPTIONS.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    /// Android's audio focus lost and given back (a phone call) is an iOS
    /// interruption: the C listener from AudioSessionInitialize hears begin
    /// (1) and end (0), the AVAudioSession delegate beginInterruption and
    /// endInterruptionWithFlags: (should resume), and observers
    /// AVAudioSessionInterruptionNotification with its type and option.
    /// A second loss while interrupted, or a gain without one, says nothing.
    #[test]
    fn losing_audio_focus_interrupts_the_audio_session() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let delegate = define_class("InterruptionTestDelegate", "NSObject", 8);
            methods(delegate, &[("beginInterruption", delegate_begin as usize), ("endInterruptionWithFlags:", delegate_end as usize), ("heard:", interruption_heard as usize)]);
        });
        let heard = || std::mem::take(&mut *INTERRUPTIONS.lock().unwrap_or_else(|e| e.into_inner()));
        assert_eq!(audio_session_initialize(0, 0, c_listener as usize, 0x55), 0);
        let session = runtime::send(runtime::class_named("AVAudioSession"), "sharedInstance", &[]);
        let delegate = runtime::alloc_instance(runtime::class_named("InterruptionTestDelegate"));
        runtime::send(session, "setDelegate:", &[delegate]);
        let center = runtime::send(runtime::class_named("NSNotificationCenter"), "defaultCenter", &[]);
        let name = string::make("AVAudioSessionInterruptionNotification");
        runtime::send(center, "addObserver:selector:name:object:", &[delegate, runtime::sel("heard:"), name, session]);

        interrupt_now(true, false);
        assert_eq!(heard(), ["listener 0x55 1", "beginInterruption", "notification type 1 option -1"]);
        interrupt_now(true, false);
        assert!(heard().is_empty(), "already interrupted");
        interrupt_now(false, true);
        assert_eq!(heard(), ["listener 0x55 0", "endInterruptionWithFlags: 1", "notification type 0 option 1"]);
        interrupt_now(false, true);
        assert!(heard().is_empty(), "not interrupted");
        runtime::send(center, "removeObserver:", &[delegate]);
        runtime::send(session, "setDelegate:", &[0]);
    }

    static MOVIE_HEARD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn movie_heard(_this: Id, _: Sel, notification: Id) {
        // (Never a panic here: it would abort the test run from C.)
        MOVIE_HEARD.lock().unwrap_or_else(|e| e.into_inner()).push(string::get(runtime::send(notification, "name", &[])));
    }

    /// The movie player tells its observers what Apple's does: once loaded,
    /// the load state, the duration and the preload; each change of
    /// playback state.
    #[test]
    fn the_movie_player_posts_load_and_playback_state_changes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let listener = define_class("MovieTestListener", "NSObject", 8);
            methods(listener, &[("heard:", movie_heard as usize)]);
        });
        let url = runtime::send(runtime::class_named("NSURL"), "fileURLWithPath:", &[string::make("/nowhere/movie.mp4")]);
        let movie = runtime::send(runtime::send(runtime::class_named("MPMoviePlayerController"), "alloc", &[]), "initWithContentURL:", &[url]);
        let listener = runtime::alloc_instance(runtime::class_named("MovieTestListener"));
        let center = runtime::send(runtime::class_named("NSNotificationCenter"), "defaultCenter", &[]);
        runtime::send(center, "addObserver:selector:name:object:", &[listener, runtime::sel("heard:"), 0, movie]);
        NOTICES_AT_ONCE.with(|n| n.set(true));
        runtime::send(movie, "setShouldAutoplay:", &[0]);
        runtime::send(movie, "prepareToPlay", &[]);
        runtime::send(movie, "pause", &[]);
        runtime::send(movie, "pause", &[]);
        runtime::send(movie, "stop", &[]);
        let wanted = [
            "MPMoviePlayerLoadStateDidChangeNotification",
            "MPMovieDurationAvailableNotification",
            "MPMoviePlayerContentPreloadDidFinishNotification",
            "MPMoviePlayerPlaybackStateDidChangeNotification",
            "MPMoviePlayerPlaybackStateDidChangeNotification",
        ];
        assert_eq!(*MOVIE_HEARD.lock().unwrap(), wanted, "a pause while paused changes nothing");
        // A new movie in a paused player: it stops (said so). (Not a playing
        // one: with no video screen here, play ends at once on the main loop,
        // which other tests run.)
        runtime::send(movie, "pause", &[]);
        MOVIE_HEARD.lock().unwrap().clear();
        let other = runtime::send(runtime::class_named("NSURL"), "fileURLWithPath:", &[string::make("/nowhere/other.mp4")]);
        runtime::send(movie, "setContentURL:", &[other]);
        assert_eq!(runtime::send(movie, "playbackState", &[]), 0, "stopped");
        assert_eq!(*MOVIE_HEARD.lock().unwrap(), ["MPMoviePlayerPlaybackStateDidChangeNotification"]);
        NOTICES_AT_ONCE.with(|n| n.set(false));
    }
}
