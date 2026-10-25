//! NSPredicate (NSComparisonPredicate, NSCompoundPredicate), NSExpression and
//! NSSortDescriptor: Apple's predicate format language parsed into objects
//! and evaluated against objects through key-value coding, and ordering by
//! key paths; plus the NSArray/NSSet methods that filter and sort with them.
//! Predicates and expressions: { isa, *mut Pred / *mut Expr }.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::cmp::Ordering;
use std::sync::OnceLock;

// NSPredicateOperatorType
const LESS: usize = 0;
const LESS_OR_EQUAL: usize = 1;
const GREATER: usize = 2;
const GREATER_OR_EQUAL: usize = 3;
const EQUAL: usize = 4;
const NOT_EQUAL: usize = 5;
const MATCHES: usize = 6;
const LIKE: usize = 7;
const BEGINS_WITH: usize = 8;
const ENDS_WITH: usize = 9;
const IN: usize = 10;
const CONTAINS: usize = 99;
const BETWEEN: usize = 100;
// NSComparisonPredicateModifier
const DIRECT: usize = 0;
const ALL: usize = 1;
const ANY: usize = 2;
// NSComparisonPredicateOptions
const CASE_INSENSITIVE: usize = 1;
const DIACRITIC_INSENSITIVE: usize = 2;
// NSCompoundPredicateType
const NOT: usize = 0;
const AND: usize = 1;
const OR: usize = 2;
// NSExpressionType
const CONSTANT_TYPE: usize = 0;
const EVALUATED_OBJECT_TYPE: usize = 1;
const VARIABLE_TYPE: usize = 2;
const KEY_PATH_TYPE: usize = 3;
const AGGREGATE_TYPE: usize = 14;

enum Pred {
    Value(bool),
    Comparison { left: Id, right: Id, op: usize, modifier: usize, options: usize },
    Compound { kind: usize, subs: Vec<Id> },
}

enum Expr {
    Constant(Id),
    Evaluated,
    Variable(String),
    KeyPath(String),
    Aggregate(Vec<Id>),
}

struct Classes {
    predicate: Class,
    comparison: Class,
    compound: Class,
    expression: Class,
    sort: Class,
}

static CLASSES: OnceLock<Classes> = OnceLock::new();

fn classes() -> &'static Classes {
    CLASSES.get().unwrap()
}

fn pred(object: Id) -> Option<&'static Pred> {
    unsafe { (*((object + 8) as *const *const Pred)).as_ref() }
}

fn expr(object: Id) -> Option<&'static Expr> {
    unsafe { (*((object + 8) as *const *const Expr)).as_ref() }
}

fn make_pred(value: Pred) -> Id {
    let class = match value {
        Pred::Value(_) => classes().predicate,
        Pred::Comparison { .. } => classes().comparison,
        Pred::Compound { .. } => classes().compound,
    };
    fill_pred(runtime::alloc_instance(class), value)
}

/// Gives `object` (a new predicate, maybe of a game subclass) its contents.
fn fill_pred(object: Id, value: Pred) -> Id {
    match &value {
        Pred::Comparison { left, right, .. } => [*left, *right].iter().for_each(|&o| crate::fw::counted::keep(o)),
        Pred::Compound { subs, .. } => subs.iter().for_each(|&o| crate::fw::counted::keep(o)),
        Pred::Value(_) => {}
    }
    unsafe { *((object + 8) as *mut *mut Pred) = Box::into_raw(Box::new(value)) };
    object
}

fn make_expr(value: Expr) -> Id {
    let object = runtime::alloc_instance(classes().expression);
    match &value {
        Expr::Constant(o) => crate::fw::counted::keep(*o),
        Expr::Aggregate(list) => list.iter().for_each(|&o| crate::fw::counted::keep(o)),
        _ => {}
    }
    unsafe { *((object + 8) as *mut *mut Expr) = Box::into_raw(Box::new(value)) };
    object
}

// ----- the format language -----

/// The arguments a format's %@, %K, %d ... take, in order.
enum Args<'a> {
    Slots(*const u64),
    List(&'a [Id]),
}

struct Parser<'a> {
    s: Vec<char>,
    i: usize,
    args: Args<'a>,
    next_arg: usize,
}

