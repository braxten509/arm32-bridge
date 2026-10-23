//! Objective-C type encodings ("v12@0:4@8", "{CGRect={CGPoint=ff}{CGSize=ff}}")
//! and how each type is laid out on the two sides of the boundary:
//! armv7 iOS (guest: 4-byte words, 8-byte types only 4-byte aligned, structs
//! of more than 4 bytes returned through a hidden pointer) and the 64-bit
//! host (AAPCS64: CGFloat is double, BOOL a byte, pointers 8 bytes).

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Void,
    /// `c` (BOOL on armv7), `C`
    Char { signed: bool },
    /// `s`, `S` (unichar)
    Short { signed: bool },
    /// `i`, `I`, `l`, `L` (NSInteger, NSUInteger, long: 4 bytes on armv7)
    Int { signed: bool },
    /// `q`, `Q`
    LongLong { signed: bool },
    /// `B` (C99 bool)
    Bool,
    /// `f`: CGFloat or float (the guest cannot tell; see [`Ty::is_cgfloat_struct`])
    Float,
    /// `d`
    Double,
    /// `@`, `@"Name"`
    Object,
    /// `@?`
    Block,
    /// `#`
    Class,
    /// `:`
    Selector,
    /// `*` (char *)
    CString,
    /// `^type`, `^?` (function pointer)
    Pointer(Box<Ty>),
    /// `{Name=fields}`; fields may be empty for opaque structs (`{CGColor=}`)
    Struct { name: String, fields: Vec<Ty> },
    /// `(Name=fields)`
    Union { name: String, fields: Vec<Ty> },
    /// `[count type]`
    Array { count: u32, element: Box<Ty> },
    /// `?` (unknown, e.g. a function)
    Unknown,
}

/// Struct names whose `f` fields are CGFloat (double on the host).
const CGFLOAT_STRUCTS: &[&str] = &["CGPoint", "CGSize", "CGRect", "CGAffineTransform", "UIEdgeInsets", "CGVector"];

impl Ty {
    /// Size on armv7.
    pub fn guest_size(&self) -> u32 {
        match self {
            Ty::Void => 0,
            Ty::Char { .. } | Ty::Bool => 1,
            Ty::Short { .. } => 2,
            Ty::LongLong { .. } | Ty::Double => 8,
            Ty::Struct { fields, .. } => {
                let mut size = 0;
                for f in fields {
                    size = align_up(size, f.guest_align()) + f.guest_size();
                }
                align_up(size, self.guest_align())
            }
            Ty::Union { fields, .. } => {
                align_up(fields.iter().map(Ty::guest_size).max().unwrap_or(0), self.guest_align())
            }
            Ty::Array { count, element } => count * element.guest_size(),
            _ => 4,
        }
    }

    /// Alignment on armv7 iOS: 8-byte scalars are 4-byte aligned.
    pub fn guest_align(&self) -> u32 {
        match self {
            Ty::Void => 1,
            Ty::Char { .. } | Ty::Bool => 1,
            Ty::Short { .. } => 2,
            Ty::Struct { fields, .. } | Ty::Union { fields, .. } => {
                fields.iter().map(Ty::guest_align).max().unwrap_or(1)
            }
            Ty::Array { element, .. } => element.guest_align(),
            _ => 4,
        }
    }

    pub fn is_struct(&self) -> bool {
        matches!(self, Ty::Struct { .. } | Ty::Union { .. } | Ty::Array { .. })
    }

    /// A struct whose float fields are CGFloat.
    pub fn is_cgfloat_struct(&self) -> bool {
        matches!(self, Ty::Struct { name, .. } if CGFLOAT_STRUCTS.contains(&name.as_str()))
    }

    /// armv7 returns this through a hidden first pointer (objc_msgSend_stret).
    pub fn guest_returns_in_memory(&self) -> bool {
        self.is_struct() && self.guest_size() > 4
    }

    /// The object-like types the boundary turns into proxies.
    pub fn is_object(&self) -> bool {
        matches!(self, Ty::Object | Ty::Block | Ty::Class)
    }
}

pub fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align.max(1)) * align.max(1)
}

/// A parsed method type string: return type, then every argument including
/// self and _cmd.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub ret: Ty,
    pub args: Vec<Ty>,
}

impl Signature {
    /// Any type string: a method's, a C function's or a block's.
    pub fn parse(text: &str) -> Result<Signature, String> {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        let ret = p.ty()?;
        p.skip_offset();
        let mut args = Vec::new();
        while p.i < p.s.len() {
            args.push(p.ty()?);
            p.skip_offset();
        }
        Ok(Signature { ret, args })
    }

