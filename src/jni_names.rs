//! JNI method names and signatures: `Java_*` symbol names and the argument
//! kinds of a signature.

/// Argument kinds and return kind of a JNI method signature such as `(I[BLjava/lang/String;)V`.
pub fn kinds(signature: &str) -> Option<(Vec<u8>, u8)> {
    let bytes = signature.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut params = Vec::new();
    let mut i = 1;
    let read_one = |i: &mut usize| -> Option<u8> {
        let start = *i;
        while *bytes.get(*i)? == b'[' {
            *i += 1;
        }
        let kind = *bytes.get(*i)?;
        if kind == b'L' {
            *i += bytes[*i..].iter().position(|&b| b == b';')?;
        }
        *i += 1;
        Some(if bytes[start] == b'[' || kind == b'L' { b'L' } else { kind })
    };
    while *bytes.get(i)? != b')' {
        params.push(read_one(&mut i)?);
    }
    i += 1;
    let returns = read_one(&mut i)?;
    Some((params, returns))
}

/// The JNI short name of a method: `Java_<class>_<method>` with JNI escapes.
pub fn mangle(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => out.push_str(&format!("_0{:04x}", c as u32)),
        }
    }
    out
}

/// Splits `Java_<class>_<method>[__<arguments>]` into class (with `/`),
/// method and, for overloaded methods, the argument part of the signature.
pub fn demangle(symbol: &str) -> Option<(String, String, Option<String>)> {
    let rest = symbol.strip_prefix("Java_")?;
    // Undo the escapes of one part; plain `_` separates parts.
    let parts = |text: &str| -> Option<Vec<String>> {
        let mut parts = vec![String::new()];
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '_' {
                parts.last_mut()?.push(c);
                continue;
            }
            match chars.peek() {
                Some('1') => {
                    chars.next();
                    parts.last_mut()?.push('_');
                }
                Some('2') => {
                    chars.next();
                    parts.last_mut()?.push(';');
                }
                Some('3') => {
                    chars.next();
                    parts.last_mut()?.push('[');
                }
                Some('0') => {
                    chars.next();
                    let hex: String = chars.by_ref().take(4).collect();
                    parts.last_mut()?.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => parts.push(String::new()),
            }
        }
        Some(parts)
    };
    // The overload separator is `__`. A `__` followed by 0 or 1 is instead a part
    // separator plus an escape (a method `_draw` is `Renderer__1draw`); argument
    // lists never start with those escapes.
    let separator = rest
        .match_indices("__")
        .map(|(at, _)| at)
        .find(|&at| !matches!(rest.as_bytes().get(at + 2), Some(b'0' | b'1')));
    let (name, arguments) = match separator {
        Some(at) => (&rest[..at], Some(&rest[at + 2..])),
        None => (rest, None),
    };
    let mut name_parts = parts(name)?;
    let method = name_parts.pop()?;
    if name_parts.is_empty() || method.is_empty() {
        return None;
    }
    let arguments = match arguments {
        Some(text) => Some(parts(text)?.join("/")),
        None => None,
    };
    Some((name_parts.join("/"), method, arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_signatures() {
        assert_eq!(
            demangle("Java_org_cocos2dx_lib_Cocos2dxRenderer_nativeRender"),
            Some(("org/cocos2dx/lib/Cocos2dxRenderer".into(), "nativeRender".into(), None))
        );
        assert_eq!(
            demangle("Java_com_a_1b_My_00024Inner_do_1it__ILjava_lang_String_2"),
            Some(("com/a_b/My$Inner".into(), "do_it".into(), Some("ILjava/lang/String;".into())))
        );
        assert_eq!(mangle("com/a_b/My$Inner"), "com_a_1b_My_00024Inner");
        assert_eq!(
            demangle("Java_com_game_Renderer__1draw"),
            Some(("com/game/Renderer".into(), "_draw".into(), None))
        );
        assert_eq!(
            demangle("Java_com_game_Renderer__1draw__I"),
            Some(("com/game/Renderer".into(), "_draw".into(), Some("I".into())))
        );
        assert_eq!(
            demangle("Java_com_game_Renderer_draw___3B"),
            Some(("com/game/Renderer".into(), "draw".into(), Some("[B".into())))
        );
        assert_eq!(kinds("(I[BLjava/lang/String;JFD)Z"), Some((b"ILLJFD".to_vec(), b'Z')));
        assert_eq!(kinds("()[Ljava/lang/String;"), Some((vec![], b'L')));
    }
}
