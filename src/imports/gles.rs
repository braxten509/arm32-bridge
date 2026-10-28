//! OpenGL ES 1, 2 and 3.x for guest code. Most functions are generated from
//! the NDK headers (gles_generated.rs); the ones here need special care:
//! - vertex, index, pixel and indirect pointers are either a guest pointer or
//!   an offset into a bound buffer, depending on GL state, so buffer bindings
//!   are tracked per GL context (and mapped buffers per share group: a
//!   buffer mapped in one context can be unmapped in another).
//! - glMapBufferOES / glMapBufferRange return driver memory outside the guest's
//!   4 GiB, so the guest writes into a guest copy that is uploaded on unmap.
//! - glShaderSource and friends take arrays of pointers; glGetString(i) returns one.
//! - GLsync objects are 64-bit pointers, so the guest gets small handles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::handler;
use crate::runtime::{mem, shared, ArgSource, Handler, Ret};
use super::s3tc;

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const GL_DRAW_INDIRECT_BUFFER: u32 = 0x8F3F;
const GL_MAP_READ_BIT: u32 = 0x0001;
const GL_MAP_WRITE_BIT: u32 = 0x0002;
const GL_MAP_INVALIDATE_RANGE_BIT: u32 = 0x0004;
const GL_MAP_INVALIDATE_BUFFER_BIT: u32 = 0x0008;
const GL_MAP_FLUSH_EXPLICIT_BIT: u32 = 0x0010;
const GL_BUFFER_SIZE: u32 = 0x8764;

pub fn guest_pointer(guest: u32) -> *mut c_void {
    mem().host_or_null(guest) as *mut c_void
}

/// Where host GL functions come from when an embedder has its own (the
/// iPhone layer: Apple's extensions translated, PVRTC decoded); names it
/// doesn't know fall back to the system's library.
static HOST_LOOKUP: std::sync::OnceLock<fn(&str) -> usize> = std::sync::OnceLock::new();

/// Sets the embedder's host GL lookup (once, before the first GL call).
pub fn set_host_lookup(lookup: fn(&str) -> usize) {
    let _ = HOST_LOOKUP.set(lookup);
}

/// Where the host context current on this thread comes from when the
/// embedder runs its own EGL (the iPhone layer's EAGL on Linux, where the
/// translator's EGL is absent). Unset: the translator's EGL.
static CURRENT_CONTEXT: std::sync::OnceLock<fn() -> usize> = std::sync::OnceLock::new();

/// Sets the embedder's current-context lookup (once, before the first GL call).
pub fn set_current_context_source(source: fn() -> usize) {
    let _ = CURRENT_CONTEXT.set(source);
}

/// Every GL name `handler` knows.
pub fn names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = [include_str!("gles.rs"), include_str!("gles_generated.rs")]
        .iter()
        .flat_map(|source| source.lines())
        .filter_map(|line| line.split_once("=>").map(|(arms, _)| arms))
        .flat_map(|arms| arms.split('"').skip(1).step_by(2))
        .filter(|name| name.starts_with("gl") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .filter(|name| handler(name).is_some())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The host GL function `name`, looked up once and cached in `slot`.
pub fn host_function(slot: &AtomicUsize, name: &str) -> usize {
    let cached = slot.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let address = HOST_LOOKUP.get().map_or(0, |embedder| embedder(name));
    let address = if address != 0 { address } else { lookup(name) };
    if address == 0 {
        panic!("GL function {name} is not available on this phone");
    }
    slot.store(address, Ordering::Relaxed);
    address
}

#[cfg(target_os = "android")]
fn lookup(name: &str) -> usize {
    #[link(name = "EGL")]
    extern "C" {
        fn eglGetProcAddress(name: *const libc::c_char) -> *mut c_void;
    }
    let symbol = std::ffi::CString::new(name).unwrap();
    unsafe {
        // GLES2's library also serves GLES1 contexts for the names both have;
        // GLES1-only functions (glMatrixMode, ...) come from libGLESv1_CM.
        for name in [c"libGLESv2.so", c"libGLESv1_CM.so"] {
            let library = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
            let library = if library.is_null() { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) } else { library };
            let direct = if library.is_null() { std::ptr::null_mut() } else { libc::dlsym(library, symbol.as_ptr()) };
            if !direct.is_null() {
                return direct as usize;
            }
        }
        eglGetProcAddress(symbol.as_ptr()) as usize
    }
}

#[cfg(not(target_os = "android"))]
fn lookup(_name: &str) -> usize {
    0
}

macro_rules! host {
    ($name:literal, fn($($t:ty),*) $(-> $r:ty)?) => {{
        static ADDRESS: AtomicUsize = AtomicUsize::new(0);
        // Only used inside `unsafe` blocks at the call sites.
        let function: unsafe extern "C" fn($($t),*) $(-> $r)? =
            std::mem::transmute(host_function(&ADDRESS, $name));
        function
    }};
}

#[derive(Default)]
struct Bindings {
    /// Bound buffer per target, except the element buffer (that one belongs to
    /// the vertex array object).
    buffers: HashMap<u32, u32>,
    vertex_array: u32,
    /// Element buffer binding per vertex array object (0 = the default one).
    element_buffers: HashMap<u32, u32>,
}

/// What contexts sharing objects share.
#[derive(Default)]
struct ShareGroup {
    /// Mapped buffers by buffer name. GL keeps a buffer mapped after another
    /// buffer is bound to the same target, so the target can't be the key.
    mapped: HashMap<u32, Mapped>,
}

#[derive(Clone)]
struct Context {
    bindings: Arc<Mutex<Bindings>>,
    group: Arc<Mutex<ShareGroup>>,
}

