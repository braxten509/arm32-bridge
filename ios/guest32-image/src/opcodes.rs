//! The dyld rebase and bind opcode streams of `LC_DYLD_INFO(_ONLY)`, decoded
//! into typed 32-bit records. Every destination is checked against the
//! segment the stream names; nothing is applied.

use crate::error::{fail, Result};
use crate::error::ErrorKind::{InvalidReference, Malformed, OutOfRange, Overflow, Unsupported};
use crate::read::{step32, Stream};

/// Width in bytes of every fix-up slot in a 32-bit image.
pub const FIXUP_WIDTH: u8 = 4;

/// How the fixed-up slot is written. Kept apart because the runtime must
/// write them differently (`*slot = value` versus `*slot = value - (slot + 4)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixupKind {
    /// `REBASE_TYPE_POINTER` / `BIND_TYPE_POINTER`.
    Pointer,
    /// `*_TYPE_TEXT_ABSOLUTE32`: an absolute 32-bit value inside code.
    TextAbsolute32,
    /// `*_TYPE_TEXT_PCREL32`: a 32-bit value relative to slot + 4, inside code.
    TextPcrel32,
}

impl FixupKind {
    fn from_type(value: u8, what: &str) -> Result<Self> {
        match value {
            1 => Ok(FixupKind::Pointer),
            2 => Ok(FixupKind::TextAbsolute32),
            3 => Ok(FixupKind::TextPcrel32),
            other => fail(Malformed, format!("{what}: unknown fix-up type {other}")),
        }
    }

    /// The word llvm-objdump prints for this type.
    pub fn name(self) -> &'static str {
        match self {
            FixupKind::Pointer => "pointer",
            FixupKind::TextAbsolute32 => "text abs32",
            FixupKind::TextPcrel32 => "text rel32",
        }
    }

    pub fn is_text(self) -> bool {
        self != FixupKind::Pointer
    }
}

/// Which bind stream a record came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindSource {
    Normal,
    Lazy,
    Weak,
}

/// Where a bind looks for its symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryRef {
    /// Ordinal 0: this image.
    SelfImage,
    /// Ordinal -1: the main program.
    MainExecutable,
    /// Ordinal -2: search every loaded image.
    FlatLookup,
    /// Ordinal -3: the weak-definition lookup.
    WeakLookup,
    /// 1-based index into [`crate::Image32::dylibs`].
    Dylib(u32),
}

