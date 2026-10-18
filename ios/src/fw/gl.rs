//! OpenGL ES. The game's standard GL calls go straight to the system's
//! library (same processor, same calling rules). Apple-only extensions are
//! translated to standard OpenGL ES 3 here.

use crate::symbols::{leak_cstr, Registry};
use std::sync::OnceLock;

// ----- loading the system libraries -----

fn open_first(names: &[&str]) -> usize {
    for name in names {
        let text = std::ffi::CString::new(*name).unwrap();
        let handle = unsafe { libc::dlopen(text.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if !handle.is_null() {
            return handle as usize;
        }
    }
    0
}

pub fn gles_library() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| open_first(&["libGLESv3.so", "libGLESv2.so", "libGLESv2.so.2"]))
}

pub fn egl_library() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| open_first(&["libEGL.so", "libEGL.so.1"]))
}

fn symbol(library: usize, name: &str) -> usize {
    if library == 0 {
        return 0;
    }
    let text = std::ffi::CString::new(name).unwrap();
    unsafe { libc::dlsym(library as *mut _, text.as_ptr()) as usize }
}

/// A GL or EGL function this layer itself needs; stops loudly if the system lacks it.
pub fn required(name: &str) -> usize {
    let found = if name.starts_with("egl") { symbol(egl_library(), name) } else { symbol(gles_library(), name) };
    if found != 0 {
        return found;
    }
    let by_egl = get_proc_address(name);
    if by_egl != 0 {
        return by_egl;
    }
    crate::log!("STOP: graphics: the system has no {name}");
    crate::report::finish(1);
}

fn get_proc_address(name: &str) -> usize {
    let address = symbol(egl_library(), "eglGetProcAddress");
    if address == 0 {
        return 0;
    }
    let f: unsafe extern "C" fn(*const libc::c_char) -> usize = unsafe { std::mem::transmute(address) };
    let text = std::ffi::CString::new(name).unwrap();
    unsafe { f(text.as_ptr()) }
}

/// A standard (no vendor ending) GL function from the system library, for
/// the loader. Extension functions only come through the table below.
pub fn core_function(name: &str) -> Option<usize> {
    if !name.starts_with("gl") || has_vendor_ending(name) {
        return None;
    }
    let found = symbol(gles_library(), name);
    (found != 0).then_some(found)
}

fn has_vendor_ending(name: &str) -> bool {
    ["EXT", "OES", "APPLE", "IMG", "ARB", "KHR", "NV", "AMD", "QCOM", "ARM", "ANGLE"].iter().any(|v| name.ends_with(v))
}

/// Declares a typed call into the system library, found on first use.
macro_rules! system {
    ($($name:ident ( $($arg:ident : $ty:ty),* ) $(-> $ret:ty)? ;)*) => {
        $(
            #[allow(non_snake_case, dead_code, clippy::too_many_arguments)]
            pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                static ADDRESS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
                let address = *ADDRESS.get_or_init(|| $crate::fw::gl::required(stringify!($name)));
                let f: unsafe extern "C" fn($($ty),*) $(-> $ret)? = std::mem::transmute(address);
                f($($arg),*)
            }
        )*
    };
}
pub(crate) use system;

pub type Enum = u32;

system! {
    eglGetCurrentContext() -> usize;
    glGetError() -> Enum;
    glGetIntegerv(name: Enum, out: *mut i32);
    glGetString(name: Enum) -> *const u8;
    glIsEnabled(cap: Enum) -> u8;
    glEnable(cap: Enum);
    glDisable(cap: Enum);
    glColorMask(r: u8, g: u8, b: u8, a: u8);
    glGetBooleanv(name: Enum, out: *mut u8);
    glBindFramebuffer(target: Enum, framebuffer: u32);
    glGenFramebuffers(count: i32, out: *mut u32);
    glDeleteFramebuffers(count: i32, names: *const u32);
    glFramebufferRenderbuffer(target: Enum, attachment: Enum, rb_target: Enum, renderbuffer: u32);
    glCheckFramebufferStatus(target: Enum) -> Enum;
    glBindRenderbuffer(target: Enum, renderbuffer: u32);
    glRenderbufferStorage(target: Enum, format: Enum, width: i32, height: i32);
    glRenderbufferStorageMultisample(target: Enum, samples: i32, format: Enum, width: i32, height: i32);
    glGetRenderbufferParameteriv(target: Enum, name: Enum, out: *mut i32);
    glGetFramebufferAttachmentParameteriv(target: Enum, attachment: Enum, name: Enum, out: *mut i32);
    glGetTexLevelParameteriv(target: Enum, level: i32, name: Enum, out: *mut i32);
    glBindTexture(target: Enum, texture: u32);
    glBlitFramebuffer(sx0: i32, sy0: i32, sx1: i32, sy1: i32, dx0: i32, dy0: i32, dx1: i32, dy1: i32, mask: u32, filter: Enum);
    glInvalidateFramebuffer(target: Enum, count: i32, attachments: *const Enum);
    glReadPixels(x: i32, y: i32, width: i32, height: i32, format: Enum, kind: Enum, out: *mut u8);
    glPixelStorei(name: Enum, value: i32);
    glBindBuffer(target: Enum, buffer: u32);
    glGetBufferParameteriv(target: Enum, name: Enum, out: *mut i32);
    glMapBufferRange(target: Enum, offset: isize, length: isize, access: u32) -> usize;
    glUnmapBuffer(target: Enum) -> u8;
    glGetBufferPointerv(target: Enum, name: Enum, out: *mut usize);
    glTexImage2D(target: Enum, level: i32, internal: i32, width: i32, height: i32, border: i32, format: Enum, kind: Enum, data: *const u8);
    glTexSubImage2D(target: Enum, level: i32, x: i32, y: i32, width: i32, height: i32, format: Enum, kind: Enum, data: *const u8);
    glCompressedTexImage2D(target: Enum, level: i32, internal: Enum, width: i32, height: i32, border: i32, size: i32, data: *const u8);
    glCompileShader(shader: u32);
    glShaderSource(shader: u32, count: i32, sources: *const *const u8, lengths: *const i32);
    glGetShaderiv(shader: u32, name: Enum, out: *mut i32);
    glGetShaderInfoLog(shader: u32, size: i32, length: *mut i32, out: *mut u8);
    glLinkProgram(program: u32);
    glGetProgramiv(program: u32, name: Enum, out: *mut i32);
    glGetProgramInfoLog(program: u32, size: i32, length: *mut i32, out: *mut u8);
    glFinish();
    glFramebufferTexture2D(target: Enum, attachment: Enum, texture_target: Enum, texture: u32, level: i32);
    glClear(mask: u32);
    glClearColor(r: f32, g: f32, b: f32, a: f32);
    glGetFloatv(name: Enum, out: *mut f32);
}