/// Every host EGL context seen, by its pointer. Contexts the game did not
/// make (Java's GLSurfaceView makes them) get their own group on first sight.
static CONTEXTS: Mutex<Option<HashMap<usize, Context>>> = Mutex::new(None);
/// Bumped when a context is made (it may reuse a destroyed one's pointer).
static CONTEXT_CHANGES: AtomicU64 = AtomicU64::new(0);

pub fn context_created(context: usize, share: usize) {
    let mut contexts = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    let contexts = contexts.get_or_insert_with(HashMap::new);
    let group = match share {
        0 => None,
        share => Some(Arc::clone(&contexts.entry(share).or_insert_with(new_context).group)),
    };
    let entry = Context { bindings: Arc::default(), group: group.unwrap_or_default() };
    contexts.insert(context, entry);
    CONTEXT_CHANGES.fetch_add(1, Ordering::Release);
}

/// A thread that still has it current keeps its state until it moves on
/// (EGL deletes a current context only once released): its cached lookup
/// stays valid, as only a new context made at the same address bumps
/// CONTEXT_CHANGES.
pub fn context_destroyed(context: usize) {
    if let Some(contexts) = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        contexts.remove(&context);
    }
}

/// Whether host context `context` has tracking entries (for tests and diagnostics).
pub fn context_tracked(context: usize) -> bool {
    CONTEXTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|contexts| contexts.contains_key(&context))
}

fn new_context() -> Context {
    Context { bindings: Arc::default(), group: Arc::default() }
}

thread_local! {
    /// The context current on this thread, as last looked up: (host context,
    /// CONTEXT_CHANGES then, its state).
    static CURRENT: RefCell<Option<(usize, u64, Context)>> = const { RefCell::new(None) };
}

/// The state of the context current on this thread (checked against EGL each
/// time, so a MakeCurrent from Java is seen too).
fn current() -> Context {
    let host = match CURRENT_CONTEXT.get() {
        Some(source) => source(),
        None => crate::imports::egl::current_context(),
    };
    let changes = CONTEXT_CHANGES.load(Ordering::Acquire);
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        if let Some((context, seen, state)) = current.as_ref() {
            if *context == host && *seen == changes {
                return state.clone();
            }
        }
        let state = {
            let mut contexts = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
            contexts.get_or_insert_with(HashMap::new).entry(host).or_insert_with(new_context).clone()
        };
        *current = Some((host, changes, state.clone()));
        state
    })
}

fn with_bindings<R>(work: impl FnOnce(&mut Bindings) -> R) -> R {
    let context = current();
    let mut bindings = context.bindings.lock().unwrap_or_else(|e| e.into_inner());
    work(&mut bindings)
}

fn with_group<R>(work: impl FnOnce(&mut ShareGroup) -> R) -> R {
    let context = current();
    let mut group = context.group.lock().unwrap_or_else(|e| e.into_inner());
    work(&mut group)
}

/// A guest copy standing in for mapped buffer memory.
struct Mapped {
    target: u32,
    copy: u32,
    offset: u32,
    length: u32,
    /// Upload the whole copy on unmap (else only flushed ranges were uploaded).
    upload_on_unmap: bool,
}

fn bound(target: u32) -> u32 {
    with_bindings(|b| b.buffers.get(&target).copied().unwrap_or(0))
}

/// The buffer tracked as bound to `target` in the context current on this
/// thread (for tests and diagnostics).
pub fn bound_buffer(target: u32) -> u32 {
    bound(target)
}

/// The buffer the context has bound to `target` right now (asked from GL, as
/// glBindBufferBase/Range binds aren't tracked here).
fn buffer_bound_to(target: u32) -> u32 {
    let binding = match target {
        0x8892 => 0x8894, // array
        0x8893 => 0x8895, // element array
        0x88EB => 0x88ED, // pixel pack
        0x88EC => 0x88EF, // pixel unpack
        0x8A11 => 0x8A28, // uniform
        0x8C8E => 0x8C8F, // transform feedback
        0x90D2 => 0x90D3, // shader storage
        0x8F3F => 0x8F43, // draw indirect
        0x90EE => 0x90EF, // dispatch indirect
        0x92C0 => 0x92C1, // atomic counter
        other => other,   // copy read/write and texture buffer use their own name
    };
    let mut buffer = 0i32;
    unsafe { host!("glGetIntegerv", fn(u32, *mut i32))(binding, &mut buffer) };
    buffer as u32
}

/// `pointer` as a buffer offset if `buffer` is bound, else as a guest address.
fn offset_or_guest(buffer: u32, pointer: u32) -> *mut c_void {
    if buffer != 0 {
        pointer as usize as *mut c_void
    } else {
        guest_pointer(pointer)
    }
}

/// The guest's view of a pointer GL hands back: guest memory as a guest
/// address, a buffer offset as itself, anything else as NULL.
fn guest_pointer_or_offset(pointer: *mut c_void) -> u32 {
    let in_guest = (pointer as usize).wrapping_sub(mem().host(0) as usize);
    if !pointer.is_null() && in_guest < (1usize << 32) {
        in_guest as u32
    } else if (pointer as usize) < (1usize << 32) {
        pointer as usize as u32
    } else {
        0
    }
}

/// A vertex array pointer.
pub fn array_pointer(pointer: u32) -> *mut c_void {
    offset_or_guest(bound(GL_ARRAY_BUFFER), pointer)
}

/// The indices of an indexed draw.
pub fn index_pointer(pointer: u32) -> *mut c_void {
    offset_or_guest(element_buffer(), pointer)
}

/// The command of an indirect draw.
pub fn indirect_pointer(pointer: u32) -> *mut c_void {
    offset_or_guest(bound(GL_DRAW_INDIRECT_BUFFER), pointer)
}

