//! Plain-C framework functions for 32-bit games (OpenGL ES, OpenAL,
//! Core Graphics, Core Foundation, AudioToolbox, UIKit's C functions,
//! libxml2's text writer, SQLite). Each one reads the guest's arguments by Apple's
//! armv7 rules and calls this layer's 64-bit implementation, found by name
//! in the symbol table, so the framework logic exists once.
//!
//! armv7 rules (soft-float at calls): integers, pointers and floats take
//! one word each, r0-r3 then the stack; a double or 64-bit integer takes the
//! next two words with no even-register padding; a struct by value is its
//! words in order and may straddle r3 and the stack; a struct result bigger
//! than 4 bytes goes through a hidden pointer that comes first. CGFloat is a
//! float here and a double in the host.
//!
//! Host objects (CGContext, CGImage, CFString, UIImage, ...: Objective-C
//! objects in this layer) cross as guest words through the object bridge
//! (`set_object_bridge`, installed by the Objective-C bridge so a CGImage is
//! the same guest object to C functions and to messages); until one is
//! installed a table of small guest proxies stands in. Host pointers that
//! aren't objects (OpenAL devices, AudioFileIDs) cross as handles.

pub mod audio;
mod addressbook;
mod cf;
mod gl;
mod graphics;
mod openal;
mod sqlite;
#[cfg(test)]
mod tests;
mod xml;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};

use crate::fw::geometry::{Point, Rect, Size, Transform, Transform3D};
use arm32_bridge::runtime::{self, mem, ArgSource, Args, Handler, Ret, Uc};

// ----- objects and handles -----

/// Which guest word is which host object. The Objective-C bridge provides
/// this (its pair table and proxies) so CF/CG objects and messages agree.
pub trait ObjectBridge: Send + Sync {
    /// The host object of guest object `guest` (never 0 for 0).
    fn to_host(&self, guest: u32) -> usize;
    /// The guest object for host object `host` (made on first sight; 0 for 0).
    fn to_guest(&self, host: usize) -> u32;
}

static BRIDGE: RwLock<Option<&'static dyn ObjectBridge>> = RwLock::new(None);

/// Installs the object bridge (before the program is loaded: object constants
/// are made when they are bound).
pub fn set_object_bridge(bridge: &'static dyn ObjectBridge) {
    *BRIDGE.write().unwrap_or_else(|e| e.into_inner()) = Some(bridge);
}

/// Host pointer <-> guest word pairs. Each guest word is a small block of
/// guest memory of its own, so it is a real, unique, non-null address.
#[derive(Default)]
struct Pairs {
    to_host: HashMap<u32, usize>,
    to_guest: HashMap<usize, u32>,
}

impl Pairs {
    fn guest(&mut self, host: usize, size: u32) -> u32 {
        if host == 0 {
            return 0;
        }
        if let Some(&guest) = self.to_guest.get(&host) {
            return guest;
        }
        let guest = super::alloc_permanent(size, 4);
        unsafe { std::ptr::write_bytes(mem().host(guest), 0, size as usize) };
        self.to_host.insert(guest, host);
        self.to_guest.insert(host, guest);
        guest
    }

    fn host(&self, guest: u32) -> usize {
        self.to_host.get(&guest).copied().unwrap_or(0)
    }

    fn forget(&mut self, guest: u32) -> usize {
        let host = self.to_host.remove(&guest).unwrap_or(0);
        self.to_guest.remove(&host);
        host
    }
}

/// Stand-in proxies { 0, 0 } until the Objective-C bridge is installed.
static PROXIES: Mutex<Option<Pairs>> = Mutex::new(None);
static HANDLES: Mutex<Option<Pairs>> = Mutex::new(None);

fn with_pairs<R>(table: &Mutex<Option<Pairs>>, work: impl FnOnce(&mut Pairs) -> R) -> R {
    work(table.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Pairs::default))
}

pub fn object_to_host(guest: u32) -> usize {
    if guest == 0 {
        return 0;
    }
    match *BRIDGE.read().unwrap_or_else(|e| e.into_inner()) {
        Some(bridge) => bridge.to_host(guest),
        None => with_pairs(&PROXIES, |p| p.host(guest)),
    }
}

pub fn object_to_guest(host: usize) -> u32 {
    if host == 0 {
        return 0;
    }
    match *BRIDGE.read().unwrap_or_else(|e| e.into_inner()) {
        Some(bridge) => bridge.to_guest(host),
        None => with_pairs(&PROXIES, |p| p.guest(host, 8)),
    }
}

