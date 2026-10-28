//! NSAttributedString: text with one set of attributes (font, colour, ...)
//! over all of it. Changes to part of the text apply to all of it.

use crate::fw::object::{define_class, methods};
use crate::fw::string::{self, Range};
use crate::fw::uikit::{property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};

/// The attribute names' values, as on Apple.
const NAMES: [(&str, &str); 6] = [
    ("NSFontAttributeName", "NSFont"),
    ("NSForegroundColorAttributeName", "NSColor"),
    ("NSBackgroundColorAttributeName", "NSBackgroundColor"),
    ("NSShadowAttributeName", "NSShadow"),
    ("NSParagraphStyleAttributeName", "NSParagraphStyle"),
    ("NSKernAttributeName", "NSKern"),
];

/// The attribute named `name` ("NSFont", "NSColor", ...) of an attributed string (0: none).
pub fn attribute(text: Id, name: &str) -> Id {
    let attributes = attributes_of(text);
    if attributes == 0 {
        return 0;
    }
    runtime::send(attributes, "objectForKey:", &[string::make(name)])
}

/// The plain text of an attributed string.
pub fn text(text: Id) -> String {
    match property_get(text, "string") {
        Some(s) if s != 0 => string::get(s as Id),
        _ => String::new(),
    }
}

fn attributes_of(text: Id) -> Id {
    property_get(text, "attributes").unwrap_or(0) as Id
}

/// Keeps `text`'s string (copied) and attributes (copied, 0 for none).
fn store(this: Id, text: Id, attributes: Id) {
    let text = if text == 0 { String::new() } else { string::get(text) };
    property_set(this, "string", string::make(&text) as u64);
    set_attributes(this, attributes);
}

fn set_attributes(this: Id, attributes: Id) {
    let copy = if attributes == 0 { 0 } else { runtime::send(attributes, "mutableCopyWithZone:", &[0]) };
    property_set(this, "attributes", copy as u64);
}

extern "C" fn init(this: Id, _: Sel) -> Id {
    store(this, 0, 0);
    this
}

extern "C" fn init_with_string(this: Id, _: Sel, text: Id) -> Id {
    store(this, text, 0);
    this
}

extern "C" fn init_with_string_attributes(this: Id, _: Sel, text: Id, attributes: Id) -> Id {
    store(this, text, attributes);
    this
}

extern "C" fn init_with_attributed(this: Id, _: Sel, other: Id) -> Id {
    if other == 0 {
        store(this, 0, 0);
    } else {
        store(this, property_get(other, "string").unwrap_or(0) as Id, attributes_of(other));
    }
    this
}

/// From a nib: NSString and NSAttributes (one run's dictionary, or a list
/// of them, of which the first is used).
extern "C" fn init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let decode = |key: &str| runtime::send(coder, "decodeObjectForKey:", &[string::make(key)]);
    let mut attributes = decode("NSAttributes");
    if attributes != 0 && !runtime::responds(attributes, "objectForKey:") {
        attributes = if runtime::responds(attributes, "objectAtIndex:") && runtime::send(attributes, "count", &[]) > 0 {
            runtime::send(attributes, "objectAtIndex:", &[0])
        } else {
            0
        };
    }
    store(this, decode("NSString"), attributes);
    this
}

extern "C" fn get_string(this: Id, _: Sel) -> Id {
    property_get(this, "string").unwrap_or(0) as Id
}

extern "C" fn length(this: Id, _: Sel) -> usize {
    text(this).encode_utf16().count()
}

/// The attributes at an index: the same over the whole text.
extern "C" fn attributes_at(this: Id, _: Sel, _index: usize, range: *mut Range) -> Id {
    if !range.is_null() {
        unsafe { *range = Range { location: 0, length: length(this, runtime::sel("length")) } };
    }
    match attributes_of(this) {
        0 => crate::fw::collections::dictionary_from(&[]),
        attributes => attributes,
    }
}

extern "C" fn attribute_at(this: Id, _: Sel, name: Id, _index: usize, range: *mut Range) -> Id {
    if !range.is_null() {
        unsafe { *range = Range { location: 0, length: length(this, runtime::sel("length")) } };
    }
    attribute(this, &string::get(name))
}

