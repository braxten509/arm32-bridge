//! NSRegularExpression on top of the regex crate. Ranges are counted in
//! UTF-16 units, as Apple counts them.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::string::{self, Range};
use crate::fw::collections;
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

const CASE_INSENSITIVE: usize = 1;
const ALLOW_COMMENTS: usize = 2;
const IGNORE_METACHARACTERS: usize = 4;
const DOT_MATCHES_NEWLINES: usize = 8;
const ANCHORS_MATCH_LINES: usize = 16;
const NOT_FOUND: usize = isize::MAX as usize;

fn compiled(object: Id) -> &'static regex::Regex {
    unsafe { &**((object + 8) as *const *const regex::Regex) }
}

fn build(pattern: &str, options: usize) -> Result<regex::Regex, regex::Error> {
    let pattern = if options & IGNORE_METACHARACTERS != 0 { regex::escape(pattern) } else { pattern.to_string() };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(options & CASE_INSENSITIVE != 0)
        .ignore_whitespace(options & ALLOW_COMMENTS != 0)
        .dot_matches_new_line(options & DOT_MATCHES_NEWLINES != 0)
        .multi_line(options & ANCHORS_MATCH_LINES != 0)
        .build()
}

extern "C" fn init_with_pattern(this: Id, _: Sel, pattern: Id, options: usize, error: *mut Id) -> Id {
    let text = string::get(pattern);
    match build(&text, options) {
        Ok(compiled) => {
            unsafe { *((this + 8) as *mut *const regex::Regex) = Box::into_raw(Box::new(compiled)) };
            this
        }
        Err(e) => {
            crate::log!("regular expression {text:?} not understood: {e}");
            if !error.is_null() {
                unsafe { *error = crate::fw::misc::error("NSCocoaErrorDomain", 2048, &e.to_string()) };
            }
            0
        }
    }
}

extern "C" fn with_pattern(class: Class, s: Sel, pattern: Id, options: usize, error: *mut Id) -> Id {
    init_with_pattern(runtime::alloc_instance(class), s, pattern, options, error)
}

/// The searched part of the string as UTF-8, with a map back to UTF-16 positions.
struct Searched {
    text: String,
    /// UTF-16 position of each byte boundary (indexed by byte offset).
    utf16_at: Vec<usize>,
}

fn searched(object: Id, range: Range) -> Searched {
    let units: Vec<u16> = {
        let all = string::get(object).encode_utf16().collect::<Vec<u16>>();
        let start = range.location.min(all.len());
        let end = (range.location + range.length).min(all.len());
        all[start..end].to_vec()
    };
    let text = String::from_utf16_lossy(&units);
    let mut utf16_at = vec![0; text.len() + 1];
    let mut position = range.location;
    for (byte, c) in text.char_indices() {
        utf16_at[byte] = position;
        position += c.len_utf16();
    }
    utf16_at[text.len()] = position;
    Searched { text, utf16_at }
}

fn to_range(found: &Searched, start: usize, end: usize) -> Range {
    let a = found.utf16_at[start];
    Range { location: a, length: found.utf16_at[end] - a }
}

fn match_result(found: &Searched, captures: &regex::Captures) -> Id {
    let ranges: Vec<Range> = (0..captures.len())
        .map(|i| match captures.get(i) {
            Some(m) => to_range(found, m.start(), m.end()),
            None => Range { location: NOT_FOUND, length: 0 },
        })
        .collect();
    let object = runtime::alloc_instance(runtime::class_named("NSTextCheckingResult"));
    unsafe { *((object + 8) as *mut *const Vec<Range>) = Box::into_raw(Box::new(ranges)) };
    object
}

// Matching options (NSMatchingOptions).
const MATCH_ANCHORED: usize = 4;
const MATCH_TRANSPARENT_BOUNDS: usize = 8;
const MATCH_WITHOUT_ANCHORING_BOUNDS: usize = 16;

/// Every match, honouring the matching options. Anchored: each match must
/// start where the last one ended (the first at the start of the range).
fn all_matches<'t>(this: Id, found: &'t Searched, options: usize) -> Vec<regex::Captures<'t>> {
    if options & (MATCH_TRANSPARENT_BOUNDS | MATCH_WITHOUT_ANCHORING_BOUNDS) != 0 {
        crate::log!("regular expression: matching options {options:#x} (bounds) not supported; ^ and $ still match at the range edges");
    }
    let mut list = Vec::new();
    let mut expected = 0;
    for c in compiled(this).captures_iter(&found.text) {
        let whole = c.get(0).unwrap();
        if options & MATCH_ANCHORED != 0 {
            if whole.start() != expected {
                break;
            }
            expected = whole.end();
        }
        list.push(c);
    }
    list
}