pub const COLOR_CLEAR_VALUE: Enum = 0x0C22;

pub const RENDERBUFFER: Enum = 0x8D41;
pub const RENDERBUFFER_BINDING: Enum = 0x8CA7;
pub const FRAMEBUFFER: Enum = 0x8D40;
pub const READ_FRAMEBUFFER: Enum = 0x8CA8;
pub const DRAW_FRAMEBUFFER: Enum = 0x8CA9;
pub const DRAW_FRAMEBUFFER_BINDING: Enum = 0x8CA6;
pub const READ_FRAMEBUFFER_BINDING: Enum = 0x8CAA;
pub const COLOR_ATTACHMENT0: Enum = 0x8CE0;
pub const COLOR_BUFFER_BIT: u32 = 0x4000;
pub const NEAREST: Enum = 0x2600;
pub const LINEAR: Enum = 0x2601;
pub const SCISSOR_TEST: Enum = 0x0C11;
pub const RASTERIZER_DISCARD: Enum = 0x8C89;
pub const COLOR_WRITEMASK: Enum = 0x0C23;
pub const RGBA8: Enum = 0x8058;
pub const RGB565: Enum = 0x8D62;
pub const RGBA: Enum = 0x1908;
pub const UNSIGNED_BYTE: Enum = 0x1401;
pub const RENDERBUFFER_WIDTH: Enum = 0x8D42;
pub const RENDERBUFFER_HEIGHT: Enum = 0x8D43;
pub const PIXEL_PACK_BUFFER: Enum = 0x88EB;
pub const PIXEL_PACK_BUFFER_BINDING: Enum = 0x88ED;
pub const PACK_ALIGNMENT: Enum = 0x0D05;
pub const FRAMEBUFFER_COMPLETE: Enum = 0x8CD5;
const ATTACHMENT_OBJECT_TYPE: Enum = 0x8CD0;
const ATTACHMENT_OBJECT_NAME: Enum = 0x8CD1;
const ATTACHMENT_TEXTURE_LEVEL: Enum = 0x8CD2;
const TEXTURE: Enum = 0x1702;
const TEXTURE_2D: Enum = 0x0DE1;
const TEXTURE_BINDING_2D: Enum = 0x8069;
const TEXTURE_WIDTH: Enum = 0x1000;
const TEXTURE_HEIGHT: Enum = 0x1001;
const BUFFER_SIZE: Enum = 0x8764;
const MAP_WRITE_BIT: u32 = 0x2;
const MAP_READ_BIT: u32 = 0x1;
pub const PIXEL_UNPACK_BUFFER: Enum = 0x88EC;
pub const PIXEL_UNPACK_BUFFER_BINDING: Enum = 0x88EF;
pub const UNPACK_ALIGNMENT: Enum = 0x0CF5;
pub const UNPACK_ROW_LENGTH: Enum = 0x0CF2;
pub const UNPACK_SKIP_ROWS: Enum = 0x0CF3;
pub const UNPACK_SKIP_PIXELS: Enum = 0x0CF4;
pub const PACK_ROW_LENGTH: Enum = 0x0D02;
pub const PACK_SKIP_ROWS: Enum = 0x0D03;
pub const PACK_SKIP_PIXELS: Enum = 0x0D04;
const EXTENSIONS: Enum = 0x1F03;
const BGRA: Enum = 0x80E1;
const HALF_FLOAT_OES: Enum = 0x8D61;
const HALF_FLOAT: Enum = 0x140B;
const COMPILE_STATUS: Enum = 0x8B81;
const LINK_STATUS: Enum = 0x8B82;
const MAX_SAMPLES: Enum = 0x8D57;

