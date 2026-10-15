//! Full-size home-screen icons. Phones draw every app icon in one shape (a
//! Pixel: a circle); an old game's square bitmap icon is shrunk into a white
//! circle. A converted game gets an adaptive icon instead, made from its own
//! bitmap scaled to fill the shape (round_icon.xml: the bitmap inset by 1/6
//! of the 108dp layer, which is exactly the part the shape shows).
//!
//! In resources.arsc each bitmap icon gets one new entry in its own resource
//! type, with one value for Android 8 and newer (`anydpi-v26`): the adaptive
//! icon file, which points back at the bitmap. The manifest's android:icon
//! then points at the new entry. The bitmap's own entry is untouched, so
//! anything else that shows it (Game Bridge's cards, the game's own screens)
//! is unchanged.

const TABLE_TYPE: u16 = 0x0002;
const STRING_POOL_TYPE: u16 = 0x0001;
const PACKAGE_TYPE: u16 = 0x0200;
const TYPE_TYPE: u16 = 0x0201;
const TYPE_SPEC_TYPE: u16 = 0x0202;
const XML_START_ELEMENT_TYPE: u16 = 0x0102;
const XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const UTF8_FLAG: u32 = 0x100;
const SORTED_FLAG: u32 = 0x1;
const SPARSE_FLAG: u8 = 0x01;
const OFFSET16_FLAG: u8 = 0x02;
const COMPLEX_ENTRY: u16 = 0x0001;
const COMPACT_ENTRY: u16 = 0x0008;
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_STRING: u8 = 0x03;
const NO_ENTRY: u32 = 0xFFFF_FFFF;
/// android:icon and android:roundIcon (which phones set to prefer round icons use instead).
const ICON_ATTRIBUTES: [u32; 2] = [0x0101_0002, 0x0101_052c];
/// Entry numbers are 16 bits.
const MOST_ENTRIES: usize = 0xFFFF;
/// ResTable_config: density 0xFFFE is "anydpi"; sdkVersion 26 is Android 8.
const DENSITY_AT: usize = 14;
const SDK_VERSION_AT: usize = 24;
const ANY_DENSITY: u16 = 0xFFFE;
const ADAPTIVE_ICON_SDK: u16 = 26;
/// Which configuration parts the new entry's values differ in (typeSpec flags).
const CONFIG_DENSITY_AND_VERSION: u32 = 0x0100 | 0x0400;

/// The compiled adaptive icon (aapt2), pointing at TEMPLATE_BITMAP.
const TEMPLATE: &[u8] = include_bytes!("round_icon.xml");
const TEMPLATE_BITMAP: u32 = 0x7f01_0000;

