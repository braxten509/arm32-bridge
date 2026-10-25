//! Converting message arguments and results between armv7 guest code and
//! the 64-bit host layer, driven by type encodings.
//!
//! Values travel as bytes in each side's own layout: guest bytes (armv7:
//! 4-byte pointers, CGFloat float, NSUInteger 4 bytes) are turned into host
//! bytes (8-byte pointers, CGFloat double, NSUInteger 8 bytes inside the
//! structs that use it), then placed into AAPCS64 registers (integer and
//! floating-point registers are counted separately; homogeneous float
//! structs of up to four members go in d registers; other structs up to 16
//! bytes in x registers, larger ones by reference) — and back.
//!
//! Everything that needs guest memory or object identity goes through
//! [`Boundary`], so this file runs without the translator.

use super::encoding::{align_up, Signature, Ty};
use super::selectors::{IdArrays, Varargs};

/// What the conversion needs from the world around it.
pub trait Boundary {
    fn read(&self, addr: u32, len: u32) -> Vec<u8>;
    fn write(&mut self, addr: u32, data: &[u8]);
    /// Guest memory that stays valid until [`Boundary::free`] (or, for
    /// copied C strings, until the boundary decides; see its docs).
    fn alloc(&mut self, size: u32) -> u32;
    fn free(&mut self, addr: u32);
    /// Where guest address `addr` is in host memory.
    fn host_address(&self, addr: u32) -> usize;
    /// The guest address of a host pointer into guest memory.
    fn guest_address(&self, host: usize) -> Option<u32>;
    fn id_to_host(&mut self, guest: u32) -> Result<usize, String>;
    fn id_to_guest(&mut self, host: usize) -> Result<u32, String>;
    fn sel_to_host(&mut self, guest: u32) -> Result<usize, String>;
    fn sel_to_guest(&mut self, host: usize) -> Result<u32, String>;
    /// The text of a guest string object (for format arguments).
    fn object_text(&mut self, guest: u32) -> Option<String>;
    /// The guest word standing for a host pointer outside guest memory (an
    /// opaque framework object such as a CGImage, or a handle).
    fn opaque_to_guest(&mut self, host: usize) -> Result<u32, String>;
    /// The host pointer a guest pointer word stands for, when it is such a
    /// word (otherwise it is a guest address).
    fn opaque_to_host(&mut self, guest: u32) -> Option<usize>;
}

// ---------- host types ----------

#[derive(Clone, Debug, PartialEq)]
pub enum HostTy {
    Int { size: u32, signed: bool },
    F32,
    F64,
    Ptr,
    Struct { fields: Vec<(u32, HostTy)>, size: u32, align: u32 },
}

/// Struct names whose float fields are CGFloat (double on the host).
const CGFLOAT_STRUCTS: &[&str] = &["CGPoint", "CGSize", "CGRect", "CGAffineTransform", "UIEdgeInsets", "CGVector", "CATransform3D"];
/// Struct names whose int fields are NSInteger/NSUInteger (8 bytes on the host).
const WIDE_INT_STRUCTS: &[&str] = &["_NSRange", "NSFastEnumerationState"];

#[derive(Clone, Copy, Default)]
struct Ctx {
    cg: bool,
    wide: bool,
    scalar: bool,
    real_float: bool,
}

impl HostTy {
    pub fn size(&self) -> u32 {
        match self {
            HostTy::Int { size, .. } => *size,
            HostTy::F32 => 4,
            HostTy::F64 | HostTy::Ptr => 8,
            HostTy::Struct { size, .. } => *size,
        }
    }

    pub fn align(&self) -> u32 {
        match self {
            HostTy::Struct { align, .. } => *align,
            other => other.size().max(1),
        }
    }

    /// A homogeneous floating-point aggregate: (element, count).
    fn hfa(&self) -> Option<(HostTy, u32)> {
        let HostTy::Struct { fields, .. } = self else { return None };
        let mut leaves = Vec::new();
        fn flatten(t: &HostTy, out: &mut Vec<HostTy>) {
            match t {
                HostTy::Struct { fields, .. } => fields.iter().for_each(|(_, f)| flatten(f, out)),
                other => out.push(other.clone()),
            }
        }
        fields.iter().for_each(|(_, f)| flatten(f, &mut leaves));
        let first = leaves.first()?.clone();
        let uniform = matches!(first, HostTy::F32 | HostTy::F64) && leaves.iter().all(|l| *l == first);
        (uniform && (1..=4).contains(&leaves.len())).then(|| (first, leaves.len() as u32))
    }
}

/// The host type of a scalar argument or result (`real_float`: a scalar
/// `f` is a C float rather than CGFloat).
pub fn host_ty(ty: &Ty, real_float: bool) -> Result<HostTy, String> {
    host_ty_in(ty, Ctx { scalar: true, real_float, ..Ctx::default() })
}

