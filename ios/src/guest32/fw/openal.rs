//! OpenAL and ALC for 32-bit games: this layer's mixer (fw/openal.rs).
//! Devices and contexts are host pointers, so the guest gets handles.

use super::{host_functions, register_all, Byte, CStr, Handle, Ptr, F32, F64, I32, U32};
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

pub fn register() {
    register_all(host_functions! {
        "alGenBuffers" => fn(count: I32, out: Ptr);
        "alDeleteBuffers" => fn(count: I32, names: Ptr);
        "alIsBuffer" => fn(name: U32) -> Byte;
        "alBufferData" => fn(buffer: U32, format: I32, data: Ptr, size: I32, rate: I32);
        "alBufferDataStatic" => fn(buffer: U32, format: I32, data: Ptr, size: I32, rate: I32);
        "alGetBufferi" => fn(buffer: U32, parameter: I32, out: Ptr);
        "alGetBufferf" => fn(buffer: U32, parameter: I32, out: Ptr);
        "alGenSources" => fn(count: I32, out: Ptr);
        "alDeleteSources" => fn(count: I32, names: Ptr);
        "alIsSource" => fn(name: U32) -> Byte;
        "alSourcef" => fn(name: U32, parameter: I32, value: F32);
        "alSource3f" => fn(name: U32, parameter: I32, x: F32, y: F32, z: F32);
        "alSourcefv" => fn(name: U32, parameter: I32, values: Ptr);
        "alSourcei" => fn(name: U32, parameter: I32, value: I32);
        "alSource3i" => fn(name: U32, parameter: I32, x: I32, y: I32, z: I32);
        "alSourceiv" => fn(name: U32, parameter: I32, values: Ptr);
        "alGetSourcei" => fn(name: U32, parameter: I32, out: Ptr);
        "alGetSourcef" => fn(name: U32, parameter: I32, out: Ptr);
        "alGetSource3f" => fn(name: U32, parameter: I32, x: Ptr, y: Ptr, z: Ptr);
        "alGetSourcefv" => fn(name: U32, parameter: I32, out: Ptr);
        "alGetSourceiv" => fn(name: U32, parameter: I32, out: Ptr);
        "alSourcePlay" => fn(name: U32);
        "alSourceStop" => fn(name: U32);
        "alSourcePause" => fn(name: U32);
        "alSourceRewind" => fn(name: U32);
        "alSourcePlayv" => fn(count: I32, names: Ptr);
        "alSourceStopv" => fn(count: I32, names: Ptr);
        "alSourcePausev" => fn(count: I32, names: Ptr);
        "alSourceRewindv" => fn(count: I32, names: Ptr);
        "alSourceQueueBuffers" => fn(name: U32, count: I32, buffers: Ptr);
        "alSourceUnqueueBuffers" => fn(name: U32, count: I32, out: Ptr);
        "alListenerf" => fn(parameter: I32, value: F32);
        "alListener3f" => fn(parameter: I32, x: F32, y: F32, z: F32);
        "alListenerfv" => fn(parameter: I32, values: Ptr);
        "alListeneri" => fn(parameter: I32, value: I32);
        "alGetListenerf" => fn(parameter: I32, out: Ptr);
        "alGetListener3f" => fn(parameter: I32, x: Ptr, y: Ptr, z: Ptr);
        "alGetListenerfv" => fn(parameter: I32, out: Ptr);
        "alDistanceModel" => fn(model: I32);
        "alDopplerFactor" => fn(value: F32);
        "alSpeedOfSound" => fn(value: F32);
        "alGetError" => fn() -> I32;
        "alGetInteger" => fn(parameter: I32) -> I32;
        "alGetFloat" => fn(parameter: I32) -> F32;
        "alGetString" => fn(parameter: I32) -> CStr;
        "alIsExtensionPresent" => fn(name: Ptr) -> Byte;
        "alGetEnumValue" => fn(name: Ptr) -> I32;
        "alcOpenDevice" => fn(name: Ptr) -> Handle;
        "alcCloseDevice" => fn(device: Handle) -> Byte;
        "alcCreateContext" => fn(device: Handle, attributes: Ptr) -> Handle;
        "alcMakeContextCurrent" => fn(context: Handle) -> Byte;
        "alcDestroyContext" => fn(context: Handle);
        "alcProcessContext" => fn(context: Handle);
        "alcSuspendContext" => fn(context: Handle);
        "alcGetCurrentContext" => fn() -> Handle;
        "alcGetContextsDevice" => fn(context: Handle) -> Handle;
        "alcGetError" => fn(device: Handle) -> I32;
        "alcGetString" => fn(device: Handle, parameter: I32) -> CStr;
        "alcGetIntegerv" => fn(device: Handle, parameter: I32, size: I32, out: Ptr);
        "alcIsExtensionPresent" => fn(device: Handle, name: Ptr) -> Byte;
        "alcMacOSXMixerOutputRate" => fn(rate: F64);
        "alcMacOSXGetMixerOutputRate" => fn() -> F64;
    });
    // Function addresses for the guest: stand-ins for what is registered here.
    for name in ["alGetProcAddress", "alcGetProcAddress"] {
        let handler = if name == "alGetProcAddress" {
            handler!(|uc, a| Ret::U32(proc_address(a.u32())))
        } else {
            handler!(|uc, a| {
                let _device = a.u32();
                Ret::U32(proc_address(a.u32()))
            })
        };
        super::super::function(name, handler.unwrap());
    }
}

fn proc_address(name: u32) -> u32 {
    if name == 0 {
        return 0;
    }
    let name = mem().string(name);
    if arm32_bridge::ios32::provides_function(&name) {
        arm32_bridge::runtime::stub_for(&name)
    } else {
        0
    }
}
