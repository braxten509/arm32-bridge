//! AudioToolbox for 32-bit games: AudioFile, ExtAudioFile, system sounds,
//! audio units (AudioComponent, RemoteIO, AUGraph) and the C AudioSession
//! calls, on this layer's (fw/audiofile.rs, fw/extaudiofile.rs, fw/sound.rs,
//! fw/media.rs). File ids, components, graphs and units are host pointers (handles
//! here); an AudioBufferList holds pointers, so it is rebuilt in the host's
//! layout for the call and the byte counts copied back. A unit's render
//! callback is game code the host's sound thread calls: see `guest_render`.

use super::{
    forget_handle, handle_to_guest, handle_to_host, host_fn, host_functions, object_to_guest, object_to_host, register_all, Byte, Handle,
    Obj, Ptr, F32, I32, I64, U32,
};
use crate::fw::extaudiofile::{BUFFER_LIST_FIRST, BUFFER_SIZE};
use arm32_bridge::handler;
use arm32_bridge::runtime::{self, mem, ArgSource, Ret};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Runs `work` with a host AudioBufferList made from the guest's
/// `{ UInt32 count; { UInt32 channels; UInt32 bytes; void* data (4 bytes) } ... }`
/// (data pointers into guest memory), then copies the byte counts back.
pub fn with_host_buffer_list<R>(guest: u32, work: impl FnOnce(usize) -> R) -> Option<R> {
    if guest == 0 {
        return None;
    }
    let count = mem().read_u32(guest) as usize;
    let mut host = vec![0u64; (BUFFER_LIST_FIRST + count.max(1) * BUFFER_SIZE) / 8];
    let bytes = host.as_mut_ptr() as *mut u8;
    unsafe {
        *(bytes as *mut u32) = count as u32;
        for i in 0..count {
            let from = guest + 4 + i as u32 * 12;
            let to = bytes.add(BUFFER_LIST_FIRST + i * BUFFER_SIZE);
            *(to as *mut u32) = mem().read_u32(from);
            *(to.add(4) as *mut u32) = mem().read_u32(from + 4);
            *(to.add(8) as *mut *mut u8) = mem().host_or_null(mem().read_u32(from + 8));
        }
    }
    let result = work(bytes as usize);
    for i in 0..count {
        let size = unsafe { *(bytes.add(BUFFER_LIST_FIRST + i * BUFFER_SIZE + 4) as *const u32) };
        mem().write_u32(guest + 4 + i as u32 * 12 + 4, size);
    }
    Some(result)
}

/// Opens through `open` (which writes a host id), handing the guest a handle.
fn open_into(out: u32, open: impl FnOnce(*mut usize) -> i32) -> Ret {
    let mut id = 0usize;
    let status = open(&mut id);
    if out != 0 {
        mem().write_u32(out, if status == 0 { handle_to_guest(id) } else { 0 });
    }
    Ret::U32(status as u32)
}

// ----- audio units: the game's render callback -----

/// kAudioUnitProperty_SetRenderCallback.
const SET_RENDER_CALLBACK: u32 = 23;
/// kAudioUnitErr_InvalidPropertyValue.
const INVALID_PROPERTY_VALUE: i32 = -10851;
/// kAudioUnitRenderAction_OutputIsSilence.
const OUTPUT_IS_SILENCE: u32 = 1 << 4;
/// How long the sound device waits for its turn to run game code; past it
/// that buffer is silence, so a busy game never holds up the device.
const RENDER_WAIT: Duration = Duration::from_millis(5);
/// The 64-byte AudioTimeStamp (the same layout in 32 and 64 bits).
const STAMP_SIZE: u32 = 64;

/// A game render callback (AURenderCallbackStruct) as the host calls it.
struct GuestRender {
    function: u32,
    refcon: u32,
}

/// The host refCon for a game callback: one per distinct callback, kept for
/// good (the sound thread may still be using it when the game replaces it).
pub(crate) fn guest_render_for(function: u32, refcon: u32) -> usize {
    static MADE: Mutex<Vec<(u32, u32, usize)>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, _, render)) = made.iter().find(|m| m.0 == function && m.1 == refcon) {
        return render;
    }
    let render = Box::leak(Box::new(GuestRender { function, refcon })) as *const GuestRender as usize;
    made.push((function, refcon, render));
    render
}

