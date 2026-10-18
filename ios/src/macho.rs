//! Reader for the 64-bit ARM part of an iPhone program (Mach-O): segments,
//! sections, the dyld fix-up lists, symbols and the entry point.

use std::fmt;

const FAT_MAGIC: u32 = 0xCAFE_BABE;
const MH_MAGIC_64: u32 = 0xFEED_FACF;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;

const LC_SYMTAB: u32 = 0x2;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_SEGMENT_64: u32 = 0x19;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_DYLD_INFO: u32 = 0x22;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_MAIN: u32 = 0x8000_0028;
const LC_ENCRYPTION_INFO_64: u32 = 0x2C;

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub segment: String,
    pub address: u64,
    pub size: u64,
    pub file_offset: u32,
    pub flags: u32,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub name: String,
    pub vm_address: u64,
    pub vm_size: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub initial_protection: u32,
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindKind {
    Normal,
    Lazy,
    Weak,
}

#[derive(Clone, Debug)]
pub struct Bind {
    pub kind: BindKind,
    pub address: u64,
    /// 1-based library number, 0 = this program, negative = special lookups.
    pub library_ordinal: i64,
    pub symbol: String,
    pub addend: i64,
    pub weak_import: bool,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub address: u64,
    pub kind: u8,
}

pub struct Image {
    /// The thin 64-bit ARM file (copied out of a universal file if needed).
    pub data: Vec<u8>,
    pub segments: Vec<Segment>,
    pub libraries: Vec<String>,
    pub entry_offset: Option<u64>,
    pub rebases: Vec<u64>,
    pub binds: Vec<Bind>,
    pub symbols: Vec<Symbol>,
    pub encrypted: bool,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, position: 0 }
    }

    fn done(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or_else(|| Error("fix-up list ends early".into()))?;
        self.position += 1;
        Ok(value)
    }

    fn uleb(&mut self) -> Result<u64, Error> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift < 64 {
                result |= u64::from(byte & 0x7F) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    fn sleb(&mut self) -> Result<i64, Error> {
        let mut result = 0i64;
        let mut shift = 0;
        let mut byte;
        loop {
            byte = self.byte()?;
            if shift < 64 {
                result |= i64::from(byte & 0x7F) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        if shift < 64 && byte & 0x40 != 0 {
            result |= -1i64 << shift;
        }
        Ok(result)
    }

    fn cstring(&mut self) -> Result<String, Error> {
        let start = self.position;
        while self.byte()? != 0 {}
        Ok(String::from_utf8_lossy(&self.bytes[start..self.position - 1]).into_owned())
    }
}

fn u32_le(data: &[u8], offset: usize) -> Result<u32, Error> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| Error(format!("file too short at {offset:#x}")))
}

fn u64_le(data: &[u8], offset: usize) -> Result<u64, Error> {
    data.get(offset..offset + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| Error(format!("file too short at {offset:#x}")))
}

fn u32_be(data: &[u8], offset: usize) -> Result<u32, Error> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| Error(format!("file too short at {offset:#x}")))
}

fn fixed_name(data: &[u8], offset: usize) -> Result<String, Error> {
    let raw = data
        .get(offset..offset + 16)
        .ok_or_else(|| Error("file too short for a name".into()))?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

fn slice<'a>(data: &'a [u8], offset: u32, size: u32) -> Result<&'a [u8], Error> {
    let start = offset as usize;
    let end = start
        .checked_add(size as usize)
        .ok_or_else(|| Error("fix-up list size overflows".into()))?;
    data.get(start..end)
        .ok_or_else(|| Error(format!("fix-up list {start:#x}+{size:#x} lies outside the file")))
}

/// Picks the 64-bit ARM part out of a universal file, or takes a thin file as is.
pub fn thin_arm64(file: &[u8]) -> Result<Vec<u8>, Error> {
    if file.len() >= 4 && u32_be(file, 0)? == FAT_MAGIC {
        let count = u32_be(file, 4)? as usize;
        for index in 0..count {
            let entry = 8 + index * 20;
            if u32_be(file, entry)? == CPU_TYPE_ARM64 {
                let offset = u32_be(file, entry + 8)? as usize;
                let size = u32_be(file, entry + 12)? as usize;
                let end = offset
                    .checked_add(size)
                    .filter(|&end| end <= file.len())
                    .ok_or_else(|| Error("64-bit part lies outside the file".into()))?;
                return Ok(file[offset..end].to_vec());
            }
        }
        return err("this program has no 64-bit ARM part");
    }
    if file.len() >= 8 && u32_le(file, 0)? == MH_MAGIC_64 && u32_le(file, 4)? == CPU_TYPE_ARM64 {
        return Ok(file.to_vec());
    }
    err("not a 64-bit ARM iPhone program")
}

