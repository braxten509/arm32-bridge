//! NSMethodSignature and NSInvocation. Arguments are placed in registers and
//! on the stack by Apple's arm64 rules, and the call goes through our
//! objc_msgSend, so the message finds its method the way any other does.
//!
//! A 32-bit game's signatures have the armv7 layout (4-byte pointers, 8-byte
//! types 4-byte aligned): its invocations hold every argument as the guest
//! has it (objects as guest objects), and the game's bridge sends them
//! (see [`GuestInvocations`]). One class serves both kinds of game.

use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, ClassObject, Id, Sel};
use crate::symbols::{leak_cstr, Registry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

// ----- type encodings -----

/// Whose rules a signature's sizes and an invocation's argument bytes follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 64-bit games and our own code.
    Arm64,
    /// 32-bit games (values as guest memory has them).
    Armv7,
}

impl Layout {
    fn pointer_size(self) -> usize {
        match self {
            Layout::Arm64 => 8,
            Layout::Armv7 => 4,
        }
    }

    /// The largest alignment a scalar gets: armv7 iOS aligns 8-byte types to 4.
    fn max_align(self) -> usize {
        match self {
            Layout::Arm64 => 16,
            Layout::Armv7 => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub size: usize,
    pub align: usize,
    /// Plain scalar: (is floating point, is signed).
    pub scalar: Option<(bool, bool)>,
    /// The float leaves, for homogeneous float aggregates: (count, size).
    pub floats: Option<(usize, usize)>,
    pub is_struct: bool,
    pub is_void: bool,
}

impl Type {
    fn scalar(size: usize, float: bool, signed: bool) -> Type {
        Type { size, align: size, scalar: Some((float, signed)), floats: float.then_some((1, size)), is_struct: false, is_void: false }
    }

    fn scalar_in(layout: Layout, size: usize, float: bool, signed: bool) -> Type {
        Type { align: size.min(layout.max_align()), ..Type::scalar(size, float, signed) }
    }

    fn pointer(layout: Layout) -> Type {
        Type::scalar(layout.pointer_size(), false, false)
    }
}

/// Skips Objective-C type qualifiers (const, in, out, oneway...).
fn skip_qualifiers(t: &[u8], i: &mut usize) {
    while *i < t.len() && b"rnNoORVAj".contains(&t[*i]) {
        *i += 1;
    }
}

fn skip_digits(t: &[u8], i: &mut usize) -> usize {
    let start = *i;
    while *i < t.len() && (t[*i].is_ascii_digit() || t[*i] == b'-') {
        *i += 1;
    }
    std::str::from_utf8(&t[start..*i]).ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn aggregate(members: Vec<Type>, union: bool) -> Type {
    let mut size = 0;
    let mut align = 1;
    let mut float_size = None;
    let mut float_count = 0;
    let mut all_float = !members.is_empty();
    for m in &members {
        align = align.max(m.align);
        if union {
            size = size.max(m.size);
        } else {
            size = (size + m.align - 1) / m.align * m.align + m.size;
        }
        match m.floats {
            Some((count, each)) if float_size.is_none() || float_size == Some(each) => {
                float_size = Some(each);
                float_count = if union { float_count.max(count) } else { float_count + count };
            }
            _ => all_float = false,
        }
    }
    let size = (size + align - 1) / align * align;
    let floats = match float_size {
        Some(each) if all_float && (1..=4).contains(&float_count) && float_count * each == size => Some((float_count, each)),
        _ => None,
    };
    Type { size, align, scalar: None, floats, is_struct: true, is_void: false }
}

/// Reads one type from an encoding like "{CGRect={CGPoint=dd}{CGSize=dd}}".
pub fn parse_type(t: &[u8], i: &mut usize) -> Type {
    parse_type_in(t, i, Layout::Arm64)
}

/// [`parse_type`] by `layout`'s sizes.
pub fn parse_type_in(t: &[u8], i: &mut usize, layout: Layout) -> Type {
    skip_qualifiers(t, i);
    let Some(&c) = t.get(*i) else {
        return Type { size: 0, align: 1, scalar: None, floats: None, is_struct: false, is_void: true };
    };
    *i += 1;
    match c {
        b'c' => Type::scalar(1, false, true),
        b'C' | b'B' => Type::scalar(1, false, false),
        b's' => Type::scalar(2, false, true),
        b'S' => Type::scalar(2, false, false),
        b'i' | b'l' => Type::scalar(4, false, true),
        b'I' | b'L' => Type::scalar(4, false, false),
        b'q' => Type::scalar_in(layout, 8, false, true),
        b'Q' => Type::scalar_in(layout, 8, false, false),
        b'f' => Type::scalar(4, true, true),
        // long double is a double on Apple's ARM targets.
        b'd' | b'D' => Type::scalar_in(layout, 8, true, true),
        b'v' => Type { size: 0, align: 1, scalar: None, floats: None, is_struct: false, is_void: true },
        b'@' => {
            // "@?" is a block; '@"NSString"' names the class.
            if t.get(*i) == Some(&b'?') {
                *i += 1;
            } else if t.get(*i) == Some(&b'"') {
                *i += 1;
                while *i < t.len() && t[*i] != b'"' {
                    *i += 1;
                }
                *i += 1;
            }
            Type::pointer(layout)
        }
        b'#' | b':' | b'*' | b'?' => Type::pointer(layout),
        b'^' => {
            parse_type_in(t, i, layout);
            Type::pointer(layout)
        }
        b'b' => {
            let bits = skip_digits(t, i);
            Type::scalar_in(layout, if bits > 32 { 8 } else { 4 }, false, false)
        }
        b'[' => {
            let count = skip_digits(t, i);
            let element = parse_type_in(t, i, layout);
            if t.get(*i) == Some(&b']') {
                *i += 1;
            }
            aggregate(vec![element; count.max(1)], false)
        }
        b'{' | b'(' => {
            let close = if c == b'{' { b'}' } else { b')' };
            while *i < t.len() && t[*i] != b'=' && t[*i] != close {
                *i += 1;
            }
            let mut members = Vec::new();
            if t.get(*i) == Some(&b'=') {
                *i += 1;
                while *i < t.len() && t[*i] != close {
                    if t[*i] == b'"' {
                        // A member name: "x"d.
                        *i += 1;
                        while *i < t.len() && t[*i] != b'"' {
                            *i += 1;
                        }
                        *i += 1;
                        continue;
                    }
                    members.push(parse_type_in(t, i, layout));
                }
            }
            *i += 1;
            aggregate(members, c == b'(')
        }
        _ => {
            crate::log!("type encoding '{}' not known; read as a pointer", c as char);
            Type::pointer(layout)
        }
    }
}

/// A method's types: the return type, then self, _cmd and the arguments.
pub struct Signature {
    pub text: String,
    pub parts: Vec<(String, Type)>,
    pub layout: Layout,
    part_names: Vec<usize>,
}

pub fn parse_signature(text: &str) -> Signature {
    parse_signature_in(text, Layout::Arm64)
}

pub fn parse_signature_in(text: &str, layout: Layout) -> Signature {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut parts = Vec::new();
    while i < bytes.len() {
        let start = i;
        let ty = parse_type_in(bytes, &mut i, layout);
        let part = text[start..i].to_string();
        skip_digits(bytes, &mut i);
        parts.push((part, ty));
    }
    let part_names = parts.iter().map(|(p, _)| leak_cstr(p)).collect();
    Signature { text: text.to_string(), parts, layout, part_names }
}

// ----- the call -----

/// What the assembly below reads and fills in.
#[repr(C)]
#[derive(Default)]
struct Frame {
    function: usize,
    x: [u64; 8],
    d: [u64; 8],
    x8: u64,
    stack: usize,
    stack_size: u64,
    result_x: [u64; 2],
    result_d: [u64; 4],
}

std::arch::global_asm!(
    ".globl ios_invoke_call",
    ".p2align 2",
    "ios_invoke_call:",
    "stp x29, x30, [sp, #-32]!",
    "mov x29, sp",
    "str x19, [sp, #16]",
    "mov x19, x0",
    "ldr x9, [x19, #152]",
    "sub sp, sp, x9",
    "ldr x10, [x19, #144]",
    "mov x11, #0",
    "1:",
    "cmp x11, x9",
    "b.ge 2f",
    "ldr x12, [x10, x11]",
    "str x12, [sp, x11]",
    "add x11, x11, #8",
    "b 1b",
    "2:",
    "ldp d0, d1, [x19, #72]",
    "ldp d2, d3, [x19, #88]",
    "ldp d4, d5, [x19, #104]",
    "ldp d6, d7, [x19, #120]",
    "ldr x8, [x19, #136]",
    "ldr x16, [x19, #0]",
    "ldp x0, x1, [x19, #8]",
    "ldp x2, x3, [x19, #24]",
    "ldp x4, x5, [x19, #40]",
    "ldp x6, x7, [x19, #56]",
    "blr x16",
    "stp x0, x1, [x19, #160]",
    "stp d0, d1, [x19, #176]",
    "stp d2, d3, [x19, #192]",
    "mov sp, x29",
    "ldr x19, [sp, #16]",
    "ldp x29, x30, [sp], #32",
    "ret",
);

extern "C" {
    fn ios_invoke_call(frame: *mut Frame);
}

fn widen(bytes: &[u8], signed: bool) -> u64 {
    let mut raw = [0u8; 8];
    raw[..bytes.len().min(8)].copy_from_slice(&bytes[..bytes.len().min(8)]);
    let value = u64::from_le_bytes(raw);
    // iOS has the caller extend small integers to 32 bits.
    match (bytes.len(), signed) {
        (1, true) => value as u8 as i8 as i32 as u32 as u64,
        (2, true) => value as u16 as i16 as i32 as u32 as u64,
        _ => value,
    }
}

/// Lays out arguments by Apple's arm64 rules and calls `function`; the
/// return value's bytes are written to `result`.
pub fn call(function: usize, types: &[Type], values: &[Vec<u8>], result_type: &Type, result: &mut [u8]) {
    let mut frame = Frame { function, ..Default::default() };
    let mut stack: Vec<u8> = Vec::new();
    let (mut next_x, mut next_d) = (0usize, 0usize);
    let mut big_copies: Vec<Vec<u8>> = Vec::new();
    let on_stack = |stack: &mut Vec<u8>, bytes: &[u8], align: usize| {
        let at = (stack.len() + align - 1) / align * align;
        stack.resize(at, 0);
        stack.extend_from_slice(bytes);
    };
    for (ty, value) in types.iter().zip(values) {
        let mut value = value.clone();
        value.resize(ty.size.max(1), 0);
        if let Some((count, each)) = ty.floats {
            if next_d + count <= 8 {
                for k in 0..count {
                    let mut raw = [0u8; 8];
                    raw[..each].copy_from_slice(&value[k * each..(k + 1) * each]);
                    frame.d[next_d] = u64::from_le_bytes(raw);
                    next_d += 1;
                }
            } else {
                next_d = 8;
                on_stack(&mut stack, &value[..ty.size], ty.align);
            }
            continue;
        }
        if ty.is_struct && ty.size > 16 {
            // Big structs go by reference to a copy.
            big_copies.push(value);
            let pointer = (big_copies.last().unwrap().as_ptr() as u64).to_le_bytes();
            if next_x < 8 {
                frame.x[next_x] = u64::from_le_bytes(pointer);
                next_x += 1;
            } else {
                on_stack(&mut stack, &pointer, 8);
            }
            continue;
        }
        if ty.is_struct {
            let words = ty.size.div_ceil(8).max(1);
            if ty.align == 16 && next_x % 2 == 1 {
                next_x += 1;
            }
            if next_x + words <= 8 {
                for k in 0..words {
                    let end = ((k + 1) * 8).min(value.len());
                    frame.x[next_x] = widen(&value[k * 8..end], false);
                    next_x += 1;
                }
            } else {
                next_x = 8;
                let mut padded = value[..ty.size].to_vec();
                padded.resize(words * 8, 0);
                on_stack(&mut stack, &padded, ty.align.max(8));
            }
            continue;
        }
        let signed = ty.scalar.map(|(_, s)| s).unwrap_or(false);
        if next_x < 8 {
            frame.x[next_x] = widen(&value[..ty.size.max(1).min(8)], signed);
            next_x += 1;
        } else {
            on_stack(&mut stack, &value[..ty.size.max(1)], ty.size.clamp(1, 8));
        }
    }
    let mut indirect = Vec::new();
    if result_type.is_struct && result_type.size > 16 && result_type.floats.is_none() {
        indirect = vec![0u8; result_type.size];
        frame.x8 = indirect.as_mut_ptr() as u64;
    }
    let size = stack.len().div_ceil(16) * 16;
    stack.resize(size, 0);
    frame.stack = stack.as_ptr() as usize;
    frame.stack_size = size as u64;
    unsafe { ios_invoke_call(&mut frame) };
    drop(big_copies);
    let n = result.len().min(result_type.size);
    if !indirect.is_empty() {
        result[..n].copy_from_slice(&indirect[..n]);
    } else if let Some((count, each)) = result_type.floats {
        for k in 0..count.min(4) {
            let bytes = frame.result_d[k].to_le_bytes();
            let at = k * each;
            if at + each <= n {
                result[at..at + each].copy_from_slice(&bytes[..each]);
            }
        }
    } else {
        let mut raw = [0u8; 16];
        raw[..8].copy_from_slice(&frame.result_x[0].to_le_bytes());
        raw[8..].copy_from_slice(&frame.result_x[1].to_le_bytes());
        result[..n.min(16)].copy_from_slice(&raw[..n.min(16)]);
    }
}

/// The other half of [`call`]: each argument's bytes as a caller placed them
/// by Apple's arm64 rules, from its saved registers (x0-x7, d0-d7) and its
/// stack arguments at `stack`.
pub fn capture(types: &[Type], x: &[u64; 8], d: &[u64; 8], stack: usize) -> Vec<Vec<u8>> {
    let (mut next_x, mut next_d, mut offset) = (0usize, 0usize, 0usize);
    let mut from_stack = |size: usize, align: usize| -> Vec<u8> {
        offset = offset.div_ceil(align.max(1)) * align.max(1);
        let bytes = unsafe { std::slice::from_raw_parts((stack + offset) as *const u8, size) }.to_vec();
        offset += size;
        bytes
    };
    let mut values = Vec::new();
    for ty in types {
        let mut value = Vec::with_capacity(ty.size.max(8));
        if let Some((count, each)) = ty.floats {
            if next_d + count <= 8 {
                for k in 0..count {
                    value.extend_from_slice(&d[next_d + k].to_le_bytes()[..each]);
                }
                next_d += count;
            } else {
                next_d = 8;
                value = from_stack(ty.size, ty.align);
            }
        } else if ty.is_struct && ty.size > 16 {
            // Big structs come by reference to the caller's copy.
            let pointer = if next_x < 8 {
                next_x += 1;
                x[next_x - 1] as usize
            } else {
                usize::from_le_bytes(from_stack(8, 8).try_into().unwrap())
            };
            value = unsafe { std::slice::from_raw_parts(pointer as *const u8, ty.size) }.to_vec();
        } else if ty.is_struct {
            let words = ty.size.div_ceil(8).max(1);
            if ty.align == 16 && next_x % 2 == 1 {
                next_x += 1;
            }
            if next_x + words <= 8 {
                for k in 0..words {
                    value.extend_from_slice(&x[next_x + k].to_le_bytes());
                }
                next_x += words;
            } else {
                next_x = 8;
                value = from_stack(words * 8, ty.align.max(8));
            }
        } else if next_x < 8 {
            value.extend_from_slice(&x[next_x].to_le_bytes());
            next_x += 1;
        } else {
            value = from_stack(ty.size.max(1), ty.size.clamp(1, 8));
        }
        value.resize(ty.size.max(8), 0);
        values.push(value);
    }
    values
}

/// Where an arm64 callee leaves a result of `ty`: big structs at the
/// caller's x8 buffer (`x8`), float aggregates in d0-d3, the rest in x0/x1.
pub fn place_result(ty: &Type, bytes: &[u8], x8: usize, ret_x: &mut [u64; 2], ret_d: &mut [u64; 4]) {
    let mut bytes = bytes.to_vec();
    bytes.resize(ty.size.max(16), 0);
    if ty.is_void {
        return;
    }
    if let Some((count, each)) = ty.floats {
        for k in 0..count.min(4) {
            let mut raw = [0u8; 8];
            raw[..each].copy_from_slice(&bytes[k * each..(k + 1) * each]);
            ret_d[k] = u64::from_le_bytes(raw);
        }
    } else if ty.is_struct && ty.size > 16 {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), x8 as *mut u8, ty.size) };
    } else {
        ret_x[0] = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        ret_x[1] = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    }
}