impl Parser<'_> {
    fn arg_word(&mut self) -> u64 {
        let index = self.next_arg;
        self.next_arg += 1;
        match &self.args {
            Args::Slots(va) if !va.is_null() => unsafe { *va.add(index) },
            Args::List(list) => list.get(index).copied().unwrap_or(0) as u64,
            _ => 0,
        }
    }

    /// An argument array holds objects only: a number conversion takes the number object.
    fn arg_number(&mut self, word: u64, make: impl FnOnce(u64) -> Id) -> Id {
        match self.args {
            Args::List(_) => word as Id,
            Args::Slots(_) => make(word),
        }
    }

    fn skip_space(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_whitespace()) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        let text: String = self.s.iter().collect();
        Err(format!("{what} at {} in \"{text}\"", self.i))
    }

    /// A word (letters, digits, _, $, @, #, .) without consuming it.
    fn word_at(&self, at: usize) -> String {
        self.s[at.min(self.s.len())..].iter().take_while(|c| c.is_alphanumeric() || **c == '_').collect()
    }

    /// Consumes `word` (any case) if it is the next whole word.
    fn keyword(&mut self, word: &str) -> bool {
        self.skip_space();
        let found = self.word_at(self.i);
        if found.eq_ignore_ascii_case(word) {
            self.i += found.chars().count();
            true
        } else {
            false
        }
    }

    fn symbol(&mut self, text: &str) -> bool {
        self.skip_space();
        let chars: Vec<char> = text.chars().collect();
        if self.s.get(self.i..self.i + chars.len()) == Some(&chars[..]) {
            self.i += chars.len();
            true
        } else {
            false
        }
    }

    fn predicate(&mut self) -> Result<Id, String> {
        let mut parts = vec![self.and()?];
        while self.keyword("OR") || self.symbol("||") {
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { make_pred(Pred::Compound { kind: OR, subs: parts }) })
    }

    fn and(&mut self) -> Result<Id, String> {
        let mut parts = vec![self.not()?];
        while self.keyword("AND") || self.symbol("&&") {
            parts.push(self.not()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { make_pred(Pred::Compound { kind: AND, subs: parts }) })
    }

    fn not(&mut self) -> Result<Id, String> {
        if self.keyword("NOT") || (self.peek_symbol("!") && !self.peek_symbol("!=") && self.symbol("!")) {
            let inner = self.not()?;
            return Ok(make_pred(Pred::Compound { kind: NOT, subs: vec![inner] }));
        }
        self.primary()
    }

    fn peek_symbol(&mut self, text: &str) -> bool {
        self.skip_space();
        let chars: Vec<char> = text.chars().collect();
        self.s.get(self.i..self.i + chars.len()) == Some(&chars[..])
    }

    fn primary(&mut self) -> Result<Id, String> {
        self.skip_space();
        if self.peek() == Some('(') {
            // A parenthesized predicate, unless it is an expression such as ( 1 ) == 1.
            let save = (self.i, self.next_arg);
            self.i += 1;
            if let Ok(inner) = self.predicate() {
                if self.symbol(")") {
                    return Ok(inner);
                }
            }
            (self.i, self.next_arg) = save;
        }
        if self.keyword("TRUEPREDICATE") {
            return Ok(make_pred(Pred::Value(true)));
        }
        if self.keyword("FALSEPREDICATE") {
            return Ok(make_pred(Pred::Value(false)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Id, String> {
        let (mut modifier, mut negate) = (DIRECT, false);
        if self.keyword("ANY") || self.keyword("SOME") {
            modifier = ANY;
        } else if self.keyword("ALL") {
            modifier = ALL;
        } else if self.keyword("NONE") {
            (modifier, negate) = (ANY, true);
        }
        let left = self.expression()?;
        let op = self.operator()?;
        let options = self.options();
        let right = self.expression()?;
        let made = make_pred(Pred::Comparison { left, right, op, modifier, options });
        // NONE x == y is NOT (ANY x == y), as Apple reads it.
        Ok(if negate { make_pred(Pred::Compound { kind: NOT, subs: vec![made] }) } else { made })
    }

    fn operator(&mut self) -> Result<usize, String> {
        for (text, op) in [
            ("==", EQUAL),
            ("!=", NOT_EQUAL),
            ("<>", NOT_EQUAL),
            ("<=", LESS_OR_EQUAL),
            ("=<", LESS_OR_EQUAL),
            (">=", GREATER_OR_EQUAL),
            ("=>", GREATER_OR_EQUAL),
            ("=", EQUAL),
            ("<", LESS),
            (">", GREATER),
        ] {
            if self.symbol(text) {
                return Ok(op);
            }
        }
        for (word, op) in [
            ("BEGINSWITH", BEGINS_WITH),
            ("ENDSWITH", ENDS_WITH),
            ("CONTAINS", CONTAINS),
            ("LIKE", LIKE),
            ("MATCHES", MATCHES),
            ("IN", IN),
            ("BETWEEN", BETWEEN),
        ] {
            if self.keyword(word) {
                return Ok(op);
            }
        }
        self.fail("comparison operator expected")
    }

    /// [c], [d], [cd], [n] after an operator.
    fn options(&mut self) -> usize {
        self.skip_space();
        if self.peek() != Some('[') {
            return 0;
        }
        let start = self.i;
        self.i += 1;
        let mut options = 0;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                'c' | 'C' => options |= CASE_INSENSITIVE,
                'd' | 'D' => options |= DIACRITIC_INSENSITIVE,
                'n' | 'N' => options |= 4,
                ']' => return options,
                _ => break,
            }
        }
        self.i = start;
        0
    }

    fn expression(&mut self) -> Result<Id, String> {
        self.skip_space();
        let Some(c) = self.peek() else { return self.fail("value expected") };
        if c == '%' {
            self.i += 1;
            let mut longs = 0;
            while let Some(m) = self.peek().filter(|m| matches!(m, 'l' | 'q' | 'h' | 'z' | 't' | 'j')) {
                if matches!(m, 'l' | 'q' | 'j') {
                    longs += 1;
                }
                self.i += 1;
            }
            let Some(conversion) = self.peek() else { return self.fail("format conversion expected") };
            self.i += 1;
            let word = self.arg_word();
            return Ok(match conversion {
                '@' => make_expr(Expr::Constant(word as Id)),
                'K' => make_expr(Expr::KeyPath(string::get(word as Id))),
                'd' | 'i' | 'c' => {
                    let value = if longs > 0 { word as i64 } else { word as i32 as i64 };
                    make_expr(Expr::Constant(self.arg_number(word, |_| collections::number_int(value))))
                }
                'u' | 'x' | 'X' | 'o' => {
                    let value = if longs > 0 { word as i64 } else { word as u32 as i64 };
                    make_expr(Expr::Constant(self.arg_number(word, |_| collections::number_int(value))))
                }
                'f' | 'e' | 'g' | 'F' | 'E' | 'G' => {
                    make_expr(Expr::Constant(self.arg_number(word, |w| collections::number_float(f64::from_bits(w)))))
                }
                's' => make_expr(Expr::Constant(if word == 0 { 0 } else { string::make(&unsafe { runtime::cstr(word as usize) }) })),
                other => return self.fail(&format!("format conversion %{other} is not supported in predicates")),
            });
        }
        if c == '"' || c == '\'' {
            self.i += 1;
            let mut text = String::new();
            loop {
                match self.peek() {
                    None => return self.fail("unclosed string"),
                    Some(q) if q == c => {
                        self.i += 1;
                        break;
                    }
                    Some('\\') => {
                        self.i += 1;
                        let escaped = self.peek().unwrap_or('\\');
                        self.i += 1;
                        text.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '0' => '\0',
                            other => other,
                        });
                    }
                    Some(other) => {
                        self.i += 1;
                        text.push(other);
                    }
                }
            }
            return Ok(make_expr(Expr::Constant(string::make(&text))));
        }
        if c == '{' {
            self.i += 1;
            let mut list = Vec::new();
            if !self.symbol("}") {
                loop {
                    list.push(self.expression()?);
                    if self.symbol("}") {
                        break;
                    }
                    if !self.symbol(",") {
                        return self.fail("',' or '}' expected");
                    }
                }
            }
            return Ok(make_expr(Expr::Aggregate(list)));
        }
        if c == '$' {
            self.i += 1;
            let name = self.word_at(self.i);
            self.i += name.chars().count();
            return Ok(make_expr(Expr::Variable(name)));
        }
        if c.is_ascii_digit() || ((c == '-' || c == '+') && self.s.get(self.i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '.')) || c == '.' {
            return self.number();
        }
        if c == '(' {
            self.i += 1;
            let inner = self.expression()?;
            if !self.symbol(")") {
                return self.fail("')' expected");
            }
            return Ok(inner);
        }
        // Words: constants, SELF, or a key path (a # in front escapes a reserved word).
        let escaped = c == '#';
        if escaped {
            self.i += 1;
        }
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '@')) {
            self.i += 1;
        }
        let word: String = self.s[start..self.i].iter().collect();
        if word.is_empty() {
            return self.fail("value expected");
        }
        if !escaped {
            match word.to_ascii_uppercase().as_str() {
                "TRUE" | "YES" => return Ok(make_expr(Expr::Constant(collections::number_bool(true)))),
                "FALSE" | "NO" => return Ok(make_expr(Expr::Constant(collections::number_bool(false)))),
                "NIL" | "NULL" => return Ok(make_expr(Expr::Constant(0))),
                "SELF" => return Ok(make_expr(Expr::Evaluated)),
                _ => {}
            }
            if let Some(rest) = word.strip_prefix("SELF.").or_else(|| word.strip_prefix("self.")) {
                return Ok(make_expr(Expr::KeyPath(rest.to_string())));
            }
        }
        Ok(make_expr(Expr::KeyPath(word)))
    }

    fn number(&mut self) -> Result<Id, String> {
        let start = self.i;
        if matches!(self.peek(), Some('-' | '+')) {
            self.i += 1;
        }
        let text_from = |p: &Parser, from: usize| -> String { p.s[from..p.i].iter().collect() };
        if self.peek() == Some('0') && matches!(self.s.get(self.i + 1), Some('x' | 'X')) {
            self.i += 2;
            let digits = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.i += 1;
            }
            let value = i64::from_str_radix(&text_from(self, digits), 16).unwrap_or(0);
            let negative = self.s[start] == '-';
            return Ok(make_expr(Expr::Constant(collections::number_int(if negative { -value } else { value }))));
        }
        let mut float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == '.' || ((c == 'e' || c == 'E') && self.i > start) {
                float = true;
                self.i += 1;
                if matches!(self.peek(), Some('-' | '+')) && (c == 'e' || c == 'E') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
        let text = text_from(self, start);
        if float {
            match text.parse::<f64>() {
                Ok(value) => Ok(make_expr(Expr::Constant(collections::number_float(value)))),
                Err(_) => self.fail("number expected"),
            }
        } else {
            match text.parse::<i64>() {
                Ok(value) => Ok(make_expr(Expr::Constant(collections::number_int(value)))),
                Err(_) => self.fail("number expected"),
            }
        }
    }
}

