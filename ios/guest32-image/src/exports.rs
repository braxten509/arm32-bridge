//! The export trie of `LC_DYLD_INFO(_ONLY)`: every symbol the image offers
//! to others, decoded with checked offsets. Stripped programs keep only
//! this (their symbol table may lack the names).

use crate::error::ErrorKind::{InvalidReference, Malformed, Overflow};
use crate::error::{fail, Result};
use crate::read::Stream;

const KIND_MASK: u64 = 0x03;
const KIND_THREAD_LOCAL: u64 = 0x01;
const KIND_ABSOLUTE: u64 = 0x02;
const WEAK_DEFINITION: u64 = 0x04;
const REEXPORT: u64 = 0x08;
const STUB_AND_RESOLVER: u64 = 0x10;
const KNOWN_FLAGS: u64 = KIND_MASK | WEAK_DEFINITION | REEXPORT | STUB_AND_RESOLVER;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Regular,
    ThreadLocal,
    /// The value is an absolute address, not an offset into the image.
    Absolute,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export32 {
    pub name: String,
    pub kind: ExportKind,
    /// `EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION`: coalescing may pick another image's copy.
    pub weak: bool,
    /// Offset from the image's header (bit 0 set = Thumb code), or the
    /// absolute value for `Absolute`. None for re-exports.
    pub offset: Option<u32>,
    /// Re-exported from library `ordinal` (1-based) under this name (empty = same name).
    pub reexport: Option<(u32, String)>,
    /// `STUB_AND_RESOLVER`: offset of the resolver function.
    pub resolver: Option<u32>,
}

fn word(value: u64, what: &str) -> Result<u32> {
    u32::try_from(value).or_else(|_| fail(Overflow, format!("export trie: {what} {value:#x} does not fit 32 bits")))
}

/// Names a trie may spell out, in bytes per trie byte (shared prefixes are
/// stored once but copied into every name below them), plus a fixed floor.
const NAME_BYTES_PER_TRIE_BYTE: usize = 64;
const NAME_BYTES_FLOOR: usize = 64 * 1024;

/// Decodes a whole export trie. Every node is queued at most once, so a
/// trie whose edges loop back is refused instead of followed forever, and the
/// names built along the way are capped in proportion to the trie's size.
pub fn decode_export_trie(trie: &[u8]) -> Result<Vec<Export32>> {
    let mut out = Vec::new();
    if trie.is_empty() {
        return Ok(out);
    }
    let name_budget = trie.len().saturating_mul(NAME_BYTES_PER_TRIE_BYTE).saturating_add(NAME_BYTES_FLOOR);
    let mut name_bytes = 0usize;
    let mut queued = vec![false; trie.len()];
    queued[0] = true;
    let mut pending: Vec<(usize, String)> = vec![(0, String::new())];
    while let Some((node, prefix)) = pending.pop() {
        let mut stream = Stream::new(&trie[node..], "export trie");
        let terminal_size = stream.uleb()?;
        let children_at = (stream.position as u64)
            .checked_add(terminal_size)
            .filter(|&end| end <= (trie.len() - node) as u64)
            .ok_or_else(|| crate::Error::new(Malformed, format!("export trie: node {node:#x} information runs past the end")))?
            as usize;
        if terminal_size != 0 {
            let flags = stream.uleb()?;
            if flags & !KNOWN_FLAGS != 0 || flags & KIND_MASK == KIND_MASK {
                return fail(Malformed, format!("export trie: {prefix} has unknown flags {flags:#x}"));
            }
            let kind = match flags & KIND_MASK {
                KIND_THREAD_LOCAL => ExportKind::ThreadLocal,
                KIND_ABSOLUTE => ExportKind::Absolute,
                _ => ExportKind::Regular,
            };
            let mut export =
                Export32 { name: prefix.clone(), kind, weak: flags & WEAK_DEFINITION != 0, offset: None, reexport: None, resolver: None };
            if flags & REEXPORT != 0 {
                let ordinal = word(stream.uleb()?, "re-export library")?;
                export.reexport = Some((ordinal, stream.c_string()?));
            } else {
                export.offset = Some(word(stream.uleb()?, "address")?);
                if flags & STUB_AND_RESOLVER != 0 {
                    export.resolver = Some(word(stream.uleb()?, "resolver")?);
                }
            }
            if stream.position > children_at {
                return fail(Malformed, format!("export trie: {prefix}'s information is longer than its size"));
            }
            out.push(export);
        }
        stream.position = children_at;
        let count = stream.byte()?;
        for _ in 0..count {
            let edge = stream.c_string()?;
            let child = stream.uleb()?;
            let child = usize::try_from(child)
                .ok()
                .filter(|&child| child < trie.len())
                .ok_or_else(|| crate::Error::new(InvalidReference, format!("export trie: child {child:#x} is past the end")))?;
            if std::mem::replace(&mut queued[child], true) {
                return fail(Malformed, format!("export trie: node {child:#x} is reached twice"));
            }
            name_bytes = name_bytes.saturating_add(prefix.len() + edge.len());
            if name_bytes > name_budget {
                return fail(Malformed, format!("export trie: names exceed {name_budget} bytes for a {}-byte trie", trie.len()));
            }
            pending.push((child, format!("{prefix}{edge}")));
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
