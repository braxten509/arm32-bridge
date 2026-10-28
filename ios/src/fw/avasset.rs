//! AVFoundation beyond plain playback: a movie's tracks (AVAsset,
//! AVAssetTrack), reading them sample by sample (AVAssetReader,
//! AVAssetReaderTrackOutput), audio mixes, and the camera classes.
//!
//! - Sound tracks are read for real (WAV, AIFF, CAF, MP3, AAC/ALAC in MP4
//!   or M4A): decoded to the linear PCM the game asks for, or handed over
//!   as stored when it asks for no settings.
//! - Picture tracks are listed with their size, length and frame rate, but
//!   this layer has no video decoder yet: reading one fails the way iOS
//!   fails without a decoder (AVErrorDecoderNotFound), so games take their
//!   no-video path.
//! - There is no camera: no capture devices, access denied, as on an
//!   iPhone where the user said no.

use crate::fw::audiofile::{self, Asbd, Parsed};
use crate::fw::audioqueue::{self, Decoder};
use crate::fw::corevideo::{self, CmTime, CmTimeRange};
use crate::fw::geometry::{Size, Transform};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, misc, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

const VIDEO: &str = "vide";
const AUDIO: &str = "soun";

const ERROR_DOMAIN: &str = "AVFoundationErrorDomain";
const ERROR_UNKNOWN: isize = -11800;
const ERROR_DECODER_NOT_FOUND: isize = -11833;
const ERROR_FILE_FAILED_TO_PARSE: isize = -11829;
const ERROR_NOT_AUTHORIZED: isize = -11852;

// ----- tracks of a file -----

#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    pub id: i32,
    pub media: &'static str,
    pub codec: u32,
    pub timescale: i32,
    pub duration: i64,
    pub width: f64,
    pub height: f64,
    pub samples: u64,
}

impl TrackInfo {
    fn seconds(&self) -> f64 {
        if self.timescale > 0 { self.duration as f64 / self.timescale as f64 } else { 0.0 }
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes directly inside `b`: (type, payload).
fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let mut size = be32(b, at).unwrap_or(0) as u64;
        let kind: [u8; 4] = b[at + 4..at + 8].try_into().unwrap();
        let mut header = 8usize;
        if size == 1 {
            size = be64(b, at + 8).unwrap_or(0);
            header = 16;
        } else if size == 0 {
            size = (b.len() - at) as u64;
        }
        // A size past the end (or absurd) ends the list.
        if size < header as u64 || size > (b.len() - at) as u64 {
            break;
        }
        let size = size as usize;
        found.push((kind, &b[at + header..at + size]));
        at += size;
    }
    found
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).into_iter().find(|(k, _)| k == kind).map(|(_, p)| p)
}

/// The tracks of an MP4/MOV `moov` box.
pub fn mp4_tracks(moov: &[u8]) -> Vec<TrackInfo> {
    let mut tracks = Vec::new();
    for (kind, trak) in boxes(moov) {
        if &kind != b"trak" {
            continue;
        }
        let (Some(tkhd), Some(mdia)) = (child(trak, b"tkhd"), child(trak, b"mdia")) else { continue };
        let v1 = tkhd.first() == Some(&1);
        let id = be32(tkhd, if v1 { 20 } else { 12 }).unwrap_or(0) as i32;
        let size_at = if v1 { 88 } else { 76 };
        let width = be32(tkhd, size_at).unwrap_or(0) as f64 / 65536.0;
        let height = be32(tkhd, size_at + 4).unwrap_or(0) as f64 / 65536.0;
        let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12)).unwrap_or(b"????");
        let media = match handler {
            b"vide" => VIDEO,
            b"soun" => AUDIO,
            b"text" | b"sbtl" => "text",
            _ => "meta",
        };
        let (timescale, duration) = child(mdia, b"mdhd")
            .map(|m| if m.first() == Some(&1) { (be32(m, 20), be64(m, 24)) } else { (be32(m, 12), be32(m, 16).map(|d| d as u64)) })
            .map(|(s, d)| (s.unwrap_or(0) as i32, d.unwrap_or(0) as i64))
            .unwrap_or((0, 0));
        let stbl = child(mdia, b"minf").and_then(|m| child(m, b"stbl"));
        let codec = stbl.and_then(|s| child(s, b"stsd")).and_then(|s| be32(s, 12)).unwrap_or(0);
        let samples = stbl
            .and_then(|s| child(s, b"stts"))
            .map(|stts| {
                // No more entries than the box holds, whatever its count says.
                let count = (be32(stts, 4).unwrap_or(0) as usize).min(stts.len().saturating_sub(8) / 8);
                (0..count).map(|i| be32(stts, 8 + i * 8).unwrap_or(0) as u64).sum()
            })
            .unwrap_or(0);
        tracks.push(TrackInfo { id, media, codec, timescale, duration, width, height, samples });
    }
    tracks
}

/// The tracks of the file at `path`: an MP4/MOV movie's from its `moov`
/// box, or one sound track for a plain sound file.
pub fn file_tracks(path: &str) -> Result<Vec<TrackInfo>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut head = [0u8; 12];
    file.read_exact(&mut head).map_err(|e| format!("{path}: {e}"))?;
    if &head[4..8] == b"ftyp" || &head[4..8] == b"moov" || &head[4..8] == b"mdat" || &head[4..8] == b"wide" {
        // Walk the top-level boxes without reading the media data.
        let mut at = 0u64;
        while at + 8 <= length {
            let mut header = [0u8; 16];
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            let got = file.read(&mut header).map_err(|e| e.to_string())?;
            if got < 8 {
                break;
            }
            let mut size = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
            let mut skip = 8;
            if size == 1 && got >= 16 {
                size = u64::from_be_bytes(header[8..16].try_into().unwrap());
                skip = 16;
            } else if size == 0 {
                size = length - at;
            }
            // A box past the end of the file: the file is cut short or broken.
            if size < skip || size > length - at {
                break;
            }
            if &header[4..8] == b"moov" {
                let mut moov = vec![0u8; (size - skip) as usize];
                file.seek(SeekFrom::Start(at + skip)).map_err(|e| e.to_string())?;
                file.read_exact(&mut moov).map_err(|e| e.to_string())?;
                return Ok(mp4_tracks(&moov));
            }
            at += size;
        }
        return Err(format!("{path}: no movie header"));
    }
    let parsed = audiofile::parse_path(path).map_err(|code| format!("{path}: not a sound file this layer reads ({})", crate::fw::sound::four_cc(code as u32)))?;
    let rate = parsed.format.rate.round().max(1.0) as i32;
    Ok(vec![TrackInfo {
        id: 1,
        media: AUDIO,
        codec: parsed.format.format,
        timescale: rate,
        duration: parsed.valid_frames() as i64,
        width: 0.0,
        height: 0.0,
        samples: parsed.packet_count(),
    }])
}

