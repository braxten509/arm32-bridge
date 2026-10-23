//! The selector -> type encoding table for Apple framework selectors that
//! 32-bit games send (framework_selectors.tsv; see its header).

use super::encoding::Signature;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Varargs {
    #[default]
    None,
    Format,
    NilList,
}

/// Pointer-to-id arguments of a counted message (`objects:count:`): they
/// hold as many ids as the message's last integer argument says, read
/// before the call (`in-array`) or filled by it (`out-array`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdArrays {
    #[default]
    None,
    In,
    Out,
}

#[derive(Clone, Debug)]
pub struct FrameworkSelector {
    pub encoding: &'static str,
    pub signature: Signature,
    pub varargs: Varargs,
    pub real_float: bool,
    pub arrays: IdArrays,
}

pub struct FrameworkSelectors {
    map: HashMap<&'static str, FrameworkSelector>,
}

impl FrameworkSelectors {
    /// The table in framework_selectors.tsv. A bad row is a build mistake,
    /// so it panics naming the row.
    pub fn builtin() -> &'static FrameworkSelectors {
        static TABLE: std::sync::OnceLock<FrameworkSelectors> = std::sync::OnceLock::new();
        TABLE.get_or_init(|| FrameworkSelectors::parse(include_str!("framework_selectors.tsv")).unwrap())
    }

    pub fn parse(text: &'static str) -> Result<FrameworkSelectors, String> {
        let mut map = HashMap::new();
        for (number, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut columns = line.split('\t');
            let (Some(selector), Some(encoding)) = (columns.next(), columns.next()) else {
                return Err(format!("line {}: needs a selector and an encoding", number + 1));
            };
            let notes: Vec<&str> = columns.next().map(|n| n.split(',').collect()).unwrap_or_default();
            let signature = Signature::parse(encoding).map_err(|e| format!("line {}: {e}", number + 1))?;
            let colons = selector.matches(':').count();
            if signature.args.len() != colons + 2 {
                return Err(format!("line {}: {selector} takes {colons} arguments, encoding has {}", number + 1, signature.args.len() - 2));
            }
            let varargs = if notes.contains(&"format") {
                Varargs::Format
            } else if notes.contains(&"nil-list") {
                Varargs::NilList
            } else {
                Varargs::None
            };
            let arrays = if notes.contains(&"in-array") {
                IdArrays::In
            } else if notes.contains(&"out-array") {
                IdArrays::Out
            } else {
                IdArrays::None
            };
            let entry = FrameworkSelector { encoding, signature, varargs, real_float: notes.contains(&"float"), arrays };
            if map.insert(selector, entry).is_some() {
                return Err(format!("line {}: {selector} is listed twice", number + 1));
            }
        }
        Ok(FrameworkSelectors { map })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn get(&self, selector: &str) -> Option<&FrameworkSelector> {
        self.map.get(selector)
    }
}
