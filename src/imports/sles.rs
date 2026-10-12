//! OpenSL ES (sound effects) for guest code.
//!
//! An OpenSL interface is a pointer to a pointer to a table of functions.
//! The guest gets its own 4-byte cell pointing at a guest table of bridge
//! stubs ("sl:Play.SetPlayState", ...); each cell maps to the real host
//! interface. Most methods only take numbers and pointers to numbers, which
//! are forwarded by a generic caller; the ones taking structs, interface
//! IDs or callbacks are handled one by one. Host calls run without the
//! guest lock, because OpenSL may wait for its audio thread, which calls
//! back into the game.

use std::collections::HashMap;

use crate::handler;
use crate::runtime::{mem, shared, stub_for, ArgSource, Args, Handler, Ret, Uc};

const SL_RESULT_SUCCESS: u32 = 0;
const SL_RESULT_PARAMETER_INVALID: u32 = 2;
const SL_RESULT_FEATURE_UNSUPPORTED: u32 = 12;

/// Method names and argument kinds (after `self`): 'u' number, 'p' pointer
/// to plain data, '*' handled by hand.
const INTERFACES: &[(&str, &[(&str, &str)])] = &[
    (
        "Object",
        &[
            ("Realize", "u"),
            ("Resume", "u"),
            ("GetState", "p"),
            ("GetInterface", "*"),
            ("RegisterCallback", "*"),
            ("AbortAsyncOperation", ""),
            ("Destroy", ""),
            ("SetPriority", "uu"),
            ("GetPriority", "pp"),
            ("SetLossOfControlInterfaces", "*"),
        ],
    ),
    (
        "Engine",
        &[
            ("CreateLEDDevice", "*"),
            ("CreateVibraDevice", "*"),
            ("CreateAudioPlayer", "*"),
            ("CreateAudioRecorder", "*"),
            ("CreateMidiPlayer", "*"),
            ("CreateListener", "*"),
            ("Create3DGroup", "*"),
            ("CreateOutputMix", "*"),
            ("CreateMetadataExtractor", "*"),
            ("CreateExtensionObject", "*"),
            ("QueryNumSupportedInterfaces", "up"),
            ("QuerySupportedInterfaces", "*"),
            ("QueryNumSupportedExtensions", "p"),
            ("QuerySupportedExtension", "upp"),
            ("IsExtensionSupported", "pp"),
        ],
    ),
    (
        "Play",
        &[
            ("SetPlayState", "u"),
            ("GetPlayState", "p"),
            ("GetDuration", "p"),
            ("GetPosition", "p"),
            ("RegisterCallback", "*"),
            ("SetCallbackEventsMask", "u"),
            ("GetCallbackEventsMask", "p"),
            ("SetMarkerPosition", "u"),
            ("ClearMarkerPosition", ""),
            ("GetMarkerPosition", "p"),
            ("SetPositionUpdatePeriod", "u"),
            ("GetPositionUpdatePeriod", "p"),
        ],
    ),
    ("Seek", &[("SetPosition", "uu"), ("SetLoop", "uuu"), ("GetLoop", "ppp")]),
    (
        "Volume",
        &[
            ("SetVolumeLevel", "u"),
            ("GetVolumeLevel", "p"),
            ("GetMaxVolumeLevel", "p"),
            ("SetMute", "u"),
            ("GetMute", "p"),
            ("EnableStereoPosition", "u"),
            ("IsEnabledStereoPosition", "p"),
            ("SetStereoPosition", "u"),
            ("GetStereoPosition", "p"),
        ],
    ),
    (
        "PrefetchStatus",
        &[
            ("GetPrefetchStatus", "p"),
            ("GetFillLevel", "p"),
            ("RegisterCallback", "*"),
            ("SetCallbackEventsMask", "u"),
            ("GetCallbackEventsMask", "p"),
            ("SetFillUpdatePeriod", "u"),
            ("GetFillUpdatePeriod", "p"),
        ],
    ),
    ("BufferQueue", &[("Enqueue", "pu"), ("Clear", ""), ("GetState", "p"), ("RegisterCallback", "*")]),
];