// ----- AVAsset and AVAssetTrack -----

static TRACKS: Mutex<Option<HashMap<Id, (Id, TrackInfo)>>> = Mutex::new(None);

fn track_of(track: Id) -> Option<(Id, TrackInfo)> {
    TRACKS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&track).cloned()
}

fn asset_path(asset: Id) -> Option<String> {
    let url = property_get(asset, "url").unwrap_or(0) as Id;
    if url == 0 || runtime::send(url, "isFileURL", &[]) & 0xFF == 0 {
        return None;
    }
    Some(string::get(runtime::send(url, "path", &[])))
}

/// The asset's track objects (made once).
fn asset_tracks(asset: Id) -> Vec<Id> {
    if let Some(list) = property_get(asset, "tracks") {
        return collections::array_items(list as Id);
    }
    let infos = match asset_path(asset).map(|p| file_tracks(&p)) {
        Some(Ok(tracks)) => tracks,
        Some(Err(e)) => {
            crate::log!("AVAsset: {e}");
            Vec::new()
        }
        None => Vec::new(),
    };
    let objects: Vec<Id> = infos
        .into_iter()
        .map(|info| {
            let track = runtime::alloc_instance(runtime::class_named("AVAssetTrack"));
            TRACKS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(track, (asset, info));
            track
        })
        .collect();
    property_set(asset, "tracks", collections::array_from(&objects) as u64);
    objects
}

extern "C" fn asset_with_url(class: Class, _: Sel, url: Id) -> Id {
    let object = runtime::alloc_instance(if class == runtime::class_named("AVAsset") { runtime::class_named("AVURLAsset") } else { class });
    property_set(object, "url", url as u64);
    object
}

extern "C" fn asset_init_with_url(this: Id, _: Sel, url: Id, _options: Id) -> Id {
    property_set(this, "url", url as u64);
    this
}

extern "C" fn asset_url(this: Id, _: Sel) -> Id {
    property_get(this, "url").unwrap_or(0) as Id
}

extern "C" fn asset_tracks_method(this: Id, _: Sel) -> Id {
    asset_tracks(this);
    property_get(this, "tracks").unwrap_or(0) as Id
}

extern "C" fn asset_tracks_with_type(this: Id, _: Sel, media: Id) -> Id {
    let wanted = string::get(media);
    let list: Vec<Id> = asset_tracks(this).into_iter().filter(|&t| track_of(t).is_some_and(|(_, i)| i.media == wanted)).collect();
    collections::array_from(&list)
}

extern "C" fn asset_track_with_id(this: Id, _: Sel, id: i32) -> Id {
    asset_tracks(this).into_iter().find(|&t| track_of(t).is_some_and(|(_, i)| i.id == id)).unwrap_or(0)
}

/// The longest track's length.
extern "C" fn asset_duration(this: Id, _: Sel) -> CmTime {
    let longest = asset_tracks(this).into_iter().filter_map(track_of).map(|(_, i)| i).max_by(|a, b| a.seconds().total_cmp(&b.seconds()));
    match longest {
        Some(i) if i.timescale > 0 => CmTime::new(i.duration, i.timescale),
        _ => CmTime::new(0, 1),
    }
}

extern "C" fn asset_is_readable(this: Id, _: Sel) -> bool {
    !asset_tracks(this).is_empty()
}

/// Playable as iOS would say: it has tracks (pictures are shown by the
/// phone's own player).
extern "C" fn asset_is_playable(this: Id, _: Sel) -> bool {
    !asset_tracks(this).is_empty()
}

extern "C" fn asset_natural_size(this: Id, _: Sel) -> Size {
    asset_tracks(this)
        .into_iter()
        .filter_map(track_of)
        .find(|(_, i)| i.media == VIDEO)
        .map(|(_, i)| Size { width: i.width, height: i.height })
        .unwrap_or(Size { width: 0.0, height: 0.0 })
}

/// Every key is known at once (the file was read); the handler runs on the
/// main thread afterwards, as iOS calls it later.
extern "C" fn asset_load_values(this: Id, _: Sel, _keys: Id, handler: usize) {
    asset_tracks(this);
    call_later(handler, |invoke, block| {
        let f: extern "C" fn(usize) = unsafe { std::mem::transmute(invoke) };
        f(block);
    });
}

/// AVKeyValueStatusLoaded.
extern "C" fn asset_status_of_value(_this: Id, _: Sel, _key: Id, _error: *mut Id) -> isize {
    2
}

extern "C" fn track_media_type(this: Id, _: Sel) -> Id {
    string::make(track_of(this).map(|(_, i)| i.media).unwrap_or(""))
}

extern "C" fn track_id(this: Id, _: Sel) -> i32 {
    track_of(this).map(|(_, i)| i.id).unwrap_or(0)
}

extern "C" fn track_asset(this: Id, _: Sel) -> Id {
    track_of(this).map(|(a, _)| a).unwrap_or(0)
}

extern "C" fn track_natural_size(this: Id, _: Sel) -> Size {
    track_of(this).map(|(_, i)| Size { width: i.width, height: i.height }).unwrap_or(Size { width: 0.0, height: 0.0 })
}

extern "C" fn track_time_range(this: Id, _: Sel) -> CmTimeRange {
    let (scale, duration) = track_of(this).map(|(_, i)| (i.timescale.max(1), i.duration)).unwrap_or((1, 0));
    CmTimeRange { start: CmTime::new(0, scale), duration: CmTime::new(duration, scale) }
}

extern "C" fn track_time_scale(this: Id, _: Sel) -> i32 {
    track_of(this).map(|(_, i)| i.timescale).unwrap_or(0)
}

extern "C" fn track_frame_rate(this: Id, _: Sel) -> f32 {
    match track_of(this) {
        Some((_, i)) if i.media == VIDEO && i.seconds() > 0.0 => (i.samples as f64 / i.seconds()) as f32,
        _ => 0.0,
    }
}

extern "C" fn track_identity(_this: Id, _: Sel) -> Transform {
    crate::fw::geometry::IDENTITY
}

