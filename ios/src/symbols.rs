//! Symbols the game can import: name (without Apple's leading underscore)
//! to an address in this process. Functions and data share one table.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Data,
}

#[derive(Default)]
pub struct Registry {
    entries: HashMap<String, (usize, Kind)>,
}

impl Registry {
    pub fn function(&mut self, name: &str, address: usize) {
        self.entries.insert(name.to_string(), (address, Kind::Function));
    }

    pub fn data(&mut self, name: &str, address: usize) {
        self.entries.insert(name.to_string(), (address, Kind::Data));
    }

    pub fn get(&self, name: &str) -> Option<(usize, Kind)> {
        self.entries.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

pub fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    f(&mut registry().lock().unwrap())
}

/// Looks up an Apple symbol name such as "_malloc" or "_OBJC_CLASS_$_NSObject".
pub fn lookup(apple_name: &str) -> Option<(usize, Kind)> {
    let name = apple_name.strip_prefix('_').unwrap_or(apple_name);
    with(|r| r.get(name))
}

/// Gives the address of a value that lives for the rest of the run.
pub fn leak<T>(value: T) -> usize {
    Box::into_raw(Box::new(value)) as usize
}

/// A C string that lives for the rest of the run.
pub fn leak_cstr(text: &str) -> usize {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    Box::leak(bytes.into_boxed_slice()).as_ptr() as usize
}