    /// A method type string: it must have self and _cmd.
    pub fn parse_method(text: &str) -> Result<Signature, String> {
        let sig = Signature::parse(text)?;
        if sig.args.len() < 2 {
            return Err(format!("method type {text:?} lacks self and _cmd"));
        }
        Ok(sig)
    }

    /// Word offset (from r0) of each argument on armv7: arguments are laid
    /// out one after another in 4-byte steps (no even-register padding for
    /// 8-byte types on iOS); words 0-3 are r0-r3, the rest is the stack.
    /// With a hidden struct-return pointer, everything moves up one word.
    pub fn guest_arg_words(&self) -> Vec<u32> {
        let mut word = u32::from(self.ret.guest_returns_in_memory());
        self.args
            .iter()
            .map(|a| {
                let at = word;
                word += a.guest_size().div_ceil(4).max(1);
                at
            })
            .collect()
    }

    /// Words of arguments in total (registers and stack).
    pub fn guest_total_words(&self) -> u32 {
        let words = self.guest_arg_words();
        match (words.last(), self.args.last()) {
            (Some(w), Some(a)) => w + a.guest_size().div_ceil(4).max(1),
            _ => u32::from(self.ret.guest_returns_in_memory()),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_offset(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9' | b'-')) {
            self.i += 1;
        }
    }

    fn number(&mut self) -> u32 {
        let mut n = 0u32;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            n = n.saturating_mul(10).saturating_add(u32::from(c - b'0'));
            self.i += 1;
        }
        n
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at {} in {:?}", self.i, String::from_utf8_lossy(self.s)))
    }