extern "C" fn track_yes(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn track_empty_list(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

// ----- AVPlayerItem: duration, tracks, audio mix -----

extern "C" fn item_duration(this: Id, s: Sel) -> CmTime {
    let asset = property_get(this, "asset").unwrap_or(0) as Id;
    if asset == 0 {
        return CmTime { flags: corevideo::TIME_VALID | corevideo::TIME_INDEFINITE, ..CmTime::INVALID };
    }
    asset_duration(asset, s)
}

extern "C" fn item_tracks(this: Id, _: Sel) -> Id {
    if let Some(list) = property_get(this, "itemTracks") {
        return list as Id;
    }
    let asset = property_get(this, "asset").unwrap_or(0) as Id;
    let list: Vec<Id> = asset_tracks(asset)
        .into_iter()
        .map(|track| {
            let item_track = runtime::alloc_instance(runtime::class_named("AVPlayerItemTrack"));
            property_set(item_track, "assetTrack", track as u64);
            item_track
        })
        .collect();
    let array = collections::array_from(&list);
    property_set(this, "itemTracks", array as u64);
    array
}

extern "C" fn item_presentation_size(this: Id, s: Sel) -> Size {
    let asset = property_get(this, "asset").unwrap_or(0) as Id;
    if asset == 0 { Size { width: 0.0, height: 0.0 } } else { asset_natural_size(asset, s) }
}

extern "C" fn item_set_audio_mix(this: Id, _: Sel, mix: Id) {
    property_set(this, "audioMix", mix as u64);
    let volumes: Vec<f32> = collections::array_items(property_get(mix, "inputParameters").unwrap_or(0) as Id)
        .into_iter()
        .map(|p| volume_at(p, 0.0))
        .collect();
    crate::log!("AVPlayerItem: audio mix set (track volumes at the start: {volumes:?}); the phone's video player plays the movie's own sound level");
}

extern "C" fn item_audio_mix(this: Id, _: Sel) -> Id {
    property_get(this, "audioMix").unwrap_or(0) as Id
}

extern "C" fn item_track_asset_track(this: Id, _: Sel) -> Id {
    property_get(this, "assetTrack").unwrap_or(0) as Id
}

extern "C" fn item_track_enabled(this: Id, _: Sel) -> bool {
    property_get(this, "disabled").unwrap_or(0) == 0
}

extern "C" fn item_track_set_enabled(this: Id, _: Sel, enabled: bool) {
    property_set(this, "disabled", (!enabled) as u64);
}

// ----- AVMutableAudioMix and its input parameters -----

/// A volume ramp: from `start` to `end` volume over `from`..`to` seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ramp {
    from: f64,
    to: f64,
    start: f32,
    end: f32,
}

static RAMPS: Mutex<Option<HashMap<Id, Vec<Ramp>>>> = Mutex::new(None);

fn add_ramp(parameters: Id, ramp: Ramp) {
    let mut guard = RAMPS.lock().unwrap_or_else(|e| e.into_inner());
    let list = guard.get_or_insert_with(HashMap::new).entry(parameters).or_default();
    list.retain(|r| r.from != ramp.from);
    list.push(ramp);
    list.sort_by(|a, b| a.from.total_cmp(&b.from));
}

fn ramps_of(parameters: Id) -> Vec<Ramp> {
    RAMPS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|r| r.get(&parameters).cloned()).unwrap_or_default()
}

/// The volume at `seconds`: inside a ramp, along it; after one, its end
/// volume; before any, full volume (iOS's default).
fn volume_at(parameters: Id, seconds: f64) -> f32 {
    let mut volume = 1.0;
    for r in ramps_of(parameters) {
        if seconds < r.from {
            break;
        }
        volume = if seconds < r.to && r.to > r.from { r.start + (r.end - r.start) * ((seconds - r.from) / (r.to - r.from)) as f32 } else { r.end };
    }
    volume
}

extern "C" fn mix_new(class: Class, _: Sel) -> Id {
    let mix = runtime::alloc_instance(class);
    property_set(mix, "inputParameters", collections::array_from(&[]) as u64);
    mix
}

extern "C" fn mix_input_parameters(this: Id, _: Sel) -> Id {
    property_get(this, "inputParameters").unwrap_or_else(|| collections::array_from(&[]) as u64) as Id
}

extern "C" fn mix_set_input_parameters(this: Id, _: Sel, list: Id) {
    let copy = collections::array_from(&collections::array_items(list));
    property_set(this, "inputParameters", copy as u64);
}

extern "C" fn parameters_new(class: Class, _: Sel) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn parameters_with_track(class: Class, _: Sel, track: Id) -> Id {
    let parameters = runtime::alloc_instance(class);
    property_set(parameters, "trackID", track_of(track).map(|(_, i)| i.id).unwrap_or(0) as u64);
    parameters
}

extern "C" fn parameters_track_id(this: Id, _: Sel) -> i32 {
    property_get(this, "trackID").unwrap_or(0) as i32
}

extern "C" fn parameters_set_track_id(this: Id, _: Sel, id: i32) {
    property_set(this, "trackID", id as u32 as u64);
}

extern "C" fn parameters_set_volume(this: Id, _: Sel, volume: f32, time: CmTime) {
    let at = if time.is_numeric() { time.seconds() } else { 0.0 };
    add_ramp(this, Ramp { from: at, to: at, start: volume, end: volume });
}

extern "C" fn parameters_set_ramp(this: Id, _: Sel, start: f32, end: f32, range: CmTimeRange) {
    let from = if range.start.is_numeric() { range.start.seconds() } else { 0.0 };
    let length = if range.duration.is_numeric() { range.duration.seconds().max(0.0) } else { 0.0 };
    add_ramp(this, Ramp { from, to: from + length, start, end });
}

/// getVolumeRampForTime:startVolume:endVolume:timeRange: — the ramp in
/// effect at `time`, if any.
extern "C" fn parameters_get_ramp(this: Id, _: Sel, time: CmTime, start: *mut f32, end: *mut f32, range: *mut CmTimeRange) -> bool {
    let at = time.seconds();
    let Some(r) = ramps_of(this).into_iter().rev().find(|r| r.from <= at) else { return false };
    unsafe {
        if !start.is_null() {
            *start = r.start;
        }
        if !end.is_null() {
            *end = r.end;
        }
        if !range.is_null() {
            *range = CmTimeRange { start: corevideo::time_from_seconds(r.from, 600), duration: corevideo::time_from_seconds(r.to - r.from, 600) };
        }
    }
    true
}

// ----- AVAssetReader and AVAssetReaderTrackOutput -----

const STATUS_UNKNOWN: isize = 0;
const STATUS_READING: isize = 1;
const STATUS_COMPLETED: isize = 2;
const STATUS_FAILED: isize = 3;
const STATUS_CANCELLED: isize = 4;

