//! OpenGL ES. The game's standard GL calls go straight to the system's
//! library (same processor, same calling rules). Apple-only extensions are
//! translated to standard OpenGL ES 3 here. OpenGL ES 1 functions come from
//! the system's OpenGL ES 1 library (or EGL's lookup where there is none).

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

/// The OpenGL ES 1 library (0 where there is none, as on the PC's Mesa
/// without glvnd's libGLESv1_CM: EGL's lookup serves those functions then).
pub fn gles1_library() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| open_first(&["libGLESv1_CM.so", "libGLESv1_CM.so.1"]))
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
    let found = if name.starts_with("egl") { symbol(egl_library(), name) } else { gles_symbol(name) };
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
    let found = gles_symbol(name);
    // EGL's lookup answers any name (glvnd makes a stub for it), so only the
    // OpenGL ES 1.1 functions are asked from it.
    let found = if found == 0 && ES1_CORE.contains(&name) { get_proc_address(name) } else { found };
    (found != 0).then_some(found)
}

/// A GL function from the OpenGL ES 3/2 library, else the OpenGL ES 1 one.
/// (Both serve whichever kind of context is current for the names they share.)
fn gles_symbol(name: &str) -> usize {
    match symbol(gles_library(), name) {
        0 => symbol(gles1_library(), name),
        found => found,
    }
}

/// OpenGL ES 1.1 functions that OpenGL ES 2 and 3 don't have.
const ES1_CORE: &[&str] = &[
    "glAlphaFunc", "glAlphaFuncx", "glClearColorx", "glClearDepthx", "glClientActiveTexture", "glClipPlanef",
    "glClipPlanex", "glColor4f", "glColor4ub", "glColor4x", "glColorPointer", "glDepthRangex",
    "glDisableClientState", "glEnableClientState", "glFogf", "glFogfv", "glFogx", "glFogxv", "glFrustumf",
    "glFrustumx", "glGetClipPlanef", "glGetClipPlanex", "glGetFixedv", "glGetLightfv", "glGetLightxv",
    "glGetMaterialfv", "glGetMaterialxv", "glGetPointerv", "glGetTexEnvfv", "glGetTexEnviv", "glGetTexEnvxv",
    "glGetTexParameterxv", "glLightModelf", "glLightModelfv", "glLightModelx", "glLightModelxv", "glLightf",
    "glLightfv", "glLightx", "glLightxv", "glLineWidthx", "glLoadIdentity", "glLoadMatrixf", "glLoadMatrixx",
    "glLogicOp", "glMaterialf", "glMaterialfv", "glMaterialx", "glMaterialxv", "glMatrixMode", "glMultMatrixf",
    "glMultMatrixx", "glMultiTexCoord4f", "glMultiTexCoord4x", "glNormal3f", "glNormal3x", "glNormalPointer",
    "glOrthof", "glOrthox", "glPointParameterf", "glPointParameterfv", "glPointParameterx", "glPointParameterxv",
    "glPointSize", "glPointSizex", "glPolygonOffsetx", "glPopMatrix", "glPushMatrix", "glRotatef", "glRotatex",
    "glSampleCoveragex", "glScalef", "glScalex", "glShadeModel", "glTexCoordPointer", "glTexEnvf", "glTexEnvfv",
    "glTexEnvi", "glTexEnviv", "glTexEnvx", "glTexEnvxv", "glTexParameterx", "glTexParameterxv", "glTranslatef",
    "glTranslatex", "glVertexPointer",
];

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
    glTexParameteri(target: Enum, name: Enum, value: i32);
    glTexParameterf(target: Enum, name: Enum, value: f32);
    glTexParameteriv(target: Enum, name: Enum, values: *const i32);
    glTexParameterfv(target: Enum, name: Enum, values: *const f32);
    glViewport(x: i32, y: i32, width: i32, height: i32);
    glScissor(x: i32, y: i32, width: i32, height: i32);
}

pub const VIEWPORT: Enum = 0x0BA2;
pub const SCISSOR_BOX: Enum = 0x0C10;

/// OpenGL ES 1 functions and numbers this layer itself uses (to show an
/// OpenGL ES 1 game's frames).
pub mod es1 {
    use super::Enum;

    system! {
        glActiveTexture(unit: Enum);
        glClientActiveTexture(unit: Enum);
        glMatrixMode(mode: Enum);
        glLoadIdentity();
        glLoadMatrixf(matrix: *const f32);
        glTexEnvi(target: Enum, name: Enum, value: i32);
        glGetTexEnviv(target: Enum, name: Enum, out: *mut i32);
        glEnableClientState(array: Enum);
        glDisableClientState(array: Enum);
        glVertexPointer(size: i32, kind: Enum, stride: i32, pointer: usize);
        glTexCoordPointer(size: i32, kind: Enum, stride: i32, pointer: usize);
        glGetPointerv(name: Enum, out: *mut usize);
        glDrawArrays(mode: Enum, first: i32, count: i32);
        glGenTextures(count: i32, out: *mut u32);
        glCopyTexSubImage2D(target: Enum, level: i32, x_offset: i32, y_offset: i32, x: i32, y: i32, width: i32, height: i32);
    }

