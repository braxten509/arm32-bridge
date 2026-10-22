use std::fmt;

/// What sort of problem stopped the read or the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The file ends before a field that must be there.
    Truncated,
    /// A field holds a value the format does not allow.
    Malformed,
    /// Arithmetic on file values left the range it must stay in.
    Overflow,
    /// The format allows it, but this reader does not handle it yet.
    Unsupported,
    /// The program slice is still encrypted (cryptid is not 0).
    Encrypted,
    /// No slice for 32-bit ARM in a universal file.
    NoMatchingSlice,
    /// A segment, symbol, library or segment index points at nothing valid.
    InvalidReference,
    /// An address lies outside the allowed guest window.
    OutOfRange,
    /// Two things claim the same addresses.
    Overlap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error { kind, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn fail<T>(kind: ErrorKind, message: impl Into<String>) -> Result<T> {
    Err(Error::new(kind, message))
}
