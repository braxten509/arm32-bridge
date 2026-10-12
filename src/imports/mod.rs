//! Host implementations of the functions and data a 32-bit Android library
//! imports from the phone (libc, libm, liblog, libdl, JNI, ...).

pub mod jni;
pub mod libc_calls;
pub mod math;
pub mod printf;

use std::collections::HashMap;

use crate::runtime::{Handler, Uc};

/// A guest FILE* maps to one of these.
#[derive(Clone, Copy)]
pub enum HostFile {
    /// stdin/stdout/stderr: written straight to the file descriptor.
    Descriptor(i32),
    Stream(usize),
}

#[derive(Default)]
pub struct State {
    pub files: HashMap<u32, HostFile>,
    pub directories: HashMap<u32, (usize, u32)>,
    pub thread_keys: Vec<Option<u32>>,
    pub at_exit: Vec<(u32, u32)>,
    pub strtok_next: usize,
    pub static_tm: u32,
    pub static_text: u32,
    pub data_imports: HashMap<String, u32>,
    /// (start, end, exidx address, exidx entry count) per loaded library.
    pub unwind_tables: Vec<(u32, u32, u32, u32)>,
    pub jni: jni::Jni,
}

/// Wraps a closure body as a `Handler` with the right parameter types.
#[macro_export]
macro_rules! handler {
    (|$uc:ident, $args:ident| $body:expr) => {{
        #[allow(unused_variables, unused_mut)]
        let function: $crate::runtime::Handler =
            |$uc: &mut $crate::runtime::Uc, $args: &mut $crate::runtime::Args| $body;
        Some(function)
    }};
}

pub fn handler_for(name: &str) -> Option<Handler> {
    if let Some(jni_name) = name.strip_prefix("jni:") {
        return jni::handler(jni_name);
    }
    libc_calls::handler(name).or_else(|| math::handler(name))
}

pub fn install_data(_uc: &mut Uc) {}

const BIONIC_FILE_SIZE: u32 = 84;

/// Guest address for a data object the game imports (stdio, ctype tables...).
pub fn data_import(uc: &mut Uc, name: &str) -> u32 {
    if let Some(&address) = uc.get_data().state.data_imports.get(name) {
        return address;
    }
    let runtime = uc.get_data_mut();
    let memory = runtime.mem;
    let address = match name {
        "__sF" => {
            let base = runtime.misc_alloc(BIONIC_FILE_SIZE * 3, 8);
            for fd in 0..3 {
                runtime.state.files.insert(base + fd * BIONIC_FILE_SIZE, HostFile::Descriptor(fd as i32));
            }
            base
        }
        "__stack_chk_guard" => {
            let address = runtime.misc_alloc(4, 4);
            let mut guard = [0u8; 4];
            unsafe { libc::getrandom(guard.as_mut_ptr().cast(), 4, 0) };
            memory.write_bytes(address, &guard);
            address
        }
        "_ctype_" => {
            // bionic: `const char* _ctype_`, indexed as (_ctype_ + 1)[c].
            let table = runtime.misc_alloc(257, 4);
            for c in 0u8..=255 {
                memory.write_u8(table + 1 + c as u32, ctype_flags(c));
            }
            let pointer = runtime.misc_alloc(4, 4);
            memory.write_u32(pointer, table);
            pointer
        }
        "_tolower_tab_" | "_toupper_tab_" => {
            let table = runtime.misc_alloc(257 * 2, 4);
            memory.write_u16(table, (-1i16) as u16);
            for c in 0u8..=255 {
                let mapped = if name == "_tolower_tab_" { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() };
                memory.write_u16(table + 2 + c as u32 * 2, mapped as u16);
            }
            let pointer = runtime.misc_alloc(4, 4);
            memory.write_u32(pointer, table);
            pointer
        }
        _ if name.starts_with("SL_IID_") => {
            // OpenSL ES interface IDs: a pointer to a 16-byte ID.
            let id = runtime.misc_alloc(16, 4);
            memory.write_bytes(id, &name.as_bytes()[..16.min(name.len())]);
            let pointer = runtime.misc_alloc(4, 4);
            memory.write_u32(pointer, id);
            pointer
        }
        _ => {
            eprintln!("[bridge] unknown data import {name}; giving it 64 zero bytes");
            runtime.misc_alloc(64, 8)
        }
    };
    uc.get_data_mut().state.data_imports.insert(name.to_string(), address);
    address
}

/// bionic/BSD ctype bits: _U 1, _L 2, _N 4, _S 8, _P 0x10, _C 0x20, _X 0x40, _B 0x80.
fn ctype_flags(c: u8) -> u8 {
    if c > 127 {
        return 0;
    }
    let mut flags = 0;
    if c.is_ascii_uppercase() {
        flags |= 0x01;
    }
    if c.is_ascii_lowercase() {
        flags |= 0x02;
    }
    if c.is_ascii_digit() {
        flags |= 0x04;
    }
    if c == b' ' || (9..=13).contains(&c) {
        flags |= 0x08;
    }
    if c.is_ascii_punctuation() {
        flags |= 0x10;
    }
    if c.is_ascii_control() {
        flags |= 0x20;
    }
    if c.is_ascii_hexdigit() && !c.is_ascii_digit() {
        flags |= 0x40;
    }
    if c == b' ' {
        flags |= 0x80;
    }
    flags
}