    fn ty(&mut self) -> Result<Ty, String> {
        // Qualifiers: const, in, inout, out, bycopy, byref, oneway, atomic.
        while matches!(self.peek(), Some(b'r' | b'n' | b'N' | b'o' | b'O' | b'R' | b'V' | b'A')) {
            self.i += 1;
        }
        let Some(c) = self.peek() else { return self.fail("type expected") };
        self.i += 1;
        Ok(match c {
            b'v' => Ty::Void,
            b'c' => Ty::Char { signed: true },
            b'C' => Ty::Char { signed: false },
            b's' => Ty::Short { signed: true },
            b'S' => Ty::Short { signed: false },
            b'i' | b'l' => Ty::Int { signed: true },
            b'I' | b'L' => Ty::Int { signed: false },
            b'q' => Ty::LongLong { signed: true },
            b'Q' => Ty::LongLong { signed: false },
            b'B' => Ty::Bool,
            b'f' => Ty::Float,
            b'd' => Ty::Double,
            b'#' => Ty::Class,
            b':' => Ty::Selector,
            b'*' => Ty::CString,
            b'?' => Ty::Unknown,
            b'@' => match self.peek() {
                Some(b'?') => {
                    self.i += 1;
                    Ty::Block
                }
                Some(b'"') => {
                    let end = self.s[self.i + 1..].iter().position(|&b| b == b'"');
                    match end {
                        Some(end) => self.i += end + 2,
                        None => return self.fail("unclosed class name"),
                    }
                    Ty::Object
                }
                _ => Ty::Object,
            },
            b'^' => Ty::Pointer(Box::new(self.ty()?)),
            b'[' => {
                let count = self.number();
                let element = self.ty()?;
                if self.peek() != Some(b']') {
                    return self.fail("']' expected");
                }
                self.i += 1;
                Ty::Array { count, element: Box::new(element) }
            }
            b'{' | b'(' => {
                let close = if c == b'{' { b'}' } else { b')' };
                let start = self.i;
                while !matches!(self.peek(), Some(b'=') | None) && self.peek() != Some(close) {
                    self.i += 1;
                }
                let name = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                let mut fields = Vec::new();
                if self.peek() == Some(b'=') {
                    self.i += 1;
                    while self.peek() != Some(close) {
                        if self.peek().is_none() {
                            return self.fail("unclosed struct");
                        }
                        // Field names ("x"field) appear in ivar encodings.
                        if self.peek() == Some(b'"') {
                            let end = self.s[self.i + 1..].iter().position(|&b| b == b'"');
                            match end {
                                Some(end) => self.i += end + 2,
                                None => return self.fail("unclosed field name"),
                            }
                        }
                        fields.push(self.ty()?);
                    }
                }
                if self.peek() != Some(close) {
                    return self.fail("struct end expected");
                }
                self.i += 1;
                if c == b'{' {
                    Ty::Struct { name, fields }
                } else {
                    Ty::Union { name, fields }
                }
            }
            b'b' => {
                // Bit fields only appear inside structs; treat as an int's worth.
                self.number();
                Ty::Int { signed: false }
            }
            other => return self.fail(&format!("unknown type {:?}", other as char)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Ty {
        Signature::parse("{CGRect={CGPoint=ff}{CGSize=ff}}@:").unwrap().ret
    }

    #[test]
    fn method_types_parse_with_offsets_qualifiers_and_class_names() {
        let s = Signature::parse("c16@0:4@\"UIApplication\"8r^{__CFDictionary=}12").unwrap();
        assert_eq!(s.ret, Ty::Char { signed: true });
        assert_eq!(s.args[0], Ty::Object);
        assert_eq!(s.args[1], Ty::Selector);
        assert_eq!(s.args[2], Ty::Object);
        assert!(matches!(&s.args[3], Ty::Pointer(t) if matches!(&**t, Ty::Struct { name, fields } if name == "__CFDictionary" && fields.is_empty())));
        assert_eq!(Signature::parse("v@:@?").unwrap().args[2], Ty::Block);
        let e = Signature::parse("I@:^{NSFastEnumerationState=I^@^I[5I]}^@I").unwrap();
        let Ty::Pointer(state) = &e.args[2] else { panic!() };
        assert_eq!(state.guest_size(), 32);
        // C functions (NSLog's "v@") and blocks ("v4@?0") have no self/_cmd;
        // only method types must have them.
        assert_eq!(Signature::parse("v@").unwrap().args, [Ty::Object]);
        assert_eq!(Signature::parse("v4@?0").unwrap().args, [Ty::Block]);
        assert!(Signature::parse_method("v@").is_err());
        assert!(Signature::parse_method("v@:").is_ok());
        assert!(Signature::parse("v@:{CGPoint=ff").is_err());
    }

    #[test]
    fn armv7_sizes_alignment_and_struct_returns() {
        assert_eq!((rect().guest_size(), rect().guest_align()), (16, 4));
        assert!(rect().guest_returns_in_memory() && rect().is_cgfloat_struct());
        let range = Signature::parse("{_NSRange=II}@:").unwrap().ret;
        assert!(range.guest_returns_in_memory() && !range.is_cgfloat_struct());
        // A struct of one 4-byte field comes back in r0.
        assert!(!Signature::parse("{Small=i}@:").unwrap().ret.guest_returns_in_memory());
        // double/long long: 8 bytes but 4-byte aligned on iOS armv7.
        let mixed = Signature::parse("{M=cdi}@:").unwrap().ret;
        assert_eq!((mixed.guest_size(), mixed.guest_align()), (16, 4));
    }

    #[test]
    fn argument_words_pack_without_even_register_padding() {
        // self, _cmd, int, double, float -> words 0,1,2,3-4,5
        let s = Signature::parse("v@:idf").unwrap();
        assert_eq!(s.guest_arg_words(), [0, 1, 2, 3, 5]);
        assert_eq!(s.guest_total_words(), 6);
        // A CGRect return goes through r0: self is r1.
        let s = Signature::parse("{CGRect={CGPoint=ff}{CGSize=ff}}@:{CGRect={CGPoint=ff}{CGSize=ff}}@").unwrap();
        assert_eq!(s.guest_arg_words(), [1, 2, 3, 7]);
        // A BOOL argument still takes a whole word.
        assert_eq!(Signature::parse("v@:c@").unwrap().guest_arg_words(), [0, 1, 2, 3]);
    }

    #[test]
    fn framework_table_parses_and_has_each_selector_once() {
        let table = super::super::selectors::FrameworkSelectors::builtin();
        assert!(table.len() > 500);
        let rect_sel = table.get("frame").unwrap();
        assert!(rect_sel.signature.ret.guest_returns_in_memory());
        assert_eq!(table.get("stringWithFormat:").unwrap().varargs, super::super::selectors::Varargs::Format);
        assert_eq!(table.get("arrayWithObjects:").unwrap().varargs, super::super::selectors::Varargs::NilList);
        assert!(table.get("numberWithFloat:").unwrap().real_float && !table.get("setAlpha:").unwrap().real_float);
        assert!(table.get("noSuchSelector:").is_none());
    }
}