// ----- Apple extensions that are the same as a standard function -----

/// Extension name → standard OpenGL ES 3 function with the same arguments and meaning.
const SAME_AS_STANDARD: &[(&str, &str)] = &[
    ("glDiscardFramebufferEXT", "glInvalidateFramebuffer"),
    ("glGenQueriesEXT", "glGenQueries"),
    ("glDeleteQueriesEXT", "glDeleteQueries"),
    ("glIsQueryEXT", "glIsQuery"),
    ("glBeginQueryEXT", "glBeginQuery"),
    ("glEndQueryEXT", "glEndQuery"),
    ("glGetQueryivEXT", "glGetQueryiv"),
    ("glGetQueryObjectuivEXT", "glGetQueryObjectuiv"),
    ("glBindVertexArrayOES", "glBindVertexArray"),
    ("glGenVertexArraysOES", "glGenVertexArrays"),
    ("glDeleteVertexArraysOES", "glDeleteVertexArrays"),
    ("glIsVertexArrayOES", "glIsVertexArray"),
    ("glUnmapBufferOES", "glUnmapBuffer"),
    ("glGetBufferPointervOES", "glGetBufferPointerv"),
    ("glMapBufferRangeEXT", "glMapBufferRange"),
    ("glFlushMappedBufferRangeEXT", "glFlushMappedBufferRange"),
    ("glTexStorage2DEXT", "glTexStorage2D"),
    ("glDrawArraysInstancedEXT", "glDrawArraysInstanced"),
    ("glDrawElementsInstancedEXT", "glDrawElementsInstanced"),
    ("glVertexAttribDivisorEXT", "glVertexAttribDivisor"),
    ("glDrawBuffersEXT", "glDrawBuffers"),
    ("glFenceSyncAPPLE", "glFenceSync"),
    ("glIsSyncAPPLE", "glIsSync"),
    ("glDeleteSyncAPPLE", "glDeleteSync"),
    ("glClientWaitSyncAPPLE", "glClientWaitSync"),
    ("glWaitSyncAPPLE", "glWaitSync"),
    ("glGetInteger64vAPPLE", "glGetInteger64v"),
    ("glGetSyncivAPPLE", "glGetSynciv"),
];

/// What an iPhone 5s (Apple A7, iOS 8) reports. Games choose texture formats
/// and drawing paths from this list, so it is copied, limited to what this
/// layer and the graphics chip can really do.
const IPHONE_EXTENSIONS: &[&str] = &[
    "GL_APPLE_clip_distance", "GL_APPLE_color_buffer_packed_float", "GL_APPLE_copy_texture_levels",
    "GL_APPLE_framebuffer_multisample", "GL_APPLE_rgb_422", "GL_APPLE_sync", "GL_APPLE_texture_format_BGRA8888",
    "GL_APPLE_texture_max_level", "GL_APPLE_texture_packed_float", "GL_EXT_blend_minmax",
    "GL_EXT_color_buffer_half_float", "GL_EXT_debug_label", "GL_EXT_debug_marker", "GL_EXT_discard_framebuffer",
    "GL_EXT_draw_instanced", "GL_EXT_instanced_arrays", "GL_EXT_map_buffer_range", "GL_EXT_occlusion_query_boolean",
    "GL_EXT_pvrtc_sRGB", "GL_EXT_read_format_bgra", "GL_EXT_separate_shader_objects", "GL_EXT_shader_framebuffer_fetch",
    "GL_EXT_shader_texture_lod", "GL_EXT_shadow_samplers", "GL_EXT_sRGB", "GL_EXT_texture_filter_anisotropic",
    "GL_EXT_texture_rg", "GL_EXT_texture_storage", "GL_IMG_read_format", "GL_IMG_texture_compression_pvrtc",
    "GL_OES_depth24", "GL_OES_depth_texture", "GL_OES_element_index_uint", "GL_OES_fbo_render_mipmap",
    "GL_OES_mapbuffer", "GL_OES_packed_depth_stencil", "GL_OES_rgb8_rgba8", "GL_OES_standard_derivatives",
    "GL_OES_texture_float", "GL_OES_texture_half_float", "GL_OES_texture_half_float_linear",
    "GL_OES_vertex_array_object",
];

