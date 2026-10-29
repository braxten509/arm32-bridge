//! Builds the type encodings of our own Objective-C methods (the ones in
//! src/fw and src/objc) from their Rust signatures, so NSMethodSignature and
//! NSInvocation describe and call them exactly as Apple's runtime would.
//! Output: $OUT_DIR/method_types.rs, a list of
//! (class name or "" when not known here, class method?, selector, types).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

pub fn generate() {
    println!("cargo:rerun-if-changed=build/method_types.rs");
    let mut entries: Vec<(String, bool, String, String)> = Vec::new();
    let mut files = Vec::new();
    for dir in ["src/fw", "src/objc"] {
        println!("cargo:rerun-if-changed={dir}");
        let mut list: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
        list.sort();
        files.extend(list.into_iter().filter(|p| p.extension().is_some_and(|e| e == "rs")));
    }
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), expand_macros(&strip_comments(&std::fs::read_to_string(path).unwrap()))))
        .collect();
    // Every extern "C" fn by name, per file and over all files (when unique).
    let mut all: HashMap<String, Vec<Signature>> = HashMap::new();
    let per_file: Vec<HashMap<String, Signature>> = sources
        .iter()
        .map(|(_, text)| {
            let found = signatures(text);
            for (name, sig) in &found {
                all.entry(name.clone()).or_default().push(sig.clone());
            }
            found
        })
        .collect();
    // A function serving several selectors is a shared helper whose Rust
    // types are loose (an Id standing for a BOOL, an i64 for an int): none
    // of its selectors get types from it.
    let found: Vec<Vec<(Option<String>, bool, String, String)>> = sources.iter().map(|(file, text)| registrations(file, text)).collect();
    let mut selectors_of: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for (_, _, selector, function) in found.iter().flatten() {
        selectors_of.entry(function.rsplit("::").next().unwrap().to_string()).or_default().insert(selector.clone());
    }
    for (((file, _), local), registered) in sources.iter().zip(&per_file).zip(found) {
        let lookup = |name: &str| -> Option<Signature> {
            let name = name.rsplit("::").next().unwrap();
            local.get(name).cloned().or_else(|| all.get(name).filter(|v| v.len() == 1).map(|v| v[0].clone()))
        };
        for (class, meta, selector, function) in registered {
            let dedicated = selectors_of.get(function.rsplit("::").next().unwrap()).is_some_and(|s| s.len() == 1);
            match lookup(&function).filter(|_| dedicated).and_then(|s| s.encode(&selector)) {
                Some(types) => entries.push((class.unwrap_or_default(), meta, selector, types)),
                // IOS_TYPES_DEBUG=1 lists the methods left without types.
                None if std::env::var_os("IOS_TYPES_DEBUG").is_some() => eprintln!("no types: {file} {selector} ({function})"),
                None => {}
            }
        }
    }
    let mut out = String::from("pub static METHOD_TYPES: &[(&str, bool, &str, &str)] = &[\n");
    for (class, meta, selector, types) in &entries {
        writeln!(out, "    ({class:?}, {meta}, {selector:?}, {types:?}),").unwrap();
    }
    out.push_str("];\n");
    let target = Path::new(&std::env::var("OUT_DIR").unwrap()).join("method_types.rs");
    std::fs::write(target, out).unwrap();
}