fn parse(format: &str, args: Args) -> Result<Id, String> {
    let mut p = Parser { s: format.chars().collect(), i: 0, args, next_arg: 0 };
    let result = p.predicate()?;
    p.skip_space();
    if p.i != p.s.len() {
        return p.fail("unexpected text");
    }
    Ok(result)
}

/// A predicate from a format and its arguments (8-byte slots), as
/// +predicateWithFormat: makes; nil (logged) for a format that can't be read.
pub fn from_format(format: &str, va: *const u64) -> Id {
    match parse(format, Args::Slots(va)) {
        Ok(p) => p,
        Err(error) => {
            crate::log!("NSPredicate: can't read the format: {error}; nil");
            0
        }
    }
}

crate::libsystem::libc::variadic!(ios_predicate_with_format, 3, ios_predicate_with_format_impl);

#[no_mangle]
extern "C" fn ios_predicate_with_format_impl(_: Class, _: Sel, format: Id, va: *const u64) -> Id {
    from_format(&string::get(format), va)
}

/// +predicateWithFormat:arguments: (an arm64 va_list points at the same slots).
extern "C" fn predicate_with_format_arguments(_: Class, _: Sel, format: Id, va: *const u64) -> Id {
    from_format(&string::get(format), va)
}

extern "C" fn predicate_with_format_array(_: Class, _: Sel, format: Id, list: Id) -> Id {
    let items = collections::array_items(list);
    match parse(&string::get(format), Args::List(&items)) {
        Ok(p) => p,
        Err(error) => {
            crate::log!("NSPredicate: can't read the format: {error}; nil");
            0
        }
    }
}

extern "C" fn predicate_with_value(_: Class, _: Sel, value: u8) -> Id {
    make_pred(Pred::Value(value != 0))
}

// ----- evaluating -----

fn key_path_value(object: Id, path: &str) -> Id {
    if object == 0 {
        return 0;
    }
    runtime::send(object, "valueForKeyPath:", &[string::make(path)])
}

fn null() -> Id {
    runtime::send(runtime::class_named("NSNull"), "null", &[])
}

fn value_of(e: Id, object: Id, variables: Id) -> Id {
    match expr(e) {
        Some(Expr::Constant(value)) => *value,
        Some(Expr::Evaluated) => object,
        Some(Expr::KeyPath(path)) => key_path_value(object, path),
        Some(Expr::Variable(name)) => collections::dictionary_get(variables, string::make(name)),
        Some(Expr::Aggregate(list)) => collections::array_from(&list.iter().map(|&x| value_of(x, object, variables)).collect::<Vec<_>>()),
        None => 0,
    }
}