/// Which interface type each SL_IID_ name gives.
const INTERFACE_IDS: &[(&str, &str)] = &[
    ("SL_IID_OBJECT", "Object"),
    ("SL_IID_ENGINE", "Engine"),
    ("SL_IID_PLAY", "Play"),
    ("SL_IID_SEEK", "Seek"),
    ("SL_IID_VOLUME", "Volume"),
    ("SL_IID_PREFETCHSTATUS", "PrefetchStatus"),
    ("SL_IID_BUFFERQUEUE", "BufferQueue"),
    ("SL_IID_ANDROIDSIMPLEBUFFERQUEUE", "BufferQueue"),
];

#[derive(Default)]
pub struct Sound {
    /// Guest interface cell → (host interface pointer, interface type).
    interfaces: HashMap<u32, (usize, &'static str)>,
    /// Interface type → guest function table.
    tables: HashMap<&'static str, u32>,
}

/// The real 16-byte interface ID behind an SL_IID_ name.
fn host_interface_id(name: &str) -> Option<usize> {
    #[cfg(target_os = "android")]
    unsafe {
        let library = libc::dlopen(c"libOpenSLES.so".as_ptr(), libc::RTLD_NOW);
        let symbol = std::ffi::CString::new(name).ok()?;
        let variable = libc::dlsym(library, symbol.as_ptr()) as *const usize;
        if variable.is_null() {
            return None;
        }
        Some(*variable)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = name;
        None
    }
}

/// Guest data for an `SL_IID_*` import: a pointer to a copy of the real ID.
pub fn interface_id_import(name: &str) -> u32 {
    let mut shared = shared();
    let id = shared.misc_alloc(16, 4);
    match host_interface_id(name) {
        Some(host) => mem().write_bytes(id, unsafe { std::slice::from_raw_parts(host as *const u8, 16) }),
        None => mem().write_bytes(id, &format!("{name:<16}").as_bytes()[..16]),
    }
    let pointer = shared.misc_alloc(4, 4);
    mem().write_u32(pointer, id);
    pointer
}

/// (host interface ID pointer, interface type) for a guest interface ID.
fn resolve_interface_id(guest_id: u32) -> Option<(usize, &'static str)> {
    if guest_id == 0 {
        return None;
    }
    let wanted = mem().bytes(guest_id, 16).to_vec();
    for (name, kind) in INTERFACE_IDS {
        if let Some(host) = host_interface_id(name) {
            if unsafe { std::slice::from_raw_parts(host as *const u8, 16) } == wanted.as_slice() {
                return Some((host, kind));
            }
        }
    }
    // Interface types the bridge doesn't know still work through GetInterface;
    // the matching host ID is found by scanning a few more names.
    for name in [
        "SL_IID_ANDROIDCONFIGURATION",
        "SL_IID_ANDROIDEFFECT",
        "SL_IID_ANDROIDEFFECTSEND",
        "SL_IID_EFFECTSEND",
        "SL_IID_METADATAEXTRACTION",
        "SL_IID_PLAYBACKRATE",
        "SL_IID_MUTESOLO",
        "SL_IID_ANDROIDBUFFERQUEUESOURCE",
    ] {
        if let Some(host) = host_interface_id(name) {
            if unsafe { std::slice::from_raw_parts(host as *const u8, 16) } == wanted.as_slice() {
                return Some((host, "Unknown"));
            }
        }
    }
    None
}

fn table_for(kind: &'static str) -> u32 {
    if let Some(&table) = shared().state.sound.tables.get(kind) {
        return table;
    }
    let methods: Vec<String> = match INTERFACES.iter().find(|(name, _)| *name == kind) {
        Some((_, methods)) => methods.iter().map(|(method, _)| format!("sl:{kind}.{method}")).collect(),
        None => (0..32).map(|i| format!("sl:{kind}.{i}")).collect(),
    };
    let table = shared().misc_alloc(methods.len() as u32 * 4, 4);
    for (i, method) in methods.iter().enumerate() {
        let stub = stub_for(method);
        mem().write_u32(table + i as u32 * 4, stub);
    }
    shared().state.sound.tables.insert(kind, table);
    table
}

/// A guest interface cell for a host interface pointer.
fn wrap(host: usize, kind: &'static str) -> u32 {
    if host == 0 {
        return 0;
    }
    let table = table_for(kind);
    let cell = shared().misc_alloc(4, 4);
    mem().write_u32(cell, table);
    shared().state.sound.interfaces.insert(cell, (host, kind));
    cell
}

fn unwrap(cell: u32) -> usize {
    shared().state.sound.interfaces.get(&cell).map(|(host, _)| *host).unwrap_or(0)
}

/// Calls entry `index` of a host interface's function table with integer-sized arguments.
fn call_host(interface: usize, index: usize, args: &[u64]) -> u32 {
    let table = unsafe { *(interface as *const *const usize) };
    let function = unsafe { *table.add(index) };
    let this = interface as u64;
    crate::runtime::without_gil(|| unsafe {
        match args.len() {
            0 => std::mem::transmute::<usize, extern "C" fn(u64) -> u32>(function)(this),
            1 => std::mem::transmute::<usize, extern "C" fn(u64, u64) -> u32>(function)(this, args[0]),
            2 => std::mem::transmute::<usize, extern "C" fn(u64, u64, u64) -> u32>(function)(this, args[0], args[1]),
            3 => std::mem::transmute::<usize, extern "C" fn(u64, u64, u64, u64) -> u32>(function)(
                this, args[0], args[1], args[2],
            ),
            4 => std::mem::transmute::<usize, extern "C" fn(u64, u64, u64, u64, u64) -> u32>(function)(
                this, args[0], args[1], args[2], args[3],
            ),
            5 => std::mem::transmute::<usize, extern "C" fn(u64, u64, u64, u64, u64, u64) -> u32>(function)(
                this, args[0], args[1], args[2], args[3], args[4],
            ),
            _ => std::mem::transmute::<usize, extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u32>(function)(
                this, args[0], args[1], args[2], args[3], args[4], args[5],
            ),
        }
    })
}

/// Host copy of a guest SLDataSource / SLDataSink ({locator*, format*}).
/// Each field is widened to an 8-byte slot, which matches the 64-bit C
/// layout of these structs (a u32 type first, then 8-aligned pointers).
/// The buffers are kept in `keep` until the call returns.
fn convert_data(guest: u32, keep: &mut Vec<Vec<u64>>) -> *mut u64 {
    if guest == 0 {
        return std::ptr::null_mut();
    }
    let memory = mem();
    let (locator, format) = (memory.read_u32(guest), memory.read_u32(guest + 4));
    let host_locator: Vec<u64> = if locator == 0 {
        Vec::new()
    } else {
        let kind = memory.read_u32(locator);
        match kind {
            // URI: {type, char* uri}
            1 => vec![kind as u64, memory.host_or_null(memory.read_u32(locator + 4)) as u64],
            // ADDRESS: {type, void* address, length}
            2 => vec![kind as u64, memory.host_or_null(memory.read_u32(locator + 4)) as u64, memory.read_u32(locator + 8) as u64],
            // OUTPUTMIX: {type, SLObjectItf}
            4 => vec![kind as u64, unwrap(memory.read_u32(locator + 4)) as u64],
            // Android FD: {type, fd, i64 offset, i64 length} has the same layout on both sides.
            0x800007BC => {
                let words: Vec<u64> = (0..3).map(|i| memory.read_u64(locator + i * 8)).collect();
                words
            }
            // Buffer queues: {type, count}, packed into one 8-byte word.
            _ => vec![kind as u64 | ((memory.read_u32(locator + 4) as u64) << 32)],
        }
    };
    let host_format: Vec<u64> = if format == 0 {
        Vec::new()
    } else {
        let kind = memory.read_u32(format);
        match kind {
            // MIME: {type, char* mime, container}
            1 => vec![kind as u64, memory.host_or_null(memory.read_u32(format + 4)) as u64, memory.read_u32(format + 8) as u64],
            // PCM and others: all 32-bit fields, same layout; copy 8 words.
            _ => (0..4).map(|i| memory.read_u64(format + i * 8)).collect(),
        }
    };
    keep.push(host_locator);
    keep.push(host_format);
    let locator_pointer = if locator == 0 { 0 } else { keep[keep.len() - 2].as_ptr() as u64 };
    let format_pointer = if format == 0 { 0 } else { keep[keep.len() - 1].as_ptr() as u64 };
    keep.push(vec![locator_pointer, format_pointer]);
    keep.last_mut().unwrap().as_mut_ptr()
}

/// Host arrays for (count, guest interface ID array, guest required array).
fn convert_interface_ids(count: u32, ids: u32) -> Vec<u64> {
    (0..count)
        .map(|i| resolve_interface_id(mem().read_u32(ids + i * 4)).map(|(host, _)| host as u64).unwrap_or(0))
        .collect()
}

struct CallbackRecord {
    function: u32,
    context: u32,
    interface: u32,
}

/// Runs a guest OpenSL callback from the host's audio thread.
fn run_callback(record: &CallbackRecord, extra: &[u32]) {
    let _lock = crate::runtime::hold_gil();
    let uc = crate::runtime::engine();
    let mut args = vec![record.interface, record.context];
    args.extend_from_slice(extra);
    if let Err(error) = crate::runtime::call_guest(uc, record.function, &args) {
        crate::log::line(&format!("[bridge] sound callback failed: {error}"));
    }
}

extern "C" fn event_callback(_caller: usize, context: *mut CallbackRecord, event: u32) {
    run_callback(unsafe { &*context }, &[event]);
}

extern "C" fn queue_callback(_caller: usize, context: *mut CallbackRecord) {
    run_callback(unsafe { &*context }, &[]);
}

fn register_callback(this: u32, host: usize, index: usize, a: &mut Args, queue: bool) -> u32 {
    let (function, context) = (a.u32(), a.u32());
    if function == 0 {
        return call_host(host, index, &[0, 0]);
    }
    let record = Box::into_raw(Box::new(CallbackRecord { function, context, interface: this }));
    let trampoline = if queue { queue_callback as *const () as usize } else { event_callback as *const () as usize };
    call_host(host, index, &[trampoline as u64, record as u64])
}

fn create_object(a: &mut Args, host: usize, index: usize, with_source: bool, with_sink: bool) -> u32 {
    let out = a.u32();
    let mut keep = Vec::new();
    let source = if with_source { convert_data(a.u32(), &mut keep) } else { std::ptr::null_mut() };
    let sink = if with_sink { convert_data(a.u32(), &mut keep) } else { std::ptr::null_mut() };
    let (count, ids, required) = (a.u32(), a.u32(), a.u32());
    let host_ids = convert_interface_ids(count, ids);
    let mut created: usize = 0;
    let mut args = vec![&mut created as *mut usize as u64];
    if with_source {
        args.push(source as u64);
    }
    if with_sink {
        args.push(sink as u64);
    }
    args.extend([count as u64, host_ids.as_ptr() as u64, mem().host_or_null(required) as u64]);
    let result = call_host(host, index, &args);
    drop(keep);
    if result == SL_RESULT_SUCCESS {
        mem().write_u32(out, wrap(created, "Object"));
    }
    result
}

fn dispatch(uc: &mut Uc, a: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let Some((kind, method)) = name.trim_start_matches("sl:").split_once('.') else { return Ret::U32(0) };
    let this = a.u32();
    let host = unwrap(this);
    if host == 0 {
        crate::log::line(&format!("[bridge] {name} on an unknown sound interface {this:#x}"));
        return Ret::U32(SL_RESULT_PARAMETER_INVALID);
    }
    let Some((_, methods)) = INTERFACES.iter().find(|(k, _)| *k == kind) else {
        crate::log::line(&format!("[bridge] {name}: sound interface not bridged"));
        return Ret::U32(SL_RESULT_FEATURE_UNSUPPORTED);
    };
    let Some(index) = methods.iter().position(|(m, _)| *m == method) else { return Ret::U32(0) };
    let spec = methods[index].1;
    if spec != "*" {
        let args: Vec<u64> = spec
            .bytes()
            .map(|kind| {
                let value = a.u32();
                if kind == b'p' {
                    mem().host_or_null(value) as u64
                } else {
                    value as u64
                }
            })
            .collect();
        let result = call_host(host, index, &args);
        if method == "Destroy" {
            shared().state.sound.interfaces.remove(&this);
        }
        return Ret::U32(result);
    }
    let result = match (kind, method) {
        ("Object", "GetInterface") => {
            let (id, out) = (a.u32(), a.u32());
            match resolve_interface_id(id) {
                Some((host_id, interface_kind)) => {
                    let mut found: usize = 0;
                    let result = call_host(host, index, &[host_id as u64, &mut found as *mut usize as u64]);
                    if result == SL_RESULT_SUCCESS {
                        mem().write_u32(out, wrap(found, interface_kind));
                    }
                    result
                }
                None => SL_RESULT_FEATURE_UNSUPPORTED,
            }
        }
        ("Engine", "CreateAudioPlayer") => create_object(a, host, index, true, true),
        ("Engine", "CreateOutputMix") => create_object(a, host, index, false, false),
        ("Engine", "CreateAudioRecorder") => create_object(a, host, index, true, true),
        ("Play", "RegisterCallback") | ("PrefetchStatus", "RegisterCallback") | ("Object", "RegisterCallback") => {
            register_callback(this, host, index, a, false)
        }
        ("BufferQueue", "RegisterCallback") => register_callback(this, host, index, a, true),
        _ => {
            crate::log::line(&format!("[bridge] {name} is not bridged yet"));
            SL_RESULT_FEATURE_UNSUPPORTED
        }
    };
    Ret::U32(result)
}

pub fn handler(name: &str) -> Option<Handler> {
    if name.starts_with("sl:") {
        return handler!(|uc, a| dispatch(uc, a));
    }
    match name {
        "slCreateEngine" => handler!(|uc, a| {
            let (out, option_count, options) = (a.u32(), a.u32(), a.u32());
            let (count, ids, required) = (a.u32(), a.u32(), a.u32());
            #[cfg(target_os = "android")]
            {
                #[link(name = "OpenSLES")]
                extern "C" {
                    fn slCreateEngine(
                        engine: *mut usize,
                        option_count: u32,
                        options: *const u8,
                        count: u32,
                        ids: *const u64,
                        required: *const u8,
                    ) -> u32;
                }
                let host_ids = convert_interface_ids(count, ids);
                let mut engine: usize = 0;
                // SLEngineOption is {u32, u32}: same layout on both sides.
                let result = crate::runtime::without_gil(|| unsafe {
                    slCreateEngine(
                        &mut engine,
                        option_count,
                        mem().host_or_null(options),
                        count,
                        host_ids.as_ptr(),
                        mem().host_or_null(required),
                    )
                });
                if result == SL_RESULT_SUCCESS {
                    mem().write_u32(out, wrap(engine, "Object"));
                }
                return Ret::U32(result);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = (out, option_count, options, count, ids, required);
                Ret::U32(SL_RESULT_FEATURE_UNSUPPORTED)
            }
        }),
        _ => None,
    }
}