    pub const FRAMEBUFFER_BINDING: Enum = 0x8CA6;
    pub const LIGHTING: Enum = 0x0B50;
    pub const ALPHA_TEST: Enum = 0x0BC0;
    pub const DEPTH_TEST: Enum = 0x0B71;
    pub const STENCIL_TEST: Enum = 0x0B90;
    pub const CULL_FACE: Enum = 0x0B44;
    pub const FOG: Enum = 0x0B60;
    pub const COLOR_LOGIC_OP: Enum = 0x0BF2;
    pub const CLIP_PLANE0: Enum = 0x3000;
    pub const MAX_CLIP_PLANES: Enum = 0x0D32;
    pub const MAX_TEXTURE_UNITS: Enum = 0x84E2;
    pub const MATRIX_PALETTE: Enum = 0x8840;
    pub const MATRIX_INDEX_ARRAY: Enum = 0x8844;
    pub const WEIGHT_ARRAY: Enum = 0x86AD;
    pub const POINT_SIZE_ARRAY: Enum = 0x8B9C;
    pub const TEXTURE_CUBE_MAP: Enum = 0x8513;
    pub const TEXTURE_GEN_STR: Enum = 0x8D60;
    pub const ACTIVE_TEXTURE: Enum = 0x84E0;
    pub const CLIENT_ACTIVE_TEXTURE: Enum = 0x84E1;
    pub const TEXTURE0: Enum = 0x84C0;
    pub const TEXTURE_2D: Enum = 0x0DE1;
    pub const TEXTURE_BINDING_2D: Enum = 0x8069;
    pub const TEXTURE_ENV: Enum = 0x2300;
    pub const TEXTURE_ENV_MODE: Enum = 0x2200;
    pub const REPLACE: Enum = 0x1E01;
    pub const MATRIX_MODE: Enum = 0x0BA0;
    pub const MODELVIEW: Enum = 0x1700;
    pub const PROJECTION: Enum = 0x1701;
    pub const TEXTURE: Enum = 0x1702;
    pub const MODELVIEW_MATRIX: Enum = 0x0BA6;
    pub const PROJECTION_MATRIX: Enum = 0x0BA7;
    pub const TEXTURE_MATRIX: Enum = 0x0BA8;
    pub const ARRAY_BUFFER: Enum = 0x8892;
    pub const ARRAY_BUFFER_BINDING: Enum = 0x8894;
    pub const VERTEX_ARRAY: Enum = 0x8074;
    pub const NORMAL_ARRAY: Enum = 0x8075;
    pub const COLOR_ARRAY: Enum = 0x8076;
    pub const TEXTURE_COORD_ARRAY: Enum = 0x8078;
    pub const VERTEX_ARRAY_SIZE: Enum = 0x807A;
    pub const VERTEX_ARRAY_TYPE: Enum = 0x807B;
    pub const VERTEX_ARRAY_STRIDE: Enum = 0x807C;
    pub const VERTEX_ARRAY_POINTER: Enum = 0x808E;
    pub const VERTEX_ARRAY_BUFFER_BINDING: Enum = 0x8896;
    pub const TEXTURE_COORD_ARRAY_SIZE: Enum = 0x8088;
    pub const TEXTURE_COORD_ARRAY_TYPE: Enum = 0x8089;
    pub const TEXTURE_COORD_ARRAY_STRIDE: Enum = 0x808A;
    pub const TEXTURE_COORD_ARRAY_POINTER: Enum = 0x8092;
    pub const TEXTURE_COORD_ARRAY_BUFFER_BINDING: Enum = 0x889A;
    pub const RGB: Enum = 0x1907;
    pub const FLOAT: Enum = 0x1406;
    pub const TRIANGLE_STRIP: Enum = 0x0005;
    pub const TEXTURE_MIN_FILTER: Enum = 0x2801;
    pub const TEXTURE_MAG_FILTER: Enum = 0x2800;
    pub const TEXTURE_WRAP_S: Enum = 0x2802;
    pub const TEXTURE_WRAP_T: Enum = 0x2803;
    pub const CLAMP_TO_EDGE: Enum = 0x812F;
}