// ----- NSMethodSignature: { isa, *const Signature } -----

/// The types an NSMethodSignature holds.
pub fn signature_of(object: Id) -> Option<Arc<Signature>> {
    signature(object)
}

fn signature(object: Id) -> Option<Arc<Signature>> {
    if object == 0 {
        return None;
    }
    let slot = unsafe { *((object + 8) as *const *const Signature) };
    if slot.is_null() {
        return None;
    }
    unsafe {
        Arc::increment_strong_count(slot);
        Some(Arc::from_raw(slot))
    }
}

fn make_signature(text: &str) -> Id {
    make_signature_in(text, Layout::Arm64)
}

/// A new NSMethodSignature for `text` by `layout`'s sizes (nil when it
/// lacks a return type, self or _cmd).
pub fn make_signature_in(text: &str, layout: Layout) -> Id {
    let parsed = parse_signature_in(text, layout);
    if parsed.parts.len() < 3 {
        crate::log!("NSMethodSignature \"{text}\": needs a return type, self and _cmd; nil");
        return 0;
    }
    let object = runtime::alloc_instance(runtime::class_named("NSMethodSignature"));
    unsafe { *((object + 8) as *mut *const Signature) = Arc::into_raw(Arc::new(parsed)) };
    object
}

extern "C" fn ms_with_types(_: Class, _: Sel, types: usize) -> Id {
    if types == 0 {
        return 0;
    }
    make_signature(&unsafe { runtime::cstr(types) })
}