/// The members of a collection value (arrays, sets, a dictionary's values).
fn members(value: Id) -> Option<Vec<Id>> {
    if value == 0 {
        return None;
    }
    if collections::is_array(value) {
        return Some(collections::array_items(value));
    }
    if collections::is_dictionary(value) {
        return Some(collections::dictionary_pairs(value).into_iter().map(|(_, v)| v).collect());
    }
    let set = runtime::class_named("NSSet");
    if set != 0 && runtime::send(value, "isKindOfClass:", &[set]) & 0xFF != 0 {
        return Some(collections::array_items(runtime::send(value, "allObjects", &[])));
    }
    None
}

fn fold(text: &str, options: usize) -> String {
    let mut out: String = if options & DIACRITIC_INSENSITIVE != 0 { text.chars().map(strip_diacritic).collect() } else { text.to_string() };
    if options & CASE_INSENSITIVE != 0 {
        out = out.to_lowercase();
    }
    out
}

/// Latin letters with accents to their plain letter (enough for [d]).
fn strip_diacritic(c: char) -> char {
    const FROM: &str = "àáâãäåāăąçćčďèéêëēėęěìíîïīįłñńňòóôõöøōőŕřśšşťùúûüūůűųýÿźżžÀÁÂÃÄÅĀĂĄÇĆČĎÈÉÊËĒĖĘĚÌÍÎÏĪĮŁÑŃŇÒÓÔÕÖØŌŐŔŘŚŠŞŤÙÚÛÜŪŮŰŲÝŸŹŻŽ";
    const TO: &str = "aaaaaaaaacccdeeeeeeeeiiiiiilnnnoooooooorrsssstuuuuuuuuyyzzzAAAAAAAAACCCDEEEEEEEEIIIIIILNNNOOOOOOOORRSSSTUUUUUUUUYYZZZ";
    FROM.chars().position(|f| f == c).and_then(|i| TO.chars().nth(i)).unwrap_or(c)
}

fn is_null_or_nil(value: Id) -> bool {
    value == 0 || value == null()
}

/// Orders two values the way NSComparisonPredicate does: numbers by value,
/// strings by their characters (folded by the options), anything else by
/// its -compare:. None when they can't be ordered.
pub fn compare_values(a: Id, b: Id, options: usize) -> Option<Ordering> {
    if a == 0 || b == 0 {
        return None;
    }
    if collections::is_number(a) && collections::is_number(b) {
        let (x, y) = (collections::number_as_f64(a), collections::number_as_f64(b));
        if x.fract() == 0.0 && y.fract() == 0.0 && x.abs() < 9.0e15 && y.abs() < 9.0e15 {
            return Some(collections::number_as_i64(a).cmp(&collections::number_as_i64(b)));
        }
        return x.partial_cmp(&y);
    }
    if string::is_string(a) && string::is_string(b) {
        return Some(fold(&string::get(a), options).cmp(&fold(&string::get(b), options)));
    }
    if runtime::responds(a, "compare:") {
        return Some((runtime::send(a, "compare:", &[b]) as isize).cmp(&0));
    }
    None
}

fn equal(a: Id, b: Id, options: usize) -> bool {
    if is_null_or_nil(a) || is_null_or_nil(b) {
        return is_null_or_nil(a) && is_null_or_nil(b);
    }
    if string::is_string(a) && string::is_string(b) {
        return fold(&string::get(a), options) == fold(&string::get(b), options);
    }
    if collections::is_number(a) && collections::is_number(b) {
        return collections::number_as_f64(a) == collections::number_as_f64(b);
    }
    a == b || runtime::send(a, "isEqual:", &[b]) & 0xFF != 0
}

/// LIKE: * any run of characters, ? exactly one.
fn like(text: &[char], pattern: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|skip| like(&text[skip..], rest)),
        Some(('?', rest)) => !text.is_empty() && like(&text[1..], rest),
        Some(('\\', rest)) if !rest.is_empty() => text.first() == rest.first() && like(&text[1..], &rest[1..]),
        Some((c, rest)) => text.first() == Some(c) && like(&text[1..], rest),
    }
}

fn compare_one(left: Id, right: Id, op: usize, options: usize) -> bool {
    let text = |value: Id| string::is_string(value).then(|| fold(&string::get(value), options));
    match op {
        EQUAL => equal(left, right, options),
        NOT_EQUAL => !equal(left, right, options),
        LESS => compare_values(left, right, options) == Some(Ordering::Less),
        LESS_OR_EQUAL => matches!(compare_values(left, right, options), Some(Ordering::Less | Ordering::Equal)),
        GREATER => compare_values(left, right, options) == Some(Ordering::Greater),
        GREATER_OR_EQUAL => matches!(compare_values(left, right, options), Some(Ordering::Greater | Ordering::Equal)),
        BEGINS_WITH => matches!((text(left), text(right)), (Some(a), Some(b)) if a.starts_with(&b)),
        ENDS_WITH => matches!((text(left), text(right)), (Some(a), Some(b)) if a.ends_with(&b)),
        LIKE => match (text(left), text(right)) {
            (Some(a), Some(b)) => like(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>()),
            _ => false,
        },
        MATCHES => match (text(left), text(right)) {
            (Some(a), Some(b)) => regex::RegexBuilder::new(&format!("^(?:{b})$"))
                .case_insensitive(options & CASE_INSENSITIVE != 0)
                .build()
                .is_ok_and(|r| r.is_match(&a)),
            _ => false,
        },
        CONTAINS => match members(left) {
            Some(list) => list.iter().any(|&item| equal(item, right, options)),
            None => matches!((text(left), text(right)), (Some(a), Some(b)) if a.contains(&b)),
        },
        IN => match members(right) {
            Some(list) => list.iter().any(|&item| equal(left, item, options)),
            None => matches!((text(left), text(right)), (Some(a), Some(b)) if b.contains(&a)),
        },
        BETWEEN => match members(right).as_deref() {
            Some([low, high]) => {
                matches!(compare_values(left, *low, options), Some(Ordering::Greater | Ordering::Equal))
                    && matches!(compare_values(left, *high, options), Some(Ordering::Less | Ordering::Equal))
            }
            _ => false,
        },
        _ => false,
    }
}

