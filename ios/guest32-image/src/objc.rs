//! Apple's 32-bit Objective-C 2 metadata (armv7): classes, metaclasses,
//! method/ivar/property/protocol lists, categories, the selector, class and
//! superclass reference tables and constant strings.
//!
//! The reader works on unslid addresses through [`Memory32`], so it can read
//! a parsed file ([`ImageMemory`]) or any made-up memory. A pointer slot that
//! dyld fills from a library (a bind) is reported by symbol, never by value:
//! a class whose superclass is `UIView` names `UIView`.
//!
//! 32-bit layouts (all fields 4 bytes):
//! - class_t: isa, superclass, cache, vtable, data (low 2 bits are flags);
//! - class_ro_t: flags, instanceStart, instanceSize, ivarLayout, name,
//!   baseMethods, baseProtocols, ivars, weakIvarLayout, baseProperties;
//! - method_list_t: entsize|flags, count, {name, types, imp}...;
//! - ivar_list_t: entsize, count, {offset*, name, type, alignment, size}...;
//! - property_list_t: entsize, count, {name, attributes}...;
//! - protocol_list_t: count, protocol_t*...;
//! - protocol_t: isa, name, protocols, instanceMethods, classMethods,
//!   optionalInstanceMethods, optionalClassMethods, instanceProperties, size, flags;
//! - category_t: name, cls, instanceMethods, classMethods, protocols, instanceProperties;
//! - CFConstantString: isa, flags, chars, length (flags 0x7d0 = UTF-16).

use crate::error::ErrorKind::{InvalidReference, Malformed};
use crate::error::{fail, Error, Result};
use crate::Image32;
use std::collections::{BTreeMap, HashMap};

/// Read access to guest memory at unslid addresses.
pub trait Memory32 {
    /// `length` bytes at `address`, or `None` if any of them is not backed.
    fn bytes(&self, address: u32, length: u32) -> Option<&[u8]>;
    /// The symbol dyld binds into the 4-byte slot at `slot`, if any.
    fn bound_symbol(&self, slot: u32) -> Option<&str>;
}

/// Where the Objective-C sections are: (address, size) each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjcSections {
    pub classlist: Option<(u32, u32)>,
    pub catlist: Option<(u32, u32)>,
    pub protolist: Option<(u32, u32)>,
    pub selrefs: Option<(u32, u32)>,
    pub classrefs: Option<(u32, u32)>,
    pub superrefs: Option<(u32, u32)>,
    pub cfstrings: Option<(u32, u32)>,
    pub image_info: Option<(u32, u32)>,
}

impl ObjcSections {
    /// Finds the sections by name in any segment (`__DATA`, `__DATA_CONST`, ...).
    pub fn from_image(image: &Image32) -> Self {
        let find = |name: &str| {
            image
                .segments
                .iter()
                .flat_map(|s| s.sections.iter())
                .find(|s| s.name == name)
                .map(|s| (s.address, s.size))
        };
        ObjcSections {
            classlist: find("__objc_classlist"),
            catlist: find("__objc_catlist"),
            protolist: find("__objc_protolist"),
            selrefs: find("__objc_selrefs"),
            classrefs: find("__objc_classrefs"),
            superrefs: find("__objc_superrefs"),
            cfstrings: find("__cfstring"),
            image_info: find("__objc_imageinfo"),
        }
    }
}