/// Provided here (translated in this file) or part of OpenGL ES 3 itself,
/// with the same numbers and meaning as the iPhone extension.
const ALWAYS_AVAILABLE: &[&str] = &[
    "GL_EXT_discard_framebuffer", "GL_APPLE_framebuffer_multisample", "GL_EXT_occlusion_query_boolean",
    "GL_OES_vertex_array_object", "GL_OES_mapbuffer", "GL_EXT_map_buffer_range", "GL_APPLE_texture_format_BGRA8888",
    "GL_APPLE_sync", "GL_EXT_texture_storage", "GL_EXT_instanced_arrays", "GL_EXT_draw_instanced",
    "GL_EXT_debug_label", "GL_EXT_debug_marker", "GL_OES_texture_half_float", "GL_OES_depth24",
    "GL_OES_depth_texture", "GL_OES_element_index_uint", "GL_OES_fbo_render_mipmap", "GL_OES_packed_depth_stencil",
    "GL_OES_rgb8_rgba8", "GL_EXT_texture_rg", "GL_EXT_blend_minmax", "GL_APPLE_texture_max_level",
    // The games' textures are PVRTC; where the chip lacks it they are decoded here.
    "GL_IMG_texture_compression_pvrtc",
];

// ----- translated functions -----

extern "C" fn get_string(name: Enum) -> *const u8 {
    if name != EXTENSIONS {
        return unsafe { glGetString(name) };
    }
    static TEXT: OnceLock<usize> = OnceLock::new();
    *TEXT.get_or_init(|| {
        let list: Vec<&str> = IPHONE_EXTENSIONS
            .iter()
            .copied()
            .filter(|name| ALWAYS_AVAILABLE.contains(name) || system_has_extension(name))
            .collect();
        crate::log!("graphics: telling the game it has {} of the iPhone's {} extensions", list.len(), IPHONE_EXTENSIONS.len());
        leak_cstr(&list.join(" "))
    }) as *const u8
}

const NUM_COMPRESSED_TEXTURE_FORMATS: Enum = 0x86A2;
const COMPRESSED_TEXTURE_FORMATS: Enum = 0x86A3;
const MAX_TEXTURE_SIZE: Enum = 0x0D33;
const MAX_CUBE_MAP_TEXTURE_SIZE: Enum = 0x851C;
const MAX_RENDERBUFFER_SIZE: Enum = 0x84E8;

/// Like the iPhone 5s: its four PVRTC formats, and sizes up to 4096.
extern "C" fn get_integerv(name: Enum, out: *mut i32) {
    if out.is_null() {
        return;
    }
    unsafe {
        match name {
            NUM_COMPRESSED_TEXTURE_FORMATS => *out = PVRTC_FORMATS.count() as i32,
            COMPRESSED_TEXTURE_FORMATS => {
                for (i, format) in PVRTC_FORMATS.enumerate() {
                    *out.add(i) = format as i32;
                }
            }
            MAX_TEXTURE_SIZE | MAX_CUBE_MAP_TEXTURE_SIZE | MAX_RENDERBUFFER_SIZE => {
                glGetIntegerv(name, out);
                *out = (*out).min(4096);
            }
            _ => glGetIntegerv(name, out),
        }
    }
}

/// Framebuffer set-up calls are rare; the first ones are logged to show
/// what the game builds (formats, sizes, attachments).
fn trace_setup(text: std::fmt::Arguments) {
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 80 {
        crate::log!("graphics: {text}");
    }
}

fn after(what: std::fmt::Arguments) {
    let error = unsafe { glGetError() };
    if error != 0 {
        crate::log!("graphics: GL error {error:#x} after {what}");
    }
}

extern "C" fn storage_multisample_apple(target: Enum, samples: i32, format: Enum, width: i32, height: i32) {
    let mut most = 0;
    unsafe {
        glGetIntegerv(MAX_SAMPLES, &mut most);
        glRenderbufferStorageMultisample(target, samples.min(most), format, width, height);
    }
    trace_setup(format_args!("multisampled renderbuffer {format:#x} {width}x{height}, {samples} samples (chip max {most})"));
    after(format_args!("glRenderbufferStorageMultisampleAPPLE"));
}

extern "C" fn renderbuffer_storage(target: Enum, format: Enum, width: i32, height: i32) {
    unsafe { glRenderbufferStorage(target, format, width, height) };
    trace_setup(format_args!("renderbuffer {format:#x} {width}x{height}"));
    after(format_args!("glRenderbufferStorage {format:#x}"));
}

extern "C" fn framebuffer_renderbuffer(target: Enum, attachment: Enum, rb_target: Enum, renderbuffer: u32) {
    unsafe { glFramebufferRenderbuffer(target, attachment, rb_target, renderbuffer) };
    trace_setup(format_args!("framebuffer attachment {attachment:#x} = renderbuffer {renderbuffer}"));
    after(format_args!("glFramebufferRenderbuffer"));
}

extern "C" fn framebuffer_texture_2d(target: Enum, attachment: Enum, texture_target: Enum, texture: u32, level: i32) {
    unsafe { glFramebufferTexture2D(target, attachment, texture_target, texture, level) };
    trace_setup(format_args!("framebuffer attachment {attachment:#x} = texture {texture} level {level}"));
    after(format_args!("glFramebufferTexture2D"));
}

static MADE_FRAMEBUFFERS: std::sync::Mutex<Option<std::collections::HashSet<u32>>> = std::sync::Mutex::new(None);