extern "C" fn number_of_matches(this: Id, _: Sel, text: Id, options: usize, range: Range) -> usize {
    let found = searched(text, range);
    all_matches(this, &found, options).len()
}

extern "C" fn first_match(this: Id, _: Sel, text: Id, options: usize, range: Range) -> Id {
    let found = searched(text, range);
    match all_matches(this, &found, options).first() {
        Some(c) => match_result(&found, c),
        None => 0,
    }
}

extern "C" fn range_of_first_match(this: Id, _: Sel, text: Id, options: usize, range: Range) -> Range {
    let found = searched(text, range);
    match all_matches(this, &found, options).first().and_then(|c| c.get(0)) {
        Some(m) => to_range(&found, m.start(), m.end()),
        None => Range { location: NOT_FOUND, length: 0 },
    }
}

extern "C" fn matches(this: Id, _: Sel, text: Id, options: usize, range: Range) -> Id {
    let found = searched(text, range);
    let list: Vec<Id> = all_matches(this, &found, options).iter().map(|c| match_result(&found, c)).collect();
    collections::array_from(&list)
}

/// Apple templates use $1; regex uses ${1}. Backslash escapes stay literal.
fn template(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    if next == '$' {
                        out.push_str("$$");
                    } else {
                        out.push(next);
                    }
                }
            }
            '$' => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    digits.push(*d);
                    chars.next();
                }
                if digits.is_empty() {
                    out.push_str("$$");
                } else {
                    out += &format!("${{{digits}}}");
                }
            }
            other => out.push(other),
        }
    }
    out
}

extern "C" fn replacing_matches(this: Id, _: Sel, text: Id, options: usize, range: Range, with: Id) -> Id {
    let units: Vec<u16> = string::get(text).encode_utf16().collect();
    let start = range.location.min(units.len());
    let end = (range.location + range.length).min(units.len());
    let found = searched(text, range);
    let replacement = template(&string::get(with));
    let mut middle = String::new();
    let mut last = 0;
    for c in all_matches(this, &found, options) {
        let whole = c.get(0).unwrap();
        middle += &found.text[last..whole.start()];
        c.expand(&replacement, &mut middle);
        last = whole.end();
    }
    middle += &found.text[last..];
    let result = format!("{}{}{}", String::from_utf16_lossy(&units[..start]), middle, String::from_utf16_lossy(&units[end..]));
    string::make(&result)
}

extern "C" fn result_range(this: Id, _: Sel) -> Range {
    let ranges = unsafe { &**((this + 8) as *const *const Vec<Range>) };
    ranges.first().copied().unwrap_or(Range { location: NOT_FOUND, length: 0 })
}

extern "C" fn result_range_at(this: Id, _: Sel, index: usize) -> Range {
    let ranges = unsafe { &**((this + 8) as *const *const Vec<Range>) };
    ranges.get(index).copied().unwrap_or(Range { location: NOT_FOUND, length: 0 })
}

extern "C" fn result_number_of_ranges(this: Id, _: Sel) -> usize {
    unsafe { (**((this + 8) as *const *const Vec<Range>)).len() }
}

pub fn register(_: &mut Registry) {
    let class = define_class("NSRegularExpression", "NSObject", 16);
    class_methods(class, &[("regularExpressionWithPattern:options:error:", with_pattern as usize)]);
    methods(
        class,
        &[
            ("initWithPattern:options:error:", init_with_pattern as usize),
            ("numberOfMatchesInString:options:range:", number_of_matches as usize),
            ("firstMatchInString:options:range:", first_match as usize),
            ("rangeOfFirstMatchInString:options:range:", range_of_first_match as usize),
            ("matchesInString:options:range:", matches as usize),
            ("stringByReplacingMatchesInString:options:range:withTemplate:", replacing_matches as usize),
        ],
    );
    let result = define_class("NSTextCheckingResult", "NSObject", 16);
    methods(
        result,
        &[
            ("range", result_range as usize),
            ("rangeAtIndex:", result_range_at as usize),
            ("numberOfRanges", result_number_of_ranges as usize),
        ],
    );
}
