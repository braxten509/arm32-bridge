//! What the C++ runtime (libguest32cxx.so, guest32-cxx/) asks of the C
//! library beyond libSystem's basics: Darwin's xlocale `_l` calls, wide
//! characters and message catalogs.
//!
//! One locale exists: "C" with UTF-8 multibyte text, as bionic's (so
//! `___mb_cur_max_l` is 4). Every `locale_t` is the same handle and an `_l`
//! call is the plain call with its locale ignored. Wide character classes
//! use Apple's `_CTYPE_*` bits, which libc++ passes to `iswctype_l` as its
//! `ctype_base::mask`.

use std::sync::OnceLock;

use arm32_bridge::handler;
use arm32_bridge::imports::{self, printf, scanf};
use arm32_bridge::runtime::{mem, ArgSource, Args, Ret, Uc};

use super::function;
use super::libsystem::{
    rune_type, RUNE_ALPHA, RUNE_BLANK, RUNE_CONTROL, RUNE_DIGIT, RUNE_GRAPH, RUNE_LOWER, RUNE_PRINT, RUNE_PUNCT,
    RUNE_SPACE, RUNE_UPPER, RUNE_XDIGIT,
};

/// Android handlers with Darwin's call and meaning.
const REUSED: &[&str] = &[
    "strtold", "setlocale", "wcslen", "wmemchr", "wmemcmp", "getwc", "fputwc", "ungetwc",
];

/// (Darwin `_l` call, Android handler): the same arguments plus a trailing
/// `locale_t`, which is left unread.
const TRAILING_LOCALE: &[(&str, &str)] = &[
    ("btowc_l", "btowc"),
    ("isdigit_l", "isdigit"),
    ("isxdigit_l", "isxdigit"),
    ("mbrlen_l", "mbrlen"),
    ("mbrtowc_l", "mbrtowc"),
    ("mbtowc_l", "mbtowc"),
    ("strcoll_l", "strcoll"),
    ("strftime_l", "strftime"),
    ("strtod_l", "strtod"),
    ("strtof_l", "strtof"),
    ("strtold_l", "strtold"),
    ("strtoll_l", "strtoll"),
    ("strtoull_l", "strtoull"),
    ("strxfrm_l", "strxfrm"),
    ("tolower_l", "tolower"),
    ("toupper_l", "toupper"),
    ("towlower_l", "towlower"),
    ("towupper_l", "towupper"),
    ("wcrtomb_l", "wcrtomb"),
    ("wcscoll_l", "wcscoll"),
    ("wcsxfrm_l", "wcsxfrm"),
    ("wctob_l", "wctob"),
];

/// (`isw*_l` call, the class bit it tests).
const WIDE_CLASS_CALLS: &[(&str, u32)] = &[
    ("iswalpha_l", RUNE_ALPHA),
    ("iswblank_l", RUNE_BLANK),
    ("iswcntrl_l", RUNE_CONTROL),
    ("iswdigit_l", RUNE_DIGIT),
    ("iswlower_l", RUNE_LOWER),
    ("iswprint_l", RUNE_PRINT),
    ("iswpunct_l", RUNE_PUNCT),
    ("iswspace_l", RUNE_SPACE),
    ("iswupper_l", RUNE_UPPER),
    ("iswxdigit_l", RUNE_XDIGIT),
];

pub fn register() {
    for &name in REUSED {
        function(name, imports::handler_for(name).unwrap_or_else(|| panic!("no Android handler for {name}")));
    }
    for &(name, android) in TRAILING_LOCALE {
        assert!(imports::handler_for(android).is_some(), "no Android handler for {android}");
        function(name, without_locale);
    }
    for &(name, _) in WIDE_CLASS_CALLS {
        function(name, wide_class_call);
    }
    locales();
    wide_text();
    formatted_text();
    catalogs();
}

/// An `_l` call as its plain call (named as the plain call while it runs).
fn without_locale(uc: &mut Uc, a: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let &(_, android) = TRAILING_LOCALE.iter().find(|(darwin, _)| *darwin == name).expect("registered by register()");
    let handler = imports::handler_for(android).expect("checked by register()");
    uc.get_data_mut().current_call = android;
    let result = handler(uc, a);
    uc.get_data_mut().current_call = name;
    result
}