#[inline(never)]
extern "C" fn gen_framebuffers(count: i32, out: *mut u32) {
    if unsafe { eglGetCurrentContext() } == 0 {
        crate::log!("graphics: glGenFramebuffers with no context current on this thread{}", crate::report::call_chain());
    }
    unsafe { glGenFramebuffers(count, out) };
    let mut made = MADE_FRAMEBUFFERS.lock().unwrap();
    let made = made.get_or_insert_with(Default::default);
    for i in 0..count.max(0) as usize {
        let name = unsafe { *out.add(i) };
        made.insert(name);
        trace_setup(format_args!("framebuffer {name} made"));
    }
}

extern "C" fn delete_framebuffers(count: i32, names: *const u32) {
    for i in 0..count.max(0) as usize {
        let name = unsafe { *names.add(i) };
        trace_setup(format_args!("framebuffer {name} deleted"));
    }
    unsafe { glDeleteFramebuffers(count, names) };
}

/// Logs binding a framebuffer name that glGenFramebuffers never gave out.
#[inline(never)]
extern "C" fn bind_framebuffer(target: Enum, framebuffer: u32) {
    let caller: usize;
    unsafe { std::arch::asm!("mov {}, x30", out(reg) caller, options(nomem, nostack)) };
    if framebuffer != 0 && !MADE_FRAMEBUFFERS.lock().unwrap().as_ref().is_some_and(|m| m.contains(&framebuffer)) {
        trace_setup(format_args!("framebuffer {framebuffer} bound without being made, from {}", crate::loader::describe_address(caller)));
    }
    unsafe { glBindFramebuffer(target, framebuffer) };
}

extern "C" fn check_framebuffer_status(target: Enum) -> Enum {
    let status = unsafe { glCheckFramebufferStatus(target) };
    if status != FRAMEBUFFER_COMPLETE {
        trace_setup(format_args!("the game checked its framebuffer: not complete ({status:#x})"));
    }
    status
}

/// Size of a framebuffer's first color attachment.
fn attachment_size(target: Enum) -> Option<(i32, i32)> {
    unsafe {
        let (mut kind, mut name) = (0, 0);
        glGetFramebufferAttachmentParameteriv(target, COLOR_ATTACHMENT0, ATTACHMENT_OBJECT_TYPE, &mut kind);
        glGetFramebufferAttachmentParameteriv(target, COLOR_ATTACHMENT0, ATTACHMENT_OBJECT_NAME, &mut name);
        let (mut width, mut height) = (0, 0);
        if kind as Enum == RENDERBUFFER {
            let mut previous = 0;
            glGetIntegerv(RENDERBUFFER_BINDING, &mut previous);
            glBindRenderbuffer(RENDERBUFFER, name as u32);
            glGetRenderbufferParameteriv(RENDERBUFFER, RENDERBUFFER_WIDTH, &mut width);
            glGetRenderbufferParameteriv(RENDERBUFFER, RENDERBUFFER_HEIGHT, &mut height);
            glBindRenderbuffer(RENDERBUFFER, previous as u32);
        } else if kind as Enum == TEXTURE {
            let mut level = 0;
            glGetFramebufferAttachmentParameteriv(target, COLOR_ATTACHMENT0, ATTACHMENT_TEXTURE_LEVEL, &mut level);
            let mut previous = 0;
            glGetIntegerv(TEXTURE_BINDING_2D, &mut previous);
            glBindTexture(TEXTURE_2D, name as u32);
            glGetTexLevelParameteriv(TEXTURE_2D, level, TEXTURE_WIDTH, &mut width);
            glGetTexLevelParameteriv(TEXTURE_2D, level, TEXTURE_HEIGHT, &mut height);
            glBindTexture(TEXTURE_2D, previous as u32);
        }
        (width > 0 && height > 0).then_some((width, height))
    }
}

/// glResolveMultisampleFramebufferAPPLE: copies the multisampled read
/// framebuffer into the draw framebuffer (the scissor box limits it, as on Apple).
extern "C" fn resolve_multisample_apple() {
    match attachment_size(DRAW_FRAMEBUFFER) {
        Some((width, height)) => unsafe {
            glBlitFramebuffer(0, 0, width, height, 0, 0, width, height, COLOR_BUFFER_BIT, NEAREST);
        },
        None => crate::log!("glResolveMultisampleFramebufferAPPLE: the draw framebuffer has no color attachment; nothing copied"),
    }
}

/// glMapBufferOES(target, access): Apple only allows writing (WRITE_ONLY_OES).
extern "C" fn map_buffer_oes(target: Enum, _access: Enum) -> usize {
    unsafe {
        let mut size = 0;
        glGetBufferParameteriv(target, BUFFER_SIZE, &mut size);
        if size <= 0 {
            return 0;
        }
        glMapBufferRange(target, 0, size as isize, MAP_WRITE_BIT)
    }
}

/// How the game laid out the pixels it hands over (glPixelStorei), and
/// whether "pixels" is really an offset into a bound buffer.
struct Unpack {
    buffer: i32,
    row_length: i32,
    skip_rows: i32,
    skip_pixels: i32,
    alignment: i32,
}