extern "C" fn ms_count(this: Id, _: Sel) -> usize {
    signature(this).map(|s| s.parts.len() - 1).unwrap_or(0)
}

extern "C" fn ms_argument_type(this: Id, _: Sel, index: usize) -> usize {
    signature(this).and_then(|s| s.part_names.get(index + 1).copied()).unwrap_or(0)
}

extern "C" fn ms_return_type(this: Id, _: Sel) -> usize {
    signature(this).map(|s| s.part_names[0]).unwrap_or(0)
}

extern "C" fn ms_return_length(this: Id, _: Sel) -> usize {
    signature(this).map(|s| s.parts[0].1.size).unwrap_or(0)
}

extern "C" fn ms_frame_length(this: Id, _: Sel) -> usize {
    signature(this)
        .map(|s| match s.layout {
            Layout::Arm64 => s.parts[1..].iter().map(|(_, t)| t.size.div_ceil(8).max(1) * 8).sum::<usize>().max(224),
            Layout::Armv7 => s.parts[1..].iter().map(|(_, t)| t.size.div_ceil(4).max(1) * 4).sum::<usize>(),
        })
        .unwrap_or(0)
}

extern "C" fn ms_is_oneway(this: Id, _: Sel) -> bool {
    signature(this).map(|s| s.parts[0].0.contains('V')).unwrap_or(false)
}