/// The guest handle of a host pointer that isn't an object.
pub fn handle_to_guest(host: usize) -> u32 {
    with_pairs(&HANDLES, |p| p.guest(host, 4))
}

pub fn handle_to_host(guest: u32) -> usize {
    with_pairs(&HANDLES, |p| p.host(guest))
}

/// Forgets a handle whose host object is gone; returns that host pointer.
pub fn forget_handle(guest: u32) -> usize {
    with_pairs(&HANDLES, |p| p.forget(guest))
}

// ----- argument and result kinds -----

/// How one argument is read from the guest's words and given to the host.
pub trait Param {
    type Host;
    fn read(a: &mut Args) -> Self::Host;
}

/// How a host result goes back to the guest. `out`: the hidden result
/// pointer, read first when `STRET`.
pub trait Return {
    type Host;
    const STRET: bool = false;
    fn give(value: Self::Host, out: u32) -> Ret;
}

/// A 32-bit integer (enums, GLuint, OSStatus...).
pub struct U32;
pub struct I32;
/// size_t and other unsigned longs: zero-extended.
pub struct Word;
/// CFIndex, NSInteger and other signed longs: sign-extended.
pub struct Long;
pub struct F32;
pub struct F64;
/// CGFloat: a float in the guest, a double in the host.
pub struct Float;
pub struct Bool;
/// A guest pointer (0 stays null).
pub struct Ptr;
/// A host object (see the object bridge).
pub struct Obj;
/// A host pointer that isn't an object.
pub struct Handle;
/// A C string result: a permanent guest copy (one per text).
pub struct CStr;
/// A selector: the guest's is its name (a C string), the host's own SEL is
/// made from it (0 stays 0).
pub struct Sel;

impl Param for U32 {
    type Host = u32;
    fn read(a: &mut Args) -> u32 {
        a.u32()
    }
}
impl Param for I32 {
    type Host = i32;
    fn read(a: &mut Args) -> i32 {
        a.i32()
    }
}
impl Param for Word {
    type Host = usize;
    fn read(a: &mut Args) -> usize {
        a.u32() as usize
    }
}
impl Param for Long {
    type Host = isize;
    fn read(a: &mut Args) -> isize {
        a.i32() as isize
    }
}
impl Param for F32 {
    type Host = f32;
    fn read(a: &mut Args) -> f32 {
        a.f32()
    }
}
impl Param for F64 {
    type Host = f64;
    fn read(a: &mut Args) -> f64 {
        a.f64()
    }
}
impl Param for Float {
    type Host = f64;
    fn read(a: &mut Args) -> f64 {
        a.f32() as f64
    }
}
impl Param for Bool {
    type Host = bool;
    fn read(a: &mut Args) -> bool {
        a.u32() & 0xff != 0
    }
}
impl Param for Ptr {
    type Host = *mut u8;
    fn read(a: &mut Args) -> *mut u8 {
        mem().host_or_null(a.u32())
    }
}
impl Param for Obj {
    type Host = usize;
    fn read(a: &mut Args) -> usize {
        object_to_host(a.u32())
    }
}
impl Param for Handle {
    type Host = usize;
    fn read(a: &mut Args) -> usize {
        handle_to_host(a.u32())
    }
}
impl Param for Sel {
    type Host = usize;
    fn read(a: &mut Args) -> usize {
        match a.u32() {
            0 => 0,
            name => crate::objc::runtime::sel(&mem().string(name)),
        }
    }
}