pub const COLOR_CLEAR_VALUE: Enum = 0x0C22;
pub const BLEND: Enum = 0x0BE2;

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
/// (OpenGL ES 1's OES framebuffer and blending functions are the standard
/// ones under another name: the system's libraries serve them for ES 1
/// contexts under the standard name too.)
const SAME_AS_STANDARD: &[(&str, &str)] = &[
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
    // OES_framebuffer_object, OpenGL ES 1's names for framebuffers: iOS
    // answers them in ES 2 contexts too, and older engines use them there.
    ("glGenFramebuffersOES", "glGenFramebuffers"),
    ("glDeleteFramebuffersOES", "glDeleteFramebuffers"),
    ("glBindFramebufferOES", "glBindFramebuffer"),
    ("glIsFramebufferOES", "glIsFramebuffer"),
    ("glCheckFramebufferStatusOES", "glCheckFramebufferStatus"),
    ("glFramebufferRenderbufferOES", "glFramebufferRenderbuffer"),
    ("glFramebufferTexture2DOES", "glFramebufferTexture2D"),
    ("glGetFramebufferAttachmentParameterivOES", "glGetFramebufferAttachmentParameteriv"),
    ("glGenRenderbuffersOES", "glGenRenderbuffers"),
    ("glDeleteRenderbuffersOES", "glDeleteRenderbuffers"),
    ("glBindRenderbufferOES", "glBindRenderbuffer"),
    ("glIsRenderbufferOES", "glIsRenderbuffer"),
    ("glRenderbufferStorageOES", "glRenderbufferStorage"),
    ("glGetRenderbufferParameterivOES", "glGetRenderbufferParameteriv"),
    ("glGenerateMipmapOES", "glGenerateMipmap"),
    ("glBlendEquationOES", "glBlendEquation"),
    ("glBlendEquationSeparateOES", "glBlendEquationSeparate"),
    ("glBlendFuncSeparateOES", "glBlendFuncSeparate"),
];

/// OpenGL ES 1 extension functions with no standard twin: the system's own.
const ES1_EXTENSION_FUNCTIONS: &[&str] = &[
    // OES_draw_texture
    "glDrawTexsOES", "glDrawTexiOES", "glDrawTexxOES", "glDrawTexfOES",
    "glDrawTexsvOES", "glDrawTexivOES", "glDrawTexxvOES", "glDrawTexfvOES",
    // OES_point_size_array
    "glPointSizePointerOES",
    // OES_matrix_palette
    "glCurrentPaletteMatrixOES", "glLoadPaletteFromModelViewMatrixOES", "glMatrixIndexPointerOES", "glWeightPointerOES",
    // OES_texture_cube_map
    "glTexGenfOES", "glTexGenfvOES", "glTexGeniOES", "glTexGenivOES",
    "glGetTexGenfvOES", "glGetTexGenivOES",
];

/// Extension functions translated in this file (beyond `SAME_AS_STANDARD`).
const TRANSLATED_EXTENSIONS: &[&str] = &[
    "glDiscardFramebufferEXT",
    "glRenderbufferStorageMultisampleAPPLE",
    "glResolveMultisampleFramebufferAPPLE",
    "glMapBufferOES",
    "glLabelObjectEXT",
    "glGetObjectLabelEXT",
    "glInsertEventMarkerEXT",
    "glPushGroupMarkerEXT",
    "glPopGroupMarkerEXT",
];

/// Every Apple/EXT/OES function this layer gives games for the extensions it advertises.
pub fn extension_names() -> Vec<&'static str> {
    SAME_AS_STANDARD.iter().map(|(apple, _)| *apple).chain(TRANSLATED_EXTENSIONS.iter().copied()).chain(ES1_EXTENSION_FUNCTIONS.iter().copied()).collect()
}

/// The standard OpenGL ES 3 function an extension function is the same as.
pub fn standard_name(name: &str) -> Option<&'static str> {
    SAME_AS_STANDARD.iter().find(|(apple, _)| *apple == name).map(|(_, standard)| *standard)
}

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

/// What an iPhone (PowerVR SGX, iOS 6) reports in an OpenGL ES 1 context.
const IPHONE_ES1_EXTENSIONS: &[&str] = &[
    "GL_APPLE_texture_2D_limited_npot", "GL_APPLE_texture_format_BGRA8888", "GL_APPLE_texture_max_level",
    "GL_EXT_blend_minmax", "GL_EXT_debug_label", "GL_EXT_debug_marker", "GL_EXT_discard_framebuffer",
    "GL_EXT_map_buffer_range", "GL_EXT_read_format_bgra", "GL_EXT_texture_filter_anisotropic",
    "GL_EXT_texture_lod_bias", "GL_IMG_read_format", "GL_IMG_texture_compression_pvrtc",
    "GL_OES_blend_equation_separate", "GL_OES_blend_func_separate", "GL_OES_blend_subtract",
    "GL_OES_compressed_paletted_texture", "GL_OES_depth24", "GL_OES_draw_texture", "GL_OES_fbo_render_mipmap",
    "GL_OES_framebuffer_object", "GL_OES_mapbuffer", "GL_OES_matrix_palette", "GL_OES_packed_depth_stencil",
    "GL_OES_point_size_array", "GL_OES_point_sprite", "GL_OES_read_format", "GL_OES_rgb8_rgba8",
    "GL_OES_stencil_wrap", "GL_OES_stencil8", "GL_OES_texture_mirrored_repeat", "GL_OES_vertex_array_object",
];