fn host_ty_in(ty: &Ty, ctx: Ctx) -> Result<HostTy, String> {
    Ok(match ty {
        Ty::Void => HostTy::Int { size: 0, signed: false },
        Ty::Char { signed } => HostTy::Int { size: 1, signed: *signed },
        Ty::Bool => HostTy::Int { size: 1, signed: false },
        Ty::Short { signed } => HostTy::Int { size: 2, signed: *signed },
        // Scalars travel in 64-bit registers; widening suits both int and NSInteger.
        Ty::Int { signed } => HostTy::Int { size: if ctx.scalar || ctx.wide { 8 } else { 4 }, signed: *signed },
        Ty::LongLong { signed } => HostTy::Int { size: 8, signed: *signed },
        Ty::Float if ctx.scalar => {
            if ctx.real_float {
                HostTy::F32
            } else {
                HostTy::F64
            }
        }
        Ty::Float => {
            if ctx.cg {
                HostTy::F64
            } else {
                HostTy::F32
            }
        }
        Ty::Double => HostTy::F64,
        Ty::Object | Ty::Block | Ty::Class | Ty::Selector | Ty::CString | Ty::Pointer(_) | Ty::Unknown => HostTy::Ptr,
        Ty::Struct { name, fields } => {
            let inner = Ctx {
                cg: ctx.cg || CGFLOAT_STRUCTS.contains(&name.as_str()),
                wide: ctx.wide || WIDE_INT_STRUCTS.contains(&name.as_str()),
                scalar: false,
                real_float: false,
            };
            let tys = fields.iter().map(|f| host_ty_in(f, inner)).collect::<Result<Vec<_>, _>>()?;
            layout(tys)
        }
        Ty::Array { count, element } => {
            let e = host_ty_in(element, Ctx { scalar: false, ..ctx })?;
            layout(vec![e; *count as usize])
        }
        Ty::Union { name, .. } => return Err(format!("union {name} cannot cross the boundary")),
    })
}

fn layout(tys: Vec<HostTy>) -> HostTy {
    let mut offset = 0;
    let mut align = 1;
    let mut fields = Vec::with_capacity(tys.len());
    for t in tys {
        offset = align_up(offset, t.align());
        align = align.max(t.align());
        let size = t.size();
        fields.push((offset, t));
        offset += size;
    }
    HostTy::Struct { fields, size: align_up(offset, align), align }
}

// ---------- value conversion ----------

fn read_int(bytes: &[u8], size: usize, signed: bool) -> i128 {
    let mut raw = [0u8; 16];
    raw[..size].copy_from_slice(&bytes[..size]);
    let value = u128::from_le_bytes(raw);
    if signed && size > 0 && size < 16 && bytes[size - 1] & 0x80 != 0 {
        (value | (!0u128 << (size * 8))) as i128
    } else {
        value as i128
    }
}

fn put(out: &mut [u8], value: u64) {
    let n = out.len().min(8);
    out[..n].copy_from_slice(&value.to_le_bytes()[..n]);
}

fn guest_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn host_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

/// NSNotFound is NSIntegerMax, 0x7fffffff in a 32-bit game. It is only
/// turned into the host's where it means "not found": NSRange locations and
/// index results (`not-found` rows); other numbers convert as they are.
fn not_found_to_host(host: &mut [u8]) {
    if host_u64(host) == i32::MAX as u64 {
        put(host, i64::MAX as u64)
    }
}

fn not_found_to_guest(host: &[u8], guest: &mut [u8]) {
    if host_u64(host) == i64::MAX as u64 {
        guest[..4].copy_from_slice(&(i32::MAX as u32).to_le_bytes())
    }
}

/// Guest bytes of `ty` -> host bytes of `hty`.
fn to_host(ty: &Ty, hty: &HostTy, guest: &[u8], out: &mut [u8], b: &mut dyn Boundary) -> Result<(), String> {
    match (ty, hty) {
        (Ty::Char { .. } | Ty::Short { .. } | Ty::Int { .. } | Ty::LongLong { .. } | Ty::Bool, HostTy::Int { size, .. }) => {
            let signed = matches!(ty, Ty::Char { signed: true } | Ty::Short { signed: true } | Ty::Int { signed: true } | Ty::LongLong { signed: true });
            let value = read_int(guest, ty.guest_size() as usize, signed);
            out[..*size as usize].copy_from_slice(&(value as u128).to_le_bytes()[..*size as usize]);
        }
        (Ty::Float, HostTy::F64) => put(out, f64::from(f32::from_le_bytes(guest[..4].try_into().unwrap())).to_bits()),
        (Ty::Float, HostTy::F32) | (Ty::Double, HostTy::F64) => out.copy_from_slice(&guest[..out.len()]),
        (Ty::Object | Ty::Block | Ty::Class, HostTy::Ptr) => put(out, b.id_to_host(guest_u32(guest))? as u64),
        (Ty::Selector, HostTy::Ptr) => {
            let g = guest_u32(guest);
            put(out, if g == 0 { 0 } else { b.sel_to_host(g)? as u64 })
        }
        (Ty::CString, HostTy::Ptr) => {
            let g = guest_u32(guest);
            put(out, if g == 0 { 0 } else { b.host_address(g) as u64 })
        }
        // A guest address, or the guest word of an opaque host pointer.
        (Ty::Pointer(_) | Ty::Unknown, HostTy::Ptr) => {
            let g = guest_u32(guest);
            let host = match g {
                0 => 0,
                _ => b.opaque_to_host(g).unwrap_or_else(|| b.host_address(g)),
            };
            put(out, host as u64)
        }
        (Ty::Struct { name, fields }, HostTy::Struct { fields: host_fields, .. }) => {
            let mut offset = 0;
            for (i, (f, (host_offset, hf))) in fields.iter().zip(host_fields).enumerate() {
                offset = align_up(offset, f.guest_align());
                let size = f.guest_size() as usize;
                let o = *host_offset as usize;
                to_host(f, hf, &guest[offset as usize..offset as usize + size], &mut out[o..o + hf.size() as usize], b)?;
                if i == 0 && name == "_NSRange" {
                    not_found_to_host(&mut out[o..o + 8]);
                }
                offset += size as u32;
            }
        }
        (Ty::Array { element, .. }, HostTy::Struct { fields, .. }) => {
            let step = element.guest_size() as usize;
            for (i, (o, hf)) in fields.iter().enumerate() {
                let o = *o as usize;
                to_host(element, hf, &guest[i * step..(i + 1) * step], &mut out[o..o + hf.size() as usize], b)?;
            }
        }
        (ty, hty) => return Err(format!("cannot pass {ty:?} as {hty:?}")),
    }
    Ok(())
}