impl Return for () {
    type Host = ();
    fn give(_: (), _: u32) -> Ret {
        Ret::Void
    }
}
impl Return for U32 {
    type Host = u32;
    fn give(value: u32, _: u32) -> Ret {
        Ret::U32(value)
    }
}
impl Return for I32 {
    type Host = i32;
    fn give(value: i32, _: u32) -> Ret {
        Ret::U32(value as u32)
    }
}
impl Return for Word {
    type Host = usize;
    fn give(value: usize, _: u32) -> Ret {
        Ret::U32(value as u32)
    }
}
impl Return for Long {
    type Host = isize;
    fn give(value: isize, _: u32) -> Ret {
        Ret::U32(value as u32)
    }
}
impl Return for F32 {
    type Host = f32;
    fn give(value: f32, _: u32) -> Ret {
        Ret::F32(value)
    }
}
impl Return for F64 {
    type Host = f64;
    fn give(value: f64, _: u32) -> Ret {
        Ret::F64(value)
    }
}
impl Return for Float {
    type Host = f64;
    fn give(value: f64, _: u32) -> Ret {
        Ret::F32(value as f32)
    }
}
impl Return for Bool {
    type Host = bool;
    fn give(value: bool, _: u32) -> Ret {
        Ret::U32(value as u32)
    }
}
/// A byte (Boolean, ALboolean, GLboolean).
pub struct Byte;
impl Param for Byte {
    type Host = u8;
    fn read(a: &mut Args) -> u8 {
        a.u32() as u8
    }
}
/// A 64-bit integer (SInt64): the next two words by Apple's packed rule.
pub struct I64;
impl Param for I64 {
    type Host = i64;
    fn read(a: &mut Args) -> i64 {
        a.i64()
    }
}
impl Return for Byte {
    type Host = u8;
    fn give(value: u8, _: u32) -> Ret {
        Ret::U32(value as u32)
    }
}
impl Return for Obj {
    type Host = usize;
    fn give(value: usize, _: u32) -> Ret {
        Ret::U32(object_to_guest(value))
    }
}
impl Return for Handle {
    type Host = usize;
    fn give(value: usize, _: u32) -> Ret {
        Ret::U32(handle_to_guest(value))
    }
}
impl Return for CStr {
    type Host = usize;
    fn give(value: usize, _: u32) -> Ret {
        Ret::U32(guest_text(value))
    }
}
/// A pointer into guest memory (data bytes the host keeps there).
impl Return for Ptr {
    type Host = usize;
    fn give(value: usize, _: u32) -> Ret {
        Ret::U32(guest_pointer(value))
    }
}

/// The guest address of host pointer `host`, which must point into guest
/// memory (0 for NULL, and for host memory the guest can't reach).
pub fn guest_pointer(host: usize) -> u32 {
    let base = mem().host(0) as usize;
    match host.checked_sub(base) {
        _ if host == 0 => 0,
        Some(offset) if offset < 1 << 32 => offset as u32,
        _ => {
            crate::log!("32-bit: a host pointer {host:#x} outside guest memory was handed to the game as NULL");
            0
        }
    }
}

/// A permanent guest copy of host C string `host` (0 for null).
pub fn guest_text(host: usize) -> u32 {
    if host == 0 {
        return 0;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(host as *const libc::c_char) }.to_string_lossy().into_owned();
    super::cstring_permanent(&text)
}

/// Core Graphics structs: CGFloat fields, floats in the guest.
pub trait GuestStruct: Sized + Copy {
    const FIELDS: usize;
    fn from_fields(fields: &[f64]) -> Self;
    fn fields(&self) -> Vec<f64>;

    fn read_args(a: &mut Args) -> Self {
        let fields: Vec<f64> = (0..Self::FIELDS).map(|_| a.f32() as f64).collect();
        Self::from_fields(&fields)
    }
    fn read_guest(address: u32) -> Self {
        let fields: Vec<f64> = (0..Self::FIELDS as u32).map(|i| f32::from_bits(mem().read_u32(address + i * 4)) as f64).collect();
        Self::from_fields(&fields)
    }
    fn write_guest(&self, address: u32) {
        for (i, value) in self.fields().into_iter().enumerate() {
            mem().write_u32(address + i as u32 * 4, (value as f32).to_bits());
        }
    }
}

impl GuestStruct for Point {
    const FIELDS: usize = 2;
    fn from_fields(f: &[f64]) -> Point {
        Point { x: f[0], y: f[1] }
    }
    fn fields(&self) -> Vec<f64> {
        vec![self.x, self.y]
    }
}
impl GuestStruct for Size {
    const FIELDS: usize = 2;
    fn from_fields(f: &[f64]) -> Size {
        Size { width: f[0], height: f[1] }
    }
    fn fields(&self) -> Vec<f64> {
        vec![self.width, self.height]
    }
}
impl GuestStruct for Rect {
    const FIELDS: usize = 4;
    fn from_fields(f: &[f64]) -> Rect {
        Rect::new(f[0], f[1], f[2], f[3])
    }
    fn fields(&self) -> Vec<f64> {
        vec![self.origin.x, self.origin.y, self.size.width, self.size.height]
    }
}
/// UIEdgeInsets (top, left, bottom, right).
impl GuestStruct for [f64; 4] {
    const FIELDS: usize = 4;
    fn from_fields(f: &[f64]) -> [f64; 4] {
        f.try_into().unwrap()
    }
    fn fields(&self) -> Vec<f64> {
        self.to_vec()
    }
}
impl GuestStruct for Transform {
    const FIELDS: usize = 6;
    fn from_fields(f: &[f64]) -> Transform {
        Transform { a: f[0], b: f[1], c: f[2], d: f[3], tx: f[4], ty: f[5] }
    }
    fn fields(&self) -> Vec<f64> {
        vec![self.a, self.b, self.c, self.d, self.tx, self.ty]
    }
}