/// Pixels read from (texture uploads).
pub fn unpack_pointer(pointer: u32) -> *mut c_void {
    offset_or_guest(bound(GL_PIXEL_UNPACK_BUFFER), pointer)
}

/// Pixels written to (glReadPixels).
pub fn pack_pointer(pointer: u32) -> *mut c_void {
    offset_or_guest(bound(GL_PIXEL_PACK_BUFFER), pointer)
}

fn element_buffer() -> u32 {
    with_bindings(|b| b.element_buffers.get(&b.vertex_array).copied().unwrap_or(0))
}

fn bind_vertex_array(array: u32) {
    with_bindings(|b| b.vertex_array = array);
}

fn forget_vertex_arrays(count: i32, arrays: u32) {
    with_bindings(|b| {
        for i in 0..count.max(0) as u32 {
            let array = mem().read_u32(arrays + i * 4);
            b.element_buffers.remove(&array);
            if b.vertex_array == array {
                b.vertex_array = 0;
            }
        }
    });
}

/// GLsync handles (shared: a fence made on one context can be waited on in another).
#[derive(Default)]
struct Syncs {
    objects: HashMap<u32, usize>,
    next: u32,
}

static SYNCS: Mutex<Option<Syncs>> = Mutex::new(None);

pub fn sync_host(handle: u32) -> *mut c_void {
    let syncs = SYNCS.lock().unwrap();
    syncs.as_ref().and_then(|s| s.objects.get(&handle).copied()).unwrap_or(0) as *mut c_void
}

/// Copies guest string pointers into a host pointer array.
fn string_array(strings: u32, count: i32) -> Vec<*mut c_void> {
    (0..count.max(0) as u32).map(|i| guest_pointer(mem().read_u32(strings + i * 4))).collect()
}

/// Maps `length` bytes at `offset` into a fresh guest copy. Unless the game
/// said it will overwrite the range, the copy starts with the current contents,
/// because the copy is uploaded whole.
fn map_into_copy(target: u32, offset: u32, length: u32, access: u32) -> u32 {
    let copy = crate::runtime::heap().malloc(length.max(1));
    if access & (GL_MAP_INVALIDATE_RANGE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT) == 0 {
        let current = unsafe {
            host!("glMapBufferRange", fn(u32, isize, isize, u32) -> *mut c_void)(
                target,
                offset as isize,
                length as isize,
                GL_MAP_READ_BIT,
            )
        };
        if current.is_null() {
            // A GLES2-only context can't read buffers back; drop the error that left.
            unsafe {
                host!("glGetError", fn() -> u32)();
                std::ptr::write_bytes(guest_pointer(copy) as *mut u8, 0, length as usize);
            }
        } else {
            unsafe {
                std::ptr::copy_nonoverlapping(current as *const u8, guest_pointer(copy) as *mut u8, length as usize);
                host!("glUnmapBuffer", fn(u32) -> u8)(target);
            }
        }
    }
    copy
}

/// Records a new mapping of the buffer bound to its target.
fn remember_mapping(mapped: Mapped) {
    static OVERLAPS_REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let buffer = buffer_bound_to(mapped.target);
    with_group(|b| {
        let others = b.mapped.iter().filter(|(&other, m)| other != buffer && m.target == mapped.target).count();
        if others > 0 && OVERLAPS_REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
            crate::log::line(&format!(
                "[gles] buffer {buffer} mapped on target {:#x} while {others} other buffer(s) there are still mapped",
                mapped.target
            ));
        }
        // Mapping a buffer that is already mapped is a GL error; the old copy
        // may still be in use, so it is kept (leaked) rather than freed.
        b.mapped.insert(buffer, mapped);
    });
}

fn upload(target: u32, offset: u32, length: u32, data: u32) {
    unsafe {
        host!("glBufferSubData", fn(u32, isize, isize, *mut c_void))(
            target,
            offset as isize,
            length as isize,
            guest_pointer(data),
        )
    };
}

fn unmap(target: u32) -> u32 {
    let buffer = buffer_bound_to(target);
    let Some(mapped) = with_group(|b| b.mapped.remove(&buffer)) else {
        return 0;
    };
    if mapped.upload_on_unmap {
        upload(target, mapped.offset, mapped.length, mapped.copy);
    }
    crate::runtime::heap().free(mapped.copy);
    1
}

/// For debug.arm32bridge.glcheck=1: logs a GL error right after the call that
/// made it, with the call's first arguments (the first 300 errors).
pub fn check_error(name: &str, registers: &[u32]) {
    // A shader the driver refuses doesn't raise a GL error; it just draws nothing.
    if name == "glCompileShader" || name == "glLinkProgram" {
        let object = registers[0];
        let (status_name, log_name) = if name == "glCompileShader" { (0x8B81, 0x8B84) } else { (0x8B82, 0x8B84) };
        let mut status = 1i32;
        let mut length = 0i32;
        unsafe {
            if name == "glCompileShader" {
                host!("glGetShaderiv", fn(u32, u32, *mut i32))(object, status_name, &mut status);
                host!("glGetShaderiv", fn(u32, u32, *mut i32))(object, log_name, &mut length);
            } else {
                host!("glGetProgramiv", fn(u32, u32, *mut i32))(object, status_name, &mut status);
                host!("glGetProgramiv", fn(u32, u32, *mut i32))(object, log_name, &mut length);
            }
        }
        if status == 0 {
            let mut text = vec![0u8; length.clamp(1, 4096) as usize];
            unsafe {
                if name == "glCompileShader" {
                    host!("glGetShaderInfoLog", fn(u32, i32, *mut i32, *mut u8))(object, text.len() as i32, std::ptr::null_mut(), text.as_mut_ptr());
                } else {
                    host!("glGetProgramInfoLog", fn(u32, i32, *mut i32, *mut u8))(object, text.len() as i32, std::ptr::null_mut(), text.as_mut_ptr());
                }
            }
            let text = String::from_utf8_lossy(&text).trim_end_matches('\0').trim().to_string();
            crate::log::line(&format!("[gl error] {name}({object}) failed: {text}"));
        }
    }
    let error = unsafe { host!("glGetError", fn() -> u32)() };
    if error == 0 {
        return;
    }
    static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if REPORTED.fetch_add(1, Ordering::Relaxed) < 300 {
        let shown: Vec<String> = registers.iter().take(4).map(|r| format!("{r:#x}")).collect();
        crate::log::line(&format!("[gl error] {error:#x} after {name}({})", shown.join(", ")));
    }
}