impl Image {
    pub fn parse(file: &[u8]) -> Result<Image, Error> {
        let data = thin_arm64(file)?;
        let command_count = u32_le(&data, 16)? as usize;
        let mut segments = Vec::new();
        let mut libraries = Vec::new();
        let mut entry_offset = None;
        let mut dyld_info = None;
        let mut symtab = None;
        let mut encrypted = false;
        let mut offset = 32usize;
        for _ in 0..command_count {
            let command = u32_le(&data, offset)?;
            let size = u32_le(&data, offset + 4)? as usize;
            if size < 8 {
                return err("broken load command");
            }
            match command {
                LC_SEGMENT_64 => {
                    let section_count = u32_le(&data, offset + 64)? as usize;
                    let mut sections = Vec::with_capacity(section_count);
                    for index in 0..section_count {
                        let at = offset + 72 + index * 80;
                        sections.push(Section {
                            name: fixed_name(&data, at)?,
                            segment: fixed_name(&data, at + 16)?,
                            address: u64_le(&data, at + 32)?,
                            size: u64_le(&data, at + 40)?,
                            file_offset: u32_le(&data, at + 48)?,
                            flags: u32_le(&data, at + 64)?,
                        });
                    }
                    segments.push(Segment {
                        name: fixed_name(&data, offset + 8)?,
                        vm_address: u64_le(&data, offset + 24)?,
                        vm_size: u64_le(&data, offset + 32)?,
                        file_offset: u64_le(&data, offset + 40)?,
                        file_size: u64_le(&data, offset + 48)?,
                        initial_protection: u32_le(&data, offset + 60)?,
                        sections,
                    });
                }
                LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB => {
                    let name_offset = u32_le(&data, offset + 8)? as usize;
                    let raw = data
                        .get(offset + name_offset..offset + size)
                        .ok_or_else(|| Error("broken library name".into()))?;
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                    libraries.push(String::from_utf8_lossy(&raw[..end]).into_owned());
                }
                LC_MAIN => entry_offset = Some(u64_le(&data, offset + 8)?),
                LC_DYLD_INFO | LC_DYLD_INFO_ONLY => {
                    let mut fields = [0u32; 10];
                    for (index, field) in fields.iter_mut().enumerate() {
                        *field = u32_le(&data, offset + 8 + index * 4)?;
                    }
                    dyld_info = Some(fields);
                }
                LC_SYMTAB => {
                    symtab = Some([
                        u32_le(&data, offset + 8)?,
                        u32_le(&data, offset + 12)?,
                        u32_le(&data, offset + 16)?,
                        u32_le(&data, offset + 20)?,
                    ])
                }
                LC_ENCRYPTION_INFO_64 => encrypted = u32_le(&data, offset + 16)? != 0,
                _ => {}
            }
            offset += size;
        }

        let mut image = Image {
            data: Vec::new(),
            segments,
            libraries,
            entry_offset,
            rebases: Vec::new(),
            binds: Vec::new(),
            symbols: Vec::new(),
            encrypted,
        };
        if let Some(info) = dyld_info {
            image.rebases = image.read_rebases(slice(&data, info[0], info[1])?)?;
            image.read_binds(slice(&data, info[2], info[3])?, BindKind::Normal)?;
            image.read_binds(slice(&data, info[4], info[5])?, BindKind::Weak)?;
            image.read_binds(slice(&data, info[6], info[7])?, BindKind::Lazy)?;
        } else {
            return err("this program has no dyld fix-up lists (newer chained fix-ups are not read yet)");
        }
        if let Some([symbol_offset, count, string_offset, string_size]) = symtab {
            image.symbols = read_symbols(&data, symbol_offset, count, string_offset, string_size)?;
        }
        image.data = data;
        Ok(image)
    }

    fn segment_address(&self, index: u8, offset: u64) -> Result<u64, Error> {
        let segment = self
            .segments
            .get(index as usize)
            .ok_or_else(|| Error(format!("fix-up names missing segment {index}")))?;
        Ok(segment.vm_address.wrapping_add(offset))
    }

    fn read_rebases(&self, bytes: &[u8]) -> Result<Vec<u64>, Error> {
        let mut out = Vec::new();
        let mut cursor = Cursor::new(bytes);
        let mut address = 0u64;
        while !cursor.done() {
            let byte = cursor.byte()?;
            let immediate = byte & 0x0F;
            match byte & 0xF0 {
                0x00 => break,
                0x10 => {
                    if immediate != 1 {
                        return err(format!("unsupported rebase type {immediate}"));
                    }
                }
                0x20 => address = self.segment_address(immediate, cursor.uleb()?)?,
                0x30 => address = address.wrapping_add(cursor.uleb()?),
                0x40 => address = address.wrapping_add(u64::from(immediate) * 8),
                0x50 => {
                    for _ in 0..immediate {
                        out.push(address);
                        address = address.wrapping_add(8);
                    }
                }
                0x60 => {
                    for _ in 0..cursor.uleb()? {
                        out.push(address);
                        address = address.wrapping_add(8);
                    }
                }
                0x70 => {
                    out.push(address);
                    address = address.wrapping_add(cursor.uleb()?.wrapping_add(8));
                }
                0x80 => {
                    let count = cursor.uleb()?;
                    let skip = cursor.uleb()?;
                    for _ in 0..count {
                        out.push(address);
                        address = address.wrapping_add(skip.wrapping_add(8));
                    }
                }
                other => return err(format!("unknown rebase step {other:#x}")),
            }
        }
        Ok(out)
    }