fn evaluate(p: Id, object: Id, variables: Id) -> bool {
    match pred(p) {
        Some(Pred::Value(value)) => *value,
        Some(Pred::Compound { kind, subs }) => match *kind {
            AND => subs.iter().all(|&s| evaluate(s, object, variables)),
            OR => subs.iter().any(|&s| evaluate(s, object, variables)),
            _ => !subs.first().is_some_and(|&s| evaluate(s, object, variables)),
        },
        Some(Pred::Comparison { left, right, op, modifier, options }) => {
            let (l, r) = (value_of(*left, object, variables), value_of(*right, object, variables));
            match *modifier {
                ANY => members(l).is_some_and(|list| list.iter().any(|&item| compare_one(item, r, *op, *options))),
                ALL => members(l).is_some_and(|list| list.iter().all(|&item| compare_one(item, r, *op, *options))),
                _ => compare_one(l, r, *op, *options),
            }
        }
        None => runtime::send(p, "evaluateWithObject:substitutionVariables:", &[object, variables]) & 0xFF != 0,
    }
}

/// Whether `object` matches predicate `p` (any NSPredicate, the game's own too).
pub fn matches(p: Id, object: Id) -> bool {
    if pred(p).is_some() { evaluate(p, object, 0) } else { runtime::send(p, "evaluateWithObject:", &[object]) & 0xFF != 0 }
}

extern "C" fn evaluate_with_object(this: Id, _: Sel, object: Id) -> bool {
    evaluate(this, object, 0)
}

extern "C" fn evaluate_with_variables(this: Id, _: Sel, object: Id, variables: Id) -> bool {
    evaluate(this, object, variables)
}

// ----- reading predicates and expressions back -----

extern "C" fn left_expression(this: Id, _: Sel) -> Id {
    match pred(this) {
        Some(Pred::Comparison { left, .. }) => *left,
        _ => 0,
    }
}

extern "C" fn right_expression(this: Id, _: Sel) -> Id {
    match pred(this) {
        Some(Pred::Comparison { right, .. }) => *right,
        _ => 0,
    }
}

extern "C" fn operator_type(this: Id, _: Sel) -> usize {
    match pred(this) {
        Some(Pred::Comparison { op, .. }) => *op,
        _ => 0,
    }
}

extern "C" fn comparison_modifier(this: Id, _: Sel) -> usize {
    match pred(this) {
        Some(Pred::Comparison { modifier, .. }) => *modifier,
        _ => 0,
    }
}

extern "C" fn comparison_options(this: Id, _: Sel) -> usize {
    match pred(this) {
        Some(Pred::Comparison { options, .. }) => *options,
        _ => 0,
    }
}

extern "C" fn compound_type(this: Id, _: Sel) -> usize {
    match pred(this) {
        Some(Pred::Compound { kind, .. }) => *kind,
        _ => 0,
    }
}

extern "C" fn subpredicates(this: Id, _: Sel) -> Id {
    match pred(this) {
        Some(Pred::Compound { subs, .. }) => collections::array_from(subs),
        _ => 0,
    }
}

extern "C" fn comparison_init(this: Id, _: Sel, left: Id, right: Id, modifier: usize, op: usize, options: usize) -> Id {
    fill_pred(this, Pred::Comparison { left, right, op, modifier, options })
}

extern "C" fn comparison_with(_: Class, _: Sel, left: Id, right: Id, modifier: usize, op: usize, options: usize) -> Id {
    make_pred(Pred::Comparison { left, right, op, modifier, options })
}

extern "C" fn compound_init(this: Id, _: Sel, kind: usize, list: Id) -> Id {
    fill_pred(this, Pred::Compound { kind, subs: collections::array_items(list) })
}

extern "C" fn compound_and(_: Class, _: Sel, list: Id) -> Id {
    make_pred(Pred::Compound { kind: AND, subs: collections::array_items(list) })
}

extern "C" fn compound_or(_: Class, _: Sel, list: Id) -> Id {
    make_pred(Pred::Compound { kind: OR, subs: collections::array_items(list) })
}

extern "C" fn compound_not(_: Class, _: Sel, inner: Id) -> Id {
    make_pred(Pred::Compound { kind: NOT, subs: vec![inner] })
}

extern "C" fn expression_type(this: Id, _: Sel) -> usize {
    match expr(this) {
        Some(Expr::Constant(_)) | None => CONSTANT_TYPE,
        Some(Expr::Evaluated) => EVALUATED_OBJECT_TYPE,
        Some(Expr::Variable(_)) => VARIABLE_TYPE,
        Some(Expr::KeyPath(_)) => KEY_PATH_TYPE,
        Some(Expr::Aggregate(_)) => AGGREGATE_TYPE,
    }
}

extern "C" fn expression_constant(this: Id, _: Sel) -> Id {
    match expr(this) {
        Some(Expr::Constant(value)) => *value,
        Some(Expr::Aggregate(list)) => collections::array_from(list),
        _ => 0,
    }
}

extern "C" fn expression_key_path(this: Id, _: Sel) -> Id {
    match expr(this) {
        Some(Expr::KeyPath(path)) => string::make(path),
        _ => 0,
    }
}

extern "C" fn expression_variable(this: Id, _: Sel) -> Id {
    match expr(this) {
        Some(Expr::Variable(name)) => string::make(name),
        _ => 0,
    }
}

extern "C" fn expression_value(this: Id, _: Sel, object: Id, _context: Id) -> Id {
    value_of(this, object, 0)
}

extern "C" fn expression_for_constant(_: Class, _: Sel, value: Id) -> Id {
    make_expr(Expr::Constant(value))
}

