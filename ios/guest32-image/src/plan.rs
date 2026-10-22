//! Decides where an image's segments, entry and fix-ups land in guest
//! memory. Nothing is mapped or written here.

use crate::error::ErrorKind::{InvalidReference, Malformed, OutOfRange, Overflow, Overlap};
use crate::error::{fail, Error, Result};
use crate::image::{Entry32, Image32};
use crate::opcodes::FixupKind;

const ADDRESS_SPACE_END: u64 = 1 << 32;

/// A half-open guest address range; `end` may be 2^32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressRange {
    pub start: u64,
    pub end: u64,
}

impl AddressRange {
    pub fn new(start: u64, end: u64) -> Self {
        AddressRange { start, end }
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn overlaps(&self, other: &AddressRange) -> bool {
        !self.is_empty() && !other.is_empty() && self.start < other.end && other.start < self.end
    }

    pub fn contains_range(&self, inner: &AddressRange) -> bool {
        inner.start >= self.start && inner.end <= self.end
    }

    fn check(&self, what: &str) -> Result<()> {
        if self.start > self.end || self.end > ADDRESS_SPACE_END {
            return fail(Malformed, format!("{what} {:#x}..{:#x} is not a 32-bit range", self.start, self.end));
        }
        Ok(())
    }
}

/// The guest address space the caller offers.
#[derive(Clone, Debug)]
pub struct MemoryLayout {
    /// Every mapped segment (rounded to pages) must sit inside this.
    pub mapped: AddressRange,
    /// Executable segments (rounded to pages) must also sit inside this.
    pub code: AddressRange,
    /// Ranges the runtime already uses; no segment page may touch them.
    pub reserved: Vec<AddressRange>,
    /// Guest mapping granularity, a power of two. Slides are multiples of it.
    pub page_size: u32,
    /// May a movable image (an `MH_PIE` program, or any library or bundle)
    /// move if its own addresses do not fit? Other programs never move.
    pub allow_slide: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedSegment {
    /// Index into [`Image32::segments`].
    pub segment: usize,
    pub name: String,
    /// Slid start and exact size.
    pub start: u32,
    pub size: u32,
    /// The whole pages the segment touches.
    pub pages: AddressRange,
    /// File bytes to copy (offsets are inside the slice) ...
    pub file_offset: u32,
    pub file_size: u32,
    /// ... and how many zero bytes follow them.
    pub zero_fill: u32,
    pub initial_protection: u32,
    pub max_protection: u32,
    /// True when a neighbour segment uses part of the same page, so the page
    /// needs both segments' bytes and the union of their rights.
    pub shares_page: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedEntry {
    /// Slid entry address, Thumb bit kept.
    pub address: u32,
    pub thumb: bool,
    pub source: Entry32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixupAction {
    /// Index into [`Image32::rebases`]: add the slide.
    Rebase(usize),
    /// Index into [`Image32::binds`]: write the resolved symbol plus addend.
    Bind(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedFixup {
    pub guest_address: u32,
    pub width: u8,
    pub kind: FixupKind,
    pub action: FixupAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadPlan {
    /// Added to every unslid address. 0 for images that cannot move.
    pub slide: u32,
    pub segments: Vec<PlannedSegment>,
    /// The low no-access range (from `__PAGEZERO`) that must stay unmapped so
    /// null-pointer use faults. Never slid.
    pub null_guard: Option<AddressRange>,
    pub entry: Option<PlannedEntry>,
    /// All rebases then all binds, in image order.
    pub fixups: Vec<PlannedFixup>,
    /// Lowest to highest page any segment touches.
    pub span: AddressRange,
}

fn align_down(value: u64, page: u64) -> u64 {
    value & !(page - 1)
}

fn align_up(value: u64, page: u64) -> Option<u64> {
    value.checked_add(page - 1).map(|v| v & !(page - 1))
}

pub fn plan_load(image: &Image32, layout: &MemoryLayout) -> Result<LoadPlan> {
    layout.mapped.check("mapped window")?;
    layout.code.check("code window")?;
    for range in &layout.reserved {
        range.check("reserved range")?;
    }
    if layout.page_size == 0 || !layout.page_size.is_power_of_two() {
        return fail(Malformed, format!("page size {} is not a power of two", layout.page_size));
    }
    let page = u64::from(layout.page_size);

    let mut guard = None;
    let mut mapped = Vec::new();
    for (index, segment) in image.segments.iter().enumerate() {
        if segment.is_null_guard() {
            if guard.is_some() {
                return fail(Malformed, "two low guard segments");
            }
            guard = Some(AddressRange::new(0, u64::from(segment.vm_size)));
        } else if segment.vm_size != 0 {
            mapped.push(index);
        }
    }
    if mapped.is_empty() {
        return fail(Malformed, "image has no segment to map");
    }
    // Slide 0 first. If the smallest fitting slide s is above 0, then s - page
    // failed only because some segment's first page then sat inside an
    // obstacle (reserved range, guard) or below a window start; at s that
    // page starts exactly at the obstacle's or window's page-rounded end.
    // So every (obstacle end - segment page start) is a candidate, and the
    // smallest one that fits is the smallest fitting slide.
    let mut candidates = vec![0u64];
    if image.can_slide() && layout.allow_slide {
        let mut ends = vec![layout.mapped.start, layout.code.start];
        ends.extend(layout.reserved.iter().map(|r| r.end));
        ends.extend(guard.map(|g| g.end));
        for end in ends.into_iter().filter_map(|end| align_up(end, page)) {
            for &index in &mapped {
                let segment_page = align_down(u64::from(image.segments[index].vm_address), page);
                if end > segment_page {
                    candidates.push(end - segment_page);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
    }
    let mut first_error = None;
    for slide in candidates {
        match try_slide(image, layout, &mapped, guard, slide, page) {
            Ok(plan) => return Ok(plan),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap())
}

fn try_slide(
    image: &Image32,
    layout: &MemoryLayout,
    mapped: &[usize],
    guard: Option<AddressRange>,
    slide: u64,
    page: u64,
) -> Result<LoadPlan> {
    let mut segments: Vec<PlannedSegment> = Vec::with_capacity(mapped.len());
    for &index in mapped {
        let segment = &image.segments[index];
        let start = u64::from(segment.vm_address) + slide;
        let end = start + u64::from(segment.vm_size);
        let what = format!("segment {} at {start:#x}..{end:#x}", segment.name);
        let pages_end = align_up(end, page).filter(|&e| e <= ADDRESS_SPACE_END);
        let Some(pages_end) = pages_end else {
            return fail(OutOfRange, format!("{what} runs past the 32-bit guest space with slide {slide:#x}"));
        };
        let pages = AddressRange::new(align_down(start, page), pages_end);
        if !layout.mapped.contains_range(&pages) {
            return fail(OutOfRange, format!("{what} is outside the mapped window"));
        }
        if segment.executable() && !layout.code.contains_range(&pages) {
            return fail(OutOfRange, format!("{what} is code outside the code window"));
        }
        if let Some(reserved) = layout.reserved.iter().find(|r| r.overlaps(&pages)) {
            return fail(Overlap, format!("{what} touches reserved {:#x}..{:#x}", reserved.start, reserved.end));
        }
        if let Some(guard) = guard {
            // Whole pages: mapping the segment maps its first page, which may
            // reach down into an unaligned guard.
            if guard.overlaps(&pages) {
                return fail(Overlap, format!("{what} overlaps the low guard"));
            }
        }
        segments.push(PlannedSegment {
            segment: index,
            name: segment.name.clone(),
            start: start as u32,
            size: segment.vm_size,
            pages,
            file_offset: segment.file_offset,
            file_size: segment.file_size,
            zero_fill: segment.vm_size - segment.file_size,
            initial_protection: segment.initial_protection,
            max_protection: segment.max_protection,
            shares_page: false,
        });
    }
    for a in 0..segments.len() {
        for b in 0..segments.len() {
            if a != b && segments[a].pages.overlaps(&segments[b].pages) {
                segments[a].shares_page = true;
            }
        }
    }
    let span = AddressRange::new(
        segments.iter().map(|s| s.pages.start).min().unwrap(),
        segments.iter().map(|s| s.pages.end).max().unwrap(),
    );
    let slide = slide as u32;

    let entry = match &image.entry {
        Some(entry) => {
            let address = entry
                .address()
                .checked_add(slide)
                .ok_or_else(|| Error::new(Overflow, "slid entry overflows"))?;
            let code = address & !1;
            let inside = segments
                .iter()
                .any(|s| image.segments[s.segment].executable() && code >= s.start && code - s.start < s.size);
            if !inside {
                return fail(InvalidReference, format!("slid entry {address:#x} is not in a planned code segment"));
            }
            Some(PlannedEntry { address, thumb: entry.is_thumb(), source: entry.clone() })
        }
        None => None,
    };

    let mut fixups = Vec::with_capacity(image.rebases.len() + image.binds.len());
    let rebases = image.rebases.iter().enumerate().map(|(i, r)| (r.address, r.segment, r.width, r.kind, FixupAction::Rebase(i)));
    let binds = image.binds.iter().enumerate().map(|(i, b)| (b.address, b.segment, b.width, b.kind, FixupAction::Bind(i)));
    for (address, segment, width, kind, action) in rebases.chain(binds) {
        let guest = u64::from(address) + u64::from(slide);
        let planned = segments.iter().find(|s| s.segment == segment).ok_or_else(|| {
            Error::new(InvalidReference, format!("fix-up at {address:#x} is in an unmapped segment"))
        })?;
        let slot = AddressRange::new(guest, guest + u64::from(width));
        let segment_range = AddressRange::new(u64::from(planned.start), u64::from(planned.start) + u64::from(planned.size));
        if !segment_range.contains_range(&slot) || !layout.mapped.contains_range(&slot) {
            return fail(OutOfRange, format!("fix-up slot {guest:#x} is outside its planned segment"));
        }
        if layout.reserved.iter().any(|r| r.overlaps(&slot)) {
            return fail(Overlap, format!("fix-up slot {guest:#x} is in a reserved range"));
        }
        fixups.push(PlannedFixup { guest_address: guest as u32, width, kind, action });
    }

    Ok(LoadPlan { slide, segments, null_guard: guard, entry, fixups, span })
}