/// Frames of decoded sound per sample buffer.
const FRAMES_PER_SAMPLE: usize = 4096;

/// A sound track being read.
struct SoundReader {
    parsed: Parsed,
    /// None: the game gets the stored packets as they are.
    decoder: Option<Decoder>,
    out: Asbd,
    next_packet: u64,
    /// Decoded floats (source channels) waiting to go out.
    pending: Vec<f32>,
    /// Source frames to drop at the start and the last frame wanted.
    skip: u64,
    end: u64,
    decoded_frames: u64,
    sent_frames: u64,
    /// Resampling position (source frames into `pending`).
    position: f64,
}

static READERS: Mutex<Option<HashMap<Id, SoundReader>>> = Mutex::new(None);

fn reader_status(reader: Id) -> isize {
    property_get(reader, "status").unwrap_or(STATUS_UNKNOWN as u64) as isize
}

fn fail(reader: Id, code: isize, text: &str) {
    crate::log!("AVAssetReader: {text}");
    property_set(reader, "status", STATUS_FAILED as u64);
    property_set(reader, "error", misc::error(ERROR_DOMAIN, code, text) as u64);
}

extern "C" fn reader_with_asset(class: Class, s: Sel, asset: Id, error: *mut Id) -> Id {
    reader_init(runtime::alloc_instance(class), s, asset, error)
}

extern "C" fn reader_init(this: Id, _: Sel, asset: Id, error: *mut Id) -> Id {
    let readable = asset != 0 && asset_path(asset).is_some() && !asset_tracks(asset).is_empty();
    if !readable {
        if !error.is_null() {
            unsafe { *error = misc::error(ERROR_DOMAIN, ERROR_FILE_FAILED_TO_PARSE, "The asset can't be read (not a local file this layer reads)") };
        }
        return 0;
    }
    property_set(this, "asset", asset as u64);
    property_set(this, "outputs", collections::array_from(&[]) as u64);
    let range = CmTimeRange { start: CmTime::new(0, 1), duration: CmTime { flags: corevideo::TIME_VALID | corevideo::TIME_POSITIVE_INFINITY, ..CmTime::INVALID } };
    property_set(this, "timeRange", leak(range) as u64);
    this
}

extern "C" fn reader_asset(this: Id, _: Sel) -> Id {
    property_get(this, "asset").unwrap_or(0) as Id
}

extern "C" fn reader_outputs(this: Id, _: Sel) -> Id {
    property_get(this, "outputs").unwrap_or(0) as Id
}

extern "C" fn reader_status_method(this: Id, _: Sel) -> isize {
    reader_status(this)
}

extern "C" fn reader_error(this: Id, _: Sel) -> Id {
    property_get(this, "error").unwrap_or(0) as Id
}

extern "C" fn reader_time_range(this: Id, _: Sel) -> CmTimeRange {
    property_get(this, "timeRange").map(|p| unsafe { *(p as *const CmTimeRange) }).unwrap_or_default()
}

extern "C" fn reader_set_time_range(this: Id, _: Sel, range: CmTimeRange) {
    if reader_status(this) == STATUS_UNKNOWN {
        property_set(this, "timeRange", leak(range) as u64);
    }
}

extern "C" fn reader_can_add_output(this: Id, _: Sel, output: Id) -> bool {
    let track = property_get(output, "track").unwrap_or(0) as Id;
    reader_status(this) == STATUS_UNKNOWN
        && property_get(output, "reader").is_none()
        && track_of(track).is_some_and(|(asset, _)| asset == reader_asset(this, 0))
}

extern "C" fn reader_add_output(this: Id, s: Sel, output: Id) {
    if !reader_can_add_output(this, s, output) {
        crate::log!("AVAssetReader: an output that can't be added was added; ignored");
        return;
    }
    property_set(output, "reader", this as u64);
    let mut list = collections::array_items(reader_outputs(this, s));
    list.push(output);
    property_set(this, "outputs", collections::array_from(&list) as u64);
}

/// The PCM format asked for by AVAssetReaderTrackOutput settings (None: as stored).
fn output_format(settings: Id, source: &Asbd, rate: f64) -> Result<Option<Asbd>, String> {
    if settings == 0 {
        return Ok(None);
    }
    let get = |key: &str| runtime::send(settings, "objectForKey:", &[string::make(key)]) as Id;
    let number = |key: &str| {
        let value = get(key);
        (value != 0).then(|| collections::number_as_f64(value))
    };
    let format = number("AVFormatIDKey").map(|v| v as u32).unwrap_or(audiofile::LPCM);
    if format != audiofile::LPCM {
        return Err(format!("output format {} isn't supported (only linear PCM)", crate::fw::sound::four_cc(format)));
    }
    let rate = number("AVSampleRateKey").filter(|&r| r > 0.0).unwrap_or(rate);
    let channels = number("AVNumberOfChannelsKey").map(|c| c as u32).filter(|&c| c > 0).unwrap_or(source.channels.max(1));
    let float = number("AVLinearPCMIsFloatKey").is_some_and(|v| v != 0.0);
    let bits = number("AVLinearPCMBitDepthKey").map(|b| b as u32).unwrap_or(if float { 32 } else { 16 });
    let big = number("AVLinearPCMIsBigEndianKey").is_some_and(|v| v != 0.0);
    if number("AVLinearPCMIsNonInterleaved").is_some_and(|v| v != 0.0) {
        return Err("non-interleaved output isn't supported".into());
    }
    let out = audiofile::pcm(rate, channels.min(8), bits, float, big);
    if !audioqueue::pcm_supported(&out) {
        return Err(format!("{bits}-bit {} output isn't supported", if float { "float" } else { "integer" }));
    }
    Ok(Some(out))
}