extern "C" fn expression_for_key_path(_: Class, _: Sel, path: Id) -> Id {
    make_expr(Expr::KeyPath(string::get(path)))
}

extern "C" fn expression_for_evaluated(_: Class, _: Sel) -> Id {
    make_expr(Expr::Evaluated)
}

extern "C" fn expression_for_variable(_: Class, _: Sel, name: Id) -> Id {
    make_expr(Expr::Variable(string::get(name)))
}

fn expression_text(e: Id) -> String {
    match expr(e) {
        Some(Expr::Constant(value)) if string::is_string(*value) => format!("{:?}", string::get(*value)),
        Some(Expr::Constant(0)) => "nil".into(),
        Some(Expr::Constant(value)) => string::describe(*value),
        Some(Expr::Evaluated) => "SELF".into(),
        Some(Expr::Variable(name)) => format!("${name}"),
        Some(Expr::KeyPath(path)) => path.clone(),
        Some(Expr::Aggregate(list)) => format!("{{{}}}", list.iter().map(|&x| expression_text(x)).collect::<Vec<_>>().join(", ")),
        None => string::describe(e),
    }
}

fn predicate_text(p: Id) -> String {
    match pred(p) {
        Some(Pred::Value(true)) => "TRUEPREDICATE".into(),
        Some(Pred::Value(false)) => "FALSEPREDICATE".into(),
        Some(Pred::Compound { kind, subs }) => {
            let parts: Vec<String> = subs.iter().map(|&s| if matches!(pred(s), Some(Pred::Compound { .. })) { format!("({})", predicate_text(s)) } else { predicate_text(s) }).collect();
            match *kind {
                AND => parts.join(" AND "),
                OR => parts.join(" OR "),
                _ => format!("NOT {}", parts.first().cloned().unwrap_or_default()),
            }
        }
        Some(Pred::Comparison { left, right, op, modifier, options }) => {
            let name = match *op {
                LESS => "<",
                LESS_OR_EQUAL => "<=",
                GREATER => ">",
                GREATER_OR_EQUAL => ">=",
                EQUAL => "==",
                NOT_EQUAL => "!=",
                MATCHES => "MATCHES",
                LIKE => "LIKE",
                BEGINS_WITH => "BEGINSWITH",
                ENDS_WITH => "ENDSWITH",
                IN => "IN",
                CONTAINS => "CONTAINS",
                BETWEEN => "BETWEEN",
                _ => "?",
            };
            let flags = match *options & 3 {
                0 => "",
                CASE_INSENSITIVE => "[c]",
                DIACRITIC_INSENSITIVE => "[d]",
                _ => "[cd]",
            };
            let prefix = match *modifier {
                ANY => "ANY ",
                ALL => "ALL ",
                _ => "",
            };
            format!("{prefix}{} {name}{flags} {}", expression_text(*left), expression_text(*right))
        }
        None => string::describe(p),
    }
}

extern "C" fn predicate_format(this: Id, _: Sel) -> Id {
    string::make(&predicate_text(this))
}

extern "C" fn expression_description(this: Id, _: Sel) -> Id {
    string::make(&expression_text(this))
}

extern "C" fn return_self(this: Id, _: Sel, _zone: usize) -> Id {
    this
}

// ----- filtering collections -----

extern "C" fn array_filtered(this: Id, _: Sel, p: Id) -> Id {
    let kept: Vec<Id> = collections::array_items(this).into_iter().filter(|&item| matches(p, item)).collect();
    collections::array_from(&kept)
}

extern "C" fn array_filter(this: Id, _: Sel, p: Id) {
    let kept: Vec<Id> = collections::array_items(this).into_iter().filter(|&item| matches(p, item)).collect();
    collections::set_array_items(this, kept);
}

extern "C" fn set_filtered(this: Id, _: Sel, p: Id) -> Id {
    let all = collections::array_items(runtime::send(this, "allObjects", &[]));
    crate::fw::misc::set_from(&all.into_iter().filter(|&item| matches(p, item)).collect::<Vec<_>>())
}

// ----- NSSortDescriptor: { isa, *mut Sort } -----

struct Sort {
    key: Id,
    ascending: bool,
    selector: Sel,
    comparator: usize,
}

fn sort(object: Id) -> Option<&'static Sort> {
    unsafe { (*((object + 8) as *const *const Sort)).as_ref() }
}

fn set_sort(object: Id, key: Id, ascending: u8, selector: Sel, comparator: usize) -> Id {
    let key = if key == 0 { 0 } else { runtime::send(key, "copy", &[]) };
    crate::fw::counted::keep(key);
    let comparator = if comparator == 0 { 0 } else { crate::libsystem::mach::block_copy(comparator as *mut _) as usize };
    let state = Sort { key, ascending: ascending != 0, selector, comparator };
    unsafe { *((object + 8) as *mut *mut Sort) = Box::into_raw(Box::new(state)) };
    object
}

extern "C" fn sort_init(this: Id, _: Sel, key: Id, ascending: u8) -> Id {
    set_sort(this, key, ascending, 0, 0)
}

extern "C" fn sort_init_selector(this: Id, _: Sel, key: Id, ascending: u8, selector: Sel) -> Id {
    set_sort(this, key, ascending, selector, 0)
}

extern "C" fn sort_init_comparator(this: Id, _: Sel, key: Id, ascending: u8, comparator: usize) -> Id {
    set_sort(this, key, ascending, 0, comparator)
}

extern "C" fn sort_with(class: Class, _: Sel, key: Id, ascending: u8) -> Id {
    set_sort(runtime::alloc_instance(class), key, ascending, 0, 0)
}

extern "C" fn sort_with_selector(class: Class, _: Sel, key: Id, ascending: u8, selector: Sel) -> Id {
    set_sort(runtime::alloc_instance(class), key, ascending, selector, 0)
}

extern "C" fn sort_with_comparator(class: Class, _: Sel, key: Id, ascending: u8, comparator: usize) -> Id {
    set_sort(runtime::alloc_instance(class), key, ascending, 0, comparator)
}