/// Provided here whatever the chip has, in an OpenGL ES 1 context.
const ALWAYS_AVAILABLE_ES1: &[&str] = &[
    "GL_EXT_discard_framebuffer", "GL_APPLE_texture_format_BGRA8888", "GL_EXT_debug_label", "GL_EXT_debug_marker",
    "GL_IMG_texture_compression_pvrtc",
];

/// iPhone extensions the chip offers under another name.
fn chip_has(name: &str) -> bool {
    match name {
        // Any sides (Apple's limits: no mipmaps or repeat, which every other kind allows too).
        "GL_APPLE_texture_2D_limited_npot" => ["GL_APPLE_texture_2D_limited_npot", "GL_OES_texture_npot", "GL_ARB_texture_non_power_of_two", "GL_IMG_texture_npot"]
            .iter()
            .any(|n| system_has_extension(n)),
        _ => system_has_extension(name),
    }
}

// ----- translated functions -----

extern "C" fn get_string(name: Enum) -> *const u8 {
    if name != EXTENSIONS {
        return unsafe { glGetString(name) };
    }
    static TEXT: OnceLock<usize> = OnceLock::new();
    static TEXT_ES1: OnceLock<usize> = OnceLock::new();
    let (slot, iphone, always, kind) = if crate::fw::eagl::current_is_es1() {
        (&TEXT_ES1, IPHONE_ES1_EXTENSIONS, ALWAYS_AVAILABLE_ES1, "OpenGL ES 1 ")
    } else {
        (&TEXT, IPHONE_EXTENSIONS, ALWAYS_AVAILABLE, "")
    };
    *slot.get_or_init(|| {
        // Test switch: IOS_GL_HIDE="GL_EXT_a,GL_EXT_b" keeps those from the game.
        let hidden = std::env::var("IOS_GL_HIDE").unwrap_or_default();
        let list: Vec<&str> = iphone
            .iter()
            .copied()
            .filter(|name| always.contains(name) || chip_has(name))
            .filter(|name| !hidden.split(',').any(|h| h.trim() == *name))
            .collect();
        crate::log!("graphics: telling the game it has {} of the iPhone's {} {kind}extensions", list.len(), iphone.len());
        let from_chip: Vec<&str> = list.iter().copied().filter(|name| !always.contains(name)).collect();
        crate::log!("graphics: from the chip: {}", from_chip.join(" "));
        leak_cstr(&list.join(" "))
    }) as *const u8
}

const NUM_COMPRESSED_TEXTURE_FORMATS: Enum = 0x86A2;
const COMPRESSED_TEXTURE_FORMATS: Enum = 0x86A3;
const MAX_TEXTURE_SIZE: Enum = 0x0D33;
const MAX_CUBE_MAP_TEXTURE_SIZE: Enum = 0x851C;
const MAX_RENDERBUFFER_SIZE: Enum = 0x84E8;
const NUM_PROGRAM_BINARY_FORMATS: Enum = 0x87FE;
const PROGRAM_BINARY_FORMATS: Enum = 0x87FF;

/// Like the iPhone 5s: its four PVRTC formats, sizes up to 4096, and no way
/// to save built shader programs (Unity's shader cache, which it turns on when
/// there is one, names its files so that different shaders share a file).
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
            NUM_PROGRAM_BINARY_FORMATS => *out = 0,
            PROGRAM_BINARY_FORMATS => {}
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
    let earlier = unsafe { glGetError() };
    if earlier != 0 {
        crate::log!("graphics: GL error {earlier:#x} was already waiting before glFramebufferRenderbuffer");
    }
    unsafe { glFramebufferRenderbuffer(target, attachment, rb_target, renderbuffer) };
    trace_setup(format_args!("framebuffer attachment {attachment:#x} = renderbuffer {renderbuffer}"));
    let error = unsafe { glGetError() };
    if error != 0 {
        let mut bound = 0;
        unsafe { glGetIntegerv(DRAW_FRAMEBUFFER_BINDING, &mut bound) };
        crate::log!("graphics: GL error {error:#x} after glFramebufferRenderbuffer (target {target:#x}, framebuffer {bound} bound, context {:#x})", unsafe { eglGetCurrentContext() });
    }
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

/// The system's own function for an extension name (0 if it has none).
fn system_extension_function(slot: &OnceLock<usize>, name: &str) -> usize {
    *slot.get_or_init(|| match symbol(gles1_library(), name) {
        0 => get_proc_address(name),
        found => found,
    })
}