extern "C" fn ms_is_equal(this: Id, _: Sel, other: Id) -> bool {
    match (signature(this), signature(other)) {
        (Some(a), Some(b)) => a.parts.iter().map(|p| &p.0).eq(b.parts.iter().map(|p| &p.0)),
        _ => false,
    }
}

extern "C" fn ms_copy(this: Id, _: Sel, _zone: usize) -> Id {
    this
}

/// Our own methods' encodings, made by build.rs from their Rust signatures:
/// (class name, or "" when the class wasn't known there; class method?; selector; types).
mod generated {
    include!(concat!(env!("OUT_DIR"), "/method_types.rs"));
}

struct OurTypes {
    exact: HashMap<(&'static str, bool, &'static str), &'static str>,
    /// By selector alone, when every definition agrees (None: they differ).
    by_selector: HashMap<(bool, &'static str), Option<&'static str>>,
}

fn our_types() -> &'static OurTypes {
    static TYPES: OnceLock<OurTypes> = OnceLock::new();
    TYPES.get_or_init(|| {
        let mut exact = HashMap::new();
        let mut by_selector: HashMap<(bool, &'static str), Option<&'static str>> = HashMap::new();
        for &(class, meta, selector, types) in generated::METHOD_TYPES {
            if !class.is_empty() {
                exact.insert((class, meta, selector), types);
            }
            by_selector
                .entry((meta, selector))
                .and_modify(|known| {
                    if *known != Some(types) {
                        *known = None;
                    }
                })
                .or_insert(Some(types));
        }
        OurTypes { exact, by_selector }
    })
}

/// The game's own encodings: (class or metaclass, selector) -> types, from
/// its classes' method lists and then its categories (which win, as they
/// replace the class's own methods).
fn game_types() -> &'static HashMap<(Class, String), String> {
    static TYPES: OnceLock<HashMap<(Class, String), String>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let mut out = HashMap::new();
        let Some(loaded) = crate::loader::try_loaded() else { return out };
        unsafe fn read(address: usize) -> usize {
            std::ptr::read_unaligned(address as *const usize)
        }
        unsafe fn add_list(out: &mut HashMap<(Class, String), String>, class: Class, list: usize) {
            if list == 0 || class == 0 {
                return;
            }
            let entry_size = (std::ptr::read_unaligned(list as *const u32) & !3) as usize;
            let count = std::ptr::read_unaligned((list + 4) as *const u32) as usize;
            for index in 0..count {
                let entry = list + 8 + index * entry_size;
                let types = read(entry + 8);
                if types != 0 {
                    out.insert((class, runtime::cstr(read(entry))), runtime::cstr(types));
                }
            }
        }
        let cells = |name: &str| -> Vec<usize> {
            let mut list = Vec::new();
            for segment in ["__DATA", "__DATA_CONST", "__DATA_DIRTY"] {
                if let Some(section) = loaded.image.section(segment, name) {
                    let start = section.address.wrapping_add(loaded.slide) as usize;
                    list.extend((0..section.size as usize / 8).map(|i| unsafe { read(start + i * 8) }));
                }
            }
            list
        };
        unsafe {
            for class in cells("__objc_classlist") {
                let meta = (*(class as *const ClassObject)).isa;
                add_list(&mut out, class, read(((*(class as *const ClassObject)).data & !7) + 32));
                add_list(&mut out, meta, read(((*(meta as *const ClassObject)).data & !7) + 32));
            }
            for category in cells("__objc_catlist") {
                let class = read(category + 8);
                let meta = lock().classes.get(&class).map(|r| r.partner).unwrap_or(0);
                add_list(&mut out, class, read(category + 16));
                add_list(&mut out, meta, read(category + 24));
            }
        }
        out
    })
}