extern "C" fn sort_key(this: Id, _: Sel) -> Id {
    sort(this).map_or(0, |s| s.key)
}

extern "C" fn sort_ascending(this: Id, _: Sel) -> bool {
    sort(this).is_none_or(|s| s.ascending)
}

extern "C" fn sort_selector(this: Id, _: Sel) -> Sel {
    sort(this).map_or(0, |s| s.selector)
}

extern "C" fn sort_reversed(this: Id, _: Sel) -> Id {
    match sort(this) {
        Some(s) => set_sort(runtime::alloc_instance(classes().sort), s.key, (!s.ascending) as u8, s.selector, s.comparator),
        None => this,
    }
}

/// Orders two objects by one descriptor: their values at its key path,
/// compared with its comparator, its selector or -compare:. nil sorts first.
fn compare_by(descriptor: Id, a: Id, b: Id) -> Ordering {
    let Some(s) = sort(descriptor) else {
        return (runtime::send(descriptor, "compareObject:toObject:", &[a, b]) as isize).cmp(&0);
    };
    let (x, y) = if s.key == 0 { (a, b) } else { (key_path_value(a, &string::get(s.key)), key_path_value(b, &string::get(s.key))) };
    let order = match (x, y) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Less,
        (_, 0) => Ordering::Greater,
        _ if s.comparator != 0 => {
            let header = s.comparator as *const crate::libsystem::mach::BlockHeader;
            let invoke: extern "C" fn(usize, Id, Id) -> isize = unsafe { std::mem::transmute((*header).invoke) };
            invoke(s.comparator, x, y).cmp(&0)
        }
        _ if s.selector != 0 => {
            let name = lock().sel_name(s.selector);
            (runtime::send(x, &name, &[y]) as isize).cmp(&0)
        }
        _ => compare_values(x, y, 0).unwrap_or(Ordering::Equal),
    };
    if s.ascending { order } else { order.reverse() }
}