/// Apple's `_CTYPE_*` class bits of a wide character (the C locale's table
/// below 128, Unicode's properties above).
pub fn wide_class(c: u32) -> u32 {
    if c < 128 {
        return rune_type(c as i32) & !0xFF;
    }
    let Some(ch) = char::from_u32(c) else { return 0 };
    let blank = matches!(ch, '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}');
    let mut bits = 0;
    for (condition, bit) in [
        (ch.is_alphabetic(), RUNE_ALPHA),
        (ch.is_control(), RUNE_CONTROL),
        (ch.is_lowercase(), RUNE_LOWER),
        (ch.is_uppercase(), RUNE_UPPER),
        (ch.is_whitespace(), RUNE_SPACE),
        (blank, RUNE_BLANK),
        (!ch.is_control() && !ch.is_whitespace(), RUNE_GRAPH),
        (!ch.is_control(), RUNE_PRINT),
        (!ch.is_control() && !ch.is_whitespace() && !ch.is_alphanumeric(), RUNE_PUNCT),
    ] {
        if condition {
            bits |= bit;
        }
    }
    bits
}

fn wide_class_call(uc: &mut Uc, a: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let &(_, bit) = WIDE_CLASS_CALLS.iter().find(|(call, _)| *call == name).expect("registered by register()");
    Ret::U32((wide_class(a.u32()) & bit != 0) as u32)
}

/// The one `locale_t` (any non-null value; `LC_GLOBAL_LOCALE` is -1).
fn locale_handle() -> u32 {
    static HANDLE: OnceLock<u32> = OnceLock::new();
    *HANDLE.get_or_init(|| super::alloc_permanent(16, 4))
}

/// The C locale's `struct lconv` (Darwin armv7: ten strings, then fourteen
/// chars, all CHAR_MAX).
fn c_lconv() -> u32 {
    static LCONV: OnceLock<u32> = OnceLock::new();
    *LCONV.get_or_init(|| {
        let address = super::alloc_permanent(56, 4);
        let (point, empty) = (super::cstring_permanent("."), super::cstring_permanent(""));
        for i in 0..10 {
            mem().write_u32(address + i * 4, if i == 0 { point } else { empty });
        }
        mem().write_bytes(address + 40, &[127; 14]);
        address
    })
}

fn locales() {
    // newlocale(mask, name, base)
    function("newlocale", handler!(|uc, a| Ret::U32(locale_handle())).unwrap());
    function("freelocale", handler!(|uc, a| Ret::U32(0)).unwrap());
    function("___mb_cur_max_l", handler!(|uc, a| Ret::U32(4)).unwrap());
    function("localeconv_l", handler!(|uc, a| Ret::U32(c_lconv())).unwrap());
    function("iswctype_l", handler!(|uc, a| {
        let (c, mask) = (a.u32(), a.u32());
        Ret::U32((wide_class(c) & mask != 0) as u32)
    })
    .unwrap());
}

/// A guest wide string (until its 0).
fn read_wide(address: u32) -> Vec<u32> {
    (0..).map(|i| mem().read_u32(address + i * 4)).take_while(|&c| c != 0).collect()
}

fn write_wide(address: u32, text: &[char]) {
    for (i, &c) in text.iter().enumerate() {
        mem().write_u32(address + i as u32 * 4, c as u32);
    }
    mem().write_u32(address + text.len() as u32 * 4, 0);
}

fn set_errno(uc: &Uc, linux: i32) {
    mem().write_u32(uc.get_data().errno_slot, arm32_bridge::ios32::darwin_errno(linux) as u32);
}

/// This thread's errno on the host.
fn host_errno() -> *mut libc::c_int {
    #[cfg(target_os = "android")]
    return unsafe { libc::__errno() };
    #[cfg(not(target_os = "android"))]
    return unsafe { libc::__errno_location() };
}

/// A wide number's text as bytes for the host's strto* (it stops at the first
/// character outside ASCII, as a number would), then the result and the
/// guest end address (written to `end_out`).
fn wide_number<T>(uc: &Uc, text: u32, end_out: u32, parse: impl FnOnce(*const libc::c_char, *mut *mut libc::c_char) -> T) -> T {
    let mut bytes: Vec<u8> = read_wide(text).into_iter().take_while(|&c| c < 128).map(|c| c as u8).collect();
    bytes.push(0);
    let mut end: *mut libc::c_char = std::ptr::null_mut();
    unsafe { *host_errno() = 0 };
    let value = parse(bytes.as_ptr() as *const libc::c_char, &mut end);
    let errno = unsafe { *host_errno() };
    if errno != 0 {
        set_errno(uc, errno);
    }
    if end_out != 0 {
        let used = end as usize - bytes.as_ptr() as usize;
        mem().write_u32(end_out, text + used as u32 * 4);
    }
    value
}