/// A round icon to add: the edited resources.arsc, the icon files to add
/// (path, compiled XML) and the manifest's icon IDs to switch (old -> new).
pub struct RoundIcons {
    pub table: Vec<u8>,
    pub files: Vec<(String, Vec<u8>)>,
    pub switched: Vec<(u32, u32)>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn put_u16(data: &mut [u8], at: usize, value: u16) {
    data[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Chunks (type, start, size) one after another from `at` to `end`.
fn chunks(data: &[u8], mut at: usize, end: usize) -> Option<Vec<(u16, usize, usize)>> {
    let mut list = Vec::new();
    while at + 8 <= end {
        let (kind, size) = (u16_at(data, at)?, u32_at(data, at + 4)? as usize);
        if size < 8 || at + size > end {
            return None;
        }
        list.push((kind, at, size));
        at += size;
    }
    Some(list)
}

/// Every android:icon and android:roundIcon reference in a compiled manifest, as (value offset, resource ID).
fn icon_references(manifest: &[u8]) -> Option<Vec<(usize, u32)>> {
    if u16_at(manifest, 0)? != 0x0003 {
        return None;
    }
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut found = Vec::new();
    for (kind, at, size) in chunks(manifest, u16_at(manifest, 2)? as usize, manifest.len())? {
        let header_size = u16_at(manifest, at + 2)? as usize;
        match kind {
            XML_RESOURCE_MAP_TYPE => {
                resource_ids = (at + header_size..at + size).step_by(4).filter_map(|i| u32_at(manifest, i)).collect();
            }
            XML_START_ELEMENT_TYPE => {
                let body = at + header_size;
                let (start, step, count) = (u16_at(manifest, body + 8)? as usize, u16_at(manifest, body + 10)? as usize, u16_at(manifest, body + 12)? as usize);
                for i in 0..count {
                    let attribute = body + start + i * step;
                    let name = u32_at(manifest, attribute + 4)? as usize;
                    let value = attribute + 12;
                    let is_icon = resource_ids.get(name).is_some_and(|id| ICON_ATTRIBUTES.contains(id));
                    if is_icon && manifest.get(value + 3) == Some(&TYPE_REFERENCE) {
                        found.push((value, u32_at(manifest, value + 4)?));
                    }
                }
            }
            _ => {}
        }
    }
    Some(found)
}

/// Points every android:icon and android:roundIcon at `old` to `new` instead.
pub fn switch_icons(manifest: &mut [u8], switched: &[(u32, u32)]) -> usize {
    let mut count = 0;
    for (value, id) in icon_references(manifest).unwrap_or_default() {
        if let Some(&(_, new)) = switched.iter().find(|(old, _)| *old == id) {
            put_u32(manifest, value + 4, new);
            count += 1;
        }
    }
    count
}

/// A string pool's strings (decoded) and where its parts are.
struct Pool {
    utf8: bool,
    count: usize,
    style_count: usize,
    strings_start: usize,
    styles_start: usize,
}

impl Pool {
    fn read(data: &[u8], at: usize) -> Option<Pool> {
        Some(Pool {
            count: u32_at(data, at + 8)? as usize,
            style_count: u32_at(data, at + 12)? as usize,
            utf8: u32_at(data, at + 16)? & UTF8_FLAG != 0,
            strings_start: u32_at(data, at + 20)? as usize,
            styles_start: u32_at(data, at + 24)? as usize,
        })
    }

    fn string(&self, data: &[u8], at: usize, index: usize) -> Option<String> {
        let header = u16_at(data, at + 2)? as usize;
        let start = at + self.strings_start + u32_at(data, at + header + index * 4)? as usize;
        if self.utf8 {
            // Length in characters, then in bytes (each one or two bytes long).
            let skip = |at: usize| -> Option<(usize, usize)> {
                let first = *data.get(at)? as usize;
                Some(if first & 0x80 != 0 { (((first & 0x7F) << 8) | *data.get(at + 1)? as usize, at + 2) } else { (first, at + 1) })
            };
            let (_, after) = skip(start)?;
            let (length, text) = skip(after)?;
            Some(String::from_utf8_lossy(data.get(text..text + length)?).into_owned())
        } else {
            let first = u16_at(data, start)? as usize;
            let (length, text) = if first & 0x8000 != 0 {
                (((first & 0x7FFF) << 16) | u16_at(data, start + 2)? as usize, start + 4)
            } else {
                (first, start + 2)
            };
            let units: Option<Vec<u16>> = (0..length).map(|i| u16_at(data, text + i * 2)).collect();
            Some(String::from_utf16_lossy(&units?))
        }
    }

    /// The pool chunk at `at` with `extra` strings added at the end (their indexes follow the old ones).
    fn with_added(&self, data: &[u8], at: usize, extra: &[String]) -> Option<Vec<u8>> {
        let (header, size) = (u16_at(data, at + 2)? as usize, u32_at(data, at + 4)? as usize);
        let offsets = data.get(at + header..at + header + self.count * 4)?;
        let style_offsets = data.get(at + header + self.count * 4..at + header + (self.count + self.style_count) * 4)?;
        let strings_end = if self.style_count > 0 { self.styles_start } else { size };
        let mut strings = data.get(at + self.strings_start..at + strings_end)?.to_vec();
        let styles = if self.style_count > 0 { data.get(at + self.styles_start..at + size)? } else { &[] };
        let mut new_offsets = offsets.to_vec();
        for text in extra {
            new_offsets.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            if self.utf8 {
                let (characters, bytes) = (text.chars().count(), text.len());
                if characters >= 0x80 || bytes >= 0x80 {
                    return None;
                }
                strings.extend_from_slice(&[characters as u8, bytes as u8]);
                strings.extend_from_slice(text.as_bytes());
                strings.push(0);
            } else {
                let units: Vec<u16> = text.encode_utf16().collect();
                strings.extend_from_slice(&(units.len() as u16).to_le_bytes());
                units.iter().for_each(|unit| strings.extend_from_slice(&unit.to_le_bytes()));
                strings.extend_from_slice(&[0, 0]);
            }
        }
        while strings.len() % 4 != 0 {
            strings.push(0);
        }
        let strings_start = header + new_offsets.len() + style_offsets.len();
        let mut pool = data[at..at + header].to_vec();
        pool.extend_from_slice(&new_offsets);
        pool.extend_from_slice(style_offsets);
        pool.extend_from_slice(&strings);
        let styles_start = if self.style_count > 0 { pool.len() } else { 0 };
        pool.extend_from_slice(styles);
        let total = pool.len() as u32;
        put_u32(&mut pool, 4, total);
        put_u32(&mut pool, 8, (self.count + extra.len()) as u32);
        // New strings go at the end, so the pool is no longer sorted.
        let flags = u32_at(&pool, 16)? & !SORTED_FLAG;
        put_u32(&mut pool, 16, flags);
        put_u32(&mut pool, 20, strings_start as u32);
        put_u32(&mut pool, 24, styles_start as u32);
        Some(pool)
    }
}

/// The values of entry `entry` in a type chunk at `at`: (Res_value type, data), or None if not set.
fn entry_value(table: &[u8], at: usize, entry: usize) -> Option<(u8, u32)> {
    let header = u16_at(table, at + 2)? as usize;
    let flags = *table.get(at + 9)?;
    let count = u32_at(table, at + 12)? as usize;
    let entries_start = u32_at(table, at + 16)? as usize;
    let offset = if flags & SPARSE_FLAG != 0 {
        (0..count).find_map(|i| {
            (u16_at(table, at + header + i * 4)? as usize == entry).then(|| u16_at(table, at + header + i * 4 + 2).map(|o| o as usize * 4))?
        })?
    } else if entry >= count {
        return None;
    } else if flags & OFFSET16_FLAG != 0 {
        let offset = u16_at(table, at + header + entry * 2)?;
        if offset == 0xFFFF {
            return None;
        }
        offset as usize * 4
    } else {
        let offset = u32_at(table, at + header + entry * 4)?;
        if offset == NO_ENTRY {
            return None;
        }
        offset as usize
    };
    let item = at + entries_start + offset;
    let (size, entry_flags) = (u16_at(table, item)? as usize, u16_at(table, item + 2)?);
    if entry_flags & COMPACT_ENTRY != 0 {
        return Some((*table.get(item + 3)?, u32_at(table, item + 4)?));
    }
    if entry_flags & COMPLEX_ENTRY != 0 {
        return Some((0, 0));
    }
    Some((*table.get(item + size + 3)?, u32_at(table, item + size + 4)?))
}

/// How many values `id` has in `table`, all plain bitmaps for Android before 8: Some(0) when the table
/// has none, None when one is something else (an XML icon, perhaps already adaptive, a color, or a
/// value for Android 8+) or the table can't be read.
fn bitmap_values(table: &[u8], id: u32) -> Option<usize> {
    if u16_at(table, 0)? != TABLE_TYPE {
        return None;
    }
    let top = chunks(table, u16_at(table, 2)? as usize, table.len())?;
    let (_, values_at, _) = *top.iter().find(|(kind, _, _)| *kind == STRING_POOL_TYPE)?;
    let values = Pool::read(table, values_at)?;
    let Some(&(_, package_at, package_size)) = top.iter().find(|(kind, at, _)| *kind == PACKAGE_TYPE && u32_at(table, at + 8) == Some(id >> 24)) else {
        return Some(0);
    };
    let (type_id, entry) = (((id >> 16) & 0xFF) as u8, (id & 0xFFFF) as usize);
    let mut bitmaps = 0;
    for (kind, at, _) in chunks(table, package_at + u16_at(table, package_at + 2)? as usize, package_at + package_size)? {
        if kind != TYPE_TYPE || *table.get(at + 8)? != type_id {
            continue;
        }
        let Some((value_type, data)) = entry_value(table, at, entry) else { continue };
        let file = if value_type == TYPE_STRING { values.string(table, values_at, data as usize) } else { None };
        let config = at + 20;
        let sdk = u16_at(table, config + SDK_VERSION_AT).filter(|_| u32_at(table, config).unwrap_or(0) as usize > SDK_VERSION_AT).unwrap_or(0);
        match file {
            Some(path) if !path.ends_with(".9.png") && [".png", ".webp", ".jpg"].iter().any(|end| path.ends_with(end)) && sdk < ADAPTIVE_ICON_SDK => bitmaps += 1,
            _ => return None,
        }
    }
    Some(bitmaps)
}

/// Builds full-size icons for the bitmap icons the manifest names. `others`: the other pieces' tables
/// of a bundle (an icon's pictures may live in a screen-density piece). None when there is nothing to do
/// (no bitmap icon, an icon that is already adaptive, or a table this can't edit).
pub fn round_icons(manifest: &[u8], table: &[u8], others: &[Vec<u8>]) -> Option<RoundIcons> {
    let mut icons: Vec<u32> = icon_references(manifest)?.into_iter().map(|(_, id)| id).collect();
    icons.sort_unstable();
    icons.dedup();
    if icons.is_empty() || u16_at(table, 0)? != TABLE_TYPE {
        return None;
    }
    let table_header = u16_at(table, 2)? as usize;
    let top = chunks(table, table_header, table.len())?;
    let (_, values_at, _) = *top.iter().find(|(kind, _, _)| *kind == STRING_POOL_TYPE)?;
    let values = Pool::read(table, values_at)?;

    // The one package the icons are in.
    let package_id = icons[0] >> 24;
    if icons.iter().any(|id| id >> 24 != package_id) {
        return None;
    }
    let &(_, package_at, package_size) = top.iter().find(|(kind, at, _)| *kind == PACKAGE_TYPE && u32_at(table, at + 8) == Some(package_id))?;
    let package_header = u16_at(table, package_at + 2)? as usize;
    let key_pool_at = package_at + u32_at(table, package_at + 8 + 4 + 256 + 8)? as usize;
    let keys = Pool::read(table, key_pool_at)?;
    let inner = chunks(table, package_at + package_header, package_at + package_size)?;

    // Each bitmap icon: its type and entry, checked to be a plain bitmap in every configuration of every piece.
    let mut wanted: Vec<(u32, u8, usize)> = Vec::new();
    for &id in &icons {
        let bitmaps = std::iter::once(table).chain(others.iter().map(Vec::as_slice)).map(|t| bitmap_values(t, id)).sum::<Option<usize>>()?;
        if bitmaps == 0 {
            return None;
        }
        wanted.push((id, ((id >> 16) & 0xFF) as u8, (id & 0xFFFF) as usize));
    }

    // New entries go after each type's last one.
    let mut switched = Vec::new();
    let mut files = Vec::new();
    let mut new_keys = Vec::new();
    let mut new_values = Vec::new();
    // Per type: (spec chunk, old entry count, new entries as (key index, value string index)).
    let mut per_type: Vec<(u8, usize, u32, Vec<(u32, u32)>)> = Vec::new();
    for &(id, type_id, entry) in &wanted {
        let &(_, spec_at, _) = inner.iter().find(|&&(kind, at, _)| kind == TYPE_SPEC_TYPE && table.get(at + 8) == Some(&type_id))?;
        let index = match per_type.iter().position(|(t, ..)| *t == type_id) {
            Some(index) => index,
            None => {
                per_type.push((type_id, spec_at, u32_at(table, spec_at + 12)?, Vec::new()));
                per_type.len() - 1
            }
        };
        let (_, _, count, added) = &mut per_type[index];
        let new_id = (package_id << 24) | ((type_id as u32) << 16) | (*count + added.len() as u32);
        let path = format!("res/gamebridge/round_icon_{:08x}.xml", id);
        added.push(((keys.count + new_keys.len()) as u32, (values.count + new_values.len()) as u32));
        new_keys.push(format!("gamebridge_round_icon_{entry}"));
        new_values.push(path.clone());
        let mut xml = TEMPLATE.to_vec();
        let placeholder = xml.windows(4).position(|window| window == TEMPLATE_BITMAP.to_le_bytes())?;
        put_u32(&mut xml, placeholder, id);
        files.push((path, xml));
        switched.push((id, new_id));
    }

    if per_type.iter().any(|(_, _, count, added)| *count as usize + added.len() > MOST_ENTRIES) {
        return None;
    }

    // The new package: header, then its chunks with the key pool grown, each typeSpec grown, and
    // a type chunk for the new entries after each type's last chunk.
    let mut package = table[package_at..package_at + package_header].to_vec();
    for (position, &(kind, at, size)) in inner.iter().enumerate() {
        if at == key_pool_at {
            let key_offset = package.len() as u32;
            put_u32(&mut package, 8 + 4 + 256 + 8, key_offset);
            package.extend_from_slice(&keys.with_added(table, at, &new_keys)?);
            continue;
        }
        if kind == STRING_POOL_TYPE && at == package_at + u32_at(table, package_at + 8 + 4 + 256)? as usize {
            let type_offset = package.len() as u32;
            put_u32(&mut package, 8 + 4 + 256, type_offset);
        }
        let grown = per_type.iter().find(|(type_id, spec_at, ..)| kind == TYPE_SPEC_TYPE && *spec_at == at && table.get(at + 8) == Some(type_id));
        match grown {
            Some((_, _, count, added)) => {
                let mut spec = table[at..at + size].to_vec();
                spec.truncate(u16_at(table, at + 2)? as usize + *count as usize * 4);
                for _ in added {
                    spec.extend_from_slice(&CONFIG_DENSITY_AND_VERSION.to_le_bytes());
                }
                let total = spec.len() as u32;
                put_u32(&mut spec, 4, total);
                put_u32(&mut spec, 12, count + added.len() as u32);
                package.extend_from_slice(&spec);
            }
            None => package.extend_from_slice(&table[at..at + size]),
        }
        // After a type's last chunk (its spec or its last type chunk), the new entries' type chunk.
        let type_id = if kind == TYPE_SPEC_TYPE || kind == TYPE_TYPE { table.get(at + 8).copied() } else { None };
        let last_of_type = type_id.is_some()
            && !inner[position + 1..].iter().any(|&(k, a, _)| (k == TYPE_TYPE || k == TYPE_SPEC_TYPE) && table.get(a + 8).copied() == type_id);
        if let (true, Some((_, _, count, added))) = (last_of_type, per_type.iter().find(|(t, ..)| Some(*t) == type_id)) {
            package.extend_from_slice(&new_type_chunk(table, &inner, type_id?, *count, added)?);
        }
    }
    let package_total = package.len() as u32;
    put_u32(&mut package, 4, package_total);

    let mut out = table[..table_header].to_vec();
    for &(kind, at, size) in &top {
        if at == values_at {
            out.extend_from_slice(&values.with_added(table, at, &new_values)?);
        } else if kind == PACKAGE_TYPE && at == package_at {
            out.extend_from_slice(&package);
        } else {
            out.extend_from_slice(&table[at..at + size]);
        }
    }
    let total = out.len() as u32;
    put_u32(&mut out, 4, total);
    Some(RoundIcons { table: out, files, switched })
}

/// A type chunk for configuration anydpi-v26 holding only the new entries (file values).
fn new_type_chunk(table: &[u8], inner: &[(u16, usize, usize)], type_id: u8, old_count: u32, added: &[(u32, u32)]) -> Option<Vec<u8>> {
    // Same configuration layout as the type's existing chunks, all fields cleared.
    let &(_, sample, _) = inner.iter().find(|&&(kind, at, _)| kind == TYPE_TYPE && table.get(at + 8) == Some(&type_id))?;
    let config_size = (u32_at(table, sample + 20)? as usize).max(SDK_VERSION_AT + 4);
    let mut config = vec![0u8; config_size];
    put_u32(&mut config, 0, config_size as u32);
    put_u16(&mut config, DENSITY_AT, ANY_DENSITY);
    put_u16(&mut config, SDK_VERSION_AT, ADAPTIVE_ICON_SDK);

    let count = old_count as usize + added.len();
    let header = 20 + config_size;
    let entries_start = header + count * 4;
    let mut chunk = Vec::with_capacity(entries_start + added.len() * 16);
    chunk.extend_from_slice(&TYPE_TYPE.to_le_bytes());
    chunk.extend_from_slice(&(header as u16).to_le_bytes());
    chunk.extend_from_slice(&0u32.to_le_bytes());
    chunk.extend_from_slice(&[type_id, 0, 0, 0]);
    chunk.extend_from_slice(&(count as u32).to_le_bytes());
    chunk.extend_from_slice(&(entries_start as u32).to_le_bytes());
    chunk.extend_from_slice(&config);
    for index in 0..count {
        let offset = index.checked_sub(old_count as usize).map(|new| (new * 16) as u32).unwrap_or(NO_ENTRY);
        chunk.extend_from_slice(&offset.to_le_bytes());
    }
    for &(key, value) in added {
        // ResTable_entry (size 8, flags 0, key), then Res_value (size 8, string = the file path).
        chunk.extend_from_slice(&8u16.to_le_bytes());
        chunk.extend_from_slice(&0u16.to_le_bytes());
        chunk.extend_from_slice(&key.to_le_bytes());
        chunk.extend_from_slice(&8u16.to_le_bytes());
        chunk.extend_from_slice(&[0, TYPE_STRING]);
        chunk.extend_from_slice(&value.to_le_bytes());
    }
    let total = chunk.len() as u32;
    put_u32(&mut chunk, 4, total);
    Some(chunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_points_at_one_bitmap() {
        assert_eq!(TEMPLATE.windows(4).filter(|window| *window == TEMPLATE_BITMAP.to_le_bytes()).count(), 1);
    }

    #[test]
    fn leaves_files_it_cannot_read() {
        assert!(round_icons(b"not a manifest", b"not a table", &[]).is_none());
        let mut manifest = b"not a manifest".to_vec();
        assert_eq!(switch_icons(&mut manifest, &[(1, 2)]), 0);
        assert_eq!(manifest, b"not a manifest");
    }
}