thread_local! {
    /// This sound thread's guest block for one render call (address, size).
    static RENDER_BLOCK: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    /// This thread's guest scratch for AudioUnitRender's empty buffers.
    static INPUT_BLOCK: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    /// This sound thread's guest thread number, once it has an engine.
    static RENDER_THREAD: Cell<u32> = const { Cell::new(0) };
    /// Host copies of buffers the game supplied itself, for the host to read.
    static OWN_COPIES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// Gives this thread's guest things back when it ends.
    static RETIRE: Retire = const { Retire };
}

/// A sound thread ending (AAudio runs each stream on a thread of its own: a
/// device change or a trip to the background opens a new one) gives back
/// its guest block and engine (with its guest thread's stack and number).
struct Retire;

impl Drop for Retire {
    fn drop(&mut self) {
        // First touched after this thread's engine and lock state were made,
        // so this runs before those thread-locals end (they end in reverse).
        let _gil = runtime::hold_gil();
        for slot in [&RENDER_BLOCK, &INPUT_BLOCK] {
            let (block, _) = slot.with(|b| b.replace((0, 0)));
            if block != 0 {
                super::super::free(block);
            }
        }
        #[cfg(test)]
        RETIRED.lock().unwrap_or_else(|e| e.into_inner()).push(RENDER_THREAD.with(|t| t.get()));
        runtime::drop_engine();
    }
}

#[cfg(test)]
static RETIRED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// The guest thread numbers of sound threads that ended.
#[cfg(test)]
pub(crate) fn retired_render_threads() -> Vec<u32> {
    RETIRED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A guest block of at least `size` bytes kept in `slot`, reused from call to call.
fn reused_block(slot: &'static std::thread::LocalKey<Cell<(u32, u32)>>, size: u32) -> u32 {
    slot.with(|block| {
        let (address, have) = block.get();
        if address != 0 && have >= size {
            return address;
        }
        if address != 0 {
            super::super::free(address);
        }
        let address = super::super::alloc(size);
        block.set((address, if address == 0 { 0 } else { size }));
        address
    })
}

/// Where the parts of a render call sit in its guest block: flags, the
/// AudioTimeStamp (doubles: 8-aligned), the list (count, then { channels,
/// size, data } 12 bytes apart), then the buffers.
const FLAGS_AT: u32 = 0;
const STAMP_AT: u32 = 8;
const LIST_AT: u32 = STAMP_AT + STAMP_SIZE;

/// The host's call of a game render callback(refCon, *flags, *stamp, bus,
/// frames, *ioData), on the sound thread: the arguments are rebuilt in guest
/// memory, the game's code runs under the guest lock, and its samples are
/// copied into host memory before the lock goes (the host's own buffers, or
/// this thread's copies of buffers the game supplied itself).
pub(crate) extern "C" fn guest_render(data: usize, flags: *mut u32, stamp: *const u8, bus: u32, frames: u32, list: *mut u8) -> i32 {
    let render = unsafe { &*(data as *const GuestRender) };
    let Some(_gil) = runtime::try_hold_gil(RENDER_WAIT) else {
        if !flags.is_null() {
            unsafe { *flags |= OUTPUT_IS_SILENCE };
        }
        return 0;
    };
    let count = if list.is_null() { 0 } else { unsafe { *(list as *const u32) as usize } };
    // Host AudioBufferList: count, then { channels, size, data (8 bytes) } from 8, 16 bytes apart.
    let entry = |b: usize| unsafe { list.add(8 + 16 * b) };
    let host_buffers: Vec<(u32, u32, *mut u8)> =
        (0..count).map(|b| unsafe { (*(entry(b) as *const u32), *(entry(b).add(4) as *const u32), *(entry(b).add(8) as *const *mut u8)) }).collect();
    let first_data = (LIST_AT + 4 + 12 * count as u32 + 7) & !7;
    let total = first_data + host_buffers.iter().map(|b| (b.1 + 7) & !7).sum::<u32>();
    let block = reused_block(&RENDER_BLOCK, total);
    if block == 0 {
        return -108; // kAudio_MemFullError
    }
    let m = mem();
    m.write_u32(block + FLAGS_AT, if flags.is_null() { 0 } else { unsafe { *flags } });
    match stamp.is_null() {
        true => m.write_bytes(block + STAMP_AT, &[0; STAMP_SIZE as usize]),
        false => m.write_bytes(block + STAMP_AT, unsafe { std::slice::from_raw_parts(stamp, STAMP_SIZE as usize) }),
    }
    m.write_u32(block + LIST_AT, count as u32);
    let mut data_at = block + first_data;
    for (b, &(channels, size, host_data)) in host_buffers.iter().enumerate() {
        let at = block + LIST_AT + 4 + 12 * b as u32;
        m.write_u32(at, channels);
        m.write_u32(at + 4, size);
        // No host buffer: the caller asks the game to supply its own.
        if host_data.is_null() {
            m.write_u32(at + 8, 0);
            continue;
        }
        m.write_bytes(data_at, unsafe { std::slice::from_raw_parts(host_data, size as usize) });
        m.write_u32(at + 8, data_at);
        data_at += (size + 7) & !7;
    }
    let uc = runtime::engine();
    RENDER_THREAD.with(|t| t.set(uc.get_data().id));
    RETIRE.with(|_| ());
    let args = [render.refcon, block + FLAGS_AT, block + STAMP_AT, bus, frames, block + LIST_AT];
    let status = match runtime::call_guest(uc, render.function, &args) {
        Ok(status) => status as u32 as i32,
        Err(error) => {
            static TOLD: AtomicBool = AtomicBool::new(false);
            if !TOLD.swap(true, Ordering::Relaxed) {
                crate::log!("32-bit audio: the game's render callback {:#x} failed: {error}", render.function);
            }
            return -50;
        }
    };
    if !flags.is_null() {
        unsafe { *flags = m.read_u32(block + FLAGS_AT) };
    }
    if list.is_null() {
        return status;
    }
    let returned = m.read_u32(block + LIST_AT) as usize;
    unsafe { *(list as *mut u32) = returned as u32 };
    let guest_entry = |b: usize| block + LIST_AT + 4 + 12 * b as u32;
    // Room for copies of the game's own buffers, made all at once so growing
    // never moves a copy already handed to the host.
    let own_total: usize = (0..count.min(returned))
        .filter(|&b| host_buffers[b].2.is_null() && m.read_u32(guest_entry(b) + 8) != 0)
        .map(|b| m.read_u32(guest_entry(b) + 4) as usize)
        .sum();
    let mut copy_at = OWN_COPIES.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() < own_total {
            c.resize(own_total, 0);
        }
        c.as_mut_ptr()
    });
    for (b, &(_, room, host_data)) in host_buffers.iter().enumerate().take(returned) {
        let at = guest_entry(b);
        let (size, pointer) = (m.read_u32(at + 4), m.read_u32(at + 8));
        unsafe {
            *(entry(b) as *mut u32) = m.read_u32(at);
            if pointer == 0 {
                *(entry(b).add(8) as *mut usize) = 0;
                continue;
            }
            // Another game thread may reuse its buffer once the lock goes: the host reads a copy.
            let (to, size) = if host_data.is_null() { (copy_at, size) } else { (host_data, size.min(room)) };
            std::ptr::copy_nonoverlapping(m.bytes(pointer, size as usize).as_ptr(), to, size as usize);
            *(entry(b).add(4) as *mut u32) = size;
            *(entry(b).add(8) as *mut *mut u8) = to;
            if host_data.is_null() {
                copy_at = copy_at.add(size as usize);
            }
        }
    }
    status
}