/// Replaces comments with spaces, leaving strings and characters alone.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                out.push(b' ');
            }
            b'"' => {
                // Raw strings (r"..", r#".."#) are rare here; plain ones are enough.
                out.push(b'"');
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        out.push(b[i]);
                        i += 1;
                    }
                    out.push(b[i]);
                    i += 1;
                }
                if i < b.len() {
                    out.push(b'"');
                    i += 1;
                }
            }
            b'\'' if i + 2 < b.len() && (b[i + 2] == b'\'' || (b[i + 1] == b'\\')) => {
                // A character literal ('x' or '\n', '\''); lifetimes fall through.
                let start = i;
                i += 1;
                if b[i] == b'\\' {
                    i += 2;
                }
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
                out.extend_from_slice(&b[start..i.min(b.len())]);
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The index just after the bracket that closes the one at `open`.
fn closing(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    let mut in_string = false;
    while i < text.len() {
        let c = text[i];
        if in_string {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                // Character literals such as '(' or '"' (lifetimes have no closing quote).
                b'\'' if text.get(i + 2) == Some(&b'\'') => i += 2,
                b'\'' if text.get(i + 1) == Some(&b'\\') && text.get(i + 3) == Some(&b'\'') => i += 3,
                b'"' => in_string = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Splits on commas outside brackets and strings.
fn split_top(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut in_string = false;
    let mut current = String::new();
    let mut previous = ' ';
    for c in text.chars() {
        if in_string {
            current.push(c);
            if c == '"' && previous != '\\' {
                in_string = false;
            }
            previous = if previous == '\\' && c == '\\' { ' ' } else { c };
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '<' => angle += 1,
            '>' if previous != '-' && angle > 0 => angle -= 1,
            ',' if depth == 0 && angle == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
                previous = c;
                continue;
            }
            _ => {}
        }
        current.push(c);
        previous = c;
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Finds `word` as a whole word (or path end) at or after `from`.
fn find_word(text: &str, word: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(found) = text[at..].find(word) {
        let start = at + found;
        let before_ok = start == 0 || !is_ident(text.as_bytes()[start - 1]);
        if before_ok {
            return Some(start);
        }
        at = start + word.len();
    }
    None
}

/// Expands simple macro_rules! macros (fixed argument lists, no repetition),
/// appending each expansion to the text so the functions they make are seen.
fn expand_macros(text: &str) -> String {
    let mut out = text.to_string();
    let mut at = 0;
    let mut macros: Vec<(String, Vec<(Vec<String>, String)>)> = Vec::new();
    while let Some(start) = text[at..].find("macro_rules!").map(|i| i + at) {
        let name_start = start + "macro_rules!".len();
        let brace = match text[name_start..].find('{') {
            Some(i) => name_start + i,
            None => break,
        };
        let name = text[name_start..brace].trim().to_string();
        let end = closing(text.as_bytes(), brace).unwrap_or(text.len());
        let body = &text[brace + 1..end - 1];
        let mut arms = Vec::new();
        let mut i = 0;
        let bytes = body.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'(' {
                let pattern_end = closing(bytes, i).unwrap_or(bytes.len());
                let pattern = &body[i + 1..pattern_end - 1];
                let Some(arrow) = body[pattern_end..].find("=>") else { break };
                let open = pattern_end + arrow + 2 + body[pattern_end + arrow + 2..].find('{').unwrap_or(0);
                let close = closing(bytes, open).unwrap_or(bytes.len());
                arms.push((split_top(pattern), body[open + 1..close - 1].to_string()));
                i = close;
            } else {
                i += 1;
            }
        }
        macros.push((name, arms));
        at = end;
    }
    for (name, arms) in &macros {
        let call = format!("{name}!(");
        let mut from = 0;
        while let Some(found) = find_word(text, &call, from) {
            let open = found + call.len() - 1;
            let end = closing(text.as_bytes(), open).unwrap_or(text.len());
            let args = split_top(&text[open + 1..end - 1]);
            for (pattern, body) in arms {
                if pattern.len() != args.len() {
                    continue;
                }
                let mut vars: Vec<(String, String)> = Vec::new();
                let matched = pattern.iter().zip(&args).all(|(p, a)| match p.strip_prefix('$') {
                    Some(var) => {
                        vars.push((format!("${}", var.split(':').next().unwrap().trim()), a.clone()));
                        true
                    }
                    None => p.trim() == a.trim(),
                });
                if matched {
                    vars.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
                    let mut expanded = body.clone();
                    for (var, value) in &vars {
                        expanded = expanded.replace(var, value);
                    }
                    out.push('\n');
                    out.push_str(&expanded);
                    break;
                }
            }
            from = end;
        }
    }
    out
}

#[derive(Clone)]
struct Signature {
    /// (name, Rust type) of each parameter.
    params: Vec<(String, String)>,
    ret: String,
}

/// Lower-case words of a name: "copyWithZone" / "_zone" / "at_index" -> words.
fn words(text: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == ':' {
            out.push(String::new());
            continue;
        }
        // A capital starts a word after a small letter or digit, or ends a
        // run of capitals ("UTF8String" -> utf8, string; "URLWith" -> url, with).
        let previous = if i > 0 { chars[i - 1] } else { '_' };
        let next_small = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
        if c.is_ascii_uppercase() && (previous.is_ascii_lowercase() || previous.is_ascii_digit() || (previous.is_ascii_uppercase() && next_small)) {
            out.push(String::new());
        }
        out.last_mut().unwrap().push(c.to_ascii_lowercase());
    }
    out.retain(|w| !w.is_empty());
    out
}

/// Words that name a count, index or option (NSInteger/NSUInteger).
const INTEGER_WORDS: &[&str] = &[
    "index", "indexes", "count", "length", "capacity", "options", "style", "mode", "tag", "location", "limit",
    "priority", "state", "status", "flags", "mask", "level", "section", "row", "item", "unit", "units", "policy", "behavior",
    "encoding", "alignment", "orientation", "persistence", "hash", "integer", "position", "seconds", "milliseconds",
];
/// Words that name a block.
const BLOCK_WORDS: &[&str] = &["block", "handler", "completion", "callback"];
/// Words that name a BOOL argument.
const BOOL_WORDS: &[&str] = &["animated", "editing", "hidden", "enabled", "flag", "yes"];
/// Words that name a C string.
const C_STRING_WORDS: &[&str] = &["utf8", "cstring"];

fn has_word(list: &[String], wanted: &[&str]) -> bool {
    list.iter().any(|w| wanted.contains(&w.as_str()))
        || list.windows(2).any(|pair| wanted.contains(&format!("{}{}", pair[0], pair[1]).as_str()))
}

impl Signature {
    /// Apple's encoding (return, self, _cmd, then each argument) when every
    /// type is known for sure from the Rust types together with the
    /// selector's and parameters' names; None otherwise (no signature beats
    /// a wrong one).
    fn encode(&self, selector: &str) -> Option<String> {
        let keywords: Vec<&str> = selector.split(':').collect();
        let arguments = selector.matches(':').count();
        if self.params.len() != arguments + 2 {
            return None;
        }
        let selector_words = words(selector);
        // A getter's name describes what it returns.
        let returns = if arguments == 0 { selector_words.clone() } else { Vec::new() };
        let mut out = encode_type(&self.ret, &returns, selector)?;
        out.push_str("@:");
        for (index, (name, rust)) in self.params.iter().enumerate().skip(2) {
            let mut hints = words(name);
            hints.extend(words(keywords[index - 2]));
            if has_word(&hints, BOOL_WORDS) && !matches!(last(rust), "bool" | "u8") {
                return None;
            }
            out.push_str(&encode_type(rust, &hints, selector)?);
        }
        Some(out)
    }
}

fn last(rust: &str) -> &str {
    rust.rsplit("::").next().unwrap()
}

/// One type's encoding; `hints` are the words naming it (parameter and
/// selector keyword, or a getter's name). None when not sure.
fn encode_type(rust: &str, hints: &[String], selector: &str) -> Option<String> {
    let t: String = rust.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(inner) = t.strip_prefix("*const").or_else(|| t.strip_prefix("*mut")) {
        let inner = last(inner);
        if inner == "c_char" || (matches!(inner, "u8" | "i8") && has_word(hints, C_STRING_WORDS)) {
            return Some("*".into());
        }
        return Some(match inner {
            "c_void" | "u8" => "^v".into(),
            "Id" => "^@".into(),
            other => format!("^{}", encode_type(other, &[], selector)?),
        });
    }
    let integer = has_word(hints, INTEGER_WORDS);
    Some(
        match last(&t) {
            "" | "()" | "!" => "v",
            // A count or index is never an object.
            "Id" if integer => return None,
            "Id" => "@",
            "Class" => "#",
            "Sel" => ":",
            "usize" | "u64" | "isize" | "i64" if has_word(hints, &["zone"]) => "^{_NSZone=}",
            "usize" | "u64" | "isize" | "i64" if has_word(hints, BLOCK_WORDS) => "@?",
            "usize" | "u64" | "isize" | "i64" if has_word(hints, C_STRING_WORDS) => "*",
            "usize" | "u64" if integer => "Q",
            "isize" | "i64" if integer => "q",
            "usize" | "u64" | "isize" | "i64" => return None,
            "bool" => "B",
            // BOOL is bool on arm64; an unsigned char says so in the selector.
            "u8" if selector.contains("Char") => "C",
            "u8" => "B",
            "i8" => "c",
            "u16" => "S",
            "i16" => "s",
            "u32" | "Enum" => "I",
            "i32" => "i",
            "f32" => "f",
            "f64" => "d",
            "Point" => "{CGPoint=dd}",
            "Size" => "{CGSize=dd}",
            "Rect" => "{CGRect={CGPoint=dd}{CGSize=dd}}",
            "Transform" => "{CGAffineTransform=dddddd}",
            "Range" => "{_NSRange=QQ}",
            "CmTime" => "{?=qiIq}",
            "UuidBytes" => "{?=[16C]}",
            _ => return None,
        }
        .to_string(),
    )
}

/// Every `extern "C" fn` in the text, plus the variadic trampolines (their
/// fixed arguments are those of the function they jump to, minus the list).
fn signatures(text: &str) -> HashMap<String, Signature> {
    let mut found = HashMap::new();
    let mut at = 0;
    let key = "extern \"C\" fn ";
    while let Some(i) = text[at..].find(key) {
        let name_start = at + i + key.len();
        at = name_start;
        let Some(paren) = text[name_start..].find('(').map(|p| p + name_start) else { break };
        let name = text[name_start..paren].trim();
        if name.is_empty() || !name.bytes().all(is_ident) {
            continue;
        }
        let Some(close) = closing(text.as_bytes(), paren) else { continue };
        let params = split_top(&text[paren + 1..close - 1])
            .into_iter()
            .map(|p| p.split_once(':').map(|(n, t)| (n.trim().trim_start_matches("mut ").to_string(), t.trim().to_string())).unwrap_or_default())
            .collect();
        let rest = &text[close..];
        let body = rest.find(['{', ';']).unwrap_or(rest.len());
        let ret = rest[..body].trim().strip_prefix("->").map(|r| r.trim().to_string()).unwrap_or_default();
        found.insert(name.to_string(), Signature { params, ret });
    }
    let mut from = 0;
    while let Some(i) = find_word(text, "variadic!(", from) {
        let open = i + "variadic!".len();
        let end = closing(text.as_bytes(), open).unwrap_or(text.len());
        let args = split_top(&text[open + 1..end - 1]);
        if let [trampoline, _, target] = args.as_slice() {
            if let Some(sig) = found.get(target.as_str()).cloned() {
                let mut sig = sig;
                sig.params.pop();
                found.insert(trampoline.clone(), sig);
            }
        }
        from = end;
    }
    found
}

/// The entries of a method list held in a variable: its `let` (the last one
/// before `before`) plus `name.push((..))` calls after it, including pushes in
/// `for each in ["a", "b"]` loops. Each entry comes back as "(selector, f as usize)".
fn list_variable(text: &str, name: &str, before: usize) -> Vec<String> {
    let mut entries = Vec::new();
    if name.is_empty() || !name.bytes().all(is_ident) {
        return entries;
    }
    let mut binding = None;
    for pattern in [format!("let mut {name}"), format!("let {name}")] {
        let mut from = 0;
        while let Some(i) = find_word(text, &pattern, from).filter(|&i| i < before) {
            let after = text.as_bytes().get(i + pattern.len()).copied().unwrap_or(b' ');
            if !is_ident(after) && binding.map(|b| i > b).unwrap_or(true) {
                binding = Some(i);
            }
            from = i + pattern.len();
        }
    }
    let Some(start) = binding else { return entries };
    let statement_end = text[start..].find(';').map(|e| start + e).unwrap_or(before);
    if let Some(eq) = text[start..statement_end].find('=').map(|e| start + e) {
        if let Some(open) = text[eq..statement_end].find('[').map(|o| eq + o) {
            if let Some(close) = closing(text.as_bytes(), open) {
                entries.push(text[open + 1..close - 1].to_string());
            }
        }
    }
    let push = format!("{name}.push(");
    let mut from = statement_end;
    while let Some(i) = text[from..before].find(&push).map(|p| from + p) {
        let open = i + push.len() - 1;
        let Some(end) = closing(text.as_bytes(), open) else { break };
        let tuple = text[open + 1..end - 1].trim().trim_start_matches('(').trim_end_matches(')');
        if let [selector, function] = split_top(tuple).as_slice() {
            if selector.starts_with('"') {
                entries.push(format!("({selector}, {function})"));
            } else if let Some(loop_at) = text[..i].rfind(&format!("for {selector} in [")) {
                let open = loop_at + format!("for {selector} in ").len();
                if let Some(close) = closing(text.as_bytes(), open) {
                    for each in split_top(&text[open + 1..close - 1]) {
                        entries.push(format!("({each}, {function})"));
                    }
                }
            }
        }
        from = end;
    }
    entries
}

/// (class name if known, class method?, selector, function) for each method
/// registered with methods()/class_methods() (and NSObject's own list).
fn registrations(file: &str, text: &str) -> Vec<(Option<String>, bool, String, String)> {
    // let VAR = ...define_class("Name"... / class_named("Name")
    let mut bindings: Vec<(usize, String, Vec<String>)> = Vec::new();
    let mut at = 0;
    while let Some(i) = find_word(text, "let ", at) {
        at = i + 4;
        let rest = &text[at..];
        let Some(eq) = rest.find('=') else { break };
        let var = rest[..eq].trim().trim_start_matches("mut ").trim();
        if !var.bytes().all(is_ident) || var.is_empty() {
            continue;
        }
        let statement_end = rest.find(';').unwrap_or(rest.len());
        let rhs = &rest[eq + 1..statement_end];
        for marker in ["define_class(\"", "class_named(\""] {
            if let Some(m) = rhs.find(marker) {
                let name_start = m + marker.len();
                if let Some(len) = rhs[name_start..].find('"') {
                    bindings.push((i, var.to_string(), vec![rhs[name_start..name_start + len].to_string()]));
                }
                break;
            }
        }
        // class_named(each) inside `for each in ["A", "B"]`: every listed class.
        if let Some(m) = rhs.find("class_named(") {
            let argument = rhs[m + "class_named(".len()..].split(')').next().unwrap_or("").trim();
            if !argument.is_empty() && argument.bytes().all(is_ident) {
                if let Some(loop_at) = text[..i].rfind(&format!("for {argument} in [")) {
                    let open = loop_at + format!("for {argument} in ").len();
                    if let Some(close) = closing(text.as_bytes(), open) {
                        let names: Vec<String> = split_top(&text[open + 1..close - 1])
                            .into_iter()
                            .filter_map(|n| n.trim().strip_prefix('"').and_then(|n| n.strip_suffix('"')).map(String::from))
                            .collect();
                        if !names.is_empty() {
                            bindings.push((i, var.to_string(), names));
                        }
                    }
                }
            }
        }
    }
    let classes_at = |var: &str, position: usize| -> Vec<Option<String>> {
        match bindings.iter().filter(|(p, v, _)| *p < position && v == var).last() {
            Some((_, _, names)) => names.iter().cloned().map(Some).collect(),
            None => vec![None],
        }
    };
    let mut out = Vec::new();
    let entries_in = |list: &str, class: Option<String>, meta: bool, out: &mut Vec<_>| {
        for item in split_top(list) {
            let item = item.trim().trim_start_matches('(').trim_end_matches(')');
            let parts = split_top(item);
            if let [selector, function] = parts.as_slice() {
                if let (Some(selector), Some(function)) =
                    (selector.strip_prefix('"').and_then(|s| s.strip_suffix('"')), function.strip_suffix("as usize"))
                {
                    out.push((class.clone(), meta, selector.to_string(), function.trim().to_string()));
                }
            }
        }
    };
    for (word, meta) in [("methods(", false), ("class_methods(", true)] {
        let mut from = 0;
        while let Some(i) = find_word(text, word, from) {
            from = i + word.len();
            if !meta && text[..i].ends_with("class_") {
                continue;
            }
            let open = i + word.len() - 1;
            let Some(end) = closing(text.as_bytes(), open) else { continue };
            let args = split_top(&text[open + 1..end - 1]);
            if let [target, list] = args.as_slice() {
                for class in classes_at(target.trim(), i) {
                    if let Some(list) = list.strip_prefix("&[").and_then(|l| l.strip_suffix(']')) {
                        entries_in(list, class, meta, &mut out);
                    } else {
                        let name = list.trim_start_matches('&').trim();
                        for entry in list_variable(text, name, i) {
                            entries_in(&entry, class.clone(), meta, &mut out);
                        }
                    }
                }
            }
        }
    }
    if file == "object.rs" {
        // NSObject's own methods go through two small closures, m and c.
        for (word, meta) in [("m(\"", false), ("c(\"", true)] {
            let mut from = 0;
            while let Some(i) = find_word(text, word, from) {
                let open = i + 1;
                let Some(end) = closing(text.as_bytes(), open) else { break };
                entries_in(&format!("({})", &text[open + 1..end - 1]), Some("NSObject".into()), meta, &mut out);
                from = end;
            }
        }
    }
    out
}
