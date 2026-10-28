//! A 32-bit game's protocols on the host. Each protocol_t in the game's
//! image gets a host protocol_t in the layout the host runtime reads (isa,
//! name, protocols, the four method lists, properties, size, flags), so a
//! protocol crosses the bridge and the host's -conformsToProtocol: and
//! protocol_* functions answer for game classes. A protocol the host knows
//! by that name already is used as it is.

use crate::objc::runtime;
use crate::symbols::{leak, leak_cstr};
use guest32_image::{Method32, ObjcMetadata32, Property32};
use std::collections::HashMap;

/// The host protocol of every game protocol, by its guest address.
pub fn host_protocols(md: &ObjcMetadata32) -> HashMap<u32, usize> {
    let mut made = HashMap::new();
    for &address in md.protocols_by_address.keys() {
        host_protocol(md, address, &mut made, 0);
    }
    made
}

fn host_protocol(md: &ObjcMetadata32, address: u32, made: &mut HashMap<u32, usize>, depth: u32) -> usize {
    if let Some(&host) = made.get(&address) {
        return host;
    }
    // Protocols including each other in a loop end here.
    let Some(protocol) = md.protocols_by_address.get(&address).filter(|_| depth < 32) else { return 0 };
    let known = runtime::protocol_named(&protocol.name);
    if known != 0 {
        made.insert(address, known);
        return known;
    }
    let parents: Vec<usize> = protocol.protocols.iter().map(|&parent| host_protocol(md, parent, made, depth + 1)).filter(|&p| p != 0).collect();
    const SIZE: usize = 96;
    let host = leak([
        0,
        leak_cstr(&protocol.name),
        protocol_list(&parents),
        method_list(&protocol.instance_methods),
        method_list(&protocol.class_methods),
        method_list(&protocol.optional_instance_methods),
        method_list(&protocol.optional_class_methods),
        property_list(&protocol.properties),
        SIZE, // size, then flags 0
        0,    // extended method types
        0,    // demangled name
        0,    // class properties
    ]);
    let host = runtime::register_protocol(&protocol.name, host);
    made.insert(address, host);
    host
}

/// protocol_list_t: count (64-bit), then the protocols; 0 when empty.
fn protocol_list(protocols: &[usize]) -> usize {
    if protocols.is_empty() {
        return 0;
    }
    let words: Vec<usize> = std::iter::once(protocols.len()).chain(protocols.iter().copied()).collect();
    Box::leak(words.into_boxed_slice()).as_ptr() as usize
}

/// method_list_t: entry size 24 and count, then { name, types, imp 0 }.
fn method_list(methods: &[Method32]) -> usize {
    if methods.is_empty() {
        return 0;
    }
    let mut words = vec![24 | (methods.len() << 32)];
    for m in methods {
        words.extend([leak_cstr(&m.name), leak_cstr(&m.types), 0]);
    }
    Box::leak(words.into_boxed_slice()).as_ptr() as usize
}

/// property_list_t: entry size 16 and count, then { name, attributes }.
fn property_list(properties: &[Property32]) -> usize {
    if properties.is_empty() {
        return 0;
    }
    let mut words = vec![16 | (properties.len() << 32)];
    for p in properties {
        words.extend([leak_cstr(&p.name), leak_cstr(&p.attributes)]);
    }
    Box::leak(words.into_boxed_slice()).as_ptr() as usize
}