/// The encoding of the method `class` (a metaclass for class methods)
/// answers `sel` with: found on the class that defines it, from the game's
/// method lists or our own table. None when no type is known: then there is
/// no signature (a guess could make NSInvocation copy more bytes than the
/// caller's buffer holds, or use the wrong registers).
pub fn method_types(class: Class, sel: Sel) -> Option<String> {
    let name = lock().sel_name(sel);
    let mut current = class;
    for _ in 0..256 {
        let (defines, class_name, meta, superclass) = {
            let rt = lock();
            let rec = rt.classes.get(&current)?;
            (rec.methods.contains_key(&sel), rec.name.clone(), rec.meta, rec.superclass)
        };
        if defines {
            if let Some(types) = game_types().get(&(current, name.clone())) {
                return Some(types.clone());
            }
            let ours = our_types();
            return ours
                .exact
                .get(&(class_name.as_str(), meta, name.as_str()))
                .copied()
                .or_else(|| ours.by_selector.get(&(meta, name.as_str())).copied().flatten())
                .map(String::from);
        }
        if superclass == 0 {
            return None;
        }
        current = superclass;
    }
    None
}

/// The signature for a method of `class` (a metaclass for class methods);
/// nil when the class has no such method or its types aren't known.
fn signature_for(class: Class, sel: Sel) -> Id {
    if lock().lookup(class, sel).is_none() {
        return 0;
    }
    match method_types(class, sel) {
        Some(types) => make_signature(&types),
        None => {
            let (name, class_name) = { let rt = lock(); (rt.sel_name(sel), rt.class_name(class)) };
            crate::log!("NSMethodSignature: no type encoding known for {class_name} {name}; nil");
            0
        }
    }
}

extern "C" fn method_signature_for_selector(this: Id, _: Sel, sel: Sel) -> Id {
    let class = lock().class_of(this);
    signature_for(class, sel)
}

extern "C" fn instance_method_signature_for_selector(this: Class, _: Sel, sel: Sel) -> Id {
    signature_for(this, sel)
}

// ----- NSInvocation: { isa, *mut Invocation } -----

struct Invocation {
    signature: Id,
    types: Arc<Signature>,
    /// One buffer per argument, self and _cmd included.
    arguments: Vec<Vec<u8>>,
    result: Vec<u8>,
    retained: bool,
    /// C strings copied by retainArguments (their pointers are in `arguments`).
    copies: Vec<std::ffi::CString>,
}

/// What a 32-bit game's invocations need from its bridge. Their argument
/// bytes are the guest's (objects and selectors as guest words).
pub trait GuestInvocations: Send + Sync {
    /// Sends the message `arguments` hold (self and _cmd first, by `types`)
    /// from the guest; the result's bytes as the guest has them.
    fn invoke(&self, types: &str, arguments: &[Vec<u8>]) -> Result<Vec<u8>, String>;
    fn object_to_host(&self, guest: u32) -> Id;
    fn object_to_guest(&self, host: Id) -> u32;
    fn selector_to_host(&self, guest: u32) -> Sel;
    fn selector_to_guest(&self, host: Sel) -> u32;
    /// -retainArguments for one argument of type `kind` ("@", "@?", "*"):
    /// the word the invocation keeps instead (retained, copied).
    fn keep(&self, kind: &str, word: u32) -> u32;
    /// A host message the 32-bit game object `frame.x[0]` forwards with an
    /// armv7 `signature`: converted, sent through -forwardInvocation:, and
    /// its result left in `frame`.
    fn forward_from_host(&self, frame: &mut crate::objc::forward::Frame, signature: Id) -> Result<(), String>;
}

static GUEST: RwLock<Option<Arc<dyn GuestInvocations>>> = RwLock::new(None);

/// Installs the 32-bit game's side (its bridge does, once set up).
pub fn set_guest_invocations(hooks: Arc<dyn GuestInvocations>) {
    *GUEST.write().unwrap_or_else(|e| e.into_inner()) = Some(hooks);
}

pub(crate) fn guest_invocations() -> Option<Arc<dyn GuestInvocations>> {
    let hooks = GUEST.read().unwrap_or_else(|e| e.into_inner()).clone();
    if hooks.is_none() {
        crate::log!("NSInvocation: a 32-bit invocation without the 32-bit bridge");
    }
    hooks
}

fn invocation(this: Id) -> Option<&'static mut Invocation> {
    let slot = unsafe { *((this + 8) as *const *mut Invocation) };
    if slot.is_null() {
        None
    } else {
        Some(unsafe { &mut *slot })
    }
}