/// Whether the driver takes S3TC `format` itself; if not, the bridge decodes
/// it to RGBA8 (see `s3tc`). Asked once a context is current.
fn driver_takes(format: u32) -> bool {
    const ASKED: u8 = 1;
    const S3TC: u8 = 2;
    const DXT1: u8 = 4;
    const SRGB: u8 = 8;
    static SUPPORT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let mut support = SUPPORT.load(Ordering::Relaxed);
    if support & ASKED == 0 {
        let text = unsafe { host!("glGetString", fn(u32) -> *const libc::c_char)(0x1F03) }; // GL_EXTENSIONS
        if text.is_null() {
            return true;
        }
        let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
        let has = |name: &str| text.split_whitespace().any(|extension| extension == name);
        support = ASKED;
        if has("GL_EXT_texture_compression_s3tc") || has("GL_NV_texture_compression_s3tc") {
            support |= S3TC;
        }
        if has("GL_EXT_texture_compression_dxt1") {
            support |= DXT1;
        }
        if has("GL_EXT_texture_compression_s3tc_srgb") || has("GL_NV_sRGB_formats") {
            support |= SRGB;
        }
        SUPPORT.store(support, Ordering::Relaxed);
        if support & S3TC == 0 {
            crate::log::line("[gles] driver lacks S3TC (DXT3/DXT5) textures: the bridge decodes them to RGBA8");
        }
    }
    if s3tc::is_srgb(format) {
        support & SRGB != 0
    } else if matches!(format, s3tc::DXT1_RGB | s3tc::DXT1_RGBA) {
        support & (S3TC | DXT1) != 0
    } else {
        support & S3TC != 0
    }
}

/// An uncompressed format to store S3TC `format` as.
fn decoded_format(format: u32) -> u32 {
    if s3tc::is_srgb(format) { 0x8C43 } else { 0x8058 } // GL_SRGB8_ALPHA8 / GL_RGBA8
}

/// S3TC data from the game that the driver can't take, decoded to RGBA8.
/// None: pass the call through unchanged (the driver takes the format, the
/// data sits in a pixel unpack buffer, or it is short).
fn decode_upload(format: u32, width: i32, height: i32, size: i32, data: u32) -> Option<Vec<u8>> {
    if !s3tc::is_s3tc(format) || driver_takes(format) || bound(GL_PIXEL_UNPACK_BUFFER) != 0 || width <= 0 || height <= 0 {
        return None;
    }
    let pointer = guest_pointer(data) as *const u8;
    if pointer.is_null() {
        return None;
    }
    let length = (size.max(0) as usize).min(s3tc::data_size(format, width as u32, height as u32));
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    s3tc::decode(format, bytes, width as u32, height as u32)
}

// ---- 2x pictures (texture_pack.rs) ----

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;

fn integer(name: u32) -> i32 {
    let mut value = 0i32;
    unsafe { host!("glGetIntegerv", fn(u32, *mut i32))(name, &mut value) };
    value
}

/// GL's largest texture side, asked once.
fn max_texture_size() -> u32 {
    static SIZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let known = SIZE.load(Ordering::Relaxed);
    if known != 0 {
        return known;
    }
    let size = integer(GL_MAX_TEXTURE_SIZE).max(0) as u32;
    SIZE.store(size, Ordering::Relaxed);
    size
}

/// Whether an upload to `target` might go through the picture pack (not when
/// its pixels come from a pixel unpack buffer: those are offsets, not game memory).
fn pack_upload(target: u32) -> bool {
    target == GL_TEXTURE_2D && crate::texture_pack::active() && bound(GL_PIXEL_UNPACK_BUFFER) == 0
}

/// Runs an upload of the bridge's own pixels (rows packed tight), keeping the game's row alignment.
fn upload_packed(upload: impl FnOnce()) {
    let alignment = integer(GL_UNPACK_ALIGNMENT);
    if alignment != 1 {
        unsafe { host!("glPixelStorei", fn(u32, i32))(GL_UNPACK_ALIGNMENT, 1) };
    }
    upload();
    if alignment != 1 {
        unsafe { host!("glPixelStorei", fn(u32, i32))(GL_UNPACK_ALIGNMENT, alignment) };
    }
}

