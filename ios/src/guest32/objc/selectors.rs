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
    /// The result points to host bytes, as many as the integer
    /// out-parameter says (`host-bytes`).
    pub host_bytes: bool,
}

pub struct FrameworkSelectors {
    map: HashMap<&'static str, FrameworkSelector>,
    /// Rows for receivers of one class and its subclasses (`class:Name`),
    /// tried before the selector's general row.
    by_class: HashMap<&'static str, Vec<(&'static str, FrameworkSelector)>>,
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
        let mut by_class: HashMap<&'static str, Vec<(&'static str, FrameworkSelector)>> = HashMap::new();
        for (number, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut columns = line.split('\t');
            let (Some(selector), Some(encoding)) = (columns.next(), columns.next()) else {
                return Err(format!("line {}: needs a selector and an encoding", number + 1));
            };
            let notes: Vec<&str> = columns.next().map(|n| n.split(',').collect()).unwrap_or_default();
            let signature = Signature::parse_method(encoding).map_err(|e| format!("line {}: {e}", number + 1))?;
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
            let (real_float, host_bytes) = (notes.contains(&"float"), notes.contains(&"host-bytes"));
            let entry = FrameworkSelector { encoding, signature, varargs, real_float, arrays, host_bytes };
            if let Some(class) = notes.iter().find_map(|n| n.strip_prefix("class:")) {
                let rows = by_class.entry(selector).or_default();
                if rows.iter().any(|(c, _)| *c == class) {
                    return Err(format!("line {}: {selector} is listed twice for {class}", number + 1));
                }
                rows.push((class, entry));
            } else if map.insert(selector, entry).is_some() {
                return Err(format!("line {}: {selector} is listed twice", number + 1));
            }
        }
        Ok(FrameworkSelectors { map, by_class })
    }

    /// The row for a message to a receiver: its class's own row (`is_kind`
    /// tells whether the receiver is of a named class), else the general one.
    pub fn get_for(&self, selector: &str, is_kind: impl Fn(&str) -> bool) -> Option<&FrameworkSelector> {
        self.by_class.get(selector).and_then(|rows| rows.iter().find(|(class, _)| is_kind(class)).map(|(_, entry)| entry)).or_else(|| self.get(selector))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn get(&self, selector: &str) -> Option<&FrameworkSelector> {
        self.map.get(selector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A selector whose types differ by class (CALayer's setTransform: takes
    /// a CATransform3D, UIView's a CGAffineTransform) gets the row of the
    /// receiver's class, else the general one; a class listed twice is a mistake.
    #[test]
    fn rows_for_one_class_win_for_its_receivers() {
        let table = FrameworkSelectors::parse(
            "setTransform:\tv@:{CGAffineTransform=ffffff}\nsetTransform:\tv@:{CATransform3D=ffffffffffffffff}\tclass:CALayer\n",
        )
        .unwrap();
        let layer = |name: &str| name == "CALayer";
        assert_eq!(table.get_for("setTransform:", layer).unwrap().encoding, "v@:{CATransform3D=ffffffffffffffff}");
        assert_eq!(table.get_for("setTransform:", |_| false).unwrap().encoding, "v@:{CGAffineTransform=ffffff}");
        assert!(table.get_for("frame", layer).is_none());
        assert!(FrameworkSelectors::parse("a:\tv@:@\tclass:X\na:\tv@:i\tclass:X\n").is_err());
    }
}