/// [NSInvocation invocationWithMethodSignature:signature].
pub fn new_invocation(signature: Id) -> Id {
    inv_with_signature(runtime::class_named("NSInvocation"), 0, signature)
}

/// An invocation's argument bytes, self and _cmd first.
pub fn invocation_arguments(this: Id) -> Option<Vec<Vec<u8>>> {
    invocation(this).map(|state| state.arguments.clone())
}

/// Sets an invocation's arguments (self and _cmd first) from bytes laid
/// out as its signature says.
pub fn set_invocation_arguments(this: Id, values: &[Vec<u8>]) {
    let Some(state) = invocation(this) else { return };
    for (slot, value) in state.arguments.iter_mut().zip(values) {
        let n = slot.len().min(value.len());
        slot[..n].copy_from_slice(&value[..n]);
    }
}

/// An invocation's result bytes (as long as its return type, at least 16).
pub fn invocation_result(this: Id) -> Option<Vec<u8>> {
    invocation(this).map(|state| state.result.clone())
}

extern "C" fn inv_with_signature(class: Class, _: Sel, signature_object: Id) -> Id {
    let Some(types) = signature(signature_object) else {
        crate::log!("NSInvocation invocationWithMethodSignature: nil signature (NSInvalidArgumentException on iOS)");
        return 0;
    };
    let arguments = types.parts[1..].iter().map(|(_, t)| vec![0u8; t.size.max(8)]).collect();
    let result = vec![0u8; types.parts[0].1.size.max(16)];
    let object = runtime::alloc_instance(class);
    let state = Invocation { signature: signature_object, types, arguments, result, retained: false, copies: Vec::new() };
    unsafe { *((object + 8) as *mut *mut Invocation) = Box::into_raw(Box::new(state)) };
    object
}

extern "C" fn inv_signature(this: Id, _: Sel) -> Id {
    invocation(this).map(|i| i.signature).unwrap_or(0)
}

/// A pointer-sized argument as stored (a guest word for armv7).
fn stored_word(state: &Invocation, index: usize) -> usize {
    let Some(bytes) = state.arguments.get(index) else { return 0 };
    match state.types.layout {
        Layout::Arm64 => usize::from_le_bytes(bytes[..8].try_into().unwrap()),
        Layout::Armv7 => u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
    }
}

fn store_word(state: &mut Invocation, index: usize, value: usize) {
    let layout = state.types.layout;
    let Some(bytes) = state.arguments.get_mut(index) else { return };
    match layout {
        Layout::Arm64 => bytes[..8].copy_from_slice(&value.to_le_bytes()),
        Layout::Armv7 => bytes[..4].copy_from_slice(&(value as u32).to_le_bytes()),
    }
}

fn is_armv7(state: &Invocation) -> bool {
    state.types.layout == Layout::Armv7
}

extern "C" fn inv_target(this: Id, _: Sel) -> Id {
    let Some(state) = invocation(this) else { return 0 };
    let word = stored_word(state, 0);
    match is_armv7(state) {
        true => guest_invocations().map_or(0, |g| g.object_to_host(word as u32)),
        false => word,
    }
}

extern "C" fn inv_set_target(this: Id, _: Sel, target: Id) {
    let Some(state) = invocation(this) else { return };
    let word = match is_armv7(state) {
        true => guest_invocations().map_or(0, |g| g.object_to_guest(target) as usize),
        false => target,
    };
    store_word(state, 0, word);
    keep_argument(state, 0);
}

extern "C" fn inv_selector(this: Id, _: Sel) -> Sel {
    let Some(state) = invocation(this) else { return 0 };
    let word = stored_word(state, 1);
    match is_armv7(state) {
        true => guest_invocations().map_or(0, |g| g.selector_to_host(word as u32)),
        false => word,
    }
}

extern "C" fn inv_set_selector(this: Id, _: Sel, sel: Sel) {
    let Some(state) = invocation(this) else { return };
    let word = match is_armv7(state) {
        true => guest_invocations().map_or(0, |g| g.selector_to_guest(sel) as usize),
        false => sel,
    };
    store_word(state, 1, word);
}

extern "C" fn inv_set_argument(this: Id, _: Sel, pointer: *const u8, index: isize) {
    let Some(state) = invocation(this) else { return };
    let Some((_, ty)) = state.types.parts.get(index as usize + 1) else {
        crate::log!("NSInvocation setArgument:atIndex: {index} is out of range (NSInvalidArgumentException on iOS)");
        return;
    };
    let size = ty.size;
    if pointer.is_null() || index < 0 {
        return;
    }
    let source = unsafe { std::slice::from_raw_parts(pointer, size) };
    state.arguments[index as usize][..size].copy_from_slice(source);
    keep_argument(state, index as usize);
}

extern "C" fn inv_get_argument(this: Id, _: Sel, pointer: *mut u8, index: isize) {
    let Some(state) = invocation(this) else { return };
    let Some((_, ty)) = state.types.parts.get(index as usize + 1) else {
        crate::log!("NSInvocation getArgument:atIndex: {index} is out of range (NSInvalidArgumentException on iOS)");
        return;
    };
    if pointer.is_null() || index < 0 {
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(state.arguments[index as usize].as_ptr(), pointer, ty.size) };
}

extern "C" fn inv_get_return(this: Id, _: Sel, pointer: *mut u8) {
    let Some(state) = invocation(this) else { return };
    if !pointer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(state.result.as_ptr(), pointer, state.types.parts[0].1.size) };
    }
}

