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

/// The host GL function `name`, looked up once and cached in `slot`.
pub fn host_function(slot: &AtomicUsize, name: &str) -> usize {
    let cached = slot.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let address = lookup(name);
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
    let host = crate::imports::egl::current_context();
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
        "glGetString" => handler!(|uc, a| {
            let which = a.u32();
            let key = format!("glGetString:{which:#x}");
            if let Some(&address) = shared().state.data_imports.get(&key) {
                return Ret::U32(address);
            }
            let text = unsafe { host!("glGetString", fn(u32) -> *const libc::c_char)(which) };
            if text.is_null() {
                return Ret::U32(0);
            }
            let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned();
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
        "glGetPointerv" => handler!(|uc, a| {
            crate::log::line("[bridge] the game asked GL for a pointer; returning NULL");
            let (_name, out) = (a.u32(), a.u32());
            if out != 0 {
                mem().write_u32(out, 0);
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
