//! OpenGL ES for 32-bit games: the translator's GLES handlers (they turn
//! guest pointers, buffer offsets, mapped buffers and string arrays into
//! the host's) call this layer's GL functions, so Apple's extensions,
//! BGRA uploads, PVRTC decoding, the iPhone extension list and the shader
//! fixes are the same as for 64-bit games.

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
    for name in gles::names() {
        if let Some(handler) = gles::handler(name) {
            super::super::function(name, handler);
        }
    }
}