fn unpack_state() -> Unpack {
    let get = |name| {
        let mut value = 0;
        unsafe { glGetIntegerv(name, &mut value) };
        value
    };
    Unpack {
        buffer: get(PIXEL_UNPACK_BUFFER_BINDING),
        row_length: get(UNPACK_ROW_LENGTH),
        skip_rows: get(UNPACK_SKIP_ROWS),
        skip_pixels: get(UNPACK_SKIP_PIXELS),
        alignment: get(UNPACK_ALIGNMENT).max(1),
    }
}

/// Copies `length` bytes starting at `data`, which is an offset when a
/// pixel buffer is bound.
fn read_source(unpack: &Unpack, data: *const u8, length: usize) -> Option<Vec<u8>> {
    if unpack.buffer == 0 {
        if data.is_null() {
            return None;
        }
        return Some(unsafe { std::slice::from_raw_parts(data, length) }.to_vec());
    }
    unsafe {
        let mut size = 0;
        glGetBufferParameteriv(PIXEL_UNPACK_BUFFER, BUFFER_SIZE, &mut size);
        let offset = data as usize;
        if offset.checked_add(length).is_none_or(|end| end > size.max(0) as usize) {
            return None;
        }
        let mapped = glMapBufferRange(PIXEL_UNPACK_BUFFER, offset as isize, length as isize, MAP_READ_BIT);
        if mapped == 0 {
            return None;
        }
        let copy = std::slice::from_raw_parts(mapped as *const u8, length).to_vec();
        glUnmapBuffer(PIXEL_UNPACK_BUFFER);
        Some(copy)
    }
}

/// Runs an upload from our own tightly packed memory: no bound pixel
/// buffer, no row length or skips; the game's settings come back after.
fn upload_plain(unpack: &Unpack, upload: impl FnOnce()) {
    unsafe {
        glBindBuffer(PIXEL_UNPACK_BUFFER, 0);
        glPixelStorei(UNPACK_ROW_LENGTH, 0);
        glPixelStorei(UNPACK_SKIP_ROWS, 0);
        glPixelStorei(UNPACK_SKIP_PIXELS, 0);
        glPixelStorei(UNPACK_ALIGNMENT, 4);
        upload();
        glPixelStorei(UNPACK_ROW_LENGTH, unpack.row_length);
        glPixelStorei(UNPACK_SKIP_ROWS, unpack.skip_rows);
        glPixelStorei(UNPACK_SKIP_PIXELS, unpack.skip_pixels);
        glPixelStorei(UNPACK_ALIGNMENT, unpack.alignment);
        glBindBuffer(PIXEL_UNPACK_BUFFER, unpack.buffer as u32);
    }
}

/// Apple's BGRA textures say "RGBA" inside and "BGRA" outside; the standard
/// way can't, so the pixels are reordered to tightly packed RGBA here,
/// read with the game's row layout.
fn bgra_to_rgba(unpack: &Unpack, width: i32, height: i32, data: *const u8) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let (width, height) = (width as usize, height as usize);
    let row_pixels = if unpack.row_length > 0 { unpack.row_length as usize } else { width };
    let alignment = unpack.alignment as usize;
    let stride = (row_pixels * 4).div_ceil(alignment) * alignment;
    let start = unpack.skip_rows.max(0) as usize * stride + unpack.skip_pixels.max(0) as usize * 4;
    let source = read_source(unpack, data, start + (height - 1) * stride + width * 4)?;
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let line = &source[start + row * stride..start + row * stride + width * 4];
        for pixel in line.chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    Some(pixels)
}

fn standard_type(kind: Enum) -> Enum {
    if kind == HALF_FLOAT_OES { HALF_FLOAT } else { kind }
}

extern "C" fn tex_image_2d(target: Enum, level: i32, internal: i32, width: i32, height: i32, border: i32, format: Enum, kind: Enum, data: *const u8) {
    let kind = standard_type(kind);
    unsafe {
        if format == BGRA && kind == UNSIGNED_BYTE {
            let unpack = unpack_state();
            let pixels = if data.is_null() && unpack.buffer == 0 { None } else { bgra_to_rgba(&unpack, width, height, data) };
            let pointer = pixels.as_ref().map(|p| p.as_ptr()).unwrap_or(std::ptr::null());
            upload_plain(&unpack, || glTexImage2D(target, level, RGBA as i32, width, height, border, RGBA, kind, pointer));
        } else {
            glTexImage2D(target, level, internal, width, height, border, format, kind, data);
        }
    }
}

extern "C" fn tex_sub_image_2d(target: Enum, level: i32, x: i32, y: i32, width: i32, height: i32, format: Enum, kind: Enum, data: *const u8) {
    let kind = standard_type(kind);
    unsafe {
        if format == BGRA && kind == UNSIGNED_BYTE {
            let unpack = unpack_state();
            match bgra_to_rgba(&unpack, width, height, data) {
                Some(pixels) => upload_plain(&unpack, || glTexSubImage2D(target, level, x, y, width, height, RGBA, kind, pixels.as_ptr())),
                None => crate::log!("graphics: a BGRA texture update ({width}x{height}) could not be read; skipped"),
            }
        } else {
            glTexSubImage2D(target, level, x, y, width, height, format, kind, data);
        }
    }
}

