//! Unreal Engine 3's packed settings files (CookedIPhone/Coalesced_*.bin):
//! all the game's .ini files in one, as files > sections > key = value.
//! Some games encrypt theirs; those don't read as this layout and are left
//! alone.
//!
//! Games pick their texture detail per Apple device ([SystemSettings<device>]
//! sections). Texture groups with a "LODBias" above 0 drop that many of their
//! top picture levels, so the older iPhones (and the one we say we are) see
//! those textures at half size or less. `full_size_textures` sets them back
//! to 0; 99 and up (the engine's "never load these") stay.

/// Text as the file stores it: 8-bit, or UTF-16 (a negative length).
#[derive(Clone, Debug, PartialEq)]
struct Text {
    value: String,
    wide: bool,
}

#[derive(Debug, PartialEq)]
struct File {
    name: Text,
    sections: Vec<(Text, Vec<(Text, Text)>)>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn count(&mut self) -> Option<usize> {
        let value = i32::from_le_bytes(self.bytes.get(self.at..self.at + 4)?.try_into().ok()?);
        self.at += 4;
        // Each entry takes at least four bytes, so more than what's left is not this layout.
        usize::try_from(value).ok().filter(|&n| n <= (self.bytes.len() - self.at) / 4)
    }

    fn text(&mut self) -> Option<Text> {
        let length = i32::from_le_bytes(self.bytes.get(self.at..self.at + 4)?.try_into().ok()?);
        self.at += 4;
        if length == 0 {
            return Some(Text { value: String::new(), wide: false });
        }
        let wide = length < 0;
        let units = length.unsigned_abs() as usize;
        let size = if wide { units.checked_mul(2)? } else { units };
        let raw = self.bytes.get(self.at..self.at.checked_add(size)?)?;
        self.at += size;
        let value = if wide {
            let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let (last, text) = units.split_last()?;
            if *last != 0 {
                return None;
            }
            String::from_utf16(text).ok()?
        } else {
            let (last, text) = raw.split_last()?;
            if *last != 0 || !text.is_ascii() {
                return None;
            }
            String::from_utf8(text.to_vec()).ok()?
        };
        Some(Text { value, wide })
    }
}

fn read(bytes: &[u8]) -> Option<Vec<File>> {
    let mut r = Reader { bytes, at: 0 };
    let mut files = Vec::new();
    for _ in 0..r.count()? {
        let name = r.text()?;
        let mut sections = Vec::new();
        for _ in 0..r.count()? {
            let section = r.text()?;
            let mut items = Vec::new();
            for _ in 0..r.count()? {
                items.push((r.text()?, r.text()?));
            }
            sections.push((section, items));
        }
        files.push(File { name, sections });
    }
    // The whole file must be this layout, else it's something else (encrypted).
    (r.at == bytes.len()).then_some(files)
}

fn write(files: &[File]) -> Vec<u8> {
    fn count(out: &mut Vec<u8>, n: usize) {
        out.extend_from_slice(&(n as i32).to_le_bytes());
    }
    fn text(out: &mut Vec<u8>, t: &Text) {
        if t.value.is_empty() {
            count(out, 0);
        } else if t.wide {
            let units: Vec<u16> = t.value.encode_utf16().chain([0]).collect();
            out.extend_from_slice(&(-(units.len() as i32)).to_le_bytes());
            units.iter().for_each(|u| out.extend_from_slice(&u.to_le_bytes()));
        } else {
            count(out, t.value.len() + 1);
            out.extend_from_slice(t.value.as_bytes());
            out.push(0);
        }
    }
    let mut out = Vec::new();
    count(&mut out, files.len());
    for file in files {
        text(&mut out, &file.name);
        count(&mut out, file.sections.len());
        for (section, items) in &file.sections {
            text(&mut out, section);
            count(&mut out, items.len());
            for (key, value) in items {
                text(&mut out, key);
                text(&mut out, value);
            }
        }
    }
    out
}

/// `(…,LODBias=2,…)` with the bias set to 0; None when it has no bias to lower.
fn without_bias(value: &str) -> Option<String> {
    let start = value.find("LODBias=")? + "LODBias=".len();
    let digits = value[start..].bytes().take_while(u8::is_ascii_digit).count();
    let bias: u32 = value[start..start + digits].parse().ok()?;
    (1..99).contains(&bias).then(|| format!("{}0{}", &value[..start], &value[start + digits..]))
}

/// The settings with every device's texture groups at full size, and how
/// many entries changed; None when the file isn't this layout or nothing
/// needed changing.
pub fn full_size_textures(bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut files = read(bytes)?;
    let mut changed = 0;
    for file in &mut files {
        for (section, items) in &mut file.sections {
            if !section.value.starts_with("SystemSettings") {
                continue;
            }
            for (key, value) in items.iter_mut() {
                if !key.value.starts_with("TEXTUREGROUP_") {
                    continue;
                }
                if let Some(lowered) = without_bias(&value.value) {
                    value.value = lowered;
                    changed += 1;
                }
            }
        }
    }
    (changed > 0).then(|| (write(&files), changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(value: &str) -> Text {
        Text { value: value.to_string(), wide: false }
    }

    fn sample(bias: &str) -> Vec<u8> {
        write(&[File {
            name: Text { value: "..\\Config\\IPhone-Engine.ini".to_string(), wide: true },
            sections: vec![
                (t("SystemSettingsIPhone5S"), vec![
                    (t("BasedOn"), t("SystemSettingsMobile")),
                    (t("TEXTUREGROUP_World"), t(&format!("(MinLODSize=1,MaxLODSize=4096,LODBias={bias},MinMagFilter=aniso)"))),
                    (t("TEXTUREGROUP_Shadowmap"), t("(MinLODSize=1,LODBias=99,NumStreamedMips=3)")),
                    (t("MaxAnisotropy"), t("2")),
                ]),
                (t("Engine.Engine"), vec![(t("TEXTUREGROUP_World"), t("(LODBias=2)"))]),
            ],
        }])
    }

    #[test]
    fn reads_back_what_it_writes() {
        let bytes = sample("1");
        assert_eq!(write(&read(&bytes).unwrap()), bytes);
    }

    #[test]
    fn texture_groups_go_back_to_full_size() {
        let (bytes, changed) = full_size_textures(&sample("2")).unwrap();
        assert_eq!(changed, 1);
        let files = read(&bytes).unwrap();
        let (_, items) = &files[0].sections[0];
        assert_eq!(items[1].1.value, "(MinLODSize=1,MaxLODSize=4096,LODBias=0,MinMagFilter=aniso)");
        // "Never load" stays, and so does everything outside the device settings.
        assert_eq!(items[2].1.value, "(MinLODSize=1,LODBias=99,NumStreamedMips=3)");
        assert_eq!(files[0].sections[1].1[0].1.value, "(LODBias=2)");
        assert_eq!(files[0].name.value, "..\\Config\\IPhone-Engine.ini");
    }

    #[test]
    fn left_alone_when_nothing_to_change_or_not_this_layout() {
        assert!(full_size_textures(&sample("0")).is_none());
        // Encrypted settings (random-looking bytes) and cut-off files aren't read.
        let noise: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert!(full_size_textures(&noise).is_none());
        let whole = sample("1");
        assert!(full_size_textures(&whole[..whole.len() - 3]).is_none());
        assert!(full_size_textures(&[]).is_none());
    }
}