fn copy_into(this: Id, class: Class) -> Id {
    let copy = runtime::send(class, "alloc", &[]);
    init_with_attributed(copy, runtime::sel("initWithAttributedString:"), this)
}

extern "C" fn copy_with_zone(this: Id, _: Sel, _zone: usize) -> Id {
    copy_into(this, runtime::class_named("NSAttributedString"))
}

extern "C" fn mutable_copy_with_zone(this: Id, _: Sel, _zone: usize) -> Id {
    copy_into(this, runtime::class_named("NSMutableAttributedString"))
}

extern "C" fn is_equal(this: Id, _: Sel, other: Id) -> bool {
    other != 0 && runtime::responds(other, "attributesAtIndex:effectiveRange:") && text(this) == text(other)
}

extern "C" fn description(this: Id, _: Sel) -> Id {
    string::make(&text(this))
}

// ----- NSMutableAttributedString: changes to any range apply to all of it -----

fn mutable_attributes(this: Id) -> Id {
    if attributes_of(this) == 0 {
        set_attributes(this, crate::fw::collections::dictionary_from(&[]));
    }
    attributes_of(this)
}

extern "C" fn add_attribute(this: Id, _: Sel, name: Id, value: Id, _range: Range) {
    runtime::send(mutable_attributes(this), "setObject:forKey:", &[value, name]);
}

extern "C" fn add_attributes(this: Id, _: Sel, attributes: Id, _range: Range) {
    if attributes != 0 {
        runtime::send(mutable_attributes(this), "addEntriesFromDictionary:", &[attributes]);
    }
}

extern "C" fn set_attributes_range(this: Id, _: Sel, attributes: Id, _range: Range) {
    set_attributes(this, attributes);
}

extern "C" fn remove_attribute(this: Id, _: Sel, name: Id, _range: Range) {
    runtime::send(mutable_attributes(this), "removeObjectForKey:", &[name]);
}

extern "C" fn append(this: Id, _: Sel, other: Id) {
    let joined = text(this) + &if other == 0 { String::new() } else { text(other) };
    let attributes = attributes_of(this);
    let attributes = if attributes == 0 && other != 0 { attributes_of(other) } else { attributes };
    store(this, string::make(&joined), attributes);
}

extern "C" fn replace_characters(this: Id, _: Sel, range: Range, with: Id) {
    let units: Vec<u16> = text(this).encode_utf16().collect();
    let start = range.location.min(units.len());
    let end = (range.location + range.length).min(units.len());
    let mut joined = String::from_utf16_lossy(&units[..start]);
    joined += &if with == 0 { String::new() } else { string::get(with) };
    joined += &String::from_utf16_lossy(&units[end..]);
    store(this, string::make(&joined), attributes_of(this));
}

extern "C" fn mutable_string(this: Id, _: Sel) -> Id {
    get_string(this, runtime::sel("string"))
}

pub fn register(r: &mut Registry) {
    let class = define_class("NSAttributedString", "NSObject", 8);
    let mutable = define_class("NSMutableAttributedString", "NSAttributedString", 8);
    methods(
        class,
        &[
            ("init", init as usize),
            ("initWithString:", init_with_string as usize),
            ("initWithString:attributes:", init_with_string_attributes as usize),
            ("initWithAttributedString:", init_with_attributed as usize),
            ("initWithCoder:", init_with_coder as usize),
            ("string", get_string as usize),
            ("length", length as usize),
            ("attributesAtIndex:effectiveRange:", attributes_at as usize),
            ("attribute:atIndex:effectiveRange:", attribute_at as usize),
            ("copyWithZone:", copy_with_zone as usize),
            ("mutableCopyWithZone:", mutable_copy_with_zone as usize),
            ("isEqual:", is_equal as usize),
            ("isEqualToAttributedString:", is_equal as usize),
            ("description", description as usize),
        ],
    );
    methods(
        mutable,
        &[
            ("addAttribute:value:range:", add_attribute as usize),
            ("addAttributes:range:", add_attributes as usize),
            ("setAttributes:range:", set_attributes_range as usize),
            ("removeAttribute:range:", remove_attribute as usize),
            ("appendAttributedString:", append as usize),
            ("replaceCharactersInRange:withString:", replace_characters as usize),
            ("mutableString", mutable_string as usize),
        ],
    );
    for (name, value) in NAMES {
        r.data(name, leak(string::make(value)));
    }
}