#[allow(clippy::too_many_arguments)]
fn tex_image_2d(target: u32, level: i32, internal: i32, width: i32, height: i32, border: i32, format: u32, kind: u32, data: u32) {
    use crate::texture_pack::{Call, Image, GL_UNSIGNED_BYTE};
    let image = |internal: i32, width: i32, height: i32, format: u32, kind: u32, pixels: *const c_void| unsafe {
        host!("glTexImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void))(
            target, level, internal, width, height, border, format, kind, pixels,
        )
    };
    if !pack_upload(target) {
        return image(internal, width, height, format, kind, unpack_pointer(data));
    }
    let texture = integer(GL_TEXTURE_BINDING_2D) as u32;
    let call = Call { level, width, height, format, kind, data, ..Call::default() };
    match crate::texture_pack::image(texture, call, max_texture_size()) {
        Image::AsAsked => image(internal, width, height, format, kind, unpack_pointer(data)),
        // Made from zeros, it stays zeros (GL leaves a texture made from nothing undefined).
        Image::Allocate { width, height, format } if data != 0 => {
            let zeros = vec![0u8; (width * height * crate::texture_pack::channels(format).unwrap_or(4)) as usize];
            upload_packed(|| image(format as i32, width as i32, height as i32, format, GL_UNSIGNED_BYTE, zeros.as_ptr() as *const c_void));
        }
        Image::Allocate { width, height, format } => {
            image(format as i32, width as i32, height as i32, format, GL_UNSIGNED_BYTE, std::ptr::null())
        }
        Image::Replace { path, width: w, height: h, channels } => match crate::texture_pack::picture(&path, false) {
            Some(pixels) => {
                let format = if channels == 4 { crate::texture_pack::GL_RGBA } else { crate::texture_pack::GL_RGB };
                upload_packed(|| {
                    image(format as i32, w as i32 * 2, h as i32 * 2, format, GL_UNSIGNED_BYTE, pixels.as_ptr() as *const c_void)
                });
            }
            None => {
                crate::texture_pack::forget(texture);
                image(internal, width, height, format, kind, unpack_pointer(data));
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn tex_sub_image_2d(target: u32, level: i32, x: i32, y: i32, width: i32, height: i32, format: u32, kind: u32, data: u32) {
    use crate::texture_pack::{pixel_size, row_stride, Call, SubImage};
    let pass = || unsafe {
        host!("glTexSubImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *mut c_void))(
            target, level, x, y, width, height, format, kind, unpack_pointer(data),
        )
    };
    if !pack_upload(target) {
        return pass();
    }
    let texture = integer(GL_TEXTURE_BINDING_2D) as u32;
    let alignment = integer(GL_UNPACK_ALIGNMENT).max(1) as u32;
    let stride = pixel_size(format, kind).map_or(0, |size| row_stride(width.max(0) as u32, size, alignment));
    let call = Call { level, x, y, width, height, format, kind, data };
    match crate::texture_pack::sub_image(texture, call, stride) {
        SubImage::AsAsked => pass(),
        SubImage::Upscale { x, y } => upscale_piece(target, level, x, y, call, stride),
        SubImage::Rows { path, width: w, channels, first_row, rows, x, y, last } => {
            let row_bytes = (w * 2 * channels as u32) as usize;
            let (start, end) = (first_row as usize * row_bytes, (first_row + rows) as usize * row_bytes);
            match crate::texture_pack::picture(&path, !last) {
                Some(pixels) if end <= pixels.len() => upload_packed(|| unsafe {
                    let format = if channels == 4 { crate::texture_pack::GL_RGBA } else { crate::texture_pack::GL_RGB };
                    host!("glTexSubImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const u8))(
                        target, level, x, y, w as i32 * 2, rows as i32, format, crate::texture_pack::GL_UNSIGNED_BYTE, pixels[start..].as_ptr(),
                    )
                }),
                _ => upscale_piece(target, level, x, y, call, stride),
            }
        }
    }
}

/// A game piece put into a 2x texture, made 2x on the CPU and put at (x, y) (already doubled).
fn upscale_piece(target: u32, level: i32, x: i32, y: i32, call: crate::texture_pack::Call, stride: u32) {
    use crate::texture_pack::{channels, pixel_size, to_bytes, upscale, GL_UNSIGNED_BYTE};
    let (width, height) = (call.width as u32, call.height as u32);
    let (Some(size), Some(count)) = (pixel_size(call.format, call.kind), channels(call.format)) else { return };
    let length = stride as usize * (height as usize - 1) + (width * size) as usize;
    let Some(pixels) = to_bytes(mem().bytes(call.data, length), width, height, stride, call.format, call.kind) else { return };
    let doubled = upscale(&pixels, width, height, count);
    upload_packed(|| unsafe {
        host!("glTexSubImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const u8))(
            target, level, x, y, width as i32 * 2, height as i32 * 2, call.format, GL_UNSIGNED_BYTE, doubled.as_ptr(),
        )
    });
}

pub fn handler(name: &str) -> Option<Handler> {
    if let Some(found) = special(name) {
        return Some(found);
    }
    super::gles_generated::handler(name)
}

fn special(name: &str) -> Option<Handler> {
    match name {
        "glBindBuffer" => handler!(|uc, a| {
            let (target, buffer) = (a.u32(), a.u32());
            with_bindings(|b| {
                if target == GL_ELEMENT_ARRAY_BUFFER {
                    let vertex_array = b.vertex_array;
                    b.element_buffers.insert(vertex_array, buffer);
                } else {
                    b.buffers.insert(target, buffer);
                }
            });
            unsafe { host!("glBindBuffer", fn(u32, u32))(target, buffer) };
            Ret::Void
        }),
        "glDeleteBuffers" => handler!(|uc, a| {
            let (count, buffers) = (a.i32(), a.u32());
            let names: Vec<u32> = (0..count.max(0) as u32).map(|i| mem().read_u32(buffers + i * 4)).collect();
            with_group(|group| {
                for buffer in &names {
                    // Deleting a mapped buffer unmaps it; its copy is no longer valid.
                    if let Some(mapped) = group.mapped.remove(buffer) {
                        crate::runtime::heap().free(mapped.copy);
                    }
                }
            });
            with_bindings(|b| {
                for &buffer in &names {
                    for bound in b.buffers.values_mut().chain(b.element_buffers.values_mut()) {
                        if *bound == buffer {
                            *bound = 0;
                        }
                    }
                }
            });
            unsafe { host!("glDeleteBuffers", fn(i32, *mut c_void))(count, guest_pointer(buffers)) };
            Ret::Void
        }),
        "glBindVertexArrayOES" => handler!(|uc, a| {
            let array = a.u32();
            bind_vertex_array(array);
            unsafe { host!("glBindVertexArrayOES", fn(u32))(array) };
            Ret::Void
        }),
        "glBindVertexArray" => handler!(|uc, a| {
            let array = a.u32();
            bind_vertex_array(array);
            unsafe { host!("glBindVertexArray", fn(u32))(array) };
            Ret::Void
        }),
        "glDeleteVertexArraysOES" => handler!(|uc, a| {
            let (count, arrays) = (a.i32(), a.u32());
            forget_vertex_arrays(count, arrays);
            unsafe { host!("glDeleteVertexArraysOES", fn(i32, *mut c_void))(count, guest_pointer(arrays)) };
            Ret::Void
        }),
        "glDeleteVertexArrays" => handler!(|uc, a| {
            let (count, arrays) = (a.i32(), a.u32());
            forget_vertex_arrays(count, arrays);
            unsafe { host!("glDeleteVertexArrays", fn(i32, *mut c_void))(count, guest_pointer(arrays)) };
            Ret::Void
        }),
        "glVertexAttribPointer" => handler!(|uc, a| {
            let (index, size, kind, normalized, stride, pointer) =
                (a.u32(), a.i32(), a.u32(), a.u32() as u8, a.i32(), a.u32());
            let host_pointer = array_pointer(pointer);
            unsafe {
                host!("glVertexAttribPointer", fn(u32, i32, u32, u8, i32, *mut c_void))(
                    index,
                    size,
                    kind,
                    normalized,
                    stride,
                    host_pointer,
                )
            };
            Ret::Void
        }),
        // GLES1 arrays: (size, type, stride, pointer) or (type, stride, pointer).
        "glVertexPointer" => handler!(|uc, a| {
            let (size, kind, stride, pointer) = (a.i32(), a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glVertexPointer", fn(i32, u32, i32, *mut c_void))(size, kind, stride, pointer) };
            Ret::Void
        }),
        "glColorPointer" => handler!(|uc, a| {
            let (size, kind, stride, pointer) = (a.i32(), a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glColorPointer", fn(i32, u32, i32, *mut c_void))(size, kind, stride, pointer) };
            Ret::Void
        }),
        "glTexCoordPointer" => handler!(|uc, a| {
            let (size, kind, stride, pointer) = (a.i32(), a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glTexCoordPointer", fn(i32, u32, i32, *mut c_void))(size, kind, stride, pointer) };
            Ret::Void
        }),
        "glMatrixIndexPointerOES" => handler!(|uc, a| {
            let (size, kind, stride, pointer) = (a.i32(), a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glMatrixIndexPointerOES", fn(i32, u32, i32, *mut c_void))(size, kind, stride, pointer) };
            Ret::Void
        }),
        "glWeightPointerOES" => handler!(|uc, a| {
            let (size, kind, stride, pointer) = (a.i32(), a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glWeightPointerOES", fn(i32, u32, i32, *mut c_void))(size, kind, stride, pointer) };
            Ret::Void
        }),
        "glNormalPointer" => handler!(|uc, a| {
            let (kind, stride, pointer) = (a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glNormalPointer", fn(u32, i32, *mut c_void))(kind, stride, pointer) };
            Ret::Void
        }),
        "glPointSizePointerOES" => handler!(|uc, a| {
            let (kind, stride, pointer) = (a.u32(), a.i32(), array_pointer(a.u32()));
            unsafe { host!("glPointSizePointerOES", fn(u32, i32, *mut c_void))(kind, stride, pointer) };
            Ret::Void
        }),
        "glDrawElements" => handler!(|uc, a| {
            let (mode, count, kind, indices) = (a.u32(), a.i32(), a.u32(), a.u32());
            let host_indices = index_pointer(indices);
            unsafe { host!("glDrawElements", fn(u32, i32, u32, *mut c_void))(mode, count, kind, host_indices) };
            Ret::Void
        }),
        "glCompressedTexImage2D" => handler!(|uc, a| {
            let (target, level, format, width, height, border, size, data) =
                (a.u32(), a.i32(), a.u32(), a.i32(), a.i32(), a.i32(), a.i32(), a.u32());
            unsafe {
                match decode_upload(format, width, height, size, data) {
                    // Unsized GL_RGBA works on GLES2 and 3 contexts; sRGB needs the sized one.
                    Some(pixels) => host!("glTexImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const u8))(
                        target,
                        level,
                        if s3tc::is_srgb(format) { 0x8C43 } else { 0x1908 } as i32,
                        width,
                        height,
                        border,
                        0x1908,
                        0x1401,
                        pixels.as_ptr(),
                    ),
                    None => host!("glCompressedTexImage2D", fn(u32, i32, u32, i32, i32, i32, i32, *mut c_void))(
                        target, level, format, width, height, border, size, unpack_pointer(data),
                    ),
                }
            }
            Ret::Void
        }),
        "glCompressedTexSubImage2D" => handler!(|uc, a| {
            let (target, level, x, y, width, height, format, size, data) =
                (a.u32(), a.i32(), a.i32(), a.i32(), a.i32(), a.i32(), a.u32(), a.i32(), a.u32());
            unsafe {
                match decode_upload(format, width, height, size, data) {
                    Some(pixels) => host!("glTexSubImage2D", fn(u32, i32, i32, i32, i32, i32, u32, u32, *const u8))(
                        target, level, x, y, width, height, 0x1908, 0x1401, pixels.as_ptr(),
                    ),
                    None => host!("glCompressedTexSubImage2D", fn(u32, i32, i32, i32, i32, i32, u32, i32, *mut c_void))(
                        target, level, x, y, width, height, format, size, unpack_pointer(data),
                    ),
                }
            }
            Ret::Void
        }),
        // Through the picture pack (texture_pack.rs) when the game has one.
        "glTexImage2D" => handler!(|uc, a| {
            let (target, level, internal, width, height, border, format, kind, data) =
                (a.u32(), a.i32(), a.i32(), a.i32(), a.i32(), a.i32(), a.u32(), a.u32(), a.u32());
            tex_image_2d(target, level, internal, width, height, border, format, kind, data);
            Ret::Void
        }),
        "glTexSubImage2D" => handler!(|uc, a| {
            let (target, level, x, y, width, height, format, kind, data) =
                (a.u32(), a.i32(), a.i32(), a.i32(), a.i32(), a.i32(), a.u32(), a.u32(), a.u32());
            tex_sub_image_2d(target, level, x, y, width, height, format, kind, data);
            Ret::Void
        }),
        // A copy of the screen is 1x.
        "glCopyTexImage2D" => handler!(|uc, a| {
            let (target, level, internal, x, y, width, height, border) =
                (a.u32(), a.i32(), a.u32(), a.i32(), a.i32(), a.i32(), a.i32(), a.i32());
            if level == 0 && target == GL_TEXTURE_2D && crate::texture_pack::active() {
                crate::texture_pack::forget(integer(GL_TEXTURE_BINDING_2D) as u32);
            }
            unsafe {
                host!("glCopyTexImage2D", fn(u32, i32, u32, i32, i32, i32, i32, i32))(target, level, internal, x, y, width, height, border)
            };
            Ret::Void
        }),
        "glDeleteTextures" => handler!(|uc, a| {
            let (count, textures) = (a.i32(), a.u32());
            if crate::texture_pack::active() {
                for i in 0..count.max(0) as u32 {
                    crate::texture_pack::forget(mem().read_u32(textures + i * 4));
                }
            }
            unsafe { host!("glDeleteTextures", fn(i32, *mut c_void))(count, guest_pointer(textures)) };
            Ret::Void
        }),
        "glTexStorage2D" | "glTexStorage2DEXT" => {
            fn storage(name: &'static str, a: &mut crate::runtime::Args) {
                let (target, levels, mut format, width, height) = (a.u32(), a.i32(), a.u32(), a.i32(), a.i32());
                if s3tc::is_s3tc(format) && !driver_takes(format) {
                    format = decoded_format(format);
                }
                unsafe {
                    if name == "glTexStorage2D" {
                        host!("glTexStorage2D", fn(u32, i32, u32, i32, i32))(target, levels, format, width, height)
                    } else {
                        host!("glTexStorage2DEXT", fn(u32, i32, u32, i32, i32))(target, levels, format, width, height)
                    }
                }
            }
            if name == "glTexStorage2D" {
                handler!(|uc, a| { storage("glTexStorage2D", a); Ret::Void })
            } else {
                handler!(|uc, a| { storage("glTexStorage2DEXT", a); Ret::Void })
            }
        }
        "glShaderSource" => handler!(|uc, a| {
            let (shader, count, strings, lengths) = (a.u32(), a.i32(), a.u32(), a.u32());
            let pointers = string_array(strings, count);
            unsafe {
                host!("glShaderSource", fn(u32, i32, *const *mut c_void, *mut c_void))(
                    shader,
                    count,
                    pointers.as_ptr(),
                    guest_pointer(lengths),
                )
            };
            Ret::Void
        }),
        // One guest copy per text: contexts of different kinds (OpenGL ES 1
        // and 2 in one game) give different versions and extension lists.
        "glGetString" => handler!(|uc, a| {
            let which = a.u32();
            let text = unsafe { host!("glGetString", fn(u32) -> *const libc::c_char)(which) };
            if text.is_null() {
                return Ret::U32(0);
            }
            let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned();
            let key = format!("glGetString:{which:#x}:{text}");
            if let Some(&address) = shared().state.data_imports.get(&key) {
                return Ret::U32(address);
            }
            Ret::U32(shared().cached_cstr(key, &text))
        }),
        "glMapBufferOES" => handler!(|uc, a| {
            let (target, _access) = (a.u32(), a.u32());
            let mut size = 0i32;
            unsafe { host!("glGetBufferParameteriv", fn(u32, u32, *mut i32))(target, GL_BUFFER_SIZE, &mut size) };
            if size <= 0 {
                return Ret::U32(0);
            }
            let copy = map_into_copy(target, 0, size as u32, 0);
            remember_mapping(Mapped { target, copy, offset: 0, length: size as u32, upload_on_unmap: true });
            Ret::U32(copy)
        }),
        "glMapBufferRange" => handler!(|uc, a| {
            let (target, offset, length, access) = (a.u32(), a.u32(), a.u32(), a.u32());
            if length == 0 {
                return Ret::U32(0);
            }
            let copy = map_into_copy(target, offset, length, access);
            let upload_on_unmap = access & GL_MAP_WRITE_BIT != 0 && access & GL_MAP_FLUSH_EXPLICIT_BIT == 0;
            if access & 0x40 != 0 {
                static PERSISTENT_REPORTED: std::sync::Once = std::sync::Once::new();
                PERSISTENT_REPORTED.call_once(|| {
                    crate::log::line("[gles] game maps buffers persistently; writes reach GL only on flush or unmap")
                });
            }
            remember_mapping(Mapped { target, copy, offset, length, upload_on_unmap });
            Ret::U32(copy)
        }),
        "glFlushMappedBufferRange" => handler!(|uc, a| {
            let (target, offset, length) = (a.u32(), a.u32(), a.u32());
            let buffer = buffer_bound_to(target);
            let found = with_group(|b| b.mapped.get(&buffer).map(|m| (m.copy, m.offset)));
            if let Some((copy, start)) = found {
                upload(target, start + offset, length, copy + offset);
            }
            Ret::Void
        }),
        "glUnmapBufferOES" => handler!(|uc, a| Ret::U32(unmap(a.u32()))),
        "glUnmapBuffer" => handler!(|uc, a| Ret::U32(unmap(a.u32()))),
        "glGetStringi" => handler!(|uc, a| {
            let (which, index) = (a.u32(), a.u32());
            let key = format!("glGetStringi:{which:#x}:{index}");
            if let Some(&address) = shared().state.data_imports.get(&key) {
                return Ret::U32(address);
            }
            let text = unsafe { host!("glGetStringi", fn(u32, u32) -> *const libc::c_char)(which, index) };
            if text.is_null() {
                return Ret::U32(0);
            }
            let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned();
            Ret::U32(shared().cached_cstr(key, &text))
        }),
        "glTransformFeedbackVaryings" => handler!(|uc, a| {
            let (program, count, varyings, mode) = (a.u32(), a.i32(), a.u32(), a.u32());
            let pointers = string_array(varyings, count);
            unsafe {
                host!("glTransformFeedbackVaryings", fn(u32, i32, *const *mut c_void, u32))(
                    program,
                    count,
                    pointers.as_ptr(),
                    mode,
                )
            };
            Ret::Void
        }),
        "glGetUniformIndices" => handler!(|uc, a| {
            let (program, count, names, indices) = (a.u32(), a.i32(), a.u32(), a.u32());
            let pointers = string_array(names, count);
            unsafe {
                host!("glGetUniformIndices", fn(u32, i32, *const *mut c_void, *mut c_void))(
                    program,
                    count,
                    pointers.as_ptr(),
                    guest_pointer(indices),
                )
            };
            Ret::Void
        }),
        "glCreateShaderProgramv" => handler!(|uc, a| {
            let (kind, count, strings) = (a.u32(), a.i32(), a.u32());
            let pointers = string_array(strings, count);
            Ret::U32(unsafe {
                host!("glCreateShaderProgramv", fn(u32, i32, *const *mut c_void) -> u32)(kind, count, pointers.as_ptr())
            })
        }),
        "glFenceSync" => handler!(|uc, a| {
            let (condition, flags) = (a.u32(), a.u32());
            let sync = unsafe { host!("glFenceSync", fn(u32, u32) -> *mut c_void)(condition, flags) };
            if sync.is_null() {
                return Ret::U32(0);
            }
            let mut syncs = SYNCS.lock().unwrap();
            let syncs = syncs.get_or_insert_with(Syncs::default);
            syncs.next = syncs.next.wrapping_add(1).max(1);
            syncs.objects.insert(syncs.next, sync as usize);
            Ret::U32(syncs.next)
        }),
        "glDeleteSync" => handler!(|uc, a| {
            let handle = a.u32();
            let sync = SYNCS.lock().unwrap().as_mut().and_then(|s| s.objects.remove(&handle));
            if let Some(sync) = sync {
                unsafe { host!("glDeleteSync", fn(*mut c_void))(sync as *mut c_void) };
            }
            Ret::Void
        }),
        // The callback is guest code; the driver would call it as host code.
        "glDebugMessageCallback" => handler!(|uc, a| {
            crate::log::line("[bridge] the game set a GL debug callback; ignored");
            Ret::Void
        }),
        // The array pointers the game set: a guest pointer (it was made a host
        // one) or a buffer offset. Anything else (the debug callback, which is
        // never passed on) reads as NULL.
        "glGetPointerv" => handler!(|uc, a| {
            let (name, out) = (a.u32(), a.u32());
            let mut pointer: *mut c_void = std::ptr::null_mut();
            unsafe { host!("glGetPointerv", fn(u32, *mut *mut c_void))(name, &mut pointer) };
            if out != 0 {
                mem().write_u32(out, guest_pointer_or_offset(pointer));
            }
            Ret::Void
        }),
        // The indices argument is a list of 32-bit guest pointers (or offsets
        // into the bound element buffer); the 64-bit driver reads 8-byte ones.
        "glMultiDrawElementsEXT" => handler!(|uc, a| {
            let (mode, counts, kind, indices, draw_count) = (a.u32(), a.u32(), a.u32(), a.u32(), a.i32());
            let has_buffer = element_buffer() != 0;
            let host_indices: Vec<*mut c_void> = (0..draw_count.max(0) as u32)
                .map(|i| {
                    let entry = mem().read_u32(indices + i * 4);
                    if has_buffer { entry as usize as *mut c_void } else { guest_pointer(entry) }
                })
                .collect();
            unsafe {
                host!("glMultiDrawElementsEXT", fn(u32, *mut c_void, u32, *const *mut c_void, i32))(
                    mode,
                    guest_pointer(counts),
                    kind,
                    host_indices.as_ptr(),
                    draw_count,
                )
            };
            Ret::Void
        }),
        // Driver memory sits outside the guest's 4 GiB, so the game can't use it.
        "glExtGetBufferPointervQCOM" => handler!(|uc, a| {
            crate::log::line("[bridge] the game asked GL for a pointer; returning NULL");
            let (_target, out) = (a.u32(), a.u32());
            if out != 0 {
                mem().write_u32(out, 0);
            }
            Ret::Void
        }),
        "glGetVertexAttribPointerv" | "glGetBufferPointervOES" | "glGetBufferPointerv" => handler!(|uc, a| {
            crate::log::line("[bridge] the game asked GL for a pointer; returning NULL");
            let (_x, _y, out) = (a.u32(), a.u32(), a.u32());
            if out != 0 {
                mem().write_u32(out, 0);
            }
            Ret::Void
        }),
        _ => None,
    }
}