/// glDiscardFramebufferEXT is glInvalidateFramebuffer in OpenGL ES 3; an
/// OpenGL ES 1 context has only the extension (or nothing: discarding is a
/// hint, so leaving the contents is right too).
extern "C" fn discard_framebuffer(target: Enum, count: i32, attachments: *const Enum) {
    if !crate::fw::eagl::current_is_es1() {
        unsafe { glInvalidateFramebuffer(target, count, attachments) };
        return;
    }
    static SYSTEM: OnceLock<usize> = OnceLock::new();
    static OFFERED: OnceLock<bool> = OnceLock::new();
    let address = system_extension_function(&SYSTEM, "glDiscardFramebufferEXT");
    if address != 0 && *OFFERED.get_or_init(|| system_has_extension("GL_EXT_discard_framebuffer")) {
        let f: unsafe extern "C" fn(Enum, i32, *const Enum) = unsafe { std::mem::transmute(address) };
        unsafe { f(target, count, attachments) };
    }
}

/// glMapBufferOES(target, access): Apple only allows writing (WRITE_ONLY_OES).
extern "C" fn map_buffer_oes(target: Enum, access: Enum) -> usize {
    if crate::fw::eagl::current_is_es1() {
        // OpenGL ES 1 has no glMapBufferRange: the system's own glMapBufferOES.
        static SYSTEM: OnceLock<usize> = OnceLock::new();
        let address = system_extension_function(&SYSTEM, "glMapBufferOES");
        if address == 0 {
            return 0;
        }
        let f: unsafe extern "C" fn(Enum, Enum) -> usize = unsafe { std::mem::transmute(address) };
        return unsafe { f(target, access) };
    }
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
    if crate::fw::eagl::current_is_es1() {
        // OpenGL ES 1 has only the row alignment: no pixel buffers, row lengths or skips.
        return Unpack { buffer: 0, row_length: 0, skip_rows: 0, skip_pixels: 0, alignment: get(UNPACK_ALIGNMENT).max(1) };
    }
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
    if crate::fw::eagl::current_is_es1() {
        unsafe {
            glPixelStorei(UNPACK_ALIGNMENT, 4);
            upload();
            glPixelStorei(UNPACK_ALIGNMENT, unpack.alignment);
        }
        return;
    }
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
    trace_texture("plain", level, format, width, height, 0, std::ptr::null());
    texture_redefined(target, level);
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

/// Test switch IOS_TRACE_TEXTURES=1: logs every texture level the game sends
/// (bound texture, level, size, format, byte count and a hash of its first
/// 4096 bytes, FNV-1a), to match uploads against the game's files.
fn trace_texture(kind: &str, level: i32, internal: Enum, width: i32, height: i32, size: i32, data: *const u8) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var("IOS_TRACE_TEXTURES").is_ok_and(|v| v == "1")) {
        return;
    }
    let mut bound = 0;
    unsafe { glGetIntegerv(TEXTURE_BINDING_2D, &mut bound) };
    let bytes = read_source(&unpack_state(), data, (size.max(0) as usize).min(4096)).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    crate::log!("texture: {kind} #{bound} level {level} {width}x{height} format {internal:#x} {size} bytes key {hash:016x}");
}

// ----- Pictures merged from an Android build -----
//
// tools/merge-unreal-textures.py makes a copy of an Unreal game with an
// Android build's ASTC pictures written over its PVRTC ones (same places,
// padded to the same size). The game still sends them as PVRTC; the copy's
// GameBridgeTextures.txt lists those levels (size, byte count and a key of
// their first 4096 bytes) with their ASTC block, so they go to the chip as
// ASTC.

const MERGED_LIST: &str = "GameBridgeTextures.txt";
const ASTC_BLOCKS: [(u8, u8); 14] = [(4, 4), (5, 4), (5, 5), (6, 5), (6, 6), (8, 5), (8, 6), (8, 8), (10, 5), (10, 6), (10, 8), (10, 10), (12, 10), (12, 12)];

/// (width, height, bytes, key) -> ASTC block of each merged level.
type Merged = std::collections::HashMap<(u32, u32, u32, u64), (u8, u8)>;

/// FNV-1a 64 of the first 4096 bytes, as the merge tool writes it.
fn picture_key(bytes: &[u8]) -> u64 {
    bytes.iter().take(4096).fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn read_merged_list(text: &str) -> Merged {
    let mut list = Merged::new();
    let mut conflicting = Vec::new();
    for (key, block) in text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [w, h, size, key, bw, bh] = f[..] else { return None };
            let block = (bw.parse().ok()?, bh.parse().ok()?);
            ASTC_BLOCKS.contains(&block).then_some(())?;
            Some(((w.parse().ok()?, h.parse().ok()?, size.parse().ok()?, u64::from_str_radix(key, 16).ok()?), block))
        })
    {
        // The same level listed with two different blocks can't be trusted.
        if list.insert(key, block).is_some_and(|old| old != block) {
            conflicting.push(key);
        }
    }
    for key in conflicting {
        list.remove(&key);
    }
    list
}