extern "C" fn reader_start(this: Id, s: Sel) -> bool {
    if reader_status(this) != STATUS_UNKNOWN {
        return false;
    }
    let asset = reader_asset(this, s);
    let Some(path) = asset_path(asset) else {
        fail(this, ERROR_UNKNOWN, "the asset is not a local file");
        return false;
    };
    let range = reader_time_range(this, s);
    for output in collections::array_items(reader_outputs(this, s)) {
        let track = property_get(output, "track").unwrap_or(0) as Id;
        let Some((_, info)) = track_of(track) else { continue };
        if info.media != AUDIO {
            fail(this, ERROR_DECODER_NOT_FOUND, &format!("{path}: {} pictures can't be decoded here (no video decoder)", crate::fw::sound::four_cc(info.codec)));
            return false;
        }
        let first_sound = asset_tracks(asset).into_iter().filter_map(track_of).find(|(_, i)| i.media == AUDIO).map(|(_, i)| i.id);
        if first_sound != Some(info.id) {
            fail(this, ERROR_DECODER_NOT_FOUND, &format!("{path}: only a movie's first sound track can be read here"));
            return false;
        }
        // Only the sound is read in (a movie's pictures stay on disk).
        let parsed = match audiofile::parse_path(&path).map_err(|c| crate::fw::sound::four_cc(c as u32)) {
            Ok(parsed) => parsed,
            Err(e) => {
                fail(this, ERROR_FILE_FAILED_TO_PARSE, &format!("{path}: {e}"));
                return false;
            }
        };
        let settings = property_get(output, "settings").unwrap_or(0) as Id;
        let decoder = match Decoder::new(&parsed.format, &parsed.cookie) {
            Ok(d) => Some(d),
            Err(e) if settings != 0 => {
                fail(this, ERROR_DECODER_NOT_FOUND, &format!("{path}: {e}"));
                return false;
            }
            Err(_) => None,
        };
        let rate = decoder.as_ref().map(|d| d.rate).unwrap_or(parsed.format.rate);
        let out = match output_format(settings, &parsed.format, rate) {
            Ok(Some(out)) => out,
            Ok(None) => parsed.format,
            Err(e) => {
                fail(this, ERROR_DECODER_NOT_FOUND, &e);
                return false;
            }
        };
        let decoder = if settings == 0 { None } else { decoder };
        let start = if range.start.is_numeric() { (range.start.seconds().max(0.0) * rate) as u64 } else { 0 };
        let end = if range.duration.is_numeric() { start.saturating_add((range.duration.seconds().max(0.0) * rate) as u64) } else { u64::MAX };
        let skip = start + parsed.priming as u64;
        let end = end.saturating_add(parsed.priming as u64).min(parsed.priming as u64 + parsed.valid_frames());
        // Decoded sound is timed from the range start, in the output's frames.
        let first = (start as f64 * out.rate / rate.max(1.0)) as u64;
        READERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
            output,
            SoundReader { parsed, decoder, out, next_packet: 0, pending: Vec::new(), skip, end, decoded_frames: 0, sent_frames: first, position: 0.0 },
        );
    }
    property_set(this, "status", STATUS_READING as u64);
    true
}

extern "C" fn reader_cancel(this: Id, s: Sel) {
    if matches!(reader_status(this), STATUS_UNKNOWN | STATUS_READING) {
        property_set(this, "status", STATUS_CANCELLED as u64);
    }
    for output in collections::array_items(reader_outputs(this, s)) {
        READERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|r| r.remove(&output));
    }
}

extern "C" fn output_with_track(class: Class, s: Sel, track: Id, settings: Id) -> Id {
    output_init(runtime::alloc_instance(class), s, track, settings)
}

extern "C" fn output_init(this: Id, _: Sel, track: Id, settings: Id) -> Id {
    property_set(this, "track", track as u64);
    property_set(this, "settings", settings as u64);
    this
}

extern "C" fn output_track(this: Id, _: Sel) -> Id {
    property_get(this, "track").unwrap_or(0) as Id
}

extern "C" fn output_settings(this: Id, _: Sel) -> Id {
    property_get(this, "settings").unwrap_or(0) as Id
}

extern "C" fn output_media_type(this: Id, s: Sel) -> Id {
    track_media_type(output_track(this, s), s)
}

extern "C" fn output_copies(this: Id, _: Sel) -> bool {
    property_get(this, "noCopies").unwrap_or(0) == 0
}

extern "C" fn output_set_copies(this: Id, _: Sel, copies: bool) {
    property_set(this, "noCopies", (!copies) as u64);
}

/// The sound as stored (no output settings), inside the reader's time
/// range: linear PCM in runs of up to `FRAMES_PER_SAMPLE` frames (each
/// packet is one frame), compressed sound one packet per sample buffer.
/// Gives (bytes, samples, frames, time in frames).
fn next_stored(r: &mut SoundReader) -> Option<(Vec<u8>, usize, usize, u64)> {
    if r.parsed.format.format == audiofile::LPCM {
        let start = r.decoded_frames.max(r.skip);
        let stop = r.end.min(r.parsed.packet_count()).min(start + FRAMES_PER_SAMPLE as u64);
        if start >= stop {
            return None;
        }
        let (first, last) = (r.parsed.packet(start)?, r.parsed.packet(stop - 1)?);
        let bytes = r.parsed.data.get(first.offset..last.offset + last.size as usize)?.to_vec();
        r.decoded_frames = stop;
        let frames = (stop - start) as usize;
        return Some((bytes, frames, frames, start));
    }
    loop {
        if r.decoded_frames >= r.end {
            return None;
        }
        let packet = r.parsed.packet(r.next_packet)?;
        r.next_packet += 1;
        let at = r.decoded_frames;
        r.decoded_frames += packet.frames as u64;
        // Packets wholly before the range start are left out.
        if r.decoded_frames <= r.skip && packet.frames > 0 {
            continue;
        }
        let bytes = r.parsed.data.get(packet.offset..packet.offset + packet.size as usize)?.to_vec();
        // Times are the asset's: priming frames come before zero.
        return Some((bytes, 1, packet.frames as usize, at.saturating_sub(r.parsed.priming as u64)));
    }
}

