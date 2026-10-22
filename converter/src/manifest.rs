//! Edits the binary AndroidManifest.xml (Android's compiled XML format).
//!
//! `android:extractNativeLibs`: a game built with it set to false keeps its
//! libraries packed inside the APK, but the bridge and stand-in read the
//! 32-bit originals as files next to them. Setting it to true makes Android
//! unpack them at install time.
//!
//! `android:targetSdkVersion`: Android refuses to install games made for very
//! old versions (Android 17: below 24) unless the installer is the shell or
//! root. Without root the converter raises it to the phone's minimum.
//!
//! Values are changed in place, so nothing else in the file moves.
//!
//! `<provider>`: a content provider starts before any other part of the game,
//! so the converter can add one to set things up first. Adding one rebuilds
//! the file: new strings go at the end of the string pool, so the numbers of
//! the old strings (which the rest of the file refers to) stay the same.

/// Resource ID of the android:extractNativeLibs attribute.
const EXTRACT_NATIVE_LIBS: u32 = 0x0101_04ea;
const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
const TYPE_STRING: u8 = 0x03;
const TYPE_INT_BOOLEAN: u8 = 0x12;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
/// Resource IDs of android:minSdkVersion and android:targetSdkVersion.
const MIN_SDK_VERSION: u32 = 0x0101_020c;
const TARGET_SDK_VERSION: u32 = 0x0101_0270;
/// Resource IDs of the attributes a provider gets, in the order Android wants
/// them (sorted by ID).
const NAME: u32 = 0x0101_0003;
const EXPORTED: u32 = 0x0101_0010;
const AUTHORITIES: u32 = 0x0101_0018;
const INIT_ORDER: u32 = 0x0101_001a;
const ANDROID_NAMESPACE: &str = "http://schemas.android.com/apk/res/android";
/// String pool flags: strings are UTF-8 (else UTF-16), and strings are sorted.
const UTF8_FLAG: u32 = 0x100;
const SORTED_FLAG: u32 = 0x1;
/// Stands for "no string" in a string index field.
const NO_STRING: u32 = u32::MAX;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Calls `visit(resource ID, offset of the typed value)` for every attribute
/// of every element. None if the file isn't compiled XML.
fn attributes(data: &[u8], mut visit: impl FnMut(u32, usize)) -> Option<()> {
    if u16_at(data, 0)? != 0x0003 {
        return None;
    }
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut at = u16_at(data, 2)? as usize;
    while at + 8 <= data.len() {
        let (kind, header_size, size) = (u16_at(data, at)?, u16_at(data, at + 2)? as usize, u32_at(data, at + 4)? as usize);
        if size < 8 || at + size > data.len() {
            return None;
        }
        match kind {
            RES_XML_RESOURCE_MAP_TYPE => {
                resource_ids = (at + header_size..at + size).step_by(4).filter_map(|i| u32_at(data, i)).collect();
            }
            RES_XML_START_ELEMENT_TYPE => {
                // After the chunk header: namespace, name, then the attribute layout.
                let body = at + header_size;
                let attribute_start = u16_at(data, body + 8)? as usize;
                let attribute_size = u16_at(data, body + 10)? as usize;
                let attribute_count = u16_at(data, body + 12)? as usize;
                for i in 0..attribute_count {
                    let attribute = body + attribute_start + i * attribute_size;
                    let name = u32_at(data, attribute + 4)? as usize;
                    // Typed value: size (2), reserved (1), type (1), data (4), after namespace, name, raw string.
                    if let Some(&id) = resource_ids.get(name) {
                        if attribute + 20 <= data.len() {
                            visit(id, attribute + 12);
                        }
                    }
                }
            }
            _ => {}
        }
        at += size;
    }
    Some(())
}

/// Sets every android:extractNativeLibs="false" to true. Returns whether
/// anything changed (None if the file isn't compiled XML).
pub fn extract_native_libs(data: &mut [u8]) -> Option<bool> {
    let mut found = Vec::new();
    attributes(data, |id, value| {
        if id == EXTRACT_NATIVE_LIBS {
            found.push(value);
        }
    })?;
    let mut changed = false;
    for value in found {
        if data[value + 3] == TYPE_INT_BOOLEAN && u32_at(data, value + 4)? == 0 {
            data[value + 4..value + 8].copy_from_slice(&u32::MAX.to_le_bytes());
            changed = true;
        }
    }
    Some(changed)
}