fn merged() -> &'static Merged {
    static LIST: OnceLock<Merged> = OnceLock::new();
    LIST.get_or_init(|| {
        let program = crate::fw::paths::apple_executable_path();
        let Some((app, _)) = program.rsplit_once('/') else { return Merged::new() };
        let Ok(text) = std::fs::read_to_string(format!("{app}/{MERGED_LIST}")) else { return Merged::new() };
        let list = read_merged_list(&text);
        if !system_has_extension("GL_KHR_texture_compression_astc_ldr") {
            crate::log!("graphics: this game has {} picture levels in ASTC, but the chip has no ASTC; they will look wrong", list.len());
            return Merged::new();
        }
        crate::log!("graphics: {} picture levels merged from an Android build go to the chip as ASTC", list.len());
        list
    })
}

/// ASTC size of a level and the GL format of its block.
fn astc(width: i32, height: i32, block: (u8, u8)) -> (usize, Enum) {
    let index = ASTC_BLOCKS.iter().position(|&b| b == block).unwrap_or(0);
    let blocks = |n: i32, b: u8| (n.max(1) as usize).div_ceil(b as usize);
    (blocks(width, block.0) * blocks(height, block.1) * 16, 0x93B0 + index as Enum)
}

/// Per texture (context, bound 2D texture name): whether its first level
/// was merged, and the level it stops at when a later level turned out the
/// other kind. A texture must not mix ASTC and other levels (the chip would
/// draw it black), so it then keeps only the levels before that one.
static MERGED_TEXTURES: std::sync::Mutex<Option<std::collections::HashMap<(usize, u32), (bool, Option<i32>)>>> = std::sync::Mutex::new(None);
const TEXTURE_MAX_LEVEL: Enum = 0x813D;

/// The bound 2D texture of the current context, when `target` is a 2D texture.
fn bound_texture(target: Enum) -> Option<(usize, u32)> {
    if target != TEXTURE_2D {
        return None;
    }
    let mut name = 0;
    unsafe { glGetIntegerv(TEXTURE_BINDING_2D, &mut name) };
    (name > 0).then(|| (unsafe { eglGetCurrentContext() }, name as u32))
}

/// A texture's first level is being defined some other way: forget what
/// was known about it and undo a stop set for its earlier contents.
fn texture_redefined(target: Enum, level: i32) {
    if level != 0 || merged().is_empty() {
        return;
    }
    let Some(texture) = bound_texture(target) else { return };
    if let Some(textures) = MERGED_TEXTURES.lock().unwrap().as_mut() {
        if let Some((_, Some(_))) = textures.remove(&texture) {
            unsafe { glTexParameteri(target, TEXTURE_MAX_LEVEL, 1000) };
        }
    }
}

/// What to do with a PVRTC level of a game that has merged pictures.
enum Merge {
    /// Not a merged level: the usual path.
    Plain,
    /// A merged level: send it as ASTC.
    Astc(Vec<u8>, Enum, usize),
    /// Past where this texture stops: not sent.
    Skip,
}

fn merge_decision(target: Enum, level: i32, width: i32, height: i32, size: i32, data: *const u8) -> Merge {
    let list = merged();
    if list.is_empty() || size <= 0 || width <= 0 || height <= 0 {
        return Merge::Plain;
    }
    // Only plain 2D textures: a cube map needs all its faces alike.
    let Some(texture) = bound_texture(target) else { return Merge::Plain };
    let unpack = unpack_state();
    let hit = read_source(&unpack, data, (size as usize).min(4096))
        .and_then(|start| list.get(&(width as u32, height as u32, size as u32, picture_key(&start))).copied())
        .map(|block| astc(width, height, block))
        .filter(|&(needed, _)| needed <= size as usize);
    {
        let mut textures = MERGED_TEXTURES.lock().unwrap();
        let textures = textures.get_or_insert_with(Default::default);
        if level == 0 {
            if let Some((_, Some(_))) = textures.insert(texture, (hit.is_some(), None)) {
                unsafe { glTexParameteri(target, TEXTURE_MAX_LEVEL, 1000) };
            }
        } else if let Some((first_merged, stop)) = textures.get_mut(&texture) {
            if stop.is_some_and(|stop| level >= stop) {
                return Merge::Skip;
            }
            if *first_merged != hit.is_some() {
                *stop = Some(level);
                unsafe { glTexParameteri(target, TEXTURE_MAX_LEVEL, level - 1) };
                static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
                    crate::log!("graphics: texture {} ({width}x{height} at level {level}) mixes merged and other levels; it keeps levels 0-{}", texture.1, level - 1);
                }
                return Merge::Skip;
            }
        }
    }
    match hit.and_then(|(needed, format)| Some((read_source(&unpack, data, needed)?, format, needed))) {
        Some((bytes, format, needed)) => Merge::Astc(bytes, format, needed),
        None => Merge::Plain,
    }
}