extern "C" fn sort_compare(this: Id, _: Sel, a: Id, b: Id) -> isize {
    match compare_by(this, a, b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `list` ordered by the descriptors in turn (a stable sort, as Apple's).
pub fn sorted_by(mut list: Vec<Id>, descriptors: Id) -> Vec<Id> {
    let descriptors = collections::array_items(descriptors);
    list.sort_by(|&a, &b| descriptors.iter().map(|&d| compare_by(d, a, b)).find(|o| *o != Ordering::Equal).unwrap_or(Ordering::Equal));
    list
}

extern "C" fn array_sorted_descriptors(this: Id, _: Sel, descriptors: Id) -> Id {
    collections::array_from(&sorted_by(collections::array_items(this), descriptors))
}

extern "C" fn array_sort_descriptors(this: Id, _: Sel, descriptors: Id) {
    let sorted = sorted_by(collections::array_items(this), descriptors);
    collections::set_array_items(this, sorted);
}

extern "C" fn set_sorted_descriptors(this: Id, _: Sel, descriptors: Id) -> Id {
    collections::array_from(&sorted_by(collections::array_items(runtime::send(this, "allObjects", &[])), descriptors))
}

pub fn register(_: &mut Registry) {
    let predicate = define_class("NSPredicate", "NSObject", 16);
    let comparison = define_class("NSComparisonPredicate", "NSPredicate", 16);
    let compound = define_class("NSCompoundPredicate", "NSPredicate", 16);
    let expression = define_class("NSExpression", "NSObject", 16);
    let sort = define_class("NSSortDescriptor", "NSObject", 16);
    CLASSES.set(Classes { predicate, comparison, compound, expression, sort }).ok();
    class_methods(
        predicate,
        &[
            ("predicateWithFormat:", ios_predicate_with_format as usize),
            ("predicateWithFormat:arguments:", predicate_with_format_arguments as usize),
            ("predicateWithFormat:argumentArray:", predicate_with_format_array as usize),
            ("predicateWithValue:", predicate_with_value as usize),
        ],
    );
    methods(
        predicate,
        &[
            ("evaluateWithObject:", evaluate_with_object as usize),
            ("evaluateWithObject:substitutionVariables:", evaluate_with_variables as usize),
            ("predicateFormat", predicate_format as usize),
            ("description", predicate_format as usize),
            ("copyWithZone:", return_self as usize),
        ],
    );
    class_methods(
        comparison,
        &[("predicateWithLeftExpression:rightExpression:modifier:type:options:", comparison_with as usize)],
    );
    methods(
        comparison,
        &[
            ("initWithLeftExpression:rightExpression:modifier:type:options:", comparison_init as usize),
            ("leftExpression", left_expression as usize),
            ("rightExpression", right_expression as usize),
            ("predicateOperatorType", operator_type as usize),
            ("comparisonPredicateModifier", comparison_modifier as usize),
            ("options", comparison_options as usize),
        ],
    );
    class_methods(
        compound,
        &[
            ("andPredicateWithSubpredicates:", compound_and as usize),
            ("orPredicateWithSubpredicates:", compound_or as usize),
            ("notPredicateWithSubpredicate:", compound_not as usize),
        ],
    );
    methods(
        compound,
        &[
            ("initWithType:subpredicates:", compound_init as usize),
            ("compoundPredicateType", compound_type as usize),
            ("subpredicates", subpredicates as usize),
        ],
    );
    class_methods(
        expression,
        &[
            ("expressionForConstantValue:", expression_for_constant as usize),
            ("expressionForKeyPath:", expression_for_key_path as usize),
            ("expressionForEvaluatedObject", expression_for_evaluated as usize),
            ("expressionForVariable:", expression_for_variable as usize),
        ],
    );
    methods(
        expression,
        &[
            ("expressionType", expression_type as usize),
            ("constantValue", expression_constant as usize),
            ("keyPath", expression_key_path as usize),
            ("variable", expression_variable as usize),
            ("expressionValueWithObject:context:", expression_value as usize),
            ("description", expression_description as usize),
            ("copyWithZone:", return_self as usize),
        ],
    );
    class_methods(
        sort,
        &[
            ("sortDescriptorWithKey:ascending:", sort_with as usize),
            ("sortDescriptorWithKey:ascending:selector:", sort_with_selector as usize),
            ("sortDescriptorWithKey:ascending:comparator:", sort_with_comparator as usize),
        ],
    );
    methods(
        sort,
        &[
            ("initWithKey:ascending:", sort_init as usize),
            ("initWithKey:ascending:selector:", sort_init_selector as usize),
            ("initWithKey:ascending:comparator:", sort_init_comparator as usize),
            ("key", sort_key as usize),
            ("ascending", sort_ascending as usize),
            ("selector", sort_selector as usize),
            ("reversedSortDescriptor", sort_reversed as usize),
            ("compareObject:toObject:", sort_compare as usize),
            ("copyWithZone:", return_self as usize),
        ],
    );
    let array = runtime::class_named("NSArray");
    methods(
        array,
        &[("filteredArrayUsingPredicate:", array_filtered as usize), ("sortedArrayUsingDescriptors:", array_sorted_descriptors as usize)],
    );
    methods(
        runtime::class_named("NSMutableArray"),
        &[("filterUsingPredicate:", array_filter as usize), ("sortUsingDescriptors:", array_sort_descriptors as usize)],
    );
    methods(
        runtime::class_named("NSSet"),
        &[("filteredSetUsingPredicate:", set_filtered as usize), ("sortedArrayUsingDescriptors:", set_sorted_descriptors as usize)],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn s(text: &str) -> Id {
        string::make(text)
    }

    fn dict(pairs: &[(&str, Id)]) -> Id {
        collections::dictionary_from(&pairs.iter().map(|(k, v)| (s(k), *v)).collect::<Vec<_>>())
    }

    /// Apple's predicate formats, with %K/%@/%d arguments, parsed and
    /// evaluated through key paths, and filtering/sorting arrays with them.
    #[test]
    fn predicate_formats_parse_and_evaluate() {
        ready();
        let n = collections::number_int;
        let hero = dict(&[
            ("name", s("Siris")),
            ("level", n(7)),
            ("gold", collections::number_float(12.5)),
            ("pet", dict(&[("kind", s("Dragon"))])),
            ("tags", collections::array_from(&[s("fire"), s("earth")])),
        ]);
        let check = |format: &str, args: &[u64], want: bool| {
            let p = from_format(format, args.as_ptr());
            assert_ne!(p, 0, "{format} did not parse");
            assert_eq!(matches(p, hero), want, "{format} (read back as {})", predicate_text(p));
        };
        check("level == 7", &[], true);
        check("level = 8", &[], false);
        check("level >= 7 AND gold < 13", &[], true);
        check("level > 7 OR name == 'Siris'", &[], true);
        check("NOT (level == 7)", &[], false);
        check("!(level != 7)", &[], true);
        check("%K == %@", &[s("name") as u64, s("Siris") as u64], true);
        check("%K > %d", &[s("level") as u64, (-3i32) as u32 as u64], true);
        check("gold BETWEEN {10, 20}", &[], true);
        check("pet.kind BEGINSWITH[c] 'dra'", &[], true);
        check("pet.kind BEGINSWITH 'dra'", &[], false);
        check("name LIKE 'S*s'", &[], true);
        check("name MATCHES '[A-Z][a-z]+'", &[], true);
        check("name IN {'Ana', 'Siris'}", &[], true);
        check("tags CONTAINS 'fire'", &[], true);
        check("ANY tags ENDSWITH 'rth'", &[], true);
        check("ALL tags ENDSWITH 'rth'", &[], false);
        check("NONE tags == 'water'", &[], true);
        check("missing == nil", &[], true);
        check("TRUEPREDICATE", &[], true);
        check("SELF.level < 10.5", &[], true);
        assert_eq!(from_format("level ==", [].as_ptr()), 0, "a broken format gives nil");

        // The parts of a comparison and a compound read back.
        let p = from_format("level > 3 AND name == %@", [s("x") as u64].as_ptr());
        let subs = collections::array_items(runtime::send(p, "subpredicates", &[]));
        assert_eq!(subs.len(), 2);
        let left = runtime::send(subs[0], "leftExpression", &[]);
        assert_eq!(runtime::send(left, "expressionType", &[]), KEY_PATH_TYPE);
        let right = runtime::send(subs[1], "rightExpression", &[]);
        assert_eq!(string::get(runtime::send(right, "constantValue", &[])), "x");

        // Filtering and sorting by key path (descending, then a second key).
        let people = [("b", 2), ("a", 2), ("c", 9), ("d", 1)].map(|(name, level)| dict(&[("name", s(name)), ("level", n(level))]));
        let list = collections::array_from(&people);
        let over = runtime::send(list, "filteredArrayUsingPredicate:", &[from_format("level >= 2", [].as_ptr())]);
        assert_eq!(collections::array_items(over).len(), 3);
        let by_level = runtime::send(runtime::class_named("NSSortDescriptor"), "sortDescriptorWithKey:ascending:", &[s("level"), 0]);
        let by_name = runtime::send(runtime::alloc_instance(runtime::class_named("NSSortDescriptor")), "initWithKey:ascending:", &[s("name"), 1]);
        let sorted = runtime::send(list, "sortedArrayUsingDescriptors:", &[collections::array_from(&[by_level, by_name])]);
        let names: Vec<String> = collections::array_items(sorted).into_iter().map(|d| string::get(collections::dictionary_get(d, s("name")))).collect();
        assert_eq!(names, ["c", "a", "b", "d"]);
        let mutable = runtime::send(list, "mutableCopy", &[]);
        runtime::send(mutable, "sortUsingDescriptors:", &[collections::array_from(&[by_name])]);
        assert_eq!(string::get(collections::dictionary_get(collections::array_items(mutable)[0], s("name"))), "a");
    }
}