impl GuestStruct for Transform3D {
    const FIELDS: usize = 16;
    fn from_fields(f: &[f64]) -> Transform3D {
        Transform3D(f.try_into().unwrap())
    }
    fn fields(&self) -> Vec<f64> {
        self.0.to_vec()
    }
}

thread_local! {
    /// Converted copies a host call reads through a pointer, alive for the call.
    static AFFINE: std::cell::Cell<Transform> = const { std::cell::Cell::new(crate::fw::geometry::IDENTITY) };
    static COLOR: std::cell::Cell<[f64; 4]> = const { std::cell::Cell::new([0.0; 4]) };
}

/// `const CGAffineTransform*` (null stays null): a host copy for the call.
pub struct AffinePtr;
impl Param for AffinePtr {
    type Host = *const Transform;
    fn read(a: &mut Args) -> *const Transform {
        let guest = a.u32();
        if guest == 0 {
            return std::ptr::null();
        }
        AFFINE.with(|slot| {
            slot.set(Transform::read_guest(guest));
            slot.as_ptr() as *const Transform
        })
    }
}

/// `const CGFloat*` colour components (up to 4: RGB and alpha): a host copy for the call.
pub struct ColorPtr;
impl Param for ColorPtr {
    type Host = *const f64;
    fn read(a: &mut Args) -> *const f64 {
        let guest = a.u32();
        if guest == 0 {
            return std::ptr::null();
        }
        COLOR.with(|slot| {
            slot.set([0, 1, 2, 3].map(|i| f32::from_bits(mem().read_u32(guest + i * 4)) as f64));
            slot.as_ptr() as *const f64
        })
    }
}

macro_rules! struct_kind {
    ($($host:ty),*) => {$(
        impl Param for $host {
            type Host = $host;
            fn read(a: &mut Args) -> $host {
                <$host as GuestStruct>::read_args(a)
            }
        }
        impl Return for $host {
            type Host = $host;
            const STRET: bool = true;
            fn give(value: $host, out: u32) -> Ret {
                if out != 0 {
                    value.write_guest(out);
                }
                Ret::U32(out)
            }
        }
    )*};
}
struct_kind!(Point, Size, Rect, Transform, Transform3D);

/// This layer's function `name` as a typed pointer (cached), for handlers
/// that convert by hand: `host_fn!("CGColorCreate": unsafe extern "C" fn(usize) -> usize)`.
macro_rules! host_fn {
    ($name:literal : $ty:ty) => {{
        static ADDRESS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        $crate::guest32::fw::host_address(&ADDRESS, $name).map(|address| unsafe { std::mem::transmute::<usize, $ty>(address) })
    }};
}
pub(crate) use host_fn;

// ----- calling the host -----