/// UTF-8 length of a sequence from its first byte (None: not a first byte).
fn utf8_length(first: u8) -> Option<u32> {
    match first {
        0..=0x7F => Some(1),
        _ if first >> 5 == 0b110 => Some(2),
        _ if first >> 4 == 0b1110 => Some(3),
        _ if first >> 3 == 0b11110 => Some(4),
        _ => None,
    }
}

/// mbsnrtowcs / mbsrtowcs: UTF-8 at *`source` (at most `byte_limit` bytes)
/// into at most `length` wide characters at `out` (null `out`: only counts).
fn multibyte_to_wide(uc: &Uc, out: u32, source: u32, byte_limit: Option<u32>, length: u32) -> Ret {
    let start = mem().read_u32(source);
    let (mut at, mut count) = (start, 0u32);
    loop {
        if out != 0 && count >= length {
            break;
        }
        let left = byte_limit.map(|limit| limit - (at - start));
        if left == Some(0) {
            break;
        }
        let first = mem().read_u8(at);
        if first == 0 {
            if out != 0 {
                mem().write_u32(out + count * 4, 0);
                mem().write_u32(source, 0);
            }
            return Ret::U32(count);
        }
        let Some(size) = utf8_length(first) else {
            set_errno(uc, libc::EILSEQ);
            if out != 0 {
                mem().write_u32(source, at);
            }
            return Ret::U32(u32::MAX);
        };
        if left.is_some_and(|left| left < size) {
            break;
        }
        let Some(c) = std::str::from_utf8(mem().bytes(at, size as usize)).ok().and_then(|text| text.chars().next()) else {
            set_errno(uc, libc::EILSEQ);
            if out != 0 {
                mem().write_u32(source, at);
            }
            return Ret::U32(u32::MAX);
        };
        if out != 0 {
            mem().write_u32(out + count * 4, c as u32);
        }
        at += size;
        count += 1;
    }
    if out != 0 {
        mem().write_u32(source, at);
    }
    Ret::U32(count)
}

fn wide_text() {
    function("wcstol", handler!(|uc, a| {
        let (text, end, base) = (a.u32(), a.u32(), a.i32());
        let value = wide_number(uc, text, end, |text, end| unsafe { libc::strtoll(text, end, base) });
        let clamped = value.clamp(i32::MIN as i64, i32::MAX as i64);
        if clamped != value {
            set_errno(uc, libc::ERANGE);
        }
        Ret::U32(clamped as i32 as u32)
    })
    .unwrap());
    function("wcstoul", handler!(|uc, a| {
        let (text, end, base) = (a.u32(), a.u32(), a.i32());
        let value = wide_number(uc, text, end, |text, end| unsafe { libc::strtoull(text, end, base) });
        // "-5" wraps around 32 bits, like a 32-bit strtoul.
        if value > u32::MAX as u64 && value < u64::MAX - u32::MAX as u64 {
            set_errno(uc, libc::ERANGE);
            return Ret::U32(u32::MAX);
        }
        Ret::U32(value as u32)
    })
    .unwrap());
    function("wcstoll", handler!(|uc, a| {
        let (text, end, base) = (a.u32(), a.u32(), a.i32());
        Ret::U64(wide_number(uc, text, end, |text, end| unsafe { libc::strtoll(text, end, base) }) as u64)
    })
    .unwrap());
    function("wcstoull", handler!(|uc, a| {
        let (text, end, base) = (a.u32(), a.u32(), a.i32());
        Ret::U64(wide_number(uc, text, end, |text, end| unsafe { libc::strtoull(text, end, base) }))
    })
    .unwrap());
    // long double is double here.
    for name in ["wcstod", "wcstold"] {
        function(name, handler!(|uc, a| {
            let (text, end) = (a.u32(), a.u32());
            Ret::F64(wide_number(uc, text, end, |text, end| unsafe { libc::strtod(text, end) }))
        })
        .unwrap());
    }
    function("wcstof", handler!(|uc, a| {
        let (text, end) = (a.u32(), a.u32());
        Ret::F32(wide_number(uc, text, end, |text, end| unsafe { libc::strtof(text, end) }))
    })
    .unwrap());
    // mbsrtowcs_l(out, &source, length, state, locale)
    function("mbsrtowcs_l", handler!(|uc, a| {
        let (out, source, length) = (a.u32(), a.u32(), a.u32());
        multibyte_to_wide(uc, out, source, None, length)
    })
    .unwrap());
    // mbsnrtowcs_l(out, &source, bytes, length, state, locale)
    function("mbsnrtowcs_l", handler!(|uc, a| {
        let (out, source, bytes, length) = (a.u32(), a.u32(), a.u32(), a.u32());
        multibyte_to_wide(uc, out, source, Some(bytes), length)
    })
    .unwrap());
    // wcsnrtombs_l(out, &source, characters, length, state, locale): stops
    // before a character that doesn't fit; null `out` only counts.
    function("wcsnrtombs_l", handler!(|uc, a| {
        let (out, source, characters, length) = (a.u32(), a.u32(), a.u32(), a.u32());
        let start = mem().read_u32(source);
        let mut written = 0u32;
        for i in 0..characters {
            let c = mem().read_u32(start + i * 4);
            if c == 0 {
                if out != 0 {
                    if written >= length {
                        mem().write_u32(source, start + i * 4);
                        return Ret::U32(written);
                    }
                    mem().write_u8(out + written, 0);
                    mem().write_u32(source, 0);
                }
                return Ret::U32(written);
            }
            let Some(c) = char::from_u32(c) else {
                set_errno(uc, libc::EILSEQ);
                if out != 0 {
                    mem().write_u32(source, start + i * 4);
                }
                return Ret::U32(u32::MAX);
            };
            let mut buffer = [0u8; 4];
            let encoded = c.encode_utf8(&mut buffer).as_bytes();
            if out != 0 {
                if written + encoded.len() as u32 > length {
                    mem().write_u32(source, start + i * 4);
                    return Ret::U32(written);
                }
                mem().write_bytes(out + written, encoded);
            }
            written += encoded.len() as u32;
        }
        if out != 0 {
            mem().write_u32(source, start + characters * 4);
        }
        Ret::U32(written)
    })
    .unwrap());
}

