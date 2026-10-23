//! AudioToolbox for 32-bit games: AudioFile, ExtAudioFile and the C
//! AudioSession calls, on this layer's (fw/audiofile.rs, fw/extaudiofile.rs,
//! fw/media.rs). File ids are host pointers (handles here); an
//! AudioBufferList holds pointers, so it is rebuilt in the host's layout
//! for the call and the byte counts copied back.

use super::{
    forget_handle, handle_to_guest, handle_to_host, host_fn, host_functions, object_to_guest, object_to_host, register_all, Byte, Handle,
    Ptr, I32, I64, U32,
};
use crate::fw::extaudiofile::{BUFFER_LIST_FIRST, BUFFER_SIZE};
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

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

pub fn register() {
    register_all(host_functions! {
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
}
