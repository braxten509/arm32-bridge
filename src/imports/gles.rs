//! OpenGL ES 2 for guest code. Most functions are generated from the NDK
//! headers (gles_generated.rs); the ones here need special care:
//! - glVertexAttribPointer / glDrawElements take either a guest pointer
//!   (client-side arrays) or an offset into a bound buffer, depending on GL
//!   state, so buffer bindings are tracked per thread (GL contexts are too).
//! - glMapBufferOES returns driver memory outside the guest's 4 GiB, so the
//!   guest writes into a guest buffer that is uploaded on unmap.
//! - glShaderSource takes an array of pointers; glGetString returns one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::handler;
use crate::runtime::{mem, shared, ArgSource, Handler, Ret};

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
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
        let library = libc::dlopen(c"libGLESv2.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        let library = if library.is_null() { libc::dlopen(c"libGLESv2.so".as_ptr(), libc::RTLD_NOW) } else { library };
        let direct = libc::dlsym(library, symbol.as_ptr());
        if !direct.is_null() {
            return direct as usize;
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
    array_buffer: u32,
    vertex_array: u32,
    /// Element buffer binding per vertex array object (0 = the default one).
    element_buffers: HashMap<u32, u32>,
    /// Mapped buffers: target → (guest copy, size).
    mapped: HashMap<u32, (u32, u32)>,
}

thread_local! {
    static BINDINGS: RefCell<Bindings> = RefCell::new(Bindings::default());
}

fn element_buffer() -> u32 {
    BINDINGS.with(|b| {
        let b = b.borrow();
        b.element_buffers.get(&b.vertex_array).copied().unwrap_or(0)
    })
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
            BINDINGS.with(|b| {
                let mut b = b.borrow_mut();
                if target == GL_ARRAY_BUFFER {
                    b.array_buffer = buffer;
                } else if target == GL_ELEMENT_ARRAY_BUFFER {
                    let vertex_array = b.vertex_array;
                    b.element_buffers.insert(vertex_array, buffer);
                }
            });
            unsafe { host!("glBindBuffer", fn(u32, u32))(target, buffer) };
            Ret::Void
        }),
        "glDeleteBuffers" => handler!(|uc, a| {
            let (count, buffers) = (a.i32(), a.u32());
            BINDINGS.with(|b| {
                let mut b = b.borrow_mut();
                for i in 0..count.max(0) as u32 {
                    let buffer = mem().read_u32(buffers + i * 4);
                    if b.array_buffer == buffer {
                        b.array_buffer = 0;
                    }
                    for bound in b.element_buffers.values_mut() {
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
            BINDINGS.with(|b| b.borrow_mut().vertex_array = array);
            unsafe { host!("glBindVertexArrayOES", fn(u32))(array) };
            Ret::Void
        }),
        "glDeleteVertexArraysOES" => handler!(|uc, a| {
            let (count, arrays) = (a.i32(), a.u32());
            BINDINGS.with(|b| {
                let mut b = b.borrow_mut();
                for i in 0..count.max(0) as u32 {
                    let array = mem().read_u32(arrays + i * 4);
                    b.element_buffers.remove(&array);
                    if b.vertex_array == array {
                        b.vertex_array = 0;
                    }
                }
            });
            unsafe { host!("glDeleteVertexArraysOES", fn(i32, *mut c_void))(count, guest_pointer(arrays)) };
            Ret::Void
        }),
        "glVertexAttribPointer" => handler!(|uc, a| {
            let (index, size, kind, normalized, stride, pointer) =
                (a.u32(), a.i32(), a.u32(), a.u32() as u8, a.i32(), a.u32());
            let bound = BINDINGS.with(|b| b.borrow().array_buffer) != 0;
            let host_pointer = if bound { pointer as usize as *mut c_void } else { guest_pointer(pointer) };
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
        "glDrawElements" => handler!(|uc, a| {
            let (mode, count, kind, indices) = (a.u32(), a.i32(), a.u32(), a.u32());
            let host_indices =
                if element_buffer() != 0 { indices as usize as *mut c_void } else { guest_pointer(indices) };
            unsafe { host!("glDrawElements", fn(u32, i32, u32, *mut c_void))(mode, count, kind, host_indices) };
            Ret::Void
        }),
        "glShaderSource" => handler!(|uc, a| {
            let (shader, count, strings, lengths) = (a.u32(), a.i32(), a.u32(), a.u32());
            let pointers: Vec<*mut c_void> =
                (0..count.max(0) as u32).map(|i| guest_pointer(mem().read_u32(strings + i * 4))).collect();
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
            let address = shared().misc_cstr(&text);
            shared().state.data_imports.insert(key, address);
            Ret::U32(address)
        }),
        "glMapBufferOES" => handler!(|uc, a| {
            let (target, _access) = (a.u32(), a.u32());
            let mut size = 0i32;
            unsafe { host!("glGetBufferParameteriv", fn(u32, u32, *mut i32))(target, GL_BUFFER_SIZE, &mut size) };
            if size <= 0 {
                return Ret::U32(0);
            }
            let copy = shared().heap.malloc(size as u32);
            BINDINGS.with(|b| b.borrow_mut().mapped.insert(target, (copy, size as u32)));
            Ret::U32(copy)
        }),
        "glUnmapBufferOES" => handler!(|uc, a| {
            let target = a.u32();
            let Some((copy, size)) = BINDINGS.with(|b| b.borrow_mut().mapped.remove(&target)) else {
                return Ret::U32(0);
            };
            unsafe {
                host!("glBufferSubData", fn(u32, isize, isize, *mut c_void))(target, 0, size as isize, guest_pointer(copy))
            };
            shared().heap.free(copy);
            Ret::U32(1)
        }),
        "glGetVertexAttribPointerv" | "glGetBufferPointervOES" => handler!(|uc, a| {
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