/// Makes the game target at least `minimum`. Raises android:targetSdkVersion;
/// without one, Android uses minSdkVersion as the target, so that is raised
/// instead (the phone already runs at least `minimum`). Returns the old and
/// new target when it changed, Ok(None) when it was high enough, and Err when
/// there is nothing to change in place (no number to raise).
pub fn raise_target_sdk(data: &mut [u8], minimum: u32) -> Result<Option<(u32, u32)>, &'static str> {
    let (mut target, mut min_sdk) = (None, None);
    attributes(data, |id, value| match id {
        TARGET_SDK_VERSION => target = Some(value),
        MIN_SDK_VERSION => min_sdk = Some(value),
        _ => {}
    })
    .ok_or("the manifest isn't compiled XML")?;
    let value = target.or(min_sdk).ok_or("the manifest names no target or minimum Android version")?;
    if ![TYPE_INT_DEC, TYPE_INT_HEX].contains(&data[value + 3]) {
        return Err("the manifest's target Android version isn't a number");
    }
    let old = u32_at(data, value + 4).ok_or("the manifest is cut short")?;
    if old >= minimum {
        return Ok(None);
    }
    data[value + 4..value + 8].copy_from_slice(&minimum.to_le_bytes());
    Ok(Some((old, minimum)))
}

fn put_u32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// The string pool chunk, with its strings read into plain text. Offsets
/// count from the start of the chunk.
struct StringPool {
    at: usize,
    size: usize,
    header_size: usize,
    flags: u32,
    strings: Vec<String>,
    style_count: usize,
    strings_start: usize,
    styles_start: usize,
}

/// Reads the string pool chunk at `at`. None if it is broken.
fn read_string_pool(data: &[u8], at: usize) -> Option<StringPool> {
    let header_size = u16_at(data, at + 2)? as usize;
    let size = u32_at(data, at + 4)? as usize;
    let count = u32_at(data, at + 8)? as usize;
    let style_count = u32_at(data, at + 12)? as usize;
    let flags = u32_at(data, at + 16)?;
    let strings_start = u32_at(data, at + 20)? as usize;
    let styles_start = u32_at(data, at + 24)? as usize;
    let chunk = data.get(at..at.checked_add(size)?)?;
    let offsets_end = count.checked_add(style_count)?.checked_mul(4)?.checked_add(header_size)?;
    let strings_end = if style_count > 0 { styles_start } else { size };
    if header_size < 28 || offsets_end > size || strings_end > size {
        return None;
    }
    if count > 0 && (strings_start < offsets_end || strings_start > strings_end) {
        return None;
    }
    let string_data = chunk.get(strings_start.min(strings_end)..strings_end)?;
    let utf8 = flags & UTF8_FLAG != 0;
    let strings = (0..count)
        .map(|i| read_string(string_data, u32_at(chunk, header_size + 4 * i)? as usize, utf8))
        .collect::<Option<Vec<_>>>()?;
    Some(StringPool { at, size, header_size, flags, strings, style_count, strings_start, styles_start })
}

/// Reads one string at `at` in the string data.
fn read_string(bytes: &[u8], at: usize, utf8: bool) -> Option<String> {
    if utf8 {
        // Two lengths, in UTF-16 units and then in bytes.
        let (_, at) = utf8_length(bytes, at)?;
        let (length, at) = utf8_length(bytes, at)?;
        Some(String::from_utf8_lossy(bytes.get(at..at.checked_add(length)?)?).into_owned())
    } else {
        // Length in UTF-16 units; a second unit follows when the top bit is set.
        let first = u16_at(bytes, at)? as usize;
        let (length, at) = if first & 0x8000 == 0 {
            (first, at + 2)
        } else {
            ((first & 0x7fff) << 16 | u16_at(bytes, at + 2)? as usize, at + 4)
        };
        let units: Vec<u16> = bytes
            .get(at..at.checked_add(length.checked_mul(2)?)?)?
            .chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    }
}

/// Reads one UTF-8 pool length: one byte, or two when the top bit is set.
/// Returns the length and where the next field starts.
fn utf8_length(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let first = *bytes.get(at)? as usize;
    if first & 0x80 == 0 {
        return Some((first, at + 1));
    }
    Some(((first & 0x7f) << 8 | *bytes.get(at + 1)? as usize, at + 2))
}