/// Host bytes of `hty` -> guest bytes of `ty`.
fn to_guest(ty: &Ty, hty: &HostTy, host: &[u8], out: &mut [u8], b: &mut dyn Boundary) -> Result<(), String> {
    match (ty, hty) {
        (Ty::Char { .. } | Ty::Short { .. } | Ty::Int { .. } | Ty::LongLong { .. } | Ty::Bool, HostTy::Int { .. }) => {
            out.copy_from_slice(&host[..out.len()])
        }
        (Ty::Float, HostTy::F64) => {
            out.copy_from_slice(&(f64::from_bits(host_u64(host)) as f32).to_le_bytes());
        }
        (Ty::Float, HostTy::F32) | (Ty::Double, HostTy::F64) => out.copy_from_slice(&host[..out.len()]),
        (Ty::Object | Ty::Block | Ty::Class, HostTy::Ptr) => {
            out.copy_from_slice(&b.id_to_guest(host_u64(host) as usize)?.to_le_bytes())
        }
        (Ty::Selector, HostTy::Ptr) => {
            let h = host_u64(host) as usize;
            out.copy_from_slice(&(if h == 0 { 0 } else { b.sel_to_guest(h)? }).to_le_bytes())
        }
        (Ty::CString, HostTy::Ptr) => {
            let h = host_u64(host) as usize;
            let g = match (h, b.guest_address(h)) {
                (0, _) => 0,
                (_, Some(g)) => g,
                _ => {
                    let text = unsafe { std::ffi::CStr::from_ptr(h as *const libc::c_char) }.to_bytes_with_nul().to_vec();
                    let g = b.alloc(text.len() as u32);
                    b.write(g, &text);
                    g
                }
            };
            out.copy_from_slice(&g.to_le_bytes())
        }
        (Ty::Pointer(_) | Ty::Unknown, HostTy::Ptr) => {
            let h = host_u64(host) as usize;
            let g = match (h, b.guest_address(h)) {
                (0, _) => 0,
                (_, Some(g)) => g,
                // CGImage, CGColor, ...: a word standing for the host pointer.
                _ => b.opaque_to_guest(h)?,
            };
            out.copy_from_slice(&g.to_le_bytes())
        }
        (Ty::Struct { name, fields }, HostTy::Struct { fields: host_fields, .. }) => {
            let mut offset = 0;
            for (i, (f, (host_offset, hf))) in fields.iter().zip(host_fields).enumerate() {
                offset = align_up(offset, f.guest_align());
                let size = f.guest_size() as usize;
                let o = *host_offset as usize;
                let guest = &mut out[offset as usize..offset as usize + size];
                to_guest(f, hf, &host[o..o + hf.size() as usize], guest, b)?;
                if i == 0 && name == "_NSRange" {
                    not_found_to_guest(&host[o..o + 8], guest);
                }
                offset += size as u32;
            }
        }
        (Ty::Array { element, .. }, HostTy::Struct { fields, .. }) => {
            let step = element.guest_size() as usize;
            for (i, (o, hf)) in fields.iter().enumerate() {
                let o = *o as usize;
                to_guest(element, hf, &host[o..o + hf.size() as usize], &mut out[i * step..(i + 1) * step], b)?;
            }
        }
        (ty, hty) => return Err(format!("cannot return {ty:?} from {hty:?}")),
    }
    Ok(())
}

/// A float in a d register: a C float leaves the upper half zero (writing
/// an s register clears it), a double never does unless it is 0.0 or a
/// denormal no CGFloat holds. Returned as f64 bits.
fn float_register_as_double(raw: u64) -> u64 {
    if raw >> 32 == 0 {
        f64::from(f32::from_bits(raw as u32)).to_bits()
    } else {
        raw
    }
}

// ---------- AAPCS64 placement ----------

/// Registers and stack slots for a host call.
#[derive(Default, Debug)]
pub struct HostArgs {
    /// x0-x7, and x8 (the indirect result address).
    pub x: [u64; 9],
    pub d: [u64; 8],
    pub stack: Vec<u64>,
    ngrn: usize,
    nsrn: usize,
    /// Copies of large structs passed by reference.
    temps: Vec<Box<[u64]>>,
}

fn words_of(bytes: &[u8]) -> Vec<u64> {
    bytes.chunks(8).map(|c| {
        let mut raw = [0u8; 8];
        raw[..c.len()].copy_from_slice(c);
        u64::from_le_bytes(raw)
    }).collect()
}

impl HostArgs {
    fn push_x(&mut self, value: u64) {
        if self.ngrn < 8 {
            self.x[self.ngrn] = value;
            self.ngrn += 1;
        } else {
            self.stack.push(value);
        }
    }

    fn push_d(&mut self, value: u64) {
        if self.nsrn < 8 {
            self.d[self.nsrn] = value;
            self.nsrn += 1;
        } else {
            self.stack.push(value);
        }
    }