    fn read_binds(&mut self, bytes: &[u8], kind: BindKind) -> Result<(), Error> {
        let mut cursor = Cursor::new(bytes);
        let mut address = 0u64;
        let mut ordinal = 0i64;
        let mut symbol = String::new();
        let mut flags = 0u8;
        let mut addend = 0i64;
        while !cursor.done() {
            let byte = cursor.byte()?;
            let immediate = byte & 0x0F;
            let bind = |address: u64, binds: &mut Vec<Bind>| {
                binds.push(Bind {
                    kind: kind.clone(),
                    address,
                    library_ordinal: ordinal,
                    symbol: symbol.clone(),
                    addend,
                    weak_import: flags & 0x1 != 0,
                })
            };
            match byte & 0xF0 {
                0x00 => {
                    if kind != BindKind::Lazy {
                        break;
                    }
                }
                0x10 => ordinal = i64::from(immediate),
                0x20 => ordinal = cursor.uleb()? as i64,
                0x30 => {
                    ordinal = if immediate == 0 {
                        0
                    } else {
                        i64::from((immediate | 0xF0) as i8)
                    }
                }
                0x40 => {
                    flags = immediate;
                    symbol = cursor.cstring()?;
                }
                0x50 => {
                    if immediate != 1 {
                        return err(format!("unsupported bind type {immediate}"));
                    }
                }
                0x60 => addend = cursor.sleb()?,
                0x70 => address = self.segment_address(immediate, cursor.uleb()?)?,
                0x80 => address = address.wrapping_add(cursor.uleb()?),
                0x90 => {
                    bind(address, &mut self.binds);
                    address = address.wrapping_add(8);
                }
                0xA0 => {
                    bind(address, &mut self.binds);
                    address = address.wrapping_add(cursor.uleb()?.wrapping_add(8));
                }
                0xB0 => {
                    bind(address, &mut self.binds);
                    address = address.wrapping_add(u64::from(immediate) * 8 + 8);
                }
                0xC0 => {
                    let count = cursor.uleb()?;
                    let skip = cursor.uleb()?;
                    for _ in 0..count {
                        bind(address, &mut self.binds);
                        address = address.wrapping_add(skip.wrapping_add(8));
                    }
                }
                other => return err(format!("unknown bind step {other:#x}")),
            }
        }
        Ok(())
    }

    pub fn section(&self, segment: &str, name: &str) -> Option<&Section> {
        self.segments
            .iter()
            .flat_map(|s| s.sections.iter())
            .find(|s| s.segment == segment && s.name == name)
    }

    /// The program's lowest address (start of its first mapped segment).
    pub fn base_address(&self) -> u64 {
        self.segments
            .iter()
            .filter(|s| s.name != "__PAGEZERO")
            .map(|s| s.vm_address)
            .min()
            .unwrap_or(0)
    }

    pub fn library_name(&self, ordinal: i64) -> String {
        match ordinal {
            0 => "self".into(),
            -1 => "main program".into(),
            -2 => "flat lookup".into(),
            n if n > 0 => self
                .libraries
                .get(n as usize - 1)
                .map(|path| path.rsplit('/').next().unwrap_or(path).to_string())
                .unwrap_or_else(|| format!("library {n}")),
            n => format!("special {n}"),
        }
    }

    /// The section holding an address, as "segment section".
    pub fn section_of(&self, address: u64) -> Option<&Section> {
        self.segments
            .iter()
            .flat_map(|s| s.sections.iter())
            .find(|s| address >= s.address && address < s.address + s.size)
    }
}

fn read_symbols(
    data: &[u8],
    symbol_offset: u32,
    count: u32,
    string_offset: u32,
    string_size: u32,
) -> Result<Vec<Symbol>, Error> {
    let strings = slice(data, string_offset, string_size)?;
    let mut out = Vec::new();
    for index in 0..count as usize {
        let at = symbol_offset as usize + index * 16;
        let name_index = u32_le(data, at)? as usize;
        let kind = *data.get(at + 4).ok_or_else(|| Error("symbol table cut short".into()))?;
        let value = u64_le(data, at + 8)?;
        // Only defined symbols in a section (N_SECT) and not debug entries.
        if kind & 0xE0 != 0 || kind & 0x0E != 0x0E {
            continue;
        }
        let name = match strings.get(name_index..) {
            Some(rest) => {
                let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
                String::from_utf8_lossy(&rest[..end]).into_owned()
            }
            None => continue,
        };
        out.push(Symbol { name, address: value, kind });
    }
    out.sort_by_key(|s| s.address);
    Ok(out)
}
