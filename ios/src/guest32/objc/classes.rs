//! The game's own classes as the guest side of the bridge sees them: built
//! from the 32-bit metadata, with categories merged in (a category's
//! method replaces the class's own, as on Apple's runtime) and methods the
//! game adds at run time. Lookup walks the game's superclass chain until a
//! method is found or the chain reaches a framework (host) class.

use guest32_image::{ClassTarget, Ivar32, Method32, ObjcMetadata32, Property32};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestMethod {
    /// Code address, Thumb bit as stored.
    pub imp: u32,
    pub types: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    Guest(&'a GuestMethod),
    /// The chain reached this framework class (or its metaclass) without
    /// finding the selector: the host object answers.
    Framework { class: String, meta: bool },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Super {
    None,
    Game(u32),
    Framework(String),
}

#[derive(Clone, Debug)]
pub struct GuestClass {
    pub address: u32,
    pub name: String,
    pub meta: bool,
    /// The metaclass of a class, the class of a metaclass.
    pub partner: u32,
    pub superclass: Super,
    pub flags: u32,
    pub instance_start: u32,
    pub instance_size: u32,
    pub ivars: Vec<Ivar32>,
    pub methods: HashMap<String, GuestMethod>,
    /// Declared properties, its categories' first (as Apple's runtime attaches them).
    pub properties: Vec<Property32>,
}

#[derive(Default)]
pub struct GuestClasses {
    classes: HashMap<u32, GuestClass>,
    by_name: HashMap<String, u32>,
    /// Game categories on framework classes: (class name, metaclass) -> methods.
    framework_categories: HashMap<(String, bool), HashMap<String, GuestMethod>>,
    /// Some game method's type encoding per selector name.
    selector_types: HashMap<String, String>,
}

fn method_map(list: &[Method32]) -> HashMap<String, GuestMethod> {
    list.iter().map(|m| (m.name.clone(), GuestMethod { imp: m.imp, types: m.types.clone() })).collect()
}

fn superclass_of(target: &ClassTarget, what: &str) -> Result<Super, String> {
    Ok(match target {
        ClassTarget::Local(address) => Super::Game(*address),
        ClassTarget::External(name) => Super::Framework(name.clone()),
        ClassTarget::Null => Super::None,
        ClassTarget::OtherSymbol(symbol) => return Err(format!("{what}: superclass bound to non-class symbol {symbol}")),
    })
}

impl GuestClasses {
    pub fn from_metadata(md: &ObjcMetadata32) -> Result<GuestClasses, String> {
        let mut table = GuestClasses::default();
        for c in &md.classes {
            table.classes.insert(
                c.address,
                GuestClass {
                    address: c.address,
                    name: c.name.clone(),
                    meta: false,
                    partner: c.meta_address,
                    superclass: superclass_of(&c.superclass, &c.name)?,
                    flags: c.instance.flags,
                    instance_start: c.instance.instance_start,
                    instance_size: c.instance.instance_size,
                    ivars: c.instance.ivars.clone(),
                    methods: method_map(&c.instance.methods),
                    properties: c.instance.properties.clone(),
                },
            );
            table.classes.insert(
                c.meta_address,
                GuestClass {
                    address: c.meta_address,
                    name: c.name.clone(),
                    meta: true,
                    partner: c.address,
                    superclass: superclass_of(&c.meta_superclass, &c.name)?,
                    flags: c.meta.flags,
                    instance_start: c.meta.instance_start,
                    instance_size: c.meta.instance_size,
                    ivars: Vec::new(),
                    methods: method_map(&c.meta.methods),
                    properties: c.meta.properties.clone(),
                },
            );
            table.by_name.insert(c.name.clone(), c.address);
        }
        for category in &md.categories {
            match &category.class {
                ClassTarget::Local(address) => {
                    let meta = table
                        .classes
                        .get(address)
                        .map(|c| c.partner)
                        .ok_or_else(|| format!("category {} names unknown class {address:#x}", category.name))?;
                    for (target, list) in [(*address, &category.instance_methods), (meta, &category.class_methods)] {
                        table.classes.get_mut(&target).unwrap().methods.extend(method_map(list));
                    }
                    let class = table.classes.get_mut(address).unwrap();
                    class.properties.splice(0..0, category.instance_properties.iter().cloned());
                }
                ClassTarget::External(name) => {
                    for (meta, list) in [(false, &category.instance_methods), (true, &category.class_methods)] {
                        table.framework_categories.entry((name.clone(), meta)).or_default().extend(method_map(list));
                    }
                }
                other => return Err(format!("category {} on {other:?}", category.name)),
            }
        }
        let all = table.classes.values().map(|c| &c.methods).chain(table.framework_categories.values());
        let mut types = HashMap::new();
        for methods in all {
            for (name, m) in methods {
                types.entry(name.clone()).or_insert_with(|| m.types.clone());
            }
        }
        table.selector_types = types;
        Ok(table)
    }

    /// The type encoding of some game method named `sel` (an override has
    /// the framework method's types).
    pub fn types_for_selector(&self, sel: &str) -> Option<&str> {
        self.selector_types.get(sel).map(String::as_str)
    }

    pub fn lookup(&self, mut class: u32, sel: &str) -> Lookup<'_> {
        for _ in 0..256 {
            let Some(c) = self.classes.get(&class) else { return Lookup::Missing };
            if let Some(method) = c.methods.get(sel) {
                return Lookup::Guest(method);
            }
            match &c.superclass {
                Super::Game(next) => class = *next,
                Super::Framework(name) => return Lookup::Framework { class: name.clone(), meta: c.meta },
                Super::None => return Lookup::Missing,
            }
        }
        Lookup::Missing
    }

    pub fn class_named(&self, name: &str) -> Option<&GuestClass> {
        self.classes.get(self.by_name.get(name)?)
    }

    pub fn class(&self, address: u32) -> Option<&GuestClass> {
        self.classes.get(&address)
    }

    /// Game classes in an order where every superclass comes first.
    pub fn classes_parents_first(&self) -> Vec<&GuestClass> {
        let mut out: Vec<&GuestClass> = Vec::new();
        let mut done = std::collections::HashSet::new();
        let mut names: Vec<&String> = self.by_name.keys().collect();
        names.sort();
        for name in names {
            let mut chain = Vec::new();
            let mut at = self.by_name[name];
            while let Some(c) = self.classes.get(&at) {
                if done.contains(&at) {
                    break;
                }
                chain.push(c);
                match c.superclass {
                    Super::Game(next) => at = next,
                    _ => break,
                }
            }
            for c in chain.into_iter().rev() {
                if done.insert(c.address) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// Apple's ivar sliding (moveIvars): a class compiled when its game
    /// superclass was smaller (ivars declared in the superclass's
    /// @implementation are not seen by subclasses) starts where the
    /// superclass now ends, its ivars moved by one amount that keeps their
    /// largest alignment. Parents first, so slides add up. Returns each moved
    /// ivar offset variable and how far it moved, for guest memory.
    pub fn slide_ivars(&mut self) -> Vec<(u32, u32)> {
        let order: Vec<u32> = self.classes_parents_first().iter().map(|c| c.address).collect();
        let mut moved = Vec::new();
        for address in order {
            let Super::Game(parent) = self.classes[&address].superclass else { continue };
            let super_size = self.classes.get(&parent).map_or(0, |p| p.instance_size);
            let class = self.classes.get_mut(&address).unwrap();
            if class.instance_start >= super_size {
                continue;
            }
            let align = class.ivars.iter().map(|i| 1u32 << i.alignment.min(4)).max().unwrap_or(1);
            let diff = (super_size - class.instance_start).div_ceil(align) * align;
            for ivar in &mut class.ivars {
                ivar.offset += diff;
                if ivar.offset_address != 0 {
                    moved.push((ivar.offset_address, diff));
                }
            }
            class.instance_start += diff;
            class.instance_size += diff;
        }
        moved
    }

    pub fn framework_category_method(&self, class: &str, meta: bool, sel: &str) -> Option<&GuestMethod> {
        self.framework_categories.get(&(class.to_string(), meta))?.get(sel)
    }

    /// Every game method on a framework class: (class name, meta, selector).
    pub fn framework_category_entries(&self) -> Vec<(String, bool, String)> {
        self.framework_categories
            .iter()
            .flat_map(|((class, meta), methods)| methods.keys().map(move |sel| (class.clone(), *meta, sel.clone())))
            .collect()
    }

    /// The selectors a class (or metaclass) itself answers.
    pub fn selectors_of(&self, class: u32) -> Vec<&str> {
        self.classes.get(&class).map(|c| c.methods.keys().map(String::as_str).collect()).unwrap_or_default()
    }

    /// class_addMethod on a framework class: kept like a category method.
    pub fn add_framework_method(&mut self, class: &str, meta: bool, sel: &str, imp: u32, types: &str) -> bool {
        let methods = self.framework_categories.entry((class.to_string(), meta)).or_default();
        if methods.contains_key(sel) {
            return false;
        }
        methods.insert(sel.to_string(), GuestMethod { imp, types: types.to_string() });
        true
    }

    /// class_addMethod: adds unless the class itself already has the selector.
    pub fn add_method(&mut self, class: u32, sel: &str, imp: u32, types: &str) -> bool {
        let Some(c) = self.classes.get_mut(&class) else { return false };
        if c.methods.contains_key(sel) {
            return false;
        }
        c.methods.insert(sel.to_string(), GuestMethod { imp, types: types.to_string() });
        true
    }
}