    fn push(&mut self, hty: &HostTy, bytes: &[u8]) {
        match hty {
            HostTy::Int { .. } | HostTy::Ptr => self.push_x(words_of(bytes)[0]),
            HostTy::F32 | HostTy::F64 => self.push_d(words_of(bytes)[0]),
            HostTy::Struct { size, .. } => {
                if let Some((element, count)) = hty.hfa() {
                    if self.nsrn + count as usize <= 8 {
                        let step = element.size() as usize;
                        for i in 0..count as usize {
                            self.push_d(words_of(&bytes[i * step..(i + 1) * step])[0]);
                        }
                    } else {
                        self.nsrn = 8;
                        self.stack.extend(words_of(bytes));
                    }
                } else if *size <= 16 {
                    let words = words_of(bytes);
                    if self.ngrn + words.len() <= 8 {
                        words.into_iter().for_each(|w| self.push_x(w));
                    } else {
                        self.ngrn = 8;
                        self.stack.extend(words);
                    }
                } else {
                    let copy: Box<[u64]> = words_of(bytes).into_boxed_slice();
                    let address = copy.as_ptr() as u64;
                    self.temps.push(copy);
                    self.push_x(address);
                }
            }
        }
    }
}

/// The incoming side of the same rules: reads arguments a host caller placed.
struct ArgReader<'a> {
    frame: &'a HostFrame,
    ngrn: usize,
    nsrn: usize,
    slot: usize,
}

impl ArgReader<'_> {
    fn stack_word(&mut self) -> u64 {
        let value = if self.frame.stack == 0 {
            0
        } else {
            unsafe { (self.frame.stack as *const u64).add(self.slot).read_unaligned() }
        };
        self.slot += 1;
        value
    }

    fn x(&mut self) -> u64 {
        if self.ngrn < 8 {
            self.ngrn += 1;
            self.frame.x[self.ngrn - 1]
        } else {
            self.stack_word()
        }
    }

    fn d(&mut self) -> u64 {
        if self.nsrn < 8 {
            self.nsrn += 1;
            self.frame.d[self.nsrn - 1]
        } else {
            self.stack_word()
        }
    }

    fn take(&mut self, hty: &HostTy, scalar_float: bool) -> Vec<u8> {
        let size = hty.size() as usize;
        let mut out = match hty {
            HostTy::Int { .. } | HostTy::Ptr => self.x().to_le_bytes().to_vec(),
            HostTy::F32 | HostTy::F64 if scalar_float => float_register_as_double(self.d()).to_le_bytes().to_vec(),
            HostTy::F32 | HostTy::F64 => self.d().to_le_bytes().to_vec(),
            HostTy::Struct { size: total, .. } => {
                let words = (*total as usize).div_ceil(8);
                if let Some((element, count)) = hty.hfa() {
                    let step = element.size() as usize;
                    let mut bytes = vec![0u8; size];
                    if self.nsrn + count as usize <= 8 {
                        for i in 0..count as usize {
                            bytes[i * step..(i + 1) * step].copy_from_slice(&self.d().to_le_bytes()[..step]);
                        }
                    } else {
                        self.nsrn = 8;
                        let raw: Vec<u8> = (0..words).flat_map(|_| self.stack_word().to_le_bytes()).collect();
                        bytes.copy_from_slice(&raw[..size]);
                    }
                    bytes
                } else if *total <= 16 {
                    let raw: Vec<u8> = if self.ngrn + words <= 8 {
                        (0..words).flat_map(|_| self.x().to_le_bytes()).collect()
                    } else {
                        self.ngrn = 8;
                        (0..words).flat_map(|_| self.stack_word().to_le_bytes()).collect()
                    };
                    raw
                } else {
                    let address = self.x() as *const u8;
                    unsafe { std::slice::from_raw_parts(address, size) }.to_vec()
                }
            }
        };
        out.resize(size.max(8), 0);
        out
    }
}

// ---------- frames ----------

/// What a host function returns in registers.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct HostRet {
    pub x: [u64; 2],
    pub d: [u64; 4],
}

/// The registers of a host call into a guest method (filled by the
/// trampoline): x0-x8, d0-d7, the first stack argument's address, and the
/// result registers to load on return.
#[derive(Debug, Default)]
#[repr(C)]
pub struct HostFrame {
    pub x: [u64; 9],
    pub d: [u64; 8],
    pub stack: usize,
    pub ret: HostRet,
}

fn needs_indirect_result(hty: &HostTy) -> bool {
    matches!(hty, HostTy::Struct { size, .. } if *size > 16 && hty.hfa().is_none())
}

fn host_result_bytes(hty: &HostTy, ret: &HostRet, scalar_float: bool, indirect: Option<&[u64]>) -> Vec<u8> {
    let size = hty.size() as usize;
    let mut out: Vec<u8> = match hty {
        HostTy::Int { .. } | HostTy::Ptr => ret.x[0].to_le_bytes().to_vec(),
        HostTy::F32 | HostTy::F64 if scalar_float => float_register_as_double(ret.d[0]).to_le_bytes().to_vec(),
        HostTy::F32 | HostTy::F64 => ret.d[0].to_le_bytes().to_vec(),
        HostTy::Struct { .. } => match (hty.hfa(), indirect) {
            (Some((element, count)), _) => {
                let step = element.size() as usize;
                (0..count as usize).flat_map(|i| ret.d[i].to_le_bytes()[..step].to_vec()).collect()
            }
            (None, Some(buffer)) => buffer.iter().flat_map(|w| w.to_le_bytes()).collect(),
            (None, None) => ret.x.iter().flat_map(|w| w.to_le_bytes()).collect(),
        },
    };
    out.resize(size.max(8), 0);
    out
}