/// Decodes until `FRAMES_PER_SAMPLE` output frames are ready (or the end),
/// in the output format: channels remapped, rate converted.
fn next_decoded(r: &mut SoundReader) -> Option<(Vec<u8>, usize, usize, u64)> {
    let source_channels = r.parsed.format.channels.max(1) as usize;
    let decoder = r.decoder.as_mut()?;
    let step = decoder.rate / r.out.rate.max(1.0);
    let wanted_source = (FRAMES_PER_SAMPLE as f64 * step).ceil() as usize + 2;
    while r.pending.len() / source_channels < r.position as usize + wanted_source && r.decoded_frames < r.end {
        let Some(packet) = r.parsed.packet(r.next_packet) else { break };
        r.next_packet += 1;
        let mut decoded = Vec::new();
        decoder.decode(r.parsed.data.get(packet.offset..packet.offset + packet.size as usize).unwrap_or(&[]), packet.frames, &mut decoded);
        let frames = (decoded.len() / source_channels) as u64;
        let first = r.decoded_frames;
        r.decoded_frames += frames;
        // Keep only frames inside skip .. end.
        let from = r.skip.saturating_sub(first).min(frames) as usize;
        let to = r.end.saturating_sub(first).min(frames) as usize;
        if to > from {
            r.pending.extend_from_slice(&decoded[from * source_channels..to * source_channels]);
        }
    }
    let available = r.pending.len() / source_channels;
    let mut frames = Vec::new();
    let mut made = 0;
    while made < FRAMES_PER_SAMPLE {
        let at = r.position + made as f64 * step;
        let index = at as usize;
        if index >= available || (index + 1 >= available && step != 1.0 && r.decoded_frames < r.end) {
            break;
        }
        let next = (index + 1).min(available - 1);
        let part = (at - index as f64) as f32;
        for c in 0..source_channels {
            let a = r.pending[index * source_channels + c];
            let b = r.pending[next * source_channels + c];
            frames.push(a + (b - a) * part);
        }
        made += 1;
    }
    if made == 0 {
        return None;
    }
    let end = r.position + made as f64 * step;
    let used = (end as usize).min(available);
    r.pending.drain(..used * source_channels);
    r.position = end - used as f64;
    let mut remapped = Vec::with_capacity(made * r.out.channels as usize);
    audioqueue::remap(&frames, source_channels, r.out.channels as usize, &mut remapped);
    let mut bytes = vec![0u8; made * r.out.bytes_per_frame as usize];
    audioqueue::write_pcm(&remapped, &r.out, &mut bytes);
    let at = r.sent_frames;
    r.sent_frames += made as u64;
    Some((bytes, made, made, at))
}

extern "C" fn output_copy_next(this: Id, _: Sel) -> Id {
    let reader = property_get(this, "reader").unwrap_or(0) as Id;
    if reader == 0 || reader_status(reader) != STATUS_READING {
        return 0;
    }
    let next = {
        let mut guard = READERS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = guard.as_mut().and_then(|r| r.get_mut(&this)) else { return 0 };
        let next = if r.decoder.is_some() { next_decoded(r) } else { next_stored(r) };
        next.map(|n| (n, r.out))
    };
    match next {
        Some(((bytes, samples, frames, at), format)) => {
            let rate = format.rate.round().clamp(1.0, i32::MAX as f64) as i32;
            corevideo::make_audio_sample(bytes, format, samples, frames, CmTime::new(at as i64, rate))
        }
        None => {
            // This output is done; the reader completes when all are.
            READERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|r| r.remove(&this));
            let outputs = collections::array_items(property_get(reader, "outputs").unwrap_or(0) as Id);
            let busy = READERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|r| outputs.iter().any(|o| r.contains_key(o)));
            if !busy {
                property_set(reader, "status", STATUS_COMPLETED as u64);
            }
            0
        }
    }
}

// ----- the camera: none -----

fn empty_list() -> Id {
    collections::array_from(&[])
}

extern "C" fn devices(_class: Class, _: Sel) -> Id {
    empty_list()
}

extern "C" fn devices_with_type(_class: Class, _: Sel, _media: Id) -> Id {
    empty_list()
}

extern "C" fn default_device(_class: Class, _: Sel, media: Id) -> Id {
    crate::log!("AVCaptureDevice: no {} device here", string::get(media));
    0
}

/// AVAuthorizationStatusDenied.
extern "C" fn authorization_status(_class: Class, _: Sel, _media: Id) -> isize {
    2
}

/// Calls `block` later on the main thread through `call(invoke, block)`.
fn call_later(block: usize, call: fn(usize, usize)) {
    if block == 0 {
        return;
    }
    let copy: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(crate::libsystem::mach::block_copy as usize) };
    let block = copy(block);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let invoke = unsafe { *((block + 16) as *const usize) };
        call(invoke, block);
        // The copy goes once the handler has run.
        crate::libsystem::mach::block_release(block as *mut _);
    })));
}

/// The user says no (the answer comes later, as on iOS).
extern "C" fn request_access(_class: Class, _: Sel, media: Id, handler: usize) {
    crate::log!("AVCaptureDevice: {} access asked for; answered no (no camera or microphone here)", string::get(media));
    call_later(handler, |invoke, block| {
        let f: extern "C" fn(usize, bool) = unsafe { std::mem::transmute(invoke) };
        f(block, false);
    });
}

extern "C" fn device_input(_class: Class, _: Sel, _device: Id, error: *mut Id) -> Id {
    if !error.is_null() {
        unsafe { *error = misc::error(ERROR_DOMAIN, ERROR_NOT_AUTHORIZED, "Cannot use the camera: no access") };
    }
    0
}

extern "C" fn device_input_init(this: Id, s: Sel, device: Id, error: *mut Id) -> Id {
    let _ = this;
    device_input(0, s, device, error)
}

extern "C" fn session_init(this: Id, _: Sel) -> Id {
    property_set(this, "preset", string::make("AVCaptureSessionPresetHigh") as u64);
    property_set(this, "outputs", empty_list() as u64);
    this
}

extern "C" fn session_preset(this: Id, _: Sel) -> Id {
    property_get(this, "preset").unwrap_or(0) as Id
}

extern "C" fn session_set_preset(this: Id, _: Sel, preset: Id) {
    property_set(this, "preset", preset as u64);
}

extern "C" fn session_yes(_this: Id, _: Sel, _value: Id) -> bool {
    true
}

extern "C" fn session_no(_this: Id, _: Sel, _value: Id) -> bool {
    false
}

extern "C" fn session_add_input(_this: Id, _: Sel, _input: Id) {
    crate::log!("AVCaptureSession: an input was added, but there is no camera; nothing will be captured");
}

extern "C" fn session_add_output(this: Id, _: Sel, output: Id) {
    let mut list = collections::array_items(property_get(this, "outputs").unwrap_or(0) as Id);
    list.push(output);
    property_set(this, "outputs", collections::array_from(&list) as u64);
}

extern "C" fn session_remove_output(this: Id, _: Sel, output: Id) {
    let list: Vec<Id> = collections::array_items(property_get(this, "outputs").unwrap_or(0) as Id).into_iter().filter(|&o| o != output).collect();
    property_set(this, "outputs", collections::array_from(&list) as u64);
}

extern "C" fn session_outputs(this: Id, _: Sel) -> Id {
    property_get(this, "outputs").unwrap_or_else(|| empty_list() as u64) as Id
}

extern "C" fn session_inputs(_this: Id, _: Sel) -> Id {
    empty_list()
}

extern "C" fn session_start(_this: Id, _: Sel) {
    crate::log!("AVCaptureSession: started with no camera; no frames will come");
}