pub fn register() {
    register_all(host_functions! {
        "AudioComponentFindNext" => fn(after: Handle, description: Ptr) -> Handle;
        "AudioComponentGetDescription" => fn(component: Handle, description: Ptr) -> I32;
        "AudioUnitInitialize" => fn(unit: Handle) -> I32;
        "AudioUnitUninitialize" => fn(unit: Handle) -> I32;
        "AUGraphOpen" => fn(graph: Handle) -> I32;
        "AUGraphInitialize" => fn(graph: Handle) -> I32;
        "AUGraphUpdate" => fn(graph: Handle) -> I32;
        "AUGraphUninitialize" => fn(graph: Handle) -> I32;
        "AUGraphClose" => fn(graph: Handle) -> I32;
        "AUGraphIsRunning" => fn(graph: Handle, running: Ptr) -> I32;
        "AUGraphAddNode" => fn(graph: Handle, description: Ptr, node: Ptr) -> I32;
        "AUGraphConnectNodeInput" => fn(graph: Handle, from: I32, from_output: U32, to: I32, to_input: U32) -> I32;
        "CAShow" => fn(object: Handle);
        "AudioUnitGetProperty" => fn(unit: Handle, property: U32, scope: U32, element: U32, data: Ptr, size: Ptr) -> I32;
        "AudioUnitSetParameter" => fn(unit: Handle, parameter: U32, scope: U32, element: U32, value: F32, frames: U32) -> I32;
        "AudioUnitGetParameter" => fn(unit: Handle, parameter: U32, scope: U32, element: U32, value: Ptr) -> I32;
        "AudioFileGetPropertyInfo" => fn(file: Handle, property: U32, size: Ptr, writable: Ptr) -> I32;
        "AudioFileGetProperty" => fn(file: Handle, property: U32, size: Ptr, data: Ptr) -> I32;
        "AudioFileReadBytes" => fn(file: Handle, cache: Byte, start: I64, bytes: Ptr, out: Ptr) -> I32;
        "AudioFileReadPacketData" => fn(file: Handle, cache: Byte, bytes: Ptr, descriptions: Ptr, start: I64, packets: Ptr, out: Ptr) -> I32;
        "AudioFileReadPackets" => fn(file: Handle, cache: Byte, bytes: Ptr, descriptions: Ptr, start: I64, packets: Ptr, out: Ptr) -> I32;
        "ExtAudioFileGetProperty" => fn(file: Handle, property: U32, size: Ptr, data: Ptr) -> I32;
        "ExtAudioFileSetProperty" => fn(file: Handle, property: U32, size: U32, data: Ptr) -> I32;
        "ExtAudioFileSeek" => fn(file: Handle, frame: I64) -> I32;
        "ExtAudioFileTell" => fn(file: Handle, out: Ptr) -> I32;
        "AudioSessionSetProperty" => fn(property: U32, size: U32, data: Ptr) -> I32;
        "AudioSessionSetActive" => fn(active: Byte) -> I32;
        "AudioServicesCreateSystemSoundID" => fn(url: Obj, out: Ptr) -> I32;
        "AudioServicesDisposeSystemSoundID" => fn(id: U32) -> I32;
        "AudioServicesPlaySystemSound" => fn(id: U32);
        "AudioServicesPlayAlertSound" => fn(id: U32);
    });
    let function = |name: &str, handler: Option<arm32_bridge::runtime::Handler>| super::super::function(name, handler.expect(name));
    // AudioFileOpenURL(url, permissions, typeHint, *file).
    function(
        "AudioFileOpenURL",
        handler!(|uc, a| {
            let (url, permissions, hint, out) = (object_to_host(a.u32()), a.u32() as i8, a.u32(), a.u32());
            let Some(open) = host_fn!("AudioFileOpenURL": unsafe extern "C" fn(usize, i8, u32, *mut usize) -> i32) else {
                return super::no_host_function(uc, "AudioFileOpenURL");
            };
            open_into(out, |id| unsafe { open(url, permissions, hint, id) })
        }),
    );
    // ExtAudioFileOpenURL(url, *file).
    function(
        "ExtAudioFileOpenURL",
        handler!(|uc, a| {
            let (url, out) = (object_to_host(a.u32()), a.u32());
            let Some(open) = host_fn!("ExtAudioFileOpenURL": unsafe extern "C" fn(usize, *mut usize) -> i32) else {
                return super::no_host_function(uc, "ExtAudioFileOpenURL");
            };
            open_into(out, |id| unsafe { open(url, id) })
        }),
    );
    function(
        "AudioFileClose",
        handler!(|uc, a| {
            let id = forget_handle(a.u32());
            let close = host_fn!("AudioFileClose": unsafe extern "C" fn(usize) -> i32);
            Ret::U32(close.map_or(-50, |close| unsafe { close(id) }) as u32)
        }),
    );
    function(
        "ExtAudioFileDispose",
        handler!(|uc, a| {
            let id = forget_handle(a.u32());
            let dispose = host_fn!("ExtAudioFileDispose": unsafe extern "C" fn(usize) -> i32);
            Ret::U32(dispose.map_or(-50, |dispose| unsafe { dispose(id) }) as u32)
        }),
    );
    // ExtAudioFileRead(file, *frames, AudioBufferList*).
    function(
        "ExtAudioFileRead",
        handler!(|uc, a| {
            let (id, frames, list) = (handle_to_host(a.u32()), a.u32(), a.u32());
            let Some(read) = host_fn!("ExtAudioFileRead": unsafe extern "C" fn(usize, *mut u32, *mut u8) -> i32) else {
                return super::no_host_function(uc, "ExtAudioFileRead");
            };
            let status = with_host_buffer_list(list, |host| unsafe { read(id, mem().host_or_null(frames) as *mut u32, host as *mut u8) });
            Ret::U32(status.unwrap_or(-50) as u32)
        }),
    );
    // AudioSessionInitialize(runLoop, mode, listener, data): the listener is
    // guest code the host never calls; it isn't passed on.
    function(
        "AudioSessionInitialize",
        handler!(|uc, a| {
            let Some(initialize) = host_fn!("AudioSessionInitialize": unsafe extern "C" fn(usize, usize, usize, usize) -> i32) else {
                return super::no_host_function(uc, "AudioSessionInitialize");
            };
            Ret::U32(unsafe { initialize(0, 0, 0, 0) } as u32)
        }),
    );
    for name in ["AudioSessionAddPropertyListener", "AudioSessionRemovePropertyListenerWithUserData"] {
        function(name, handler!(|uc, a| Ret::U32(0)));
    }
    // AudioSessionGetProperty(id, *size, data): the route is a CFString.
    function(
        "AudioSessionGetProperty",
        handler!(|uc, a| {
            let (property, size, data) = (a.u32(), a.u32(), a.u32());
            let Some(get) = host_fn!("AudioSessionGetProperty": unsafe extern "C" fn(u32, *mut u32, *mut u8) -> i32) else {
                return super::no_host_function(uc, "AudioSessionGetProperty");
            };
            const ROUTE: u32 = u32::from_be_bytes(*b"rout");
            if property != ROUTE {
                return Ret::U32(unsafe { get(property, mem().host_or_null(size) as *mut u32, mem().host_or_null(data)) } as u32);
            }
            let (mut object, mut length) = (0usize, 8u32);
            let status = unsafe { get(property, &mut length, &mut object as *mut usize as *mut u8) };
            if status == 0 && data != 0 && size != 0 && mem().read_u32(size) >= 4 {
                mem().write_u32(data, object_to_guest(object));
                mem().write_u32(size, 4);
            }
            Ret::U32(status as u32)
        }),
    );
    // AudioComponentInstanceNew(component, *unit).
    function(
        "AudioComponentInstanceNew",
        handler!(|uc, a| {
            let (component, out) = (handle_to_host(a.u32()), a.u32());
            let Some(new) = host_fn!("AudioComponentInstanceNew": unsafe extern "C" fn(usize, *mut usize) -> i32) else {
                return super::no_host_function(uc, "AudioComponentInstanceNew");
            };
            open_into(out, |unit| unsafe { new(component, unit) })
        }),
    );
    // Starting may open the sound device; stopping and disposing wait for a
    // render call in progress, which may be waiting for the guest lock: all
    // let go of it meanwhile. Disposing forgets the handle first, so no other
    // game thread uses it while the host disposes.
    macro_rules! without_lock {
        ($name:literal, $forget:expr) => {
            function(
                $name,
                handler!(|uc, a| {
                    let guest = a.u32();
                    let Some(call) = host_fn!($name: unsafe extern "C" fn(usize) -> i32) else {
                        return super::no_host_function(uc, $name);
                    };
                    let host = if $forget { forget_handle(guest) } else { handle_to_host(guest) };
                    Ret::U32(runtime::without_gil(|| unsafe { call(host) }) as u32)
                }),
            )
        };
    }
    without_lock!("AudioOutputUnitStart", false);
    without_lock!("AudioOutputUnitStop", false);
    without_lock!("AudioComponentInstanceDispose", true);
    without_lock!("AUGraphStart", false);
    without_lock!("AUGraphStop", false);
    without_lock!("DisposeAUGraph", true);
    // AudioUnitSetProperty(unit, id, scope, element, data, size): the render
    // callback struct is two guest words; the host gets its own callback
    // that runs the game's (other properties hold no pointers).
    function(
        "AudioUnitSetProperty",
        handler!(|uc, a| {
            let (unit, id, scope, element, data, size) = (handle_to_host(a.u32()), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let Some(set) = host_fn!("AudioUnitSetProperty": unsafe extern "C" fn(usize, u32, u32, u32, *const u8, u32) -> i32) else {
                return super::no_host_function(uc, "AudioUnitSetProperty");
            };
            if id != SET_RENDER_CALLBACK {
                return Ret::U32(unsafe { set(unit, id, scope, element, mem().host_or_null(data), size) } as u32);
            }
            if data == 0 || size < 8 {
                return Ret::U32(INVALID_PROPERTY_VALUE as u32);
            }
            let host = host_callback(data);
            Ret::U32(unsafe { set(unit, id, scope, element, host.as_ptr() as *const u8, 16) } as u32)
        }),
    );
    // NewAUGraph(*graph).
    function(
        "NewAUGraph",
        handler!(|uc, a| {
            let out = a.u32();
            let Some(new) = host_fn!("NewAUGraph": unsafe extern "C" fn(*mut usize) -> i32) else {
                return super::no_host_function(uc, "NewAUGraph");
            };
            open_into(out, |graph| unsafe { new(graph) })
        }),
    );
    // AUGraphNodeInfo(graph, node, *description, *unit): the unit as a handle.
    function(
        "AUGraphNodeInfo",
        handler!(|uc, a| {
            let (graph, node, description, out) = (handle_to_host(a.u32()), a.u32() as i32, a.u32(), a.u32());
            let Some(info) = host_fn!("AUGraphNodeInfo": unsafe extern "C" fn(usize, i32, *mut u32, *mut usize) -> i32) else {
                return super::no_host_function(uc, "AUGraphNodeInfo");
            };
            let mut unit = 0usize;
            let status = unsafe { info(graph, node, mem().host_or_null(description) as *mut u32, &mut unit) };
            if status == 0 && out != 0 {
                mem().write_u32(out, handle_to_guest(unit));
            }
            Ret::U32(status as u32)
        }),
    );
    // AUGraphSetNodeInputCallback(graph, node, bus, const AURenderCallbackStruct*).
    function(
        "AUGraphSetNodeInputCallback",
        handler!(|uc, a| {
            let (graph, node, bus, callback) = (handle_to_host(a.u32()), a.u32() as i32, a.u32(), a.u32());
            let Some(set) = host_fn!("AUGraphSetNodeInputCallback": unsafe extern "C" fn(usize, i32, u32, *const usize) -> i32) else {
                return super::no_host_function(uc, "AUGraphSetNodeInputCallback");
            };
            if callback == 0 {
                return Ret::U32(-50i32 as u32);
            }
            let host = host_callback(callback);
            Ret::U32(unsafe { set(graph, node, bus, host.as_ptr()) } as u32)
        }),
    );
    // AudioUnitRender(unit, *flags, stamp, bus, frames, list): buffers the
    // caller leaves empty (the unit is to supply them) get this thread's
    // guest scratch, as the host's own would be out of the game's reach.
    function(
        "AudioUnitRender",
        handler!(|uc, a| {
            let (unit, flags, stamp, bus, frames, list) = (handle_to_host(a.u32()), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let Some(render) = host_fn!("AudioUnitRender": unsafe extern "C" fn(usize, *mut u32, *const u8, u32, u32, *mut u8) -> i32) else {
                return super::no_host_function(uc, "AudioUnitRender");
            };
            if list == 0 {
                return Ret::U32(-50i32 as u32);
            }
            let m = mem();
            let at = |b: u32| list + 4 + 12 * b;
            let empty: Vec<(u32, u32)> = (0..m.read_u32(list))
                .filter(|&b| m.read_u32(at(b) + 8) == 0)
                .map(|b| (b, (frames * m.read_u32(at(b)).max(1) * 4).max(m.read_u32(at(b) + 4))))
                .collect();
            let total: u32 = empty.iter().map(|e| (e.1 + 7) & !7).sum();
            if total > 0 {
                let mut scratch = reused_block(&INPUT_BLOCK, total);
                if scratch == 0 {
                    return Ret::U32(-108i32 as u32);
                }
                for &(b, size) in &empty {
                    m.write_u32(at(b) + 4, size);
                    m.write_u32(at(b) + 8, scratch);
                    scratch += (size + 7) & !7;
                }
            }
            let status = with_host_buffer_list(list, |host| unsafe {
                render(unit, m.host_or_null(flags) as *mut u32, m.host_or_null(stamp), bus, frames, host as *mut u8)
            });
            Ret::U32(status.unwrap_or(-50) as u32)
        }),
    );
}

/// The host form of a guest AURenderCallbackStruct { inputProc, refCon }:
/// a host callback that runs the game's (none stays none).
fn host_callback(guest: u32) -> [usize; 2] {
    let (procedure, refcon) = (mem().read_u32(guest), mem().read_u32(guest + 4));
    if procedure == 0 {
        [0, 0]
    } else {
        [guest_render as usize, guest_render_for(procedure, refcon)]
    }
}
