//! Checked reader and load planner for 32-bit ARM iPhone programs (Mach-O32).
//!
//! This crate only reads and plans. It never runs guest code and never writes
//! guest memory: [`Image32::parse`] decodes the file into typed records, and
//! [`plan_load`] decides where each segment and fix-up would land inside a
//! caller-supplied [`MemoryLayout`]. Mapping and applying fix-ups is left to
//! the runtime that owns guest memory.
//!
//! What is covered (anything else is refused with an error, never skipped):
//! - thin `MH_MAGIC` files and big-endian `FAT_MAGIC` universal files (armv7
//!   slice preferred over armv7s, then armv6);
//! - `LC_SEGMENT` with sections, `LC_SYMTAB`, `LC_DYSYMTAB` (indirect table
//!   only; classic relocations are refused), `LC_DYLD_INFO(_ONLY)` rebase,
//!   bind, weak-bind and lazy-bind opcode streams, the dylib load commands,
//!   `LC_MAIN`, `LC_UNIXTHREAD` (ARM_THREAD_STATE) and `LC_ENCRYPTION_INFO`.
//! - the export trie (bounds-checked at parse; [`Image32::exports`] decodes it).
//! - 32-bit Objective-C 2 metadata ([`read_objc`] over [`ImageMemory`] or any [`Memory32`]).

mod error;
mod exports;
mod image;
mod objc;
mod opcodes;
mod plan;
mod read;

pub use error::{Error, ErrorKind, Result};
pub use exports::{decode_export_trie, Export32, ExportKind};
pub use image::{
    choose_slice, ArmSubtype, ArmThreadState, ByteRange, Dylib, DylibKind, EncryptionInfo, Entry32, Image32, Section32,
    Segment32, SliceChoice, Symbol32, ThinSlice, CPU_TYPE_ARM, MH_PIE,
};
pub use objc::{
    read_objc, CfString32, ClassRef32, ClassRo32, ClassTarget, ImageMemory, Ivar32, Memory32, Method32, ObjcCategory32,
    ObjcClass32, ObjcMetadata32, ObjcProtocol32, ObjcSections, Property32, SelectorRef32, RO_HAS_CXX_STRUCTORS, RO_META,
    RO_ROOT,
};
pub use opcodes::{Bind32, BindSource, FixupKind, LibraryRef, Rebase32, WeakDefinition, FIXUP_WIDTH};
pub use plan::{plan_load, AddressRange, FixupAction, LoadPlan, MemoryLayout, PlannedEntry, PlannedFixup, PlannedSegment};