/// Writes one string the way the pool stores it: its lengths, its text, and
/// an ending zero. Lengths must fit in 0x7fff.
fn encode_string(text: &str, utf8: bool) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Vec::new();
    if utf8 {
        for length in [units.len(), text.len()] {
            if length > 0x7f {
                out.push((length >> 8) as u8 | 0x80);
            }
            out.push(length as u8);
        }
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    } else {
        out.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// Rebuilds the string pool chunk with `added` strings after the old ones.
/// Old strings keep their numbers and bytes. Styles belong to the first
/// strings and their offsets count from the style data, which moves as a
/// whole, so they are copied as they were. The pool is no longer sorted.
fn append_strings(data: &[u8], pool: &StringPool, added: &[String]) -> Vec<u8> {
    let chunk = &data[pool.at..pool.at + pool.size];
    let utf8 = pool.flags & UTF8_FLAG != 0;
    let old_count = pool.strings.len();
    let offsets_end = pool.header_size + 4 * old_count;
    let mut offsets = chunk[pool.header_size..offsets_end].to_vec();
    let style_offsets = &chunk[offsets_end..offsets_end + 4 * pool.style_count];
    let strings_end = if pool.style_count > 0 { pool.styles_start } else { pool.size };
    let mut string_data = if old_count > 0 { chunk[pool.strings_start..strings_end].to_vec() } else { Vec::new() };
    for text in added {
        offsets.extend_from_slice(&(string_data.len() as u32).to_le_bytes());
        string_data.extend(encode_string(text, utf8));
    }
    while string_data.len() % 4 != 0 {
        string_data.push(0);
    }
    let style_data = if pool.style_count > 0 { &chunk[pool.styles_start..] } else { &[][..] };
    let strings_start = pool.header_size + offsets.len() + style_offsets.len();
    let styles_start = if pool.style_count > 0 { strings_start + string_data.len() } else { 0 };

    let mut out = chunk[..pool.header_size].to_vec();
    out.extend(offsets);
    out.extend_from_slice(style_offsets);
    out.extend(string_data);
    out.extend_from_slice(style_data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let size = out.len() as u32;
    put_u32(&mut out, 4, size);
    put_u32(&mut out, 8, (old_count + added.len()) as u32);
    put_u32(&mut out, 16, pool.flags & !SORTED_FLAG);
    put_u32(&mut out, 20, strings_start as u32);
    put_u32(&mut out, 24, styles_start as u32);
    out
}

/// One attribute of an element: namespace, name, raw string value (or
/// NO_STRING), value type, and value. Names and strings are string numbers.
type Attribute = (u32, u32, u32, u8, u32);

/// Builds a start element chunk (no namespace, no comment).
fn start_element(line: u32, name: u32, attributes: &[Attribute]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&RES_XML_START_ELEMENT_TYPE.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&(36 + 20 * attributes.len() as u32).to_le_bytes());
    for field in [line, NO_STRING, NO_STRING, name] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    // Attributes start 20 bytes in and take 20 bytes each; no id, class or style attribute.
    for field in [20u16, 20, attributes.len() as u16, 0, 0, 0] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    for &(namespace, name, raw, kind, value) in attributes {
        for field in [namespace, name, raw] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&[0, kind]);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Builds an end element chunk (no namespace, no comment).
fn end_element(line: u32, name: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&RES_XML_END_ELEMENT_TYPE.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&24u32.to_le_bytes());
    for field in [line, NO_STRING, NO_STRING, name] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out
}

/// The `package` attribute of `<manifest>`: the app's package name.
pub fn package_name(data: &[u8]) -> Option<String> {
    manifest_attribute(data, "package")
}

/// The `split` attribute of `<manifest>`: set in every piece of a split app
/// except the base one (feature pieces carry code too, so this tells them apart).
pub fn split_name(data: &[u8]) -> Option<String> {
    manifest_attribute(data, "split")
}

/// A plain (namespace-less) attribute of the `<manifest>` element, by name.
fn manifest_attribute(data: &[u8], wanted: &str) -> Option<String> {
    if u16_at(data, 0)? != 0x0003 {
        return None;
    }
    let mut pool = None;
    let mut at = u16_at(data, 2)? as usize;
    while at + 8 <= data.len() {
        let (kind, header_size, size) = (u16_at(data, at)?, u16_at(data, at + 2)? as usize, u32_at(data, at + 4)? as usize);
        if size < 8 || at + size > data.len() {
            return None;
        }
        match kind {
            RES_STRING_POOL_TYPE if pool.is_none() => pool = Some(read_string_pool(data, at)?),
            RES_XML_START_ELEMENT_TYPE => {
                let strings = &pool.as_ref()?.strings;
                let text = |number: u32| strings.get(number as usize).cloned();
                let body = at + header_size;
                if text(u32_at(data, body + 4)?)? != "manifest" {
                    return None;
                }
                let attribute_start = u16_at(data, body + 8)? as usize;
                let attribute_size = u16_at(data, body + 10)? as usize;
                for i in 0..u16_at(data, body + 12)? as usize {
                    let attribute = body + attribute_start + i * attribute_size;
                    if text(u32_at(data, attribute + 4)?).as_deref() == Some(wanted) {
                        return match *data.get(attribute + 15)? {
                            TYPE_STRING => text(u32_at(data, attribute + 16)?),
                            _ => text(u32_at(data, attribute + 8)?),
                        };
                    }
                }
                return None;
            }
            _ => {}
        }
        at += size;
    }
    None
}

/// The number of a string with this text, adding it at the end if there is none.
fn string_number(strings: &mut Vec<String>, text: &str) -> u32 {
    match strings.iter().position(|string| string == text) {
        Some(number) => number as u32,
        None => {
            strings.push(text.to_string());
            (strings.len() - 1) as u32
        }
    }
}

/// Adds `<provider android:name=class android:authorities=authority
/// android:exported="false" android:initOrder="2147483647"/>` as the first
/// thing inside `<application>`. The highest init order makes Android start
/// it before the game's own providers. Returns the file unchanged when a
/// provider with this name is already there, so converting twice is safe.
/// Err when the file isn't compiled XML or has no `<application>`.
pub fn add_provider(data: &[u8], class: &str, authority: &str) -> Result<Vec<u8>, &'static str> {
    let not_xml = "the manifest isn't compiled XML";
    if class.len() > 0x7fff || authority.len() > 0x7fff {
        return Err("the provider's name is too long");
    }
    if u16_at(data, 0) != Some(0x0003) {
        return Err(not_xml);
    }
    let file_header_size = u16_at(data, 2).ok_or(not_xml)? as usize;
    if !(8..=data.len()).contains(&file_header_size) {
        return Err(not_xml);
    }
    // Every chunk: where it starts, its type, header size and size.
    let mut chunks = Vec::new();
    let mut at = file_header_size;
    while at + 8 <= data.len() {
        let (kind, header_size, size) = (u16_at(data, at), u16_at(data, at + 2), u32_at(data, at + 4));
        let (kind, header_size, size) = (kind.ok_or(not_xml)?, header_size.ok_or(not_xml)? as usize, size.ok_or(not_xml)? as usize);
        if size < 8 || header_size > size || at + size > data.len() {
            return Err(not_xml);
        }
        chunks.push((at, kind, header_size, size));
        at += size;
    }
    let tail = at;

    let &(pool_at, ..) = chunks.iter().find(|chunk| chunk.1 == RES_STRING_POOL_TYPE).ok_or(not_xml)?;
    let pool = read_string_pool(data, pool_at).ok_or("the manifest's string pool is broken")?;
    let text = |number: u32| pool.strings.get(number as usize).map(String::as_str);
    let map_chunk = chunks.iter().find(|chunk| chunk.1 == RES_XML_RESOURCE_MAP_TYPE).copied();
    let resource_ids: Vec<u32> = match map_chunk {
        Some((at, _, header_size, size)) => (at + header_size..at + size).step_by(4).filter_map(|i| u32_at(data, i)).collect(),
        None => Vec::new(),
    };

    let mut android = None;
    let mut application = None;
    for &(at, kind, header_size, size) in &chunks {
        let body = at + header_size;
        match kind {
            RES_XML_START_NAMESPACE_TYPE if android.is_none() => {
                let uri = u32_at(data, body + 4).ok_or(not_xml)?;
                if text(uri) == Some(ANDROID_NAMESPACE) {
                    android = Some(uri);
                }
            }
            RES_XML_START_ELEMENT_TYPE => {
                let name = text(u32_at(data, body + 4).ok_or(not_xml)?);
                if name == Some("application") && application.is_none() {
                    application = Some((at + size, u32_at(data, at + 8).ok_or(not_xml)?));
                }
                if name != Some("provider") {
                    continue;
                }
                let attribute_start = u16_at(data, body + 8).ok_or(not_xml)? as usize;
                let attribute_size = u16_at(data, body + 10).ok_or(not_xml)? as usize;
                let attribute_count = u16_at(data, body + 12).ok_or(not_xml)? as usize;
                for i in 0..attribute_count {
                    let attribute = body + attribute_start + i * attribute_size;
                    let name = u32_at(data, attribute + 4).ok_or(not_xml)? as usize;
                    if resource_ids.get(name) != Some(&NAME) {
                        continue;
                    }
                    let value = match data.get(attribute + 15) {
                        Some(&TYPE_STRING) => u32_at(data, attribute + 16),
                        _ => u32_at(data, attribute + 8),
                    };
                    if value.and_then(text) == Some(class) {
                        return Ok(data.to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    let android = android.ok_or("the manifest doesn't declare the android namespace")?;
    let (application_end, line) = application.ok_or("the manifest has no <application>")?;

    // Attribute names need their resource ID in the resource map. An old
    // string only counts if the map gives it the right ID; otherwise a new
    // copy goes at the end and the map grows to reach it.
    let mut strings = pool.strings.clone();
    let mut new_resource_ids = resource_ids.clone();
    let mut names = [0u32; 4];
    let wanted = [("name", NAME), ("exported", EXPORTED), ("authorities", AUTHORITIES), ("initOrder", INIT_ORDER)];
    for (slot, (name, id)) in names.iter_mut().zip(wanted) {
        let number = match (0..strings.len()).find(|&i| strings[i] == name && resource_ids.get(i) == Some(&id)) {
            Some(number) => number,
            None => {
                strings.push(name.to_string());
                strings.len() - 1
            }
        };
        if new_resource_ids.len() <= number {
            new_resource_ids.resize(number + 1, 0);
        }
        new_resource_ids[number] = id;
        *slot = number as u32;
    }
    let provider = string_number(&mut strings, "provider");
    let class = string_number(&mut strings, class);
    let authority = string_number(&mut strings, authority);

    let mut provider_chunks = start_element(
        line,
        provider,
        &[
            (android, names[0], class, TYPE_STRING, class),
            (android, names[1], NO_STRING, TYPE_INT_BOOLEAN, 0),
            (android, names[2], authority, TYPE_STRING, authority),
            (android, names[3], NO_STRING, TYPE_INT_DEC, i32::MAX as u32),
        ],
    );
    provider_chunks.extend(end_element(line, provider));

    let pool_chunk = if strings.len() == pool.strings.len() {
        data[pool.at..pool.at + pool.size].to_vec()
    } else {
        append_strings(data, &pool, &strings[pool.strings.len()..])
    };
    let mut map = Vec::new();
    map.extend_from_slice(&RES_XML_RESOURCE_MAP_TYPE.to_le_bytes());
    map.extend_from_slice(&8u16.to_le_bytes());
    map.extend_from_slice(&(8 + 4 * new_resource_ids.len() as u32).to_le_bytes());
    for id in &new_resource_ids {
        map.extend_from_slice(&id.to_le_bytes());
    }

    let mut out = data[..file_header_size].to_vec();
    for &(at, _, _, size) in &chunks {
        if at == pool_at {
            out.extend_from_slice(&pool_chunk);
            if map_chunk.is_none() {
                out.extend_from_slice(&map);
            }
        } else if Some(at) == map_chunk.map(|chunk| chunk.0) {
            out.extend_from_slice(&map);
        } else {
            out.extend_from_slice(&data[at..at + size]);
        }
        if at + size == application_end {
            out.extend_from_slice(&provider_chunks);
        }
    }
    out.extend_from_slice(&data[tail..]);
    let size = out.len() as u32;
    put_u32(&mut out, 4, size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads back the value of attribute `id` (first one) from compiled XML.
    fn value(data: &[u8], id: u32) -> Option<u32> {
        let mut found = None;
        attributes(data, |attribute, at| {
            if attribute == id && found.is_none() {
                found = Some(at);
            }
        })?;
        u32_at(data, found? + 4)
    }

    #[test]
    fn raises_target_sdk() {
        let mut data = include_bytes!("testdata/target23.axml").to_vec();
        assert_eq!(raise_target_sdk(&mut data, 24), Ok(Some((23, 24))));
        assert_eq!(value(&data, TARGET_SDK_VERSION), Some(24));
        assert_eq!(value(&data, MIN_SDK_VERSION), Some(21));
        // Already high enough: nothing changes.
        assert_eq!(raise_target_sdk(&mut data, 24), Ok(None));
        assert_eq!(raise_target_sdk(&mut data, 20), Ok(None));
    }

    #[test]
    fn raises_min_sdk_without_target() {
        let mut data = include_bytes!("testdata/min21-notarget.axml").to_vec();
        assert_eq!(value(&data, TARGET_SDK_VERSION), None);
        assert_eq!(raise_target_sdk(&mut data, 24), Ok(Some((21, 24))));
        assert_eq!(value(&data, MIN_SDK_VERSION), Some(24));
    }

    #[test]
    fn still_turns_on_extract_native_libs() {
        let mut data = include_bytes!("testdata/min21-notarget.axml").to_vec();
        assert_eq!(extract_native_libs(&mut data), Some(true));
        assert_eq!(value(&data, EXTRACT_NATIVE_LIBS), Some(u32::MAX));
        assert_eq!(extract_native_libs(&mut data), Some(false));
    }

    #[test]
    fn not_compiled_xml() {
        let mut data = b"<manifest/>".to_vec();
        assert!(raise_target_sdk(&mut data, 24).is_err());
        assert_eq!(extract_native_libs(&mut data), None);
        assert!(add_provider(&data, CLASS, AUTHORITY).is_err());
    }

    const CLASS: &str = "com.arm32bridge.Setup";
    const AUTHORITY: &str = "com.example.game.arm32bridge.setup";
    /// Resource ID of android:label, the one attribute the built manifests have.
    const LABEL: u32 = 0x0101_0001;

    /// Builds compiled XML for
    /// `<manifest><application android:label="Game"/></manifest>`. The pool
    /// also has a "name" string the resource map doesn't reach, so it must not
    /// be used as android:name. With `styled`, the first string has a style
    /// span (tag "android", characters 0 to 2).
    fn build_manifest(utf8: bool, styled: bool, resource_map: bool) -> Vec<u8> {
        let strings = ["label", "android", ANDROID_NAMESPACE, "manifest", "application", "Game", "name"];
        let style_count = styled as usize;
        let mut offsets = Vec::new();
        let mut string_data = Vec::new();
        for string in strings {
            offsets.extend_from_slice(&(string_data.len() as u32).to_le_bytes());
            string_data.extend(encode_string(string, utf8));
        }
        while string_data.len() % 4 != 0 {
            string_data.push(0);
        }
        let mut style_offsets = Vec::new();
        let mut style_data = Vec::new();
        if styled {
            style_offsets.extend_from_slice(&0u32.to_le_bytes());
            for field in [1, 0, 2, NO_STRING, NO_STRING, NO_STRING] {
                style_data.extend_from_slice(&field.to_le_bytes());
            }
        }
        let strings_start = 28 + offsets.len() + style_offsets.len();
        let styles_start = if styled { strings_start + string_data.len() } else { 0 };
        let mut pool = Vec::new();
        pool.extend_from_slice(&RES_STRING_POOL_TYPE.to_le_bytes());
        pool.extend_from_slice(&28u16.to_le_bytes());
        let size = strings_start + string_data.len() + style_data.len();
        let flags = if utf8 { UTF8_FLAG } else { 0 } | SORTED_FLAG;
        for field in [size, strings.len(), style_count, flags as usize, strings_start, styles_start] {
            pool.extend_from_slice(&(field as u32).to_le_bytes());
        }
        pool.extend(offsets);
        pool.extend(style_offsets);
        pool.extend(string_data);
        pool.extend(style_data);

        let mut out = vec![3, 0, 8, 0, 0, 0, 0, 0];
        out.extend(pool);
        if resource_map {
            out.extend_from_slice(&RES_XML_RESOURCE_MAP_TYPE.to_le_bytes());
            out.extend_from_slice(&8u16.to_le_bytes());
            out.extend_from_slice(&12u32.to_le_bytes());
            out.extend_from_slice(&LABEL.to_le_bytes());
        }
        let namespace = |kind: u16| {
            let mut chunk = Vec::new();
            chunk.extend_from_slice(&kind.to_le_bytes());
            chunk.extend_from_slice(&16u16.to_le_bytes());
            for field in [24, 1, NO_STRING, 1, 2] {
                chunk.extend_from_slice(&field.to_le_bytes());
            }
            chunk
        };
        out.extend(namespace(RES_XML_START_NAMESPACE_TYPE));
        out.extend(start_element(1, 3, &[]));
        out.extend(start_element(2, 4, &[(2, 0, 5, TYPE_STRING, 5)]));
        out.extend(end_element(2, 4));
        out.extend(end_element(3, 3));
        out.extend(namespace(0x0101));
        let size = out.len() as u32;
        put_u32(&mut out, 4, size);
        out
    }

    /// Checks the chunk layout (sizes add up to the file size, every size a
    /// multiple of 4) and writes each element as a line, like
    /// `2 <application http://...android:label(0x1010001)="Game"` and
    /// `2 </application>`. Attribute names show the ID the resource map gives.
    fn dump(data: &[u8]) -> Vec<String> {
        assert_eq!(u32_at(data, 4), Some(data.len() as u32), "file size");
        let pool = read_string_pool(data, 8).expect("string pool");
        let text = |number: u32| pool.strings.get(number as usize).cloned().unwrap_or_default();
        let mut resource_ids = Vec::new();
        let mut lines = Vec::new();
        let mut at = 8;
        while at < data.len() {
            let (kind, header_size, size) = (u16_at(data, at).unwrap(), u16_at(data, at + 2).unwrap() as usize, u32_at(data, at + 4).unwrap() as usize);
            assert_eq!(size % 4, 0, "chunk at {at} has size {size}");
            let (line, body) = (u32_at(data, at + 8).unwrap(), at + header_size);
            match kind {
                RES_XML_RESOURCE_MAP_TYPE => {
                    resource_ids = (body..at + size).step_by(4).map(|i| u32_at(data, i).unwrap()).collect();
                }
                RES_XML_START_ELEMENT_TYPE => {
                    let mut entry = format!("{line} <{}", text(u32_at(data, body + 4).unwrap()));
                    for i in 0..u16_at(data, body + 12).unwrap() as usize {
                        let attribute = body + 20 + 20 * i;
                        let field = |offset| u32_at(data, attribute + offset).unwrap();
                        let id = resource_ids.get(field(4) as usize).copied().unwrap_or(0);
                        let value = match data[attribute + 15] {
                            TYPE_STRING => {
                                assert_eq!(field(8), field(16), "raw value of a string attribute");
                                format!("{:?}", text(field(16)))
                            }
                            kind if field(8) == NO_STRING => format!("{kind:#x}/{}", field(16)),
                            kind => format!("{kind:#x}/{}(raw {:?})", field(16), text(field(8))),
                        };
                        entry += &format!(" {}:{}({id:#x})={value}", text(field(0)), text(field(4)));
                    }
                    lines.push(entry);
                }
                RES_XML_END_ELEMENT_TYPE => lines.push(format!("{line} </{}>", text(u32_at(data, body + 4).unwrap()))),
                _ => {}
            }
            at += size;
        }
        assert_eq!(at, data.len(), "chunks fill the file");
        lines
    }

    /// Adds the provider and checks the result against the input: old
    /// strings keep their numbers, the pool keeps its encoding and loses its
    /// sorted flag, and the only change to the tree is the provider right
    /// inside `<application>` on the same line.
    fn add_and_check(before: &[u8]) -> Vec<u8> {
        let after = add_provider(before, CLASS, AUTHORITY).unwrap();
        let (old_pool, new_pool) = (read_string_pool(before, 8).unwrap(), read_string_pool(&after, 8).unwrap());
        assert_eq!(new_pool.strings[..old_pool.strings.len()], old_pool.strings[..]);
        assert_eq!(new_pool.flags & UTF8_FLAG, old_pool.flags & UTF8_FLAG);
        assert_eq!(new_pool.flags & SORTED_FLAG, 0);

        let mut expected = dump(before);
        let application = expected.iter().position(|line| line.contains(" <application")).unwrap();
        let line = expected[application].split(' ').next().unwrap().to_string();
        let android = ANDROID_NAMESPACE;
        let provider = [
            format!("{line} <provider {android}:name(0x1010003)={CLASS:?} {android}:exported(0x1010010)=0x12/0"),
            format!(" {android}:authorities(0x1010018)={AUTHORITY:?} {android}:initOrder(0x101001a)=0x10/2147483647"),
        ]
        .concat();
        expected.splice(application + 1..application + 1, [provider, format!("{line} </provider>")]);
        assert_eq!(dump(&after), expected);
        // The in-place edits still find their attributes through the new map.
        assert_eq!(value(&after, INIT_ORDER), Some(i32::MAX as u32));
        after
    }

    #[test]
    fn adds_provider_with_utf16_strings() {
        let after = add_and_check(&build_manifest(false, false, true));
        let pool = read_string_pool(&after, 8).unwrap();
        // "name" at 6 isn't in the map, so a new one was added.
        assert_eq!(pool.strings[7..], ["name", "exported", "authorities", "initOrder", "provider", CLASS, AUTHORITY]);
        // Values reuse a string with the same text.
        let after = add_provider(&build_manifest(false, false, true), CLASS, "Game").unwrap();
        assert_eq!(read_string_pool(&after, 8).unwrap().strings.len(), 13);
        assert!(dump(&after)[2].contains("authorities(0x1010018)=\"Game\""));
    }

    #[test]
    fn adds_provider_with_utf8_strings() {
        let after = add_and_check(&build_manifest(true, false, true));
        assert_ne!(read_string_pool(&after, 8).unwrap().flags & UTF8_FLAG, 0);
        // Long strings need two-byte lengths.
        let long = "x".repeat(300);
        let after = add_provider(&build_manifest(true, false, true), &long, AUTHORITY).unwrap();
        assert!(dump(&after)[2].contains(&long));
    }

    #[test]
    fn keeps_styles() {
        for utf8 in [false, true] {
            let after = add_and_check(&build_manifest(utf8, true, true));
            let pool = read_string_pool(&after, 8).unwrap();
            assert_eq!(pool.style_count, 1);
            let style_offset = u32_at(&after, 8 + pool.header_size + 4 * pool.strings.len()).unwrap() as usize;
            let span = 8 + pool.styles_start + style_offset;
            let fields: Vec<u32> = (0..6).map(|i| u32_at(&after, span + 4 * i).unwrap()).collect();
            assert_eq!(fields, [1, 0, 2, NO_STRING, NO_STRING, NO_STRING]);
            assert_eq!(pool.styles_start % 4, 0);
            assert_eq!(8 + pool.size, 8 + pool.styles_start + 24);
        }
    }

    #[test]
    fn creates_missing_resource_map() {
        let before = build_manifest(false, false, false);
        let after = add_and_check(&before);
        // The map comes right after the pool and reaches the last attribute name.
        let pool = read_string_pool(&after, 8).unwrap();
        let map = 8 + pool.size;
        assert_eq!(u16_at(&after, map), Some(RES_XML_RESOURCE_MAP_TYPE));
        let entries: Vec<u32> = (0..11).map(|i| u32_at(&after, map + 8 + 4 * i).unwrap()).collect();
        assert_eq!(entries, [0, 0, 0, 0, 0, 0, 0, NAME, EXPORTED, AUTHORITIES, INIT_ORDER]);
        assert_eq!(u32_at(&after, map + 4), Some(8 + 4 * 11));
    }

    #[test]
    fn adding_twice_changes_nothing() {
        for before in [build_manifest(true, true, true), include_bytes!("testdata/made-up-unity.axml").to_vec()] {
            let once = add_provider(&before, CLASS, AUTHORITY).unwrap();
            assert_eq!(add_provider(&once, CLASS, AUTHORITY).unwrap(), once);
            // A provider with another name is still added.
            assert_ne!(add_provider(&once, "com.arm32bridge.Other", AUTHORITY).unwrap(), once);
        }
    }

    #[test]
    fn adds_provider_to_full_size_manifests() {
        let unity = include_bytes!("testdata/made-up-unity.axml");
        let classic = include_bytes!("testdata/made-up-classic.axml");
        for before in [&unity[..], &classic[..]] {
            let after = add_and_check(before);
            assert_eq!(value(&after, TARGET_SDK_VERSION), value(before, TARGET_SDK_VERSION));
            assert_eq!(value(&after, MIN_SDK_VERSION), value(before, MIN_SDK_VERSION));
        }
    }

    #[test]
    fn reads_package_name() {
        let unity = include_bytes!("testdata/made-up-unity.axml");
        let classic = include_bytes!("testdata/made-up-classic.axml");
        assert_eq!(package_name(unity).as_deref(), Some("dev.braxten.testgame.unity"));
        assert_eq!(package_name(classic).as_deref(), Some("dev.braxten.testgame.classic.android.gplay"));
        assert_eq!(package_name(&add_provider(unity, CLASS, AUTHORITY).unwrap()), package_name(unity));
        // The built manifest has no package attribute.
        assert_eq!(package_name(&build_manifest(true, false, true)), None);
        assert_eq!(package_name(b"<manifest/>"), None);
    }
}