impl LibraryRef {
    /// The raw dyld ordinal value.
    pub fn ordinal(self) -> i64 {
        match self {
            LibraryRef::SelfImage => 0,
            LibraryRef::MainExecutable => -1,
            LibraryRef::FlatLookup => -2,
            LibraryRef::WeakLookup => -3,
            LibraryRef::Dylib(n) => i64::from(n),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rebase32 {
    /// Unslid address of the slot.
    pub address: u32,
    /// Index into [`crate::Image32::segments`].
    pub segment: usize,
    pub kind: FixupKind,
    pub width: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bind32 {
    pub source: BindSource,
    /// Unslid address of the slot.
    pub address: u32,
    /// Index into [`crate::Image32::segments`].
    pub segment: usize,
    pub kind: FixupKind,
    pub width: u8,
    /// `None` for weak-stream records: those are resolved by weak coalescing,
    /// not by a library ordinal.
    pub library: Option<LibraryRef>,
    pub symbol: String,
    /// Added to the symbol's address; dyld does this sum in 32 bits.
    pub addend: i64,
    /// `BIND_SYMBOL_FLAGS_WEAK_IMPORT`: a missing symbol binds to 0.
    pub weak_import: bool,
    /// Lazy records only: the stream offset of this record's opcode run, the
    /// number the stub helper hands to dyld_stub_binder.
    pub lazy_offset: Option<u32>,
}

/// A `BIND_SYMBOL_FLAGS_NON_WEAK_DEFINITION` entry of the weak stream: this
/// image has a strong definition that should win over weak ones elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeakDefinition {
    pub symbol: String,
    /// How many weak-stream bind records came before it (stream order).
    pub position: usize,
}

/// What the decoder needs to know about a segment.
pub(crate) struct SegmentSpan {
    pub vm_address: u32,
    pub vm_size: u32,
    pub writable: bool,
    pub executable: bool,
}

struct Context<'a> {
    segments: &'a [SegmentSpan],
    dylib_count: usize,
    what: &'static str,
}

impl Context<'_> {
    fn segment_start(&self, index: u8, offset: u64) -> Result<(usize, u32)> {
        let segment = self.segments.get(usize::from(index)).ok_or_else(|| {
            crate::Error::new(InvalidReference, format!("{}: names missing segment {index}", self.what))
        })?;
        if offset > u64::from(segment.vm_size) {
            return fail(OutOfRange, format!("{}: offset {offset:#x} is past the end of segment {index}", self.what));
        }
        // A segment may end exactly at 2^32, so its end offset has no 32-bit address.
        let address = u64::from(segment.vm_address) + offset;
        if address > u64::from(u32::MAX) {
            return fail(OutOfRange, format!("{}: segment {index} offset {offset:#x} is past 32 bits", self.what));
        }
        Ok((usize::from(index), address as u32))
    }

    /// Checks a slot before it is recorded: inside the segment the stream
    /// named, all 4 bytes, and in a segment that may hold this kind of fix-up.
    fn check_slot(&self, segment: Option<usize>, address: u32, kind: FixupKind) -> Result<usize> {
        let index = match segment {
            Some(index) => index,
            None => return fail(InvalidReference, format!("{}: fix-up before any segment was set", self.what)),
        };
        let span = &self.segments[index];
        let start = u64::from(span.vm_address);
        let end = start + u64::from(span.vm_size);
        if u64::from(address) < start || u64::from(address) + u64::from(FIXUP_WIDTH) > end {
            return fail(
                OutOfRange,
                format!("{}: slot {address:#x} is outside segment {index} ({start:#x}..{end:#x})", self.what),
            );
        }
        if kind.is_text() && !span.executable {
            return fail(InvalidReference, format!("{}: text fix-up at {address:#x} in a non-code segment", self.what));
        }
        if !kind.is_text() && !span.writable {
            return fail(
                InvalidReference,
                format!("{}: pointer fix-up at {address:#x} in a read-only segment", self.what),
            );
        }
        Ok(index)
    }

    /// A repeat count can never be larger than the number of slots in the segment.
    fn check_count(&self, segment: Option<usize>, count: u64) -> Result<u64> {
        let limit = segment
            .and_then(|index| self.segments.get(index))
            .map(|span| u64::from(span.vm_size) / u64::from(FIXUP_WIDTH) + 1)
            .unwrap_or(0);
        if count > limit {
            return fail(Overflow, format!("{}: repeat count {count} exceeds the segment's slots", self.what));
        }
        Ok(count)
    }
}

pub(crate) fn decode_rebases(bytes: &[u8], segments: &[SegmentSpan]) -> Result<Vec<Rebase32>> {
    let context = Context { segments, dylib_count: 0, what: "rebase stream" };
    let mut stream = Stream::new(bytes, "rebase stream");
    let mut out = Vec::new();
    let mut kind = FixupKind::Pointer;
    let mut segment: Option<usize> = None;
    let mut address = 0u32;
    let width = u32::from(FIXUP_WIDTH);
    let push = |segment: Option<usize>, address: u32, kind: FixupKind, out: &mut Vec<Rebase32>| -> Result<()> {
        let segment = context.check_slot(segment, address, kind)?;
        out.push(Rebase32 { address, segment, kind, width: FIXUP_WIDTH });
        Ok(())
    };
    while !stream.done() {
        let byte = stream.byte()?;
        let immediate = byte & 0x0F;
        match byte & 0xF0 {
            0x00 => break,
            0x10 => kind = FixupKind::from_type(immediate, "rebase stream")?,
            0x20 => {
                let (index, start) = context.segment_start(immediate, stream.uleb()?)?;
                segment = Some(index);
                address = start;
            }
            0x30 => address = address.wrapping_add(step32(stream.uleb()?, "rebase stream")?),
            0x40 => address = address.wrapping_add(u32::from(immediate) * width),
            0x50 => {
                for _ in 0..immediate {
                    push(segment, address, kind, &mut out)?;
                    address = address.wrapping_add(width);
                }
            }
            0x60 => {
                let count = context.check_count(segment, stream.uleb()?)?;
                for _ in 0..count {
                    push(segment, address, kind, &mut out)?;
                    address = address.wrapping_add(width);
                }
            }
            0x70 => {
                push(segment, address, kind, &mut out)?;
                let step = step32(stream.uleb()?, "rebase stream")?;
                address = address.wrapping_add(step).wrapping_add(width);
            }
            0x80 => {
                let count = context.check_count(segment, stream.uleb()?)?;
                let skip = step32(stream.uleb()?, "rebase stream")?;
                for _ in 0..count {
                    push(segment, address, kind, &mut out)?;
                    address = address.wrapping_add(skip).wrapping_add(width);
                }
            }
            other => {
                return fail(Malformed, format!("rebase stream: unknown opcode {other:#04x} at {:#x}", stream.position - 1))
            }
        }
    }
    Ok(out)
}

#[derive(Default)]
struct BindState {
    ordinal: Option<i64>,
    symbol: Option<String>,
    flags: u8,
    kind: Option<FixupKind>,
    addend: i64,
    segment: Option<usize>,
    address: u32,
}

pub(crate) struct BindOutput {
    pub binds: Vec<Bind32>,
    pub weak_definitions: Vec<WeakDefinition>,
}

pub(crate) fn decode_binds(
    bytes: &[u8],
    source: BindSource,
    segments: &[SegmentSpan],
    dylib_count: usize,
) -> Result<BindOutput> {
    let what = match source {
        BindSource::Normal => "bind stream",
        BindSource::Lazy => "lazy bind stream",
        BindSource::Weak => "weak bind stream",
    };
    let context = Context { segments, dylib_count, what };
    let mut stream = Stream::new(bytes, what);
    let mut output = BindOutput { binds: Vec::new(), weak_definitions: Vec::new() };
    let mut state = BindState::default();
    let mut entry_start = 0usize;
    let width = u32::from(FIXUP_WIDTH);

    let emit = |state: &BindState, entry_start: usize, binds: &mut Vec<Bind32>| -> Result<()> {
        let kind = state.kind.unwrap_or(FixupKind::Pointer);
        let segment = context.check_slot(state.segment, state.address, kind)?;
        let symbol = match &state.symbol {
            Some(symbol) => symbol.clone(),
            None => return fail(InvalidReference, format!("{what}: bind at {:#x} has no symbol", state.address)),
        };
        if state.addend < -(1i64 << 31) || state.addend >= (1i64 << 32) {
            return fail(Overflow, format!("{what}: addend {} for {symbol} does not fit 32 bits", state.addend));
        }
        let library = if source == BindSource::Weak {
            None
        } else {
            Some(library_ref(state.ordinal.unwrap_or(0), context.dylib_count, what)?)
        };
        binds.push(Bind32 {
            source,
            address: state.address,
            segment,
            kind,
            width: FIXUP_WIDTH,
            library,
            symbol,
            addend: state.addend,
            weak_import: state.flags & 0x1 != 0,
            lazy_offset: (source == BindSource::Lazy).then_some(entry_start as u32),
        });
        Ok(())
    };

    while !stream.done() {
        let opcode_at = stream.position;
        let byte = stream.byte()?;
        let immediate = byte & 0x0F;
        match byte & 0xF0 {
            0x00 => {
                if source != BindSource::Lazy {
                    break;
                }
                // Each lazy entry is run on its own by dyld_stub_binder, starting
                // from a clean state, so nothing carries over to the next entry.
                state = BindState::default();
                entry_start = stream.position;
            }
            0x10 | 0x20 | 0x30 => {
                if source == BindSource::Weak {
                    return fail(Unsupported, format!("{what}: library ordinal at {opcode_at:#x} in the weak stream"));
                }
                state.ordinal = Some(match byte & 0xF0 {
                    0x10 => i64::from(immediate),
                    0x20 => {
                        let value = stream.uleb()?;
                        if value > u64::from(u32::MAX) {
                            return fail(Overflow, format!("{what}: library ordinal {value} is too large"));
                        }
                        value as i64
                    }
                    _ => {
                        if immediate == 0 {
                            0
                        } else {
                            i64::from((immediate | 0xF0) as i8)
                        }
                    }
                });
            }
            0x40 => {
                state.flags = immediate;
                let symbol = stream.c_string()?;
                if source == BindSource::Weak && immediate & 0x8 != 0 {
                    output.weak_definitions.push(WeakDefinition { symbol: symbol.clone(), position: output.binds.len() });
                } else if immediate & !0x1 != 0 {
                    return fail(Unsupported, format!("{what}: symbol flags {immediate:#x} for {symbol}"));
                }
                state.symbol = Some(symbol);
            }
            0x50 => state.kind = Some(FixupKind::from_type(immediate, what)?),
            0x60 => state.addend = stream.sleb()?,
            0x70 => {
                let (index, start) = context.segment_start(immediate, stream.uleb()?)?;
                state.segment = Some(index);
                state.address = start;
            }
            0x80 => state.address = state.address.wrapping_add(step32(stream.uleb()?, what)?),
            0x90 => {
                emit(&state, entry_start, &mut output.binds)?;
                state.address = state.address.wrapping_add(width);
            }
            0xA0 => {
                emit(&state, entry_start, &mut output.binds)?;
                let step = step32(stream.uleb()?, what)?;
                state.address = state.address.wrapping_add(step).wrapping_add(width);
            }
            0xB0 => {
                emit(&state, entry_start, &mut output.binds)?;
                state.address = state.address.wrapping_add(u32::from(immediate) * width + width);
            }
            0xC0 => {
                let count = context.check_count(state.segment, stream.uleb()?)?;
                let skip = step32(stream.uleb()?, what)?;
                for _ in 0..count {
                    emit(&state, entry_start, &mut output.binds)?;
                    state.address = state.address.wrapping_add(skip).wrapping_add(width);
                }
            }
            0xD0 => return fail(Unsupported, format!("{what}: threaded binds (arm64e only) at {opcode_at:#x}")),
            other => return fail(Malformed, format!("{what}: unknown opcode {other:#04x} at {opcode_at:#x}")),
        }
    }
    Ok(output)
}

fn library_ref(ordinal: i64, dylib_count: usize, what: &str) -> Result<LibraryRef> {
    match ordinal {
        0 => Ok(LibraryRef::SelfImage),
        -1 => Ok(LibraryRef::MainExecutable),
        -2 => Ok(LibraryRef::FlatLookup),
        -3 => Ok(LibraryRef::WeakLookup),
        n if n > 0 && (n as u64) <= dylib_count as u64 => Ok(LibraryRef::Dylib(n as u32)),
        n => fail(InvalidReference, format!("{what}: library ordinal {n} but only {dylib_count} libraries")),
    }
}