extern "C" fn inv_set_return(this: Id, _: Sel, pointer: *const u8) {
    let Some(state) = invocation(this) else { return };
    let size = state.types.parts[0].1.size;
    if !pointer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(pointer, state.result.as_mut_ptr(), size) };
    }
}

/// The type letter of a part, after its qualifiers (const, in, out...).
fn bare(part: &str) -> &str {
    part.trim_start_matches(|c| "rnNoORVAj".contains(c))
}

/// Once arguments are retained (Apple's contract): objects are retained,
/// blocks copied to the heap and C strings copied, so the invocation still
/// holds them after the caller's own copies are gone.
fn keep_argument(state: &mut Invocation, index: usize) {
    if !state.retained {
        return;
    }
    let Some((part, _)) = state.types.parts.get(index + 1) else { return };
    let kind = bare(part);
    let word = stored_word(state, index);
    if word == 0 {
        return;
    }
    if is_armv7(state) {
        let kind = ["@?", "@", "*"].into_iter().find(|k| kind.starts_with(k));
        if let (Some(kind), Some(guest)) = (kind, guest_invocations()) {
            let kept = guest.keep(kind, word as u32) as usize;
            store_word(state, index, kept);
        }
        return;
    }
    let kept = if kind.starts_with("@?") {
        crate::libsystem::mach::block_copy(word as *mut crate::libsystem::mach::BlockHeader) as usize
    } else if kind.starts_with('@') {
        runtime::send(word, "retain", &[])
    } else if kind.starts_with('*') {
        let text = unsafe { std::ffi::CStr::from_ptr(word as *const libc::c_char) }.to_owned();
        let pointer = text.as_ptr() as usize;
        state.copies.push(text);
        pointer
    } else {
        return;
    };
    state.arguments[index][..8].copy_from_slice(&kept.to_le_bytes());
}

extern "C" fn inv_retain_arguments(this: Id, _: Sel) {
    if let Some(state) = invocation(this) {
        if state.retained {
            return;
        }
        state.retained = true;
        for index in 0..state.arguments.len() {
            keep_argument(state, index);
        }
    }
}

extern "C" fn inv_arguments_retained(this: Id, _: Sel) -> bool {
    invocation(this).map(|s| s.retained).unwrap_or(false)
}

extern "C" fn inv_invoke(this: Id, _: Sel) {
    let Some(state) = invocation(this) else { return };
    let result_type = state.types.parts[0].1.clone();
    let mut result = vec![0u8; result_type.size.max(16)];
    if is_armv7(state) {
        let Some(guest) = guest_invocations() else { return };
        match guest.invoke(&state.types.text, &state.arguments) {
            Ok(bytes) => {
                let n = bytes.len().min(result.len());
                result[..n].copy_from_slice(&bytes[..n]);
            }
            Err(error) => crate::log!("NSInvocation -invoke ({}): {error}", state.types.text),
        }
    } else {
        let types: Vec<Type> = state.types.parts[1..].iter().map(|(_, t)| t.clone()).collect();
        call(runtime::ios_objc_msgSend as usize, &types, &state.arguments, &result_type, &mut result);
    }
    state.result = result;
    // A retained invocation also keeps its object result.
    if state.retained && bare(&state.types.parts[0].0).starts_with('@') {
        if is_armv7(state) {
            let object = u32::from_le_bytes(state.result[..4].try_into().unwrap());
            if let (true, Some(guest)) = (object != 0, guest_invocations()) {
                guest.keep("@", object);
            }
            return;
        }
        let object = usize::from_le_bytes(state.result[..8].try_into().unwrap());
        if object != 0 {
            runtime::send(object, "retain", &[]);
        }
    }
}

extern "C" fn inv_invoke_with_target(this: Id, s: Sel, target: Id) {
    inv_set_target(this, s, target);
    inv_invoke(this, s);
}

/// NSGetSizeAndAlignment(type, &size, &alignment): one type's size and
/// alignment (either output may be NULL); returns the type after it, past
/// its offset digits.
extern "C" fn get_size_and_alignment(types: *const libc::c_char, size: *mut usize, align: *mut usize) -> *const libc::c_char {
    if types.is_null() {
        return types;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(types) }.to_bytes();
    let mut i = 0;
    let ty = parse_type(text, &mut i);
    skip_digits(text, &mut i);
    unsafe {
        if !size.is_null() {
            *size = ty.size;
        }
        if !align.is_null() {
            *align = ty.align;
        }
        types.add(i.min(text.len()))
    }
}

