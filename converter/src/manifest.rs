//! Edits the binary AndroidManifest.xml (Android's compiled XML format).
//!
//! Only `android:extractNativeLibs`: a game built with it set to false keeps
//! its libraries packed inside the APK, but the bridge and stand-in read the
//! 32-bit originals as files next to them. Setting it to true makes Android
//! unpack them at install time. The value is changed in place, so nothing
//! else in the file moves.

/// Resource ID of the android:extractNativeLibs attribute.
const EXTRACT_NATIVE_LIBS: u32 = 0x0101_04ea;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const TYPE_INT_BOOLEAN: u8 = 0x12;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Sets every android:extractNativeLibs="false" to true. Returns whether
/// anything changed (None if the file isn't compiled XML).
pub fn extract_native_libs(data: &mut [u8]) -> Option<bool> {
    if u16_at(data, 0)? != 0x0003 {
        return None;
    }
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut changed = false;
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
                    if resource_ids.get(name) != Some(&EXTRACT_NATIVE_LIBS) {
                        continue;
                    }
                    // Typed value: size (2), reserved (1), type (1), data (4), after namespace, name, raw string.
                    let value = attribute + 12;
                    if data[value + 3] == TYPE_INT_BOOLEAN && u32_at(data, value + 4)? == 0 {
                        data[value + 4..value + 8].copy_from_slice(&u32::MAX.to_le_bytes());
                        changed = true;
                    }
                }
            }
            _ => {}
        }
        at += size;
    }
    Some(changed)
}