const PVRTC_FORMATS: std::ops::RangeInclusive<Enum> = 0x8C00..=0x8C03;

extern "C" fn compressed_tex_image_2d(target: Enum, level: i32, internal: Enum, width: i32, height: i32, border: i32, size: i32, data: *const u8) {
    if PVRTC_FORMATS.contains(&internal) && !pvrtc_on_chip() {
        // 0x8C00/0x8C02 are 4 bits per pixel, 0x8C01/0x8C03 are 2.
        let two_bits = internal & 1 == 1;
        let needed = crate::fw::pvrtc::size(width.max(0) as u32, height.max(0) as u32, two_bits);
        let unpack = unpack_state();
        let pixels = if (size as usize) < needed {
            None
        } else {
            read_source(&unpack, data, needed).and_then(|bytes| crate::fw::pvrtc::decode(&bytes, width as u32, height as u32, two_bits))
        };
        match pixels {
            Some(rgba) => upload_plain(&unpack, || unsafe { glTexImage2D(target, level, RGBA as i32, width, height, border, RGBA, UNSIGNED_BYTE, rgba.as_ptr()) }),
            None => {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| crate::log!("graphics: a PVRTC texture ({width}x{height}, {size} bytes) could not be decoded; it stays empty"));
            }
        }
        return;
    }
    unsafe { glCompressedTexImage2D(target, level, internal, width, height, border, size, data) };
}

fn pvrtc_on_chip() -> bool {
    static ON_CHIP: OnceLock<bool> = OnceLock::new();
    *ON_CHIP.get_or_init(|| {
        let found = system_has_extension("GL_IMG_texture_compression_pvrtc");
        crate::log!("graphics: PVRTC textures {}", if found { "go straight to the graphics chip" } else { "are decoded here (the chip has no PVRTC)" });
        found
    })
}

fn system_has_extension(name: &str) -> bool {
    let text = unsafe { glGetString(EXTENSIONS) };
    !text.is_null() && unsafe { std::ffi::CStr::from_ptr(text as *const libc::c_char) }.to_string_lossy().split_whitespace().any(|e| e == name)
}

/// Shader text as the game gave it, kept to show when it doesn't compile.
static SOURCES: std::sync::Mutex<Option<std::collections::HashMap<u32, String>>> = std::sync::Mutex::new(None);

/// Apple's shader compiler lets a global variable start with a value worked
/// out from uniforms; the standard requires a constant there. Such starting
/// values are moved to the top of main(), which gives the same result.
pub fn move_global_initializers(source: &str) -> String {
    const NOT_PLAIN: &[&str] = &["const", "uniform", "attribute", "varying", "precision", "struct", "in", "out", "return"];
    let mut depth = 0i32;
    let mut out = Vec::new();
    let mut moved = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if depth == 0 && !trimmed.starts_with('#') && trimmed.ends_with(';') && !trimmed.contains('{') && !trimmed.contains("//") {
            let statement = &trimmed[..trimmed.len() - 1];
            if let Some((left, value)) = statement.split_once('=') {
                let words: Vec<&str> = left.split_whitespace().collect();
                let precision = words.iter().take_while(|w| matches!(**w, "highp" | "mediump" | "lowp")).count();
                let rest = &words[precision..];
                let plain_name = |w: &str| w.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                let several = {
                    let mut level = 0i32;
                    value.chars().any(|c| {
                        match c {
                            '(' | '[' => level += 1,
                            ')' | ']' => level -= 1,
                            _ => {}
                        }
                        c == ',' && level == 0
                    })
                };
                if rest.len() == 2 && plain_name(rest[0]) && plain_name(rest[1]) && !NOT_PLAIN.contains(&rest[0]) && !value.trim_start().starts_with('=') && !several {
                    out.push(format!("{};", left.trim_end()));
                    moved.push(format!("{} = {};", rest[1], value.trim()));
                    continue;
                }
            }
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        out.push(line.to_string());
    }
    let mut text = out.join("\n");
    if moved.is_empty() {
        return source.to_string();
    }
    match find_main_body(&text) {
        Some(position) => {
            text.insert_str(position, &format!("\n{}\n", moved.join("\n")));
            text
        }
        None => source.to_string(),
    }
}

/// Position just after the opening brace of `void main()`.
fn find_main_body(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find("main") {
        let start = from + found;
        let before = text[..start].trim_end();
        let after = text[start + 4..].trim_start();
        if before.ends_with("void") && after.starts_with('(') {
            let close = start + 4 + text[start + 4..].find(')')?;
            let brace = close + text[close..].find('{')?;
            if text[close + 1..brace].trim().is_empty() {
                return Some(brace + 1);
            }
        }
        from = start + 4;
    }
    None
}