pub fn register(r: &mut Registry) {
    r.function("NSGetSizeAndAlignment", get_size_and_alignment as usize);
    let signature_class = define_class("NSMethodSignature", "NSObject", 16);
    class_methods(signature_class, &[("signatureWithObjCTypes:", ms_with_types as usize)]);
    methods(
        signature_class,
        &[
            ("numberOfArguments", ms_count as usize),
            ("getArgumentTypeAtIndex:", ms_argument_type as usize),
            ("methodReturnType", ms_return_type as usize),
            ("methodReturnLength", ms_return_length as usize),
            ("frameLength", ms_frame_length as usize),
            ("isOneway", ms_is_oneway as usize),
            ("isEqual:", ms_is_equal as usize),
            ("copyWithZone:", ms_copy as usize),
        ],
    );
    let invocation_class = define_class("NSInvocation", "NSObject", 16);
    class_methods(invocation_class, &[("invocationWithMethodSignature:", inv_with_signature as usize)]);
    methods(
        invocation_class,
        &[
            ("methodSignature", inv_signature as usize),
            ("target", inv_target as usize),
            ("setTarget:", inv_set_target as usize),
            ("selector", inv_selector as usize),
            ("setSelector:", inv_set_selector as usize),
            ("setArgument:atIndex:", inv_set_argument as usize),
            ("getArgument:atIndex:", inv_get_argument as usize),
            ("getReturnValue:", inv_get_return as usize),
            ("setReturnValue:", inv_set_return as usize),
            ("retainArguments", inv_retain_arguments as usize),
            ("argumentsRetained", inv_arguments_retained as usize),
            ("invoke", inv_invoke as usize),
            ("invokeWithTarget:", inv_invoke_with_target as usize),
        ],
    );
    let root = runtime::class_named("NSObject");
    methods(root, &[("methodSignatureForSelector:", method_signature_for_selector as usize)]);
    class_methods(
        root,
        &[
            ("instanceMethodSignatureForSelector:", instance_method_signature_for_selector as usize),
            ("methodSignatureForSelector:", method_signature_for_selector as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Big {
        a: u64,
        b: u64,
        c: u64,
    }

    /// Ten integers (two on the stack), a float and a CGRect in d registers,
    /// and a big struct by reference. The stack ones are 8 bytes wide: this
    /// Linux function packs smaller ones differently from iOS.
    extern "C" fn target(a: i64, b: i8, c: i32, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64, j: i64, x: f32, r: Rect, big: Big) -> Rect {
        Rect {
            x: (a + b as i64 + c as i64 + d + e + f + g + h) as f64,
            y: (i * 1000 + j) as f64,
            w: x as f64 + r.x + r.y + r.w + r.h,
            h: (big.a + big.b + big.c) as f64,
        }
    }

    #[test]
    fn invocations_follow_the_arm64_rules() {
        let signature = parse_signature("{CGRect={CGPoint=dd}{CGSize=dd}}96q8c16i20q24q32q40q48q56q64q72f80{CGRect={CGPoint=dd}{CGSize=dd}}84{Big=QQQ}116");
        assert_eq!(signature.parts.len(), 14);
        assert_eq!(signature.parts[0].1.floats, Some((4, 8)));
        assert_eq!(signature.parts[13].1.size, 24);
        let rect = Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.5 };
        let big = Big { a: 10, b: 20, c: 30 };
        let raw = |bytes: &[u8]| bytes.to_vec();
        let values = vec![
            raw(&1i64.to_le_bytes()),
            raw(&(-2i8).to_le_bytes()),
            raw(&3i32.to_le_bytes()),
            raw(&4i64.to_le_bytes()),
            raw(&5i64.to_le_bytes()),
            raw(&6i64.to_le_bytes()),
            raw(&7i64.to_le_bytes()),
            raw(&8i64.to_le_bytes()),
            raw(&(-9i64).to_le_bytes()),
            raw(&11i64.to_le_bytes()),
            raw(&0.5f32.to_le_bytes()),
            unsafe { std::slice::from_raw_parts(&rect as *const Rect as *const u8, 32) }.to_vec(),
            unsafe { std::slice::from_raw_parts(&big as *const Big as *const u8, 24) }.to_vec(),
        ];
        let types: Vec<Type> = signature.parts[1..].iter().map(|(_, t)| t.clone()).collect();
        let mut result = vec![0u8; 32];
        call(target as usize, &types, &values, &signature.parts[0].1, &mut result);
        let got: Rect = unsafe { std::ptr::read_unaligned(result.as_ptr() as *const Rect) };
        assert_eq!(got, Rect { x: 32.0, y: -8989.0, w: 11.0, h: 60.0 });

        // Our own methods' encodings come from their Rust signatures.
        let ours = our_types();
        assert_eq!(ours.exact.get(&("NSNumber", false, "boolValue")), Some(&"B@:"));
        assert_eq!(ours.exact.get(&("UIView", false, "setFrame:")), Some(&"v@:{CGRect={CGPoint=dd}{CGSize=dd}}"));
        // Only types known for sure: a Rust helper's loose types (an i64 for
        // an int, an Id for a BOOL) give no signature rather than a wrong one.
        let known = |class: &str, meta: bool, selector: &str| {
            ours.exact.get(&(class, meta, selector)).copied().or_else(|| ours.by_selector.get(&(meta, selector)).copied().flatten())
        };
        assert!(matches!(known("NSNumber", false, "intValue"), None | Some("i@:")), "{:?}", known("NSNumber", false, "intValue"));
        assert!(matches!(known("UIView", false, "endEditing:"), None | Some("B@:B")), "{:?}", known("UIView", false, "endEditing:"));
        assert_eq!(known("NSOperation", false, "setCompletionBlock:"), Some("v@:@?"));
        assert_eq!(known("NSString", false, "initWithUTF8String:"), Some("@@:*"));
        assert_eq!(known("NSDate", false, "copyWithZone:"), Some("@@:^{_NSZone=}"));

        // retainArguments copies C strings: the caller's buffer may go away.
        let types = Arc::new(parse_signature("v@:*"));
        let mut caller = *b"hello\0";
        let mut state = Invocation { signature: 0, types, arguments: vec![vec![0; 8]; 3], result: vec![0; 16], retained: false, copies: Vec::new() };
        state.arguments[2][..8].copy_from_slice(&(caller.as_ptr() as usize).to_le_bytes());
        state.retained = true;
        keep_argument(&mut state, 2);
        caller[0] = b'X';
        let kept = usize::from_le_bytes(state.arguments[2][..8].try_into().unwrap());
        assert_ne!(kept, caller.as_ptr() as usize);
        assert_eq!(unsafe { std::ffi::CStr::from_ptr(kept as *const libc::c_char) }.to_str(), Ok("hello"));
    }
}