extern "C" fn compressed_tex_image_2d(target: Enum, level: i32, internal: Enum, width: i32, height: i32, border: i32, size: i32, data: *const u8) {
    trace_texture("compressed", level, internal, width, height, size, data);
    if !PVRTC_FORMATS.contains(&internal) {
        texture_redefined(target, level);
    } else {
        match merge_decision(target, level, width, height, size, data) {
            Merge::Plain => {}
            Merge::Skip => return,
            Merge::Astc(bytes, format, needed) => {
                let unpack = unpack_state();
                upload_plain(&unpack, || unsafe { glCompressedTexImage2D(target, level, format, width, height, border, needed as i32, bytes.as_ptr()) });
                static FIRST: std::sync::Once = std::sync::Once::new();
                FIRST.call_once(|| crate::log!("graphics: first merged picture sent as ASTC ({width}x{height})"));
                return;
            }
        }
    }
    if PVRTC_FORMATS.contains(&internal) && !pvrtc_on_chip() {
        // 0x8C00/0x8C02 are 4 bits per pixel, 0x8C01/0x8C03 are 2.
        let two_bits = internal & 1 == 1;
        let needed = crate::fw::pvrtc::size(width.max(0) as u32, height.max(0) as u32, two_bits);
        let unpack = unpack_state();
        // Small mipmap levels: Apple's tools write fewer bytes than the
        // 8x8-block minimum the decoder reads; the rest is taken as zero.
        let given = (size.max(0) as usize).min(needed);
        let pixels = read_source(&unpack, data, given).and_then(|mut bytes| {
            bytes.resize(needed, 0);
            crate::fw::pvrtc::decode(&bytes, width as u32, height as u32, two_bits)
        });
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

// ----- Sharper surfaces seen at an angle -----
//
// Games set how much texture sharpening at an angle (anisotropic filtering)
// the iPhones of their day could afford, often 2x or none. Like a graphics
// driver's override, every mipmapped texture gets the chip's most (up to
// 16x) instead, whatever the game asks for.

const TEXTURE_MIN_FILTER: Enum = 0x2801;
const TEXTURE_MAX_ANISOTROPY: Enum = 0x84FE;
const MAX_TEXTURE_MAX_ANISOTROPY: Enum = 0x84FF;

/// The sharpening every mipmapped texture gets; None when the chip has none.
fn sharpening() -> Option<f32> {
    static MOST: OnceLock<Option<f32>> = OnceLock::new();
    *MOST.get_or_init(|| {
        // Test switch: IOS_SHARPENING=0 leaves the game's own setting.
        if std::env::var("IOS_SHARPENING").is_ok_and(|v| v == "0") {
            crate::log!("graphics: sharpening at an angle left to the game (IOS_SHARPENING=0)");
            return None;
        }
        if !system_has_extension("GL_EXT_texture_filter_anisotropic") {
            crate::log!("graphics: the chip has no sharpening at an angle (anisotropic filtering)");
            return None;
        }
        let mut most = 0f32;
        unsafe { glGetFloatv(MAX_TEXTURE_MAX_ANISOTROPY, &mut most) };
        let level = most.min(16.0);
        crate::log!("graphics: textures get {level}x sharpening at an angle (the chip's most is {most}x)");
        (level > 1.0).then_some(level)
    })
}

/// True for the minifying filters that read mipmap levels.
fn uses_mipmaps(filter: i32) -> bool {
    (0x2700..=0x2703).contains(&filter)
}

/// After the game set `name` on the bound texture: our sharpening where it
/// applies. Its own sharpening value is replaced rather than passed on.
fn sharpen(target: Enum, name: Enum, value: f32) -> bool {
    let wanted = match name {
        TEXTURE_MIN_FILTER => uses_mipmaps(value as i32),
        TEXTURE_MAX_ANISOTROPY => true,
        _ => return false,
    };
    match sharpening() {
        Some(level) if wanted => {
            if name == TEXTURE_MIN_FILTER {
                unsafe { glTexParameteri(target, name, value as i32) };
            }
            unsafe { glTexParameterf(target, TEXTURE_MAX_ANISOTROPY, level) };
            true
        }
        _ => false,
    }
}

extern "C" fn tex_parameter_i(target: Enum, name: Enum, value: i32) {
    if !sharpen(target, name, value as f32) {
        unsafe { glTexParameteri(target, name, value) };
    }
}

extern "C" fn tex_parameter_f(target: Enum, name: Enum, value: f32) {
    if !sharpen(target, name, value) {
        unsafe { glTexParameterf(target, name, value) };
    }
}

extern "C" fn tex_parameter_iv(target: Enum, name: Enum, values: *const i32) {
    if values.is_null() || !sharpen(target, name, unsafe { *values } as f32) {
        unsafe { glTexParameteriv(target, name, values) };
    }
}

extern "C" fn tex_parameter_fv(target: Enum, name: Enum, values: *const f32) {
    if values.is_null() || !sharpen(target, name, unsafe { *values }) {
        unsafe { glTexParameterfv(target, name, values) };
    }
}

fn pvrtc_on_chip() -> bool {
    static ON_CHIP: OnceLock<bool> = OnceLock::new();
    *ON_CHIP.get_or_init(|| {
        let found = system_has_extension("GL_IMG_texture_compression_pvrtc");
        crate::log!("graphics: PVRTC textures {}", if found { "go straight to the graphics chip" } else { "are decoded here (the chip has no PVRTC)" });
        found
    })
}

/// Whether the current context's own extension list has `name`.
pub fn system_has_extension(name: &str) -> bool {
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
    r.function("glTexParameteri", tex_parameter_i as usize);
    r.function("glTexParameterf", tex_parameter_f as usize);
    r.function("glTexParameteriv", tex_parameter_iv as usize);
    r.function("glTexParameterfv", tex_parameter_fv as usize);
    r.function("glShaderSource", shader_source as usize);
    r.function("glCompileShader", compile_shader as usize);
    r.function("glLinkProgram", link_program as usize);
    r.function("glLabelObjectEXT", debug_label as usize);
    r.function("glGetObjectLabelEXT", debug_get_label as usize);
    r.function("glInsertEventMarkerEXT", debug_marker as usize);
    r.function("glPushGroupMarkerEXT", debug_marker as usize);
    r.function("glPopGroupMarkerEXT", debug_pop as usize);
    r.function("glDiscardFramebufferEXT", discard_framebuffer as usize);
    // An extension name of a standard function translated here takes the translation too.
    for (apple, standard) in SAME_AS_STANDARD {
        if let Some((address, crate::symbols::Kind::Function)) = r.get(standard) {
            r.function(apple, address);
        }
    }
    for name in ES1_EXTENSION_FUNCTIONS {
        let found = symbol(gles1_library(), name);
        let found = if found != 0 { found } else { get_proc_address(name) };
        if found != 0 {
            r.function(name, found);
        }
    }
}

#[cfg(test)]
mod merged_tests {
    use super::*;

    #[test]
    fn extension_names_reach_this_layers_own_version_of_their_function() {
        // glBindFramebufferOES must bind the drawable's framebuffer as
        // glBindFramebuffer does here, not go straight to the system's.
        let mut r = Registry::default();
        register(&mut r);
        let function = |name: &str| r.get(name).map(|(address, _)| address);
        assert_eq!(function("glBindFramebufferOES"), Some(bind_framebuffer as usize));
        assert_eq!(function("glRenderbufferStorageOES"), Some(renderbuffer_storage as usize));
        for (extension, standard) in SAME_AS_STANDARD {
            if function(standard).is_some() {
                assert_eq!(function(extension), function(standard), "{extension}");
            }
        }
    }

    #[test]
    fn no_saved_shader_programs_like_the_iphone() {
        // Unity turns on a shader cache when there are formats; its file names
        // aren't unique, so shaders got each other's programs.
        let mut count = 5;
        get_integerv(NUM_PROGRAM_BINARY_FORMATS, &mut count);
        assert_eq!(count, 0);
        let mut formats = [7; 2];
        get_integerv(PROGRAM_BINARY_FORMATS, formats.as_mut_ptr());
        assert_eq!(formats, [7, 7], "no formats are written");
    }

    #[test]
    fn merged_list_and_astc_sizes() {
        let list = read_merged_list("# comment\n2048 2048 1048576 00ff00ff00ff00ff 8 8\n4 4 32 0000000000000001 6 6\nbad line\n8 8 32 0000000000000002 7 7\n16 16 64 0000000000000003 8 8\n16 16 64 0000000000000003 6 6\n");
        // The bad line, the unknown block and the level listed twice with different blocks are left out.
        assert_eq!(list.len(), 2);
        assert_eq!(list[&(2048, 2048, 1048576, 0x00ff00ff00ff00ff)], (8, 8));
        assert_eq!(astc(2048, 2048, (8, 8)), (1048576, 0x93B7));
        assert_eq!(astc(2048, 2048, (6, 6)), (342 * 342 * 16, 0x93B4));
        assert_eq!(astc(1, 1, (12, 12)), (16, 0x93BD));
        assert_eq!(picture_key(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(picture_key(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