fn formatted_text() {
    // snprintf_l(buffer, size, locale, format, ...)
    function("snprintf_l", handler!(|uc, a| {
        let (buffer, size, _locale, format) = (a.u32(), a.u32(), a.u32(), a.u32());
        let text = printf::format(mem(), format, a);
        if size > 0 {
            let copy = text.len().min(size as usize - 1);
            mem().write_bytes(buffer, &text[..copy]);
            mem().write_u8(buffer + copy as u32, 0);
        }
        Ret::U32(text.len() as u32)
    })
    .unwrap());
    // asprintf_l(&out, locale, format, ...)
    function("asprintf_l", handler!(|uc, a| {
        let (out, _locale, format) = (a.u32(), a.u32(), a.u32());
        let text = printf::format(mem(), format, a);
        let buffer = super::alloc(text.len() as u32 + 1);
        if buffer == 0 {
            set_errno(uc, libc::ENOMEM);
            return Ret::U32(u32::MAX);
        }
        mem().write_bytes(buffer, &text);
        mem().write_u8(buffer + text.len() as u32, 0);
        mem().write_u32(out, buffer);
        Ret::U32(text.len() as u32)
    })
    .unwrap());
    // sscanf_l(input, locale, format, ...)
    function("sscanf_l", handler!(|uc, a| {
        let (input, _locale, format) = (a.u32(), a.u32(), a.u32());
        Ret::U32(scanf::scan(mem(), input, format, a) as u32)
    })
    .unwrap());
    // swprintf(buffer, size, wide format, ...): -1 when it doesn't fit.
    function("swprintf", handler!(|uc, a| {
        let (buffer, size, format) = (a.u32(), a.u32(), a.u32());
        let format: String = read_wide(format).into_iter().filter_map(char::from_u32).collect();
        let text = printf::format_text(mem(), format.as_bytes(), a);
        let text: Vec<char> = String::from_utf8_lossy(&text).chars().collect();
        if size == 0 || text.len() >= size as usize {
            if size > 0 {
                write_wide(buffer, &text[..size as usize - 1]);
            }
            set_errno(uc, libc::EOVERFLOW);
            return Ret::U32(u32::MAX);
        }
        write_wide(buffer, &text);
        Ret::U32(text.len() as u32)
    })
    .unwrap());
}

/// Message catalogs (std::messages): there are none, so every lookup gives
/// the caller's default text.
fn catalogs() {
    function("catopen", handler!(|uc, a| Ret::U32(u32::MAX)).unwrap());
    // catgets(catalog, set, number, default)
    function("catgets", handler!(|uc, a| {
        let (_catalog, _set, _number, default) = (a.u32(), a.u32(), a.u32(), a.u32());
        Ret::U32(default)
    })
    .unwrap());
    function("catclose", handler!(|uc, a| Ret::U32(0)).unwrap());
}