fn set_host_result(hty: &HostTy, bytes: &[u8], frame: &mut HostFrame) {
    match hty {
        HostTy::Int { .. } | HostTy::Ptr => frame.ret.x[0] = words_of(bytes)[0],
        HostTy::F32 | HostTy::F64 => frame.ret.d[0] = words_of(bytes)[0],
        HostTy::Struct { size, .. } => {
            if let Some((element, count)) = hty.hfa() {
                let step = element.size() as usize;
                for i in 0..count as usize {
                    frame.ret.d[i] = words_of(&bytes[i * step..(i + 1) * step])[0];
                }
            } else if *size <= 16 {
                for (i, w) in words_of(bytes).into_iter().enumerate().take(2) {
                    frame.ret.x[i] = w;
                }
            } else {
                let destination = frame.x[8] as *mut u8;
                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, *size as usize) };
            }
        }
    }
}

// ---------- guest words ----------

fn guest_bytes(words: &dyn Fn(u32) -> u32, first: u32, size: u32) -> Vec<u8> {
    let mut out: Vec<u8> = (0..size.div_ceil(4).max(1)).flat_map(|i| words(first + i).to_le_bytes()).collect();
    out.truncate(size.max(1) as usize);
    out
}

fn is_float_scalar(ty: &Ty) -> bool {
    matches!(ty, Ty::Float)
}

/// The armv7 r0/r1 pair (or the stret pointer) a guest caller gets back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestRet {
    pub r0: u32,
    pub r1: u32,
}

fn guest_ret_from_bytes(ty: &Ty, bytes: &[u8]) -> GuestRet {
    let mut raw = [0u8; 8];
    let n = bytes.len().min(8);
    raw[..n].copy_from_slice(&bytes[..n]);
    // Small integers come back widened to a whole register.
    let r0 = match ty {
        Ty::Char { signed: true } => raw[0] as i8 as i32 as u32,
        Ty::Char { signed: false } | Ty::Bool => u32::from(raw[0]),
        Ty::Short { signed: true } => i16::from_le_bytes([raw[0], raw[1]]) as i32 as u32,
        Ty::Short { signed: false } => u32::from(u16::from_le_bytes([raw[0], raw[1]])),
        _ => u32::from_le_bytes(raw[..4].try_into().unwrap()),
    };
    GuestRet { r0, r1: u32::from_le_bytes(raw[4..8].try_into().unwrap()) }
}

// ---------- guest -> host ----------

/// A pointer argument whose pointee has a different host layout: the host
/// gets a temporary, copied back into guest memory after the call.
struct OutParam {
    guest: u32,
    pointee: Ty,
    hty: HostTy,
    /// Elements (more than one for counted id arrays).
    count: u32,
    write_back: bool,
    temp: Box<[u64]>,
}

impl OutParam {
    /// The guest address a host pointer into this temporary stands for.
    fn guest_address(&self, host: usize) -> Option<u32> {
        let (start, step) = (self.temp.as_ptr() as usize, self.hty.size() as usize);
        let offset = host.checked_sub(start).filter(|o| *o < step * self.count as usize)?;
        let inner = guest_offset(&self.pointee, &self.hty, (offset % step) as u32)?;
        Some(self.guest + (offset / step) as u32 * self.pointee.guest_size() + inner)
    }
}

/// The guest offset of the field at host offset `offset` (it must start there).
fn guest_offset(ty: &Ty, hty: &HostTy, offset: u32) -> Option<u32> {
    let (guest_fields, host_fields): (Vec<&Ty>, _) = match (ty, hty) {
        (Ty::Struct { fields, .. }, HostTy::Struct { fields: host_fields, .. }) => (fields.iter().collect(), host_fields),
        (Ty::Array { element, count }, HostTy::Struct { fields: host_fields, .. }) => {
            (std::iter::repeat_n(&**element, *count as usize).collect(), host_fields)
        }
        _ => return (offset == 0).then_some(0),
    };
    let mut guest = 0;
    for (f, (host, hf)) in guest_fields.into_iter().zip(host_fields) {
        guest = align_up(guest, f.guest_align());
        if (*host..host + hf.size()).contains(&offset) {
            return Some(guest + guest_offset(f, hf, offset - host)?);
        }
        guest += f.guest_size();
    }
    None
}

/// The boundary with pointers into out-parameter temporaries mapped back to
/// the guest memory they stand for (fast enumeration's itemsPtr and
/// mutationsPtr point into them).
struct TempMap<'a> {
    b: &'a mut dyn Boundary,
    outs: &'a [OutParam],
}

impl Boundary for TempMap<'_> {
    fn read(&self, addr: u32, len: u32) -> Vec<u8> {
        self.b.read(addr, len)
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        self.b.write(addr, data)
    }
    fn alloc(&mut self, size: u32) -> u32 {
        self.b.alloc(size)
    }
    fn free(&mut self, addr: u32) {
        self.b.free(addr)
    }
    fn host_address(&self, addr: u32) -> usize {
        self.b.host_address(addr)
    }
    fn guest_address(&self, host: usize) -> Option<u32> {
        self.outs.iter().find_map(|o| o.guest_address(host)).or_else(|| self.b.guest_address(host))
    }
    fn id_to_host(&mut self, guest: u32) -> Result<usize, String> {
        self.b.id_to_host(guest)
    }
    fn id_to_guest(&mut self, host: usize) -> Result<u32, String> {
        self.b.id_to_guest(host)
    }
    fn sel_to_host(&mut self, guest: u32) -> Result<usize, String> {
        self.b.sel_to_host(guest)
    }
    fn sel_to_guest(&mut self, host: usize) -> Result<u32, String> {
        self.b.sel_to_guest(host)
    }
    fn object_text(&mut self, guest: u32) -> Option<String> {
        self.b.object_text(guest)
    }
    fn opaque_to_guest(&mut self, host: usize) -> Result<u32, String> {
        self.b.opaque_to_guest(host)
    }
    fn opaque_to_host(&mut self, guest: u32) -> Option<usize> {
        self.b.opaque_to_host(guest)
    }
}