extern "C" fn shader_source(shader: u32, count: i32, sources: *const *const u8, lengths: *const i32) {
    let mut text = String::new();
    for i in 0..count.max(0) as usize {
        unsafe {
            let part = *sources.add(i);
            if part.is_null() {
                continue;
            }
            let length = if lengths.is_null() { -1 } else { *lengths.add(i) };
            let bytes = if length < 0 {
                std::ffi::CStr::from_ptr(part as *const libc::c_char).to_bytes()
            } else {
                std::slice::from_raw_parts(part, length as usize)
            };
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    let fixed = move_global_initializers(&text);
    let pointer = fixed.as_ptr();
    let length = fixed.len() as i32;
    unsafe { glShaderSource(shader, 1, &pointer, &length) };
    SOURCES.lock().unwrap().get_or_insert_with(Default::default).insert(shader, fixed);
}

/// Compiles, and prints the reason when a shader doesn't compile.
extern "C" fn compile_shader(shader: u32) {
    unsafe {
        glCompileShader(shader);
        let mut ok = 0;
        glGetShaderiv(shader, COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut text = vec![0u8; 4096];
            let mut length = 0;
            glGetShaderInfoLog(shader, text.len() as i32, &mut length, text.as_mut_ptr());
            text.truncate(length.max(0) as usize);
            crate::log!("graphics: shader {shader} did not compile: {}", String::from_utf8_lossy(&text).trim());
            static SHOWN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                if let Some(source) = SOURCES.lock().unwrap().as_ref().and_then(|m| m.get(&shader).cloned()) {
                    crate::log!("graphics: shader {shader} text:\n{source}");
                }
            }
        }
    }
}

extern "C" fn link_program(program: u32) {
    unsafe {
        glLinkProgram(program);
        let mut ok = 0;
        glGetProgramiv(program, LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut text = vec![0u8; 4096];
            let mut length = 0;
            glGetProgramInfoLog(program, text.len() as i32, &mut length, text.as_mut_ptr());
            text.truncate(length.max(0) as usize);
            crate::log!("graphics: program {program} did not link: {}", String::from_utf8_lossy(&text).trim());
        }
    }
}

extern "C" fn debug_label(_kind: Enum, _object: u32, _length: i32, _label: *const u8) {}
extern "C" fn debug_get_label(_kind: Enum, _object: u32, _size: i32, length: *mut i32, label: *mut u8) {
    unsafe {
        if !length.is_null() {
            *length = 0;
        }
        if !label.is_null() {
            *label = 0;
        }
    }
}
extern "C" fn debug_marker(_length: i32, _marker: *const u8) {}
extern "C" fn debug_pop() {}

/// Reports GL errors collected since the last check (a few per run).
pub fn check_errors(when: &str) {
    static REPORTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    for _ in 0..8 {
        let error = unsafe { glGetError() };
        if error == 0 {
            return;
        }
        if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::log!("graphics: GL error {error:#x} {when}");
        }
    }
}

pub fn register(r: &mut Registry) {
    if gles_library() == 0 || egl_library() == 0 {
        crate::log!("graphics: no OpenGL ES library on this system; drawing calls will stop the run");
        return;
    }
    for (apple, standard) in SAME_AS_STANDARD {
        let found = symbol(gles_library(), standard);
        let found = if found != 0 { found } else { get_proc_address(standard) };
        if found != 0 {
            r.function(apple, found);
        }
    }
    r.function("glGetString", get_string as usize);
    r.function("glGetIntegerv", get_integerv as usize);
    r.function("glRenderbufferStorage", renderbuffer_storage as usize);
    r.function("glFramebufferRenderbuffer", framebuffer_renderbuffer as usize);
    r.function("glFramebufferTexture2D", framebuffer_texture_2d as usize);
    r.function("glCheckFramebufferStatus", check_framebuffer_status as usize);
    r.function("glGenFramebuffers", gen_framebuffers as usize);
    r.function("glDeleteFramebuffers", delete_framebuffers as usize);
    r.function("glBindFramebuffer", bind_framebuffer as usize);
    r.function("glRenderbufferStorageMultisampleAPPLE", storage_multisample_apple as usize);
    r.function("glResolveMultisampleFramebufferAPPLE", resolve_multisample_apple as usize);
    r.function("glMapBufferOES", map_buffer_oes as usize);
    r.function("glTexImage2D", tex_image_2d as usize);
    r.function("glTexSubImage2D", tex_sub_image_2d as usize);
    r.function("glCompressedTexImage2D", compressed_tex_image_2d as usize);
    r.function("glShaderSource", shader_source as usize);
    r.function("glCompileShader", compile_shader as usize);
    r.function("glLinkProgram", link_program as usize);
    r.function("glLabelObjectEXT", debug_label as usize);
    r.function("glGetObjectLabelEXT", debug_get_label as usize);
    r.function("glInsertEventMarkerEXT", debug_marker as usize);
    r.function("glPushGroupMarkerEXT", debug_marker as usize);
    r.function("glPopGroupMarkerEXT", debug_pop as usize);
}