extern "C" fn session_nothing(_this: Id, _: Sel) {}

extern "C" fn session_not_running(_this: Id, _: Sel) -> bool {
    false
}

extern "C" fn data_output_init(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn data_output_settings(this: Id, _: Sel) -> Id {
    property_get(this, "videoSettings").unwrap_or(0) as Id
}

extern "C" fn data_output_set_settings(this: Id, _: Sel, settings: Id) {
    property_set(this, "videoSettings", settings as u64);
}

extern "C" fn data_output_set_delegate(this: Id, _: Sel, delegate: Id, queue: Id) {
    property_set(this, "delegate", delegate as u64);
    property_set(this, "queue", queue as u64);
}

extern "C" fn data_output_delegate(this: Id, _: Sel) -> Id {
    property_get(this, "delegate").unwrap_or(0) as Id
}

extern "C" fn data_output_formats(_this: Id, _: Sel) -> Id {
    let list: Vec<Id> = [corevideo::FORMAT_420V, corevideo::FORMAT_420F, corevideo::FORMAT_BGRA].iter().map(|&f| collections::number_int(f as i64)).collect();
    collections::array_from(&list)
}

extern "C" fn data_output_ignore_bool(_this: Id, _: Sel, _value: bool) {}

extern "C" fn data_output_no_connection(_this: Id, _: Sel, _media: Id) -> Id {
    0
}

extern "C" fn data_output_empty(_this: Id, _: Sel) -> Id {
    empty_list()
}

pub fn register(r: &mut Registry) {
    // Assets and tracks (AVAsset/AVURLAsset/AVPlayerItem come from fw::media).
    let asset = runtime::class_named("AVAsset");
    let url_asset = runtime::class_named("AVURLAsset");
    class_methods(asset, &[("assetWithURL:", asset_with_url as usize)]);
    methods(url_asset, &[("initWithURL:options:", asset_init_with_url as usize), ("URL", asset_url as usize)]);
    methods(
        asset,
        &[
            ("tracks", asset_tracks_method as usize),
            ("tracksWithMediaType:", asset_tracks_with_type as usize),
            ("trackWithTrackID:", asset_track_with_id as usize),
            ("duration", asset_duration as usize),
            ("isReadable", asset_is_readable as usize),
            ("isPlayable", asset_is_playable as usize),
            ("naturalSize", asset_natural_size as usize),
            ("loadValuesAsynchronouslyForKeys:completionHandler:", asset_load_values as usize),
            ("statusOfValueForKey:error:", asset_status_of_value as usize),
        ],
    );
    let track = define_class("AVAssetTrack", "NSObject", 8);
    methods(
        track,
        &[
            ("mediaType", track_media_type as usize),
            ("trackID", track_id as usize),
            ("asset", track_asset as usize),
            ("naturalSize", track_natural_size as usize),
            ("timeRange", track_time_range as usize),
            ("naturalTimeScale", track_time_scale as usize),
            ("nominalFrameRate", track_frame_rate as usize),
            ("preferredTransform", track_identity as usize),
            ("isPlayable", track_yes as usize),
            ("isEnabled", track_yes as usize),
            ("formatDescriptions", track_empty_list as usize),
        ],
    );
    let item = runtime::class_named("AVPlayerItem");
    methods(
        item,
        &[
            ("duration", item_duration as usize),
            ("tracks", item_tracks as usize),
            ("presentationSize", item_presentation_size as usize),
            ("audioMix", item_audio_mix as usize),
            ("setAudioMix:", item_set_audio_mix as usize),
        ],
    );
    let item_track = define_class("AVPlayerItemTrack", "NSObject", 8);
    methods(
        item_track,
        &[
            ("assetTrack", item_track_asset_track as usize),
            ("isEnabled", item_track_enabled as usize),
            ("setEnabled:", item_track_set_enabled as usize),
        ],
    );

    // Audio mixes.
    let mix = define_class("AVAudioMix", "NSObject", 8);
    let mutable_mix = define_class("AVMutableAudioMix", "AVAudioMix", 8);
    class_methods(mutable_mix, &[("audioMix", mix_new as usize)]);
    methods(mix, &[("inputParameters", mix_input_parameters as usize)]);
    methods(mutable_mix, &[("setInputParameters:", mix_set_input_parameters as usize)]);
    let parameters = define_class("AVAudioMixInputParameters", "NSObject", 8);
    let mutable_parameters = define_class("AVMutableAudioMixInputParameters", "AVAudioMixInputParameters", 8);
    class_methods(
        mutable_parameters,
        &[("audioMixInputParameters", parameters_new as usize), ("audioMixInputParametersWithTrack:", parameters_with_track as usize)],
    );
    methods(
        parameters,
        &[("trackID", parameters_track_id as usize), ("getVolumeRampForTime:startVolume:endVolume:timeRange:", parameters_get_ramp as usize)],
    );
    methods(
        mutable_parameters,
        &[
            ("setTrackID:", parameters_set_track_id as usize),
            ("setVolume:atTime:", parameters_set_volume as usize),
            ("setVolumeRampFromStartVolume:toEndVolume:timeRange:", parameters_set_ramp as usize),
        ],
    );

    // Reading tracks.
    let reader = define_class("AVAssetReader", "NSObject", 8);
    class_methods(reader, &[("assetReaderWithAsset:error:", reader_with_asset as usize)]);
    methods(
        reader,
        &[
            ("initWithAsset:error:", reader_init as usize),
            ("asset", reader_asset as usize),
            ("outputs", reader_outputs as usize),
            ("status", reader_status_method as usize),
            ("error", reader_error as usize),
            ("timeRange", reader_time_range as usize),
            ("setTimeRange:", reader_set_time_range as usize),
            ("canAddOutput:", reader_can_add_output as usize),
            ("addOutput:", reader_add_output as usize),
            ("startReading", reader_start as usize),
            ("cancelReading", reader_cancel as usize),
        ],
    );
    let output = define_class("AVAssetReaderOutput", "NSObject", 8);
    methods(
        output,
        &[
            ("mediaType", output_media_type as usize),
            ("alwaysCopiesSampleData", output_copies as usize),
            ("setAlwaysCopiesSampleData:", output_set_copies as usize),
            ("copyNextSampleBuffer", output_copy_next as usize),
        ],
    );
    let track_output = define_class("AVAssetReaderTrackOutput", "AVAssetReaderOutput", 8);
    class_methods(track_output, &[("assetReaderTrackOutputWithTrack:outputSettings:", output_with_track as usize)]);
    methods(
        track_output,
        &[("initWithTrack:outputSettings:", output_init as usize), ("track", output_track as usize), ("outputSettings", output_settings as usize)],
    );

    // The camera.
    let device = define_class("AVCaptureDevice", "NSObject", 8);
    class_methods(
        device,
        &[
            ("devices", devices as usize),
            ("devicesWithMediaType:", devices_with_type as usize),
            ("defaultDeviceWithMediaType:", default_device as usize),
            ("authorizationStatusForMediaType:", authorization_status as usize),
            ("requestAccessForMediaType:completionHandler:", request_access as usize),
        ],
    );
    let _ = define_class("AVCaptureInput", "NSObject", 8);
    let input = define_class("AVCaptureDeviceInput", "AVCaptureInput", 8);
    class_methods(input, &[("deviceInputWithDevice:error:", device_input as usize)]);
    methods(input, &[("initWithDevice:error:", device_input_init as usize)]);
    let session = define_class("AVCaptureSession", "NSObject", 8);
    methods(
        session,
        &[
            ("init", session_init as usize),
            ("sessionPreset", session_preset as usize),
            ("setSessionPreset:", session_set_preset as usize),
            ("canSetSessionPreset:", session_yes as usize),
            ("canAddInput:", session_no as usize),
            ("addInput:", session_add_input as usize),
            ("canAddOutput:", session_yes as usize),
            ("addOutput:", session_add_output as usize),
            ("removeOutput:", session_remove_output as usize),
            ("inputs", session_inputs as usize),
            ("outputs", session_outputs as usize),
            ("startRunning", session_start as usize),
            ("stopRunning", session_nothing as usize),
            ("beginConfiguration", session_nothing as usize),
            ("commitConfiguration", session_nothing as usize),
            ("isRunning", session_not_running as usize),
            ("isInterrupted", session_not_running as usize),
        ],
    );
    let _ = define_class("AVCaptureOutput", "NSObject", 8);
    let data_output = define_class("AVCaptureVideoDataOutput", "AVCaptureOutput", 8);
    methods(
        data_output,
        &[
            ("init", data_output_init as usize),
            ("videoSettings", data_output_settings as usize),
            ("setVideoSettings:", data_output_set_settings as usize),
            ("setSampleBufferDelegate:queue:", data_output_set_delegate as usize),
            ("sampleBufferDelegate", data_output_delegate as usize),
            ("availableVideoCVPixelFormatTypes", data_output_formats as usize),
            ("setAlwaysDiscardsLateVideoFrames:", data_output_ignore_bool as usize),
            ("connectionWithMediaType:", data_output_no_connection as usize),
            ("connections", data_output_empty as usize),
        ],
    );

    // Apple's own texts for these names.
    for (name, value) in [
        ("AVMediaTypeVideo", VIDEO),
        ("AVMediaTypeAudio", AUDIO),
        ("AVMediaTypeText", "text"),
        ("AVMediaTypeMuxed", "muxx"),
        ("AVMediaTypeMetadata", "meta"),
        ("AVFoundationErrorDomain", ERROR_DOMAIN),
        ("AVFormatIDKey", "AVFormatIDKey"),
        ("AVSampleRateKey", "AVSampleRateKey"),
        ("AVNumberOfChannelsKey", "AVNumberOfChannelsKey"),
        ("AVChannelLayoutKey", "AVChannelLayoutKey"),
        ("AVLinearPCMBitDepthKey", "AVLinearPCMBitDepthKey"),
        ("AVLinearPCMIsBigEndianKey", "AVLinearPCMIsBigEndianKey"),
        ("AVLinearPCMIsFloatKey", "AVLinearPCMIsFloatKey"),
        ("AVLinearPCMIsNonInterleaved", "AVLinearPCMIsNonInterleaved"),
    ] {
        r.data(name, leak(string::make(value)));
    }
    for preset in [
        "Photo", "High", "Medium", "Low", "352x288", "640x480", "1280x720", "1920x1080", "3840x2160", "iFrame960x540", "iFrame1280x720", "InputPriority",
    ] {
        let name: &'static str = Box::leak(format!("AVCaptureSessionPreset{preset}").into_boxed_str());
        r.data(name, leak(string::make(name)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn mp4_tracks_are_listed_with_size_length_and_rate() {
        // A 640x360 picture track, 2 s at 30 frames a second (timescale 600).
        let mut tkhd = vec![0u8; 84];
        tkhd[12..16].copy_from_slice(&7u32.to_be_bytes());
        tkhd[76..80].copy_from_slice(&(640u32 << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(360u32 << 16).to_be_bytes());
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&600u32.to_be_bytes());
        mdhd[16..20].copy_from_slice(&1200u32.to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let mut stsd = vec![0u8; 16];
        stsd[12..16].copy_from_slice(b"avc1");
        let mut stts = vec![0u8; 16];
        stts[4..8].copy_from_slice(&1u32.to_be_bytes());
        stts[8..12].copy_from_slice(&60u32.to_be_bytes());
        let stbl = [sized_box(b"stsd", &stsd), sized_box(b"stts", &stts)].concat();
        let minf = sized_box(b"stbl", &stbl);
        let mdia = [sized_box(b"mdhd", &mdhd), sized_box(b"hdlr", &hdlr), sized_box(b"minf", &minf)].concat();
        let trak = [sized_box(b"tkhd", &tkhd), sized_box(b"mdia", &mdia)].concat();
        let moov = sized_box(b"trak", &trak);
        let tracks = mp4_tracks(&moov);
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!((t.id, t.media, t.width, t.height, t.timescale, t.duration, t.samples), (7, VIDEO, 640.0, 360.0, 600, 1200, 60));
        assert_eq!(t.codec, u32::from_be_bytes(*b"avc1"));
        assert!((t.samples as f64 / t.seconds() - 30.0).abs() < 1e-9);

        // Broken files: a sample-time table claiming 4 billion entries reads
        // only what it holds; a 64-bit box size past the end ends the list.
        let lying = moov.windows(4).position(|w| w == b"stts").unwrap() + 8;
        let mut broken = moov.clone();
        broken[lying..lying + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(mp4_tracks(&broken)[0].samples, 60);
        let mut huge = 1u32.to_be_bytes().to_vec();
        huge.extend_from_slice(b"trak");
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(boxes(&huge).is_empty());
        // A movie whose sound can't be found is an error, not a crash.
        let mut movie = sized_box(b"ftyp", b"M4V \0\0\0\0M4V mp42");
        movie.extend_from_slice(&sized_box(b"moov", &broken));
        assert!(audiofile::parse(&movie).is_err());
    }
}