/// Pointees that need a temporary: anything holding ids, pointers or
/// integers that may be NSInteger.
fn out_param_type(pointee: &Ty) -> Option<HostTy> {
    match pointee {
        Ty::Object | Ty::Block | Ty::Class | Ty::Selector | Ty::CString | Ty::Pointer(_) => Some(HostTy::Ptr),
        Ty::Int { signed } => Some(HostTy::Int { size: 8, signed: *signed }),
        Ty::Struct { .. } => {
            let hty = host_ty_in(pointee, Ctx::default()).ok()?;
            let differs = hty.size() != pointee.guest_size() || contains_pointer(&hty);
            differs.then_some(hty)
        }
        _ => None,
    }
}

fn contains_pointer(hty: &HostTy) -> bool {
    match hty {
        HostTy::Ptr => true,
        HostTy::Struct { fields, .. } => fields.iter().any(|(_, f)| contains_pointer(f)),
        _ => false,
    }
}

/// A guest message turned into a host call, ready to run.
pub struct PreparedHostCall {
    pub args: HostArgs,
    ret: Ty,
    ret_hty: HostTy,
    indirect: Option<Box<[u64]>>,
    stret: Option<u32>,
    outs: Vec<OutParam>,
    not_found: bool,
}

/// What the framework selector table says about a message beyond its types.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostCallOptions {
    /// A scalar `f` is a C float rather than CGFloat.
    pub real_float: bool,
    pub varargs: Varargs,
    pub arrays: IdArrays,
    /// The result points to host bytes, as many as the integer out-parameter
    /// says (the bridge copies them into guest memory).
    pub host_bytes: bool,
    /// The integer result is an index that may be NSNotFound (`not-found`).
    pub not_found: bool,
}

/// The most ids one counted array may hold (a bad count fails, not the heap).
const MAX_ID_ARRAY: u32 = 1 << 20;

/// Converts a guest message (`words`: r0-r3 then the guest stack, by word
/// index) into host registers.
pub fn prepare_host_call(
    sig: &Signature,
    options: HostCallOptions,
    words: &dyn Fn(u32) -> u32,
    b: &mut dyn Boundary,
) -> Result<PreparedHostCall, String> {
    let mut args = HostArgs::default();
    let mut outs = Vec::new();
    let array_count = match options.arrays {
        IdArrays::None => None,
        _ => sig.args.iter().zip(sig.guest_arg_words()).filter(|(t, _)| matches!(t, Ty::Int { .. })).last().map(|(_, at)| words(at)),
    };
    for (ty, at) in sig.args.iter().zip(sig.guest_arg_words()) {
        let guest = guest_bytes(words, at, ty.guest_size());
        if let Ty::Pointer(pointee) = ty {
            let address = guest_u32(&guest);
            if let (Some(hty), true) = (out_param_type(pointee), address != 0) {
                let (count, read, write_back) = match (array_count, **pointee == Ty::Object) {
                    (Some(n), true) if n > MAX_ID_ARRAY => return Err(format!("an id array of {n} elements")),
                    (Some(n), true) => (n.max(1), options.arrays == IdArrays::In, options.arrays == IdArrays::Out),
                    // `id *` (NSError **error and the like) is output-only: the
                    // guest's slot is often uninitialized, so the host starts at nil.
                    (None, true) => (1, false, true),
                    _ => (1, true, true),
                };
                let (step, guest_step) = (hty.size() as usize, pointee.guest_size() as usize);
                let mut host = vec![0u8; (step * count as usize).div_ceil(8) * 8];
                if read {
                    let current = b.read(address, (guest_step * count as usize) as u32);
                    for i in 0..count as usize {
                        let element = &current[i * guest_step..(i + 1) * guest_step];
                        to_host(pointee, &hty, element, &mut host[i * step..(i + 1) * step], b)?;
                    }
                }
                let temp = words_of(&host).into_boxed_slice();
                args.push(&HostTy::Ptr, &(temp.as_ptr() as u64).to_le_bytes());
                outs.push(OutParam { guest: address, pointee: (**pointee).clone(), hty, count, write_back, temp });
                continue;
            }
        }
        let hty = host_ty(ty, options.real_float)?;
        let mut host = vec![0u8; (hty.size() as usize).max(8)];
        to_host(ty, &hty, &guest, &mut host[..hty.size() as usize], b)?;
        args.push(&hty, &host);
    }
    let next = sig.guest_total_words();
    match options.varargs {
        Varargs::None => {}
        Varargs::NilList => {
            for i in next.. {
                let object = words(i);
                args.stack.push(b.id_to_host(object)? as u64);
                if object == 0 {
                    break;
                }
            }
        }
        Varargs::Format => {
            let at = *sig.guest_arg_words().last().unwrap();
            let format_arg = words(at);
            let text = match sig.args.last() {
                Some(Ty::CString) => {
                    let host = b.host_address(format_arg) as *const libc::c_char;
                    unsafe { std::ffi::CStr::from_ptr(host) }.to_string_lossy().into_owned()
                }
                _ => b.object_text(format_arg).ok_or("format string is not a string object")?,
            };
            let mut word = next;
            for kind in format_kinds(&text) {
                let slot = match kind {
                    FormatArg::Int => {
                        word += 1;
                        words(word - 1) as i32 as i64 as u64
                    }
                    FormatArg::Long => {
                        word += 2;
                        u64::from(words(word - 2)) | u64::from(words(word - 1)) << 32
                    }
                    FormatArg::Object => {
                        word += 1;
                        b.id_to_host(words(word - 1))? as u64
                    }
                    FormatArg::Pointer => {
                        word += 1;
                        let g = words(word - 1);
                        if g == 0 { 0 } else { b.host_address(g) as u64 }
                    }
                };
                args.stack.push(slot);
            }
        }
    }
    let ret_hty = host_ty(&sig.ret, false)?;
    let indirect = needs_indirect_result(&ret_hty).then(|| vec![0u64; (ret_hty.size() as usize).div_ceil(8)].into_boxed_slice());
    if let Some(buffer) = &indirect {
        args.x[8] = buffer.as_ptr() as u64;
    }
    let stret = sig.ret.guest_returns_in_memory().then(|| words(0));
    Ok(PreparedHostCall { args, ret: sig.ret.clone(), ret_hty, indirect, stret, outs, not_found: options.not_found })
}

