//! The 32-bit Objective-C metadata reader, on small made-up memory images.

use guest32_image::*;
use std::collections::HashMap;

/// Guest memory built by hand: one flat region starting at `BASE`, plus the
/// pointer slots dyld would fill from a library (binds).
struct Fake {
    bytes: Vec<u8>,
    binds: HashMap<u32, String>,
}

const BASE: u32 = 0x1000;

impl Fake {
    fn new() -> Self {
        Fake { bytes: vec![0; 0x40], binds: HashMap::new() }
    }
    fn here(&self) -> u32 {
        BASE + self.bytes.len() as u32
    }
    fn words(&mut self, values: &[u32]) -> u32 {
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
        let at = self.here();
        for v in values {
            self.bytes.extend_from_slice(&v.to_le_bytes());
        }
        at
    }
    fn text(&mut self, s: &str) -> u32 {
        let at = self.here();
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        at
    }
    fn bind(&mut self, slot: u32, symbol: &str) {
        self.binds.insert(slot, symbol.to_string());
    }
    fn patch(&mut self, at: u32, value: u32) {
        let i = (at - BASE) as usize;
        self.bytes[i..i + 4].copy_from_slice(&value.to_le_bytes());
    }
}

impl Memory32 for Fake {
    fn bytes(&self, address: u32, length: u32) -> Option<&[u8]> {
        let start = address.checked_sub(BASE)? as usize;
        self.bytes.get(start..start.checked_add(length as usize)?)
    }
    fn bound_symbol(&self, slot: u32) -> Option<&str> {
        self.binds.get(&slot).map(|s| s.as_str())
    }
}

fn method_list(m: &mut Fake, methods: &[(&str, &str, u32)]) -> u32 {
    let entries: Vec<(u32, u32, u32)> = methods.iter().map(|(n, t, imp)| (m.text(n), m.text(t), *imp)).collect();
    let mut words = vec![12, entries.len() as u32];
    for (n, t, imp) in entries {
        words.extend([n, t, imp]);
    }
    m.words(&words)
}

/// A game class `Hero : UIResponder <Brave>` with one ivar, one property, an
/// instance method and a class method, plus a local subclass `SuperHero`.
fn two_classes() -> (Fake, ObjcSections, u32, u32, u32) {
    let mut m = Fake::new();
    let ivar_offset = m.words(&[4]);
    let proto_name = m.text("Brave");
    let proto_methods = method_list(&mut m, &[("fight", "v8@0:4", 0)]);
    let protocol = m.words(&[0, proto_name, 0, proto_methods, 0, 0, 0, 0, 44, 0]);
    let protocols = m.words(&[1, protocol]);
    let name = m.text("Hero");
    let methods = method_list(&mut m, &[("fight", "v8@0:4", 0x2001), ("speed", "f8@0:4", 0x2011)]);
    let class_methods = method_list(&mut m, &[("hero", "@8@0:4", 0x2021)]);
    let (ivar_name, ivar_type) = (m.text("_power"), m.text("i"));
    let ivars = m.words(&[20, 1, ivar_offset, ivar_name, ivar_type, 2, 4]);
    let (prop_name, prop_attrs) = (m.text("power"), m.text("Ti,N,V_power"));
    let properties = m.words(&[8, 1, prop_name, prop_attrs]);
    let ro = m.words(&[0x184, 4, 8, 0, name, methods, protocols, ivars, 0, properties]);
    let meta_ro = m.words(&[0x185, 20, 20, 0, name, class_methods, protocols, 0, 0, 0]);
    let meta = m.words(&[0, 0, 0, 0, meta_ro]);
    // data's low bits carry runtime flags and must be masked off.
    let hero = m.words(&[meta, 0, 0, 0, ro | 1]);
    m.bind(meta, "_OBJC_METACLASS_$_NSObject");
    m.bind(meta + 4, "_OBJC_METACLASS_$_UIResponder");
    m.bind(hero + 4, "_OBJC_CLASS_$_UIResponder");

    let sub_name = m.text("SuperHero");
    let sub_ro = m.words(&[0x0, 8, 8, 0, sub_name, 0, 0, 0, 0, 0]);
    let sub_meta_ro = m.words(&[0x1, 20, 20, 0, sub_name, 0, 0, 0, 0, 0]);
    let sub_meta = m.words(&[0, meta, 0, 0, sub_meta_ro]);
    m.bind(sub_meta, "_OBJC_METACLASS_$_NSObject");
    let sub = m.words(&[sub_meta, hero, 0, 0, sub_ro]);

    let classlist = m.words(&[hero, sub]);
    let sections = ObjcSections { classlist: Some((classlist, 8)), ..Default::default() };
    (m, sections, hero, meta, ivar_offset)
}

