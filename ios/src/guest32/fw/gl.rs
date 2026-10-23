//! OpenGL ES for 32-bit games: the translator's GLES handlers (they turn
//! guest pointers, buffer offsets, mapped buffers and string arrays into
//! the host's) call this layer's GL functions, so Apple's extensions,
//! BGRA uploads, PVRTC decoding, the iPhone extension list and the shader
//! fixes are the same as for 64-bit games.

use super::{host_functions, register_all, Ptr, I32, U32};
use arm32_bridge::imports::gles;

/// This layer's function for GL name `name`: its own translation, else the system's.
fn host_gl(name: &str) -> usize {
    match crate::symbols::lookup(name) {
        Some((address, crate::symbols::Kind::Function)) => address,
        _ => crate::fw::gl::core_function(name).unwrap_or(0),
    }
}

pub fn register() {
    gles::set_host_lookup(host_gl);
    // 32-bit EAGL = the iPhone layer's host EAGLContext (ios/src/fw/eagl.rs):
    // tell the translator's GLES handlers which EGL context is current and
    // which sharegroup a new one belongs to, so bindings and mapped buffers
    // are tracked per context and per sharegroup as on the phone.
    gles::set_current_context_source(crate::fw::eagl::current_egl_context);
    crate::fw::eagl::set_context_created_observer(gles::context_created);
    crate::fw::eagl::set_context_destroyed_observer(gles::context_destroyed);
    for name in gles::names() {
        if let Some(handler) = gles::handler(name) {
            super::super::function(name, handler);
        }
    }
    // Every extension function this layer advertises: one the same as a
    // standard function takes that function's guest handler (it converts
    // pointers, syncs and mapped buffers the same way).
    for name in crate::fw::gl::extension_names() {
        if arm32_bridge::ios32::provides_function(name) {
            continue;
        }
        if let Some(handler) = crate::fw::gl::standard_name(name).and_then(gles::handler) {
            super::super::function(name, handler);
        }
    }
    // EXT_debug_label / EXT_debug_marker (no GLES handler: Android lacks them).
    register_all(host_functions! {
        "glLabelObjectEXT" => fn(kind: U32, object: U32, length: I32, label: Ptr);
        "glGetObjectLabelEXT" => fn(kind: U32, object: U32, size: I32, length: Ptr, label: Ptr);
        "glInsertEventMarkerEXT" => fn(length: I32, marker: Ptr);
        "glPushGroupMarkerEXT" => fn(length: I32, marker: Ptr);
        "glPopGroupMarkerEXT" => fn();
    });
}