impl PreparedHostCall {
    /// What the host wrote to the call's first out-parameter (its first
    /// host word), before it is converted back; None if the guest passed none.
    pub fn first_out_value(&self) -> Option<u64> {
        self.outs.first().map(|out| out.temp[0])
    }
}

/// Converts the host's result (and out-parameters) back for the guest.
pub fn finish_host_call(call: PreparedHostCall, ret: &HostRet, b: &mut dyn Boundary) -> Result<GuestRet, String> {
    let mut mapped = TempMap { b, outs: &call.outs };
    for out in call.outs.iter().filter(|o| o.write_back) {
        let host: Vec<u8> = out.temp.iter().flat_map(|w| w.to_le_bytes()).collect();
        let (step, guest_step) = (out.hty.size() as usize, out.pointee.guest_size() as usize);
        let mut guest = vec![0u8; guest_step * out.count as usize];
        for i in 0..out.count as usize {
            let element = &host[i * step..(i + 1) * step];
            to_guest(&out.pointee, &out.hty, element, &mut guest[i * guest_step..(i + 1) * guest_step], &mut mapped)?;
        }
        mapped.write(out.guest, &guest);
    }
    let b = mapped.b;
    if call.ret == Ty::Void {
        return Ok(GuestRet { r0: 0, r1: 0 });
    }
    let host = host_result_bytes(&call.ret_hty, ret, is_float_scalar(&call.ret), call.indirect.as_deref());
    // A scalar float came back as f64 bits (see float_register_as_double).
    let hty = if is_float_scalar(&call.ret) { HostTy::F64 } else { call.ret_hty.clone() };
    let mut guest = vec![0u8; call.ret.guest_size() as usize];
    to_guest(&call.ret, &hty, &host, &mut guest, b)?;
    if call.not_found && matches!(call.ret, Ty::Int { .. }) {
        not_found_to_guest(&host, &mut guest);
    }
    match call.stret {
        Some(address) => {
            b.write(address, &guest);
            Ok(GuestRet { r0: address, r1: 0 })
        }
        None => Ok(guest_ret_from_bytes(&call.ret, &guest)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatArg {
    /// 4 guest bytes, sign-extended into a slot (%d %u %x %c %ld %zu %hd ...)
    Int,
    /// 8 guest bytes (%lld %qd %jd, doubles)
    Long,
    /// %@
    Object,
    /// %s %p %S (guest pointers become host addresses)
    Pointer,
}

/// The variadic arguments a printf-style format reads, in order.
fn format_kinds(format: &str) -> Vec<FormatArg> {
    let f = format.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < f.len() {
        if f[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if f.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        while i < f.len() && b"-+ #0'".contains(&f[i]) {
            i += 1;
        }
        let mut star = |i: &mut usize| {
            if f.get(*i) == Some(&b'*') {
                out.push(FormatArg::Int);
                *i += 1;
            }
            while *i < f.len() && f[*i].is_ascii_digit() {
                *i += 1;
            }
        };
        star(&mut i);
        if f.get(i) == Some(&b'.') {
            i += 1;
            star(&mut i);
        }
        let mut longs = 0;
        while i < f.len() && b"hlqLjzt".contains(&f[i]) {
            if matches!(f[i], b'l' | b'q' | b'L' | b'j') {
                longs += if f[i] == b'l' { 1 } else { 2 };
            }
            i += 1;
        }
        let Some(&conversion) = f.get(i) else { break };
        i += 1;
        out.push(match conversion {
            // %K: a key path name in a predicate format.
            b'@' | b'K' => FormatArg::Object,
            b's' | b'S' | b'p' => FormatArg::Pointer,
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => FormatArg::Long,
            _ if longs >= 2 => FormatArg::Long,
            _ => FormatArg::Int,
        });
    }
    out
}

// ---------- host -> guest ----------

/// A host message to a guest method, as guest argument words.
pub struct PreparedGuestCall {
    /// r0-r3 then the stack words, in order.
    pub words: Vec<u32>,
    ret: Ty,
    stret: Option<u32>,
    outs: Vec<GuestOut>,
}

/// A host pointer argument (NSError **, id *, BOOL *, CGRect *, ...) of a
/// guest method: the guest gets a temporary of the pointee's guest type,
/// copied back into the host's memory after the call.
struct GuestOut {
    host: usize,
    temp: u32,
    pointee: Ty,
    hty: HostTy,
    /// The host bytes before the call (8 for an int: it may be NSInteger).
    before: Vec<u8>,
}

/// Pointees with a known layout; others (void, opaque structs, functions)
/// stay opaque handles.
fn typed_pointee(pointee: &Ty) -> bool {
    match pointee {
        Ty::Struct { fields, .. } => !fields.is_empty(),
        Ty::Void | Ty::Unknown | Ty::Union { .. } => false,
        _ => true,
    }
}

impl GuestOut {
    fn new(pointee: &Ty, host: usize, b: &mut dyn Boundary) -> Result<GuestOut, String> {
        let hty = host_ty_in(pointee, Ctx::default())?;
        let size = if matches!(pointee, Ty::Int { .. }) { 8 } else { hty.size() as usize };
        let before = unsafe { std::slice::from_raw_parts(host as *const u8, size) }.to_vec();
        let mut guest = vec![0u8; pointee.guest_size() as usize];
        // An id slot starts nil: the host's may be uninitialized (NSError *e;).
        if !pointee.is_object() && to_guest(pointee, &hty, &before, &mut guest, b).is_err() {
            guest.fill(0);
        }
        let temp = b.alloc(guest.len() as u32);
        b.write(temp, &guest);
        Ok(GuestOut { host, temp, pointee: pointee.clone(), hty, before })
    }

    fn copy_back(&self, b: &mut dyn Boundary) -> Result<(), String> {
        let guest = b.read(self.temp, self.pointee.guest_size());
        b.free(self.temp);
        let mut host = vec![0u8; (self.hty.size() as usize).max(8)];
        to_host(&self.pointee, &self.hty, &guest, &mut host[..self.hty.size() as usize], b)?;
        let mut out = host[..self.hty.size() as usize].to_vec();
        if matches!(self.pointee, Ty::Int { .. }) {
            // int or NSInteger: the upper half follows only when it held the
            // sign extension of the lower half before (a 64-bit NSInteger).
            let low = |bytes: &[u8]| i32::from_le_bytes(bytes[..4].try_into().unwrap());
            let extended = |v: i32| (i64::from(v) as u64 >> 32) as u32;
            let upper_before = u32::from_le_bytes(self.before[4..8].try_into().unwrap());
            if upper_before == extended(low(&self.before)) {
                out.extend_from_slice(&extended(low(&out)).to_le_bytes());
            }
        }
        unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), self.host as *mut u8, out.len()) };
        Ok(())
    }
}

/// Converts a host call (registers in `frame`) into guest words by the
/// guest method's own type encoding.
pub fn prepare_guest_call(sig: &Signature, frame: &HostFrame, b: &mut dyn Boundary) -> Result<PreparedGuestCall, String> {
    let mut words = vec![0u32; sig.guest_total_words() as usize];
    let mut reader = ArgReader { frame, ngrn: 0, nsrn: 0, slot: 0 };
    let mut outs = Vec::new();
    for (ty, at) in sig.args.iter().zip(sig.guest_arg_words()) {
        let hty = host_ty(ty, false)?;
        let host = reader.take(&hty, is_float_scalar(ty));
        if let Ty::Pointer(pointee) = ty {
            let h = host_u64(&host) as usize;
            if h != 0 && b.guest_address(h).is_none() && typed_pointee(pointee) {
                let out = GuestOut::new(pointee, h, b)?;
                words[at as usize] = out.temp;
                outs.push(out);
                continue;
            }
        }
        let mut guest = vec![0u8; (ty.guest_size() as usize).div_ceil(4).max(1) * 4];
        let size = ty.guest_size() as usize;
        to_guest(ty, &hty, &host, &mut guest[..size], b)?;
        for (i, chunk) in guest.chunks(4).enumerate() {
            words[at as usize + i] = guest_u32(chunk);
        }
    }
    let stret = sig.ret.guest_returns_in_memory().then(|| b.alloc(sig.ret.guest_size()));
    if let Some(address) = stret {
        words[0] = address;
    }
    Ok(PreparedGuestCall { words, ret: sig.ret.clone(), stret, outs })
}

/// Converts the guest method's result (r0/r1 or its stret buffer) into the
/// host's result registers.
pub fn finish_guest_call(call: PreparedGuestCall, r0: u32, r1: u32, frame: &mut HostFrame, b: &mut dyn Boundary) -> Result<(), String> {
    for out in &call.outs {
        out.copy_back(b)?;
    }
    if call.ret == Ty::Void {
        return Ok(());
    }
    let size = call.ret.guest_size();
    let guest = match call.stret {
        Some(address) => {
            let bytes = b.read(address, size);
            b.free(address);
            bytes
        }
        None => [r0.to_le_bytes(), r1.to_le_bytes()].concat()[..size as usize].to_vec(),
    };
    let hty = host_ty(&call.ret, false)?;
    let mut host = vec![0u8; (hty.size() as usize).div_ceil(8).max(1) * 8];
    to_host(&call.ret, &hty, &guest, &mut host[..hty.size() as usize], b)?;
    set_host_result(&hty, &host, frame);
    Ok(())
}