#[test]
fn classes_read_with_methods_ivars_properties_protocols_and_bound_superclasses() {
    let (m, sections, hero, meta, ivar_offset) = two_classes();
    let objc = read_objc(&m, &sections).unwrap();
    assert_eq!(objc.classes.len(), 2);
    let c = &objc.classes[0];
    assert_eq!((c.address, c.name.as_str(), c.meta_address), (hero, "Hero", meta));
    assert_eq!(c.superclass, ClassTarget::External("UIResponder".into()));
    assert_eq!(c.meta_isa, ClassTarget::External("NSObject".into()));
    assert_eq!(c.meta_superclass, ClassTarget::External("UIResponder".into()));
    assert_eq!((c.instance.instance_start, c.instance.instance_size, c.instance.flags), (4, 8, 0x184));
    let names: Vec<_> = c.instance.methods.iter().map(|m| (m.name.as_str(), m.types.as_str(), m.imp)).collect();
    assert_eq!(names, [("fight", "v8@0:4", 0x2001), ("speed", "f8@0:4", 0x2011)]);
    assert_eq!(c.meta.methods[0].name, "hero");
    assert!(c.meta.is_meta());
    let ivar = &c.instance.ivars[0];
    assert_eq!(
        (ivar.offset_address, ivar.offset, ivar.name.as_str(), ivar.type_encoding.as_str(), ivar.alignment, ivar.size),
        (ivar_offset, 4, "_power", "i", 2, 4)
    );
    assert_eq!(c.instance.properties[0].attributes, "Ti,N,V_power");
    assert_eq!(c.instance.protocols.len(), 1);
    let p = &objc.protocols_by_address[&c.instance.protocols[0]];
    assert_eq!((p.name.as_str(), p.instance_methods[0].name.as_str()), ("Brave", "fight"));

    let s = &objc.classes[1];
    assert_eq!(s.superclass, ClassTarget::Local(hero));
    assert_eq!(s.meta_superclass, ClassTarget::Local(meta));
    assert_eq!(objc.class_named("SuperHero").map(|c| c.address), Some(s.address));
}

#[test]
fn references_categories_and_constant_strings() {
    let (mut m, mut sections, hero, _, _) = two_classes();
    let (sel_a, sel_b) = (m.text("fight"), m.text("initWithFrame:"));
    let selrefs = m.words(&[sel_a, sel_b, sel_a]);
    let classrefs = m.words(&[hero, 0]);
    m.bind(classrefs + 4, "_OBJC_CLASS_$_NSString");
    let superrefs = m.words(&[hero]);
    let cat_name = m.text("Extras");
    let cat_methods = method_list(&mut m, &[("description", "@8@0:4", 0x3001)]);
    let category = m.words(&[cat_name, 0, cat_methods, 0, 0, 0]);
    m.bind(category + 4, "_OBJC_CLASS_$_NSObject");
    let catlist = m.words(&[category]);
    let chars = m.text("Hello");
    let cfstring = m.words(&[0, 0x7c8, chars, 5]);
    m.bind(cfstring, "___CFConstantStringClassReference");
    let image_info = m.words(&[0, 0x20]);
    sections.selrefs = Some((selrefs, 12));
    sections.classrefs = Some((classrefs, 8));
    sections.superrefs = Some((superrefs, 4));
    sections.catlist = Some((catlist, 4));
    sections.cfstrings = Some((cfstring, 16));
    sections.image_info = Some((image_info, 8));

    let objc = read_objc(&m, &sections).unwrap();
    let sels: Vec<_> = objc.selector_refs.iter().map(|r| (r.slot, r.name.as_str())).collect();
    assert_eq!(sels, [(selrefs, "fight"), (selrefs + 4, "initWithFrame:"), (selrefs + 8, "fight")]);
    assert_eq!(objc.selector_names(), ["fight", "initWithFrame:"]);
    assert_eq!(objc.class_refs[0].target, ClassTarget::Local(hero));
    assert_eq!(objc.class_refs[1].target, ClassTarget::External("NSString".into()));
    assert_eq!(objc.super_refs[0].slot, superrefs);
    let cat = &objc.categories[0];
    assert_eq!((cat.name.as_str(), &cat.class), ("Extras", &ClassTarget::External("NSObject".into())));
    assert_eq!(cat.instance_methods[0].imp, 0x3001);
    let s = &objc.cfstrings[0];
    assert_eq!((s.address, s.flags, s.text.as_deref()), (cfstring, 0x7c8, Some("Hello")));
    assert_eq!(objc.image_info, Some((0, 0x20)));
}

#[test]
fn utf16_constant_strings_decode() {
    let mut m = Fake::new();
    let units = m.words(&[u32::from_le_bytes([b'h', 0, 0xE9, 0]), 0]);
    let cfstring = m.words(&[0, 0x7d0, units, 2]);
    let sections = ObjcSections { cfstrings: Some((cfstring, 16)), ..Default::default() };
    let objc = read_objc(&m, &sections).unwrap();
    assert_eq!(objc.cfstrings[0].text.as_deref(), Some("hé"));
    assert!(objc.cfstrings[0].is_utf16());
}

#[test]
fn bad_pointers_and_entry_sizes_are_errors_not_panics() {
    let (mut m, sections, hero, _, _) = two_classes();
    // data pointer far outside memory
    let saved = m.bytes[(hero - BASE + 16) as usize..(hero - BASE + 20) as usize].to_vec();
    m.patch(hero + 16, 0x00F0_0000);
    assert!(read_objc(&m, &sections).is_err());
    m.patch(hero + 16, u32::from_le_bytes(saved.try_into().unwrap()));
    assert!(read_objc(&m, &sections).is_ok());
    // a method list whose entry size is too small
    let objc = read_objc(&m, &sections).unwrap();
    let list = objc.classes[0].instance.methods_list_address;
    m.patch(list, 4);
    let err = read_objc(&m, &sections).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Malformed);
    // a class list whose size is not whole pointers
    let (m, mut sections, ..) = two_classes();
    sections.classlist = sections.classlist.map(|(a, _)| (a, 6));
    assert!(read_objc(&m, &sections).is_err());
}