/// What a class pointer slot holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClassTarget {
    /// A class (or metaclass) in this image, by address.
    Local(u32),
    /// A class from a library, by class name (the `_OBJC_CLASS_$_` or
    /// `_OBJC_METACLASS_$_` prefix removed; which one follows from the slot).
    External(String),
    /// A bind to a symbol that is not a class symbol.
    OtherSymbol(String),
    /// Zero and not bound (a root class's superclass).
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method32 {
    pub name: String,
    pub types: String,
    /// Code address, Thumb bit as stored (0 for protocol methods).
    pub imp: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ivar32 {
    /// The ivar offset variable the game's code reads (0 if none).
    pub offset_address: u32,
    /// Its value in the file.
    pub offset: u32,
    pub name: String,
    pub type_encoding: String,
    /// log2 of the alignment.
    pub alignment: u32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property32 {
    pub name: String,
    pub attributes: String,
}

/// RO_META
pub const RO_META: u32 = 0x1;
/// RO_ROOT
pub const RO_ROOT: u32 = 0x2;
/// RO_HAS_CXX_STRUCTORS
pub const RO_HAS_CXX_STRUCTORS: u32 = 0x4;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ClassRo32 {
    pub address: u32,
    pub flags: u32,
    pub instance_start: u32,
    pub instance_size: u32,
    pub ivar_layout: u32,
    pub name: String,
    pub methods_list_address: u32,
    pub methods: Vec<Method32>,
    /// protocol_t addresses (see [`ObjcMetadata32::protocols_by_address`]).
    pub protocols: Vec<u32>,
    pub ivars: Vec<Ivar32>,
    pub weak_ivar_layout: u32,
    pub properties: Vec<Property32>,
}

impl ClassRo32 {
    pub fn is_meta(&self) -> bool {
        self.flags & RO_META != 0
    }
    pub fn is_root(&self) -> bool {
        self.flags & RO_ROOT != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcClass32 {
    pub address: u32,
    pub name: String,
    pub superclass: ClassTarget,
    /// The metaclass (this image's own; a class's isa is never bound).
    pub meta_address: u32,
    /// The metaclass's isa (normally the root metaclass, bound from a library).
    pub meta_isa: ClassTarget,
    pub meta_superclass: ClassTarget,
    pub instance: ClassRo32,
    pub meta: ClassRo32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcCategory32 {
    pub address: u32,
    pub name: String,
    pub class: ClassTarget,
    pub instance_methods: Vec<Method32>,
    pub class_methods: Vec<Method32>,
    pub protocols: Vec<u32>,
    pub instance_properties: Vec<Property32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcProtocol32 {
    pub address: u32,
    pub name: String,
    pub protocols: Vec<u32>,
    pub instance_methods: Vec<Method32>,
    pub class_methods: Vec<Method32>,
    pub optional_instance_methods: Vec<Method32>,
    pub optional_class_methods: Vec<Method32>,
    pub properties: Vec<Property32>,
}

/// One slot of `__objc_selrefs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorRef32 {
    pub slot: u32,
    pub name: String,
}

/// One slot of `__objc_classrefs` or `__objc_superrefs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassRef32 {
    pub slot: u32,
    pub target: ClassTarget,
}

/// One `__cfstring` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfString32 {
    pub address: u32,
    /// The isa slot's bind (normally `___CFConstantStringClassReference`).
    pub class_symbol: Option<String>,
    pub flags: u32,
    pub chars_address: u32,
    /// Length in characters (UTF-16 units for UTF-16 strings).
    pub length: u32,
    /// The decoded text (`None` if it is not valid in its encoding).
    pub text: Option<String>,
}

impl CfString32 {
    /// CFString's "is Unicode" info bit (0x7c8 = 8-bit, 0x7d0 = UTF-16).
    pub fn is_utf16(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjcMetadata32 {
    /// In `__objc_classlist` order.
    pub classes: Vec<ObjcClass32>,
    pub categories: Vec<ObjcCategory32>,
    /// Every protocol named from `__objc_protolist` or from a class,
    /// category or protocol, by address.
    pub protocols_by_address: BTreeMap<u32, ObjcProtocol32>,
    pub selector_refs: Vec<SelectorRef32>,
    pub class_refs: Vec<ClassRef32>,
    pub super_refs: Vec<ClassRef32>,
    pub cfstrings: Vec<CfString32>,
    /// `__objc_imageinfo`: (version, flags).
    pub image_info: Option<(u32, u32)>,
}

impl ObjcMetadata32 {
    pub fn class_named(&self, name: &str) -> Option<&ObjcClass32> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Distinct selector names from `__objc_selrefs`, first use first.
    pub fn selector_names(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        self.selector_refs.iter().map(|r| r.name.as_str()).filter(|n| seen.insert(*n)).collect()
    }
}

struct Reader<'a, M: Memory32 + ?Sized> {
    memory: &'a M,
    protocols: BTreeMap<u32, ObjcProtocol32>,
    in_progress: Vec<u32>,
}

impl<'a, M: Memory32 + ?Sized> Reader<'a, M> {
    fn word(&self, address: u32, what: &str) -> Result<u32> {
        let raw = self
            .memory
            .bytes(address, 4)
            .ok_or_else(|| Error::new(InvalidReference, format!("{what}: {address:#x} is not in the image")))?;
        Ok(u32::from_le_bytes(raw.try_into().unwrap()))
    }

    fn string(&self, address: u32, what: &str) -> Result<String> {
        if address == 0 {
            return Ok(String::new());
        }
        // Read in growing pieces so a string near the end of memory still works.
        let mut length = 64u32;
        loop {
            let available = (1..=length).rev().find_map(|n| self.memory.bytes(address, n).map(|b| (n, b)));
            let Some((got, bytes)) = available else {
                return fail(InvalidReference, format!("{what}: string at {address:#x} is not in the image"));
            };
            if let Some(end) = bytes.iter().position(|&b| b == 0) {
                return Ok(String::from_utf8_lossy(&bytes[..end]).into_owned());
            }
            if got < length || length >= 1 << 20 {
                return fail(Malformed, format!("{what}: string at {address:#x} has no ending zero"));
            }
            length *= 4;
        }
    }

    fn class_target(&self, slot: u32, what: &str) -> Result<ClassTarget> {
        if let Some(symbol) = self.memory.bound_symbol(slot) {
            let name = symbol.strip_prefix("_OBJC_CLASS_$_").or_else(|| symbol.strip_prefix("_OBJC_METACLASS_$_"));
            return Ok(match name {
                Some(name) => ClassTarget::External(name.to_string()),
                None => ClassTarget::OtherSymbol(symbol.to_string()),
            });
        }
        Ok(match self.word(slot, what)? {
            0 => ClassTarget::Null,
            address => ClassTarget::Local(address),
        })
    }

    /// The (address, count) of a list with an entry-size header.
    fn list_header(&self, list: u32, minimum_entry: u32, what: &str) -> Result<(u32, u32)> {
        let entry_size = self.word(list, what)? & !3;
        let count = self.word(list + 4, what)?;
        if entry_size < minimum_entry {
            return fail(Malformed, format!("{what} at {list:#x}: entry size {entry_size} is under {minimum_entry}"));
        }
        let total = u64::from(entry_size) * u64::from(count);
        if self.memory.bytes(list + 8, total.min(u64::from(u32::MAX)) as u32).is_none() && total != 0 {
            return fail(InvalidReference, format!("{what} at {list:#x}: {count} entries run past the image"));
        }
        Ok((entry_size, count))
    }

    fn methods(&self, list: u32, what: &str) -> Result<Vec<Method32>> {
        if list == 0 {
            return Ok(Vec::new());
        }
        let (entry_size, count) = self.list_header(list, 12, what)?;
        (0..count)
            .map(|i| {
                let at = list + 8 + i * entry_size;
                Ok(Method32 {
                    name: self.string(self.word(at, what)?, what)?,
                    types: self.string(self.word(at + 4, what)?, what)?,
                    imp: self.word(at + 8, what)?,
                })
            })
            .collect()
    }

    fn ivars(&self, list: u32, what: &str) -> Result<Vec<Ivar32>> {
        if list == 0 {
            return Ok(Vec::new());
        }
        let (entry_size, count) = self.list_header(list, 20, what)?;
        (0..count)
            .map(|i| {
                let at = list + 8 + i * entry_size;
                let offset_address = self.word(at, what)?;
                Ok(Ivar32 {
                    offset_address,
                    offset: if offset_address == 0 { 0 } else { self.word(offset_address, what)? },
                    name: self.string(self.word(at + 4, what)?, what)?,
                    type_encoding: self.string(self.word(at + 8, what)?, what)?,
                    alignment: self.word(at + 12, what)?,
                    size: self.word(at + 16, what)?,
                })
            })
            .collect()
    }

    fn properties(&self, list: u32, what: &str) -> Result<Vec<Property32>> {
        if list == 0 {
            return Ok(Vec::new());
        }
        let (entry_size, count) = self.list_header(list, 8, what)?;
        (0..count)
            .map(|i| {
                let at = list + 8 + i * entry_size;
                Ok(Property32 {
                    name: self.string(self.word(at, what)?, what)?,
                    attributes: self.string(self.word(at + 4, what)?, what)?,
                })
            })
            .collect()
    }

    /// A protocol_list_t; each protocol is read (once) into `self.protocols`.
    fn protocol_list(&mut self, list: u32, what: &str) -> Result<Vec<u32>> {
        if list == 0 {
            return Ok(Vec::new());
        }
        let count = self.word(list, what)?;
        if self.memory.bytes(list + 4, count.saturating_mul(4)).is_none() {
            return fail(InvalidReference, format!("{what} at {list:#x}: {count} protocols run past the image"));
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let protocol = self.word(list + 4 + 4 * i, what)?;
            self.protocol(protocol)?;
            out.push(protocol);
        }
        Ok(out)
    }

    fn protocol(&mut self, address: u32) -> Result<()> {
        if self.protocols.contains_key(&address) || self.in_progress.contains(&address) {
            return Ok(());
        }
        self.in_progress.push(address);
        let what = "protocol";
        let field = |n: u32| self.word(address + 4 * n, what);
        let (name, protocols, im, cm, oim, ocm, props) =
            (field(1)?, field(2)?, field(3)?, field(4)?, field(5)?, field(6)?, field(7)?);
        let protocol = ObjcProtocol32 {
            address,
            name: self.string(name, what)?,
            protocols: self.protocol_list(protocols, what)?,
            instance_methods: self.methods(im, what)?,
            class_methods: self.methods(cm, what)?,
            optional_instance_methods: self.methods(oim, what)?,
            optional_class_methods: self.methods(ocm, what)?,
            properties: self.properties(props, what)?,
        };
        self.in_progress.retain(|&a| a != address);
        self.protocols.insert(address, protocol);
        Ok(())
    }

    fn class_ro(&mut self, address: u32, what: &str) -> Result<ClassRo32> {
        let field = |n: u32| self.word(address + 4 * n, what);
        let fields: Vec<u32> = (0..10).map(field).collect::<Result<_>>()?;
        Ok(ClassRo32 {
            address,
            flags: fields[0],
            instance_start: fields[1],
            instance_size: fields[2],
            ivar_layout: fields[3],
            name: self.string(fields[4], what)?,
            methods_list_address: fields[5],
            methods: self.methods(fields[5], what)?,
            protocols: self.protocol_list(fields[6], what)?,
            ivars: self.ivars(fields[7], what)?,
            weak_ivar_layout: fields[8],
            properties: self.properties(fields[9], what)?,
        })
    }

    fn class(&mut self, address: u32) -> Result<ObjcClass32> {
        let what = "class";
        let meta_address = self.word(address, what)?;
        if meta_address == 0 {
            return fail(Malformed, format!("class at {address:#x} has no metaclass"));
        }
        let superclass = self.class_target(address + 4, what)?;
        let instance = self.class_ro(self.word(address + 16, what)? & !3, what)?;
        let meta_isa = self.class_target(meta_address, "metaclass")?;
        let meta_superclass = self.class_target(meta_address + 4, "metaclass")?;
        let meta = self.class_ro(self.word(meta_address + 16, "metaclass")? & !3, "metaclass")?;
        if instance.is_meta() || !meta.is_meta() {
            return fail(Malformed, format!("class {} at {address:#x}: RO_META flags are swapped", instance.name));
        }
        Ok(ObjcClass32 {
            address,
            name: instance.name.clone(),
            superclass,
            meta_address,
            meta_isa,
            meta_superclass,
            instance,
            meta,
        })
    }

    fn category(&mut self, address: u32) -> Result<ObjcCategory32> {
        let what = "category";
        let field = |n: u32| self.word(address + 4 * n, what);
        let (name, im, cm, protocols, props) = (field(0)?, field(2)?, field(3)?, field(4)?, field(5)?);
        Ok(ObjcCategory32 {
            address,
            name: self.string(name, what)?,
            class: self.class_target(address + 4, what)?,
            instance_methods: self.methods(im, what)?,
            class_methods: self.methods(cm, what)?,
            protocols: self.protocol_list(protocols, what)?,
            instance_properties: self.properties(props, what)?,
        })
    }

    fn cfstring(&self, address: u32) -> Result<CfString32> {
        let what = "constant string";
        let flags = self.word(address + 4, what)?;
        let chars_address = self.word(address + 8, what)?;
        let length = self.word(address + 12, what)?;
        let mut s = CfString32 {
            address,
            class_symbol: self.memory.bound_symbol(address).map(str::to_string),
            flags,
            chars_address,
            length,
            text: None,
        };
        s.text = if s.is_utf16() {
            let raw = self.memory.bytes(chars_address, length.saturating_mul(2)).ok_or_else(|| {
                Error::new(InvalidReference, format!("{what} at {address:#x}: characters are not in the image"))
            })?;
            let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16(&units).ok()
        } else {
            let raw = self.memory.bytes(chars_address, length).ok_or_else(|| {
                Error::new(InvalidReference, format!("{what} at {address:#x}: characters are not in the image"))
            })?;
            String::from_utf8(raw.to_vec()).ok()
        };
        Ok(s)
    }
}

/// The pointer-sized slots of a section.
fn slots(section: Option<(u32, u32)>, what: &str) -> Result<Vec<u32>> {
    let Some((address, size)) = section else { return Ok(Vec::new()) };
    if size % 4 != 0 {
        return fail(Malformed, format!("{what} size {size} is not whole pointers"));
    }
    Ok((0..size / 4).map(|i| address + 4 * i).collect())
}

/// Reads all Objective-C metadata the sections name.
pub fn read_objc<M: Memory32 + ?Sized>(memory: &M, sections: &ObjcSections) -> Result<ObjcMetadata32> {
    let mut r = Reader { memory, protocols: BTreeMap::new(), in_progress: Vec::new() };
    let mut out = ObjcMetadata32::default();
    for slot in slots(sections.classlist, "__objc_classlist")? {
        let class = r.word(slot, "class list")?;
        out.classes.push(r.class(class)?);
    }
    for slot in slots(sections.catlist, "__objc_catlist")? {
        let category = r.word(slot, "category list")?;
        out.categories.push(r.category(category)?);
    }
    for slot in slots(sections.protolist, "__objc_protolist")? {
        let protocol = r.word(slot, "protocol list")?;
        r.protocol(protocol)?;
    }
    for slot in slots(sections.selrefs, "__objc_selrefs")? {
        let name = r.string(r.word(slot, "selector reference")?, "selector reference")?;
        out.selector_refs.push(SelectorRef32 { slot, name });
    }
    for slot in slots(sections.classrefs, "__objc_classrefs")? {
        out.class_refs.push(ClassRef32 { slot, target: r.class_target(slot, "class reference")? });
    }
    for slot in slots(sections.superrefs, "__objc_superrefs")? {
        out.super_refs.push(ClassRef32 { slot, target: r.class_target(slot, "superclass reference")? });
    }
    if let Some((address, size)) = sections.cfstrings {
        if size % 16 != 0 {
            return fail(Malformed, format!("__cfstring size {size} is not whole 16-byte strings"));
        }
        for i in 0..size / 16 {
            out.cfstrings.push(r.cfstring(address + 16 * i)?);
        }
    }
    if let Some((address, size)) = sections.image_info {
        if size >= 8 {
            out.image_info = Some((r.word(address, "image info")?, r.word(address + 4, "image info")?));
        }
    }
    out.protocols_by_address = r.protocols;
    Ok(out)
}

/// A parsed image's file bytes at their unslid addresses, with its binds.
pub struct ImageMemory<'a> {
    image: &'a Image32,
    slice: &'a [u8],
    binds: HashMap<u32, &'a str>,
}

impl<'a> ImageMemory<'a> {
    /// `file` is the whole file `image` was parsed from.
    pub fn new(image: &'a Image32, file: &'a [u8]) -> Result<Self> {
        let slice = image.slice_bytes(file)?;
        let binds = image.binds.iter().map(|b| (b.address, b.symbol.as_str())).collect();
        Ok(ImageMemory { image, slice, binds })
    }
}

impl Memory32 for ImageMemory<'_> {
    fn bytes(&self, address: u32, length: u32) -> Option<&[u8]> {
        let segment = self.image.segments.iter().find(|s| s.contains(address))?;
        let inside = address - segment.vm_address;
        let end = inside.checked_add(length)?;
        if end > segment.file_size {
            return None;
        }
        let start = segment.file_offset.checked_add(inside)? as usize;
        self.slice.get(start..start + length as usize)
    }

    fn bound_symbol(&self, slot: u32) -> Option<&str> {
        self.binds.get(&slot).copied()
    }
}