/// The host function `name` (cached in `slot`), or None when this layer has none.
pub fn host_address(slot: &AtomicUsize, name: &str) -> Option<usize> {
    let cached = slot.load(Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    let address = match crate::symbols::lookup(name) {
        Some((address, crate::symbols::Kind::Function)) => address,
        _ => return None,
    };
    slot.store(address, Ordering::Relaxed);
    Some(address)
}

/// Stops the program: a function is registered here but this layer lacks it.
pub fn no_host_function(uc: &mut Uc, name: &str) -> Ret {
    runtime::stop_with_fault(uc, format!("{name}: the iPhone layer has no such function"));
    Ret::Void
}

/// Guest handlers that convert by kind and call this layer's function of
/// the same name: `"CGContextFillRect" => fn(context: Obj, rect: Rect);`.
macro_rules! host_functions {
    ($($name:literal => fn($($arg:ident : $kind:ty),*) $(-> $ret:ty)?;)*) => {
        vec![$(($name, {
            let handler: arm32_bridge::runtime::Handler = |uc, a| {
                #[allow(unused_imports)]
                use $crate::guest32::fw::{Param, Return};
                type R = host_functions!(@ret $($ret)?);
                static ADDRESS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let Some(address) = $crate::guest32::fw::host_address(&ADDRESS, $name) else {
                    return $crate::guest32::fw::no_host_function(uc, $name);
                };
                let out = if <R as Return>::STRET { arm32_bridge::runtime::ArgSource::u32(a) } else { 0 };
                $(let $arg = <$kind as Param>::read(a);)*
                let function: unsafe extern "C" fn($(<$kind as Param>::Host),*) -> <R as Return>::Host =
                    unsafe { std::mem::transmute(address) };
                <R as Return>::give(unsafe { function($($arg),*) }, out)
            };
            handler
        })),*]
    };
    (@ret) => { () };
    (@ret $ret:ty) => { $ret };
}
pub(crate) use host_functions;

fn register_all(list: Vec<(&'static str, Handler)>) {
    for (name, handler) in list {
        super::function(name, handler);
    }
}

// ----- data -----

/// The host's data `name`: its address.
fn host_data(name: &str) -> Option<usize> {
    match crate::symbols::lookup(name) {
        Some((address, crate::symbols::Kind::Data)) => Some(address),
        _ => None,
    }
}

/// A guest variable holding the guest form of the host's object constant `name`
/// (string constants: notification names, run loop modes, keys).
pub fn object_constant(name: &'static str) {
    super::data(name, move || {
        let variable = super::alloc_permanent(4, 4);
        let host = host_data(name).map_or(0, |address| unsafe { *(address as *const usize) });
        if host == 0 {
            crate::log!("32-bit: the iPhone layer has no object constant {name}");
        }
        mem().write_u32(variable, object_to_guest(host));
        variable
    });
}

/// A guest copy (floats) of the host's Core Graphics struct constant `name`.
pub fn struct_constant<T: GuestStruct + 'static>(name: &'static str) {
    super::data(name, move || {
        let guest = super::alloc_permanent(T::FIELDS as u32 * 4, 4);
        match host_data(name) {
            Some(address) => unsafe { *(address as *const T) }.write_guest(guest),
            None => crate::log!("32-bit: the iPhone layer has no constant {name}"),
        }
        guest
    });
}

/// A guest copy of the host's CGFloat constant `name` (a float here).
pub fn float_constant(name: &'static str) {
    super::data(name, move || {
        let guest = super::alloc_permanent(4, 4);
        let value = host_data(name).map_or(0.0, |address| unsafe { *(address as *const f64) });
        mem().write_u32(guest, (value as f32).to_bits());
        guest
    });
}

/// Makes the host's data bytes (NSData, CFData) come from the guest heap,
/// so the pointers to them the game is given are guest addresses, until the
/// answer is dropped (the session's end). Call once guest memory is set up.
pub fn keep_data_bytes_in_guest_memory() -> crate::fw::data::HeapHeld {
    crate::fw::data::keep_bytes_in(crate::fw::data::ByteHeap {
        alloc: |size| match u32::try_from(size) {
            Ok(size) => match runtime::heap().malloc(size) {
                0 => std::ptr::null_mut(),
                guest => mem().host(guest),
            },
            Err(_) => std::ptr::null_mut(),
        },
        free: |pointer| runtime::heap().free(mem().guest(pointer)),
        // Bytes a game handed over: a block of its malloc, else left alone.
        free_malloced: |pointer| {
            let guest = (pointer as usize).wrapping_sub(mem().host(0) as usize);
            let why = match u32::try_from(guest) {
                Ok(guest) => {
                    let mut heap = runtime::heap();
                    match heap.not_owned(guest) {
                        None => return heap.free(guest),
                        Some(why) => why,
                    }
                }
                Err(_) => "not in guest memory",
            };
            crate::log!("data: bytes {pointer:p} handed over to be freed are not a block of the game's malloc ({why}); left alone");
        },
    })
}

/// A guest copy of the host's pointer-sized constant `name` (e.g. kCFAllocatorDefault, NULL).
pub fn word_constant(name: &'static str) {
    super::data(name, move || {
        let guest = super::alloc_permanent(4, 4);
        mem().write_u32(guest, host_data(name).map_or(0, |address| unsafe { *(address as *const usize) }) as u32);
        guest
    });
}

pub fn register() {
    gl::register();
    openal::register();
    graphics::register();
    cf::register();
    addressbook::register();
    audio::register();
    xml::register();
    sqlite::register();
}
