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

/// Resource ID of the android:extractNativeLibs attribute.
const EXTRACT_NATIVE_LIBS: u32 = 0x0101_04ea;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const TYPE_INT_BOOLEAN: u8 = 0x12;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
/// Resource IDs of android:minSdkVersion and android:targetSdkVersion.
const MIN_SDK_VERSION: u32 = 0x0101_020c;
const TARGET_SDK_VERSION: u32 = 0x0101_0270;

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
    }
}
