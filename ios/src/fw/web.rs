//! Web requests. The game is offline: every request fails the way an iPhone
//! with no connection fails (NSURLErrorNotConnectedToInternet, -1009).

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

const NOT_CONNECTED: isize = -1009;

fn offline_error() -> Id {
    crate::fw::misc::error("NSURLErrorDomain", NOT_CONNECTED, "The Internet connection appears to be offline.")
}

// ----- percent escapes -----

fn escape(text: &str, keep: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "-._~".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            out += &format!("%{byte:02X}");
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let digit = |b: u8| (b as char).to_digit(16);
            if let (Some(high), Some(low)) = (digit(bytes[i + 1]), digit(bytes[i + 2])) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Characters Apple's old escaping leaves alone besides letters and digits.
const LEGAL_URL: &str = "!$&'()*+,/:;=?@#[]";

extern "C" fn adding_percent_escapes(this: Id, _: Sel, _encoding: usize) -> Id {
    string::make(&escape(&string::get(this), LEGAL_URL))
}

extern "C" fn replacing_percent_escapes(this: Id, _: Sel, _encoding: usize) -> Id {
    string::make(&unescape(&string::get(this)))
}

extern "C" fn removing_percent_encoding(this: Id, _: Sel) -> Id {
    string::make(&unescape(&string::get(this)))
}

/// CFURLCreateStringByAddingPercentEscapes(allocator, text, leave, add, encoding).
extern "C" fn cf_adding_percent_escapes(_allocator: usize, text: Id, leave: Id, add: Id, _encoding: u32) -> Id {
    let leave = if leave == 0 { String::new() } else { string::get(leave) };
    let add = if add == 0 { String::new() } else { string::get(add) };
    let keep: String = LEGAL_URL.chars().chain(leave.chars()).filter(|c| !add.contains(*c)).collect();
    string::make(&escape(&string::get(text), &keep))
}

/// CFURLCreateStringByReplacingPercentEscapesUsingEncoding(allocator, text, leave, encoding).
extern "C" fn cf_replacing_percent_escapes(_allocator: usize, text: Id, _leave: Id, _encoding: u32) -> Id {
    string::make(&unescape(&string::get(text)))
}

// ----- NSURLRequest -----

/// A getter and setter keeping one object per request.
macro_rules! property_id {
    ($get:ident, $set:ident, $key:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            property_get(this, $key).unwrap_or(0) as Id
        }
        extern "C" fn $set(this: Id, _: Sel, value: Id) {
            let value = if value != 0 { runtime::send(value, "copy", &[]) } else { 0 };
            property_set(this, $key, value as u64);
        }
    };
}

extern "C" fn request_with_url(class: Class, _: Sel, url: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "url", url as u64);
    object
}

extern "C" fn request_with_url_policy(class: Class, s: Sel, url: Id, _policy: usize, _timeout: f64) -> Id {
    request_with_url(class, s, url)
}

extern "C" fn request_init_with_url(this: Id, _: Sel, url: Id) -> Id {
    property_set(this, "url", url as u64);
    this
}

extern "C" fn request_init_with_url_policy(this: Id, s: Sel, url: Id, _policy: usize, _timeout: f64) -> Id {
    request_init_with_url(this, s, url)
}

extern "C" fn request_url(this: Id, _: Sel) -> Id {
    property_get(this, "url").unwrap_or(0) as Id
}

extern "C" fn request_set_url(this: Id, _: Sel, url: Id) {
    property_set(this, "url", url as u64);
}

/// Copies give a separate request, so later changes to a mutable one don't show in the copy.
fn copy_request(this: Id, class_name: &str) -> Id {
    let object = runtime::alloc_instance(runtime::class_named(class_name));
    for key in ["url", "method", "body", "headers", "timeout", "cachePolicy", "mainDocumentURL"] {
        if let Some(value) = property_get(this, key) {
            property_set(object, key, value);
        }
    }
    object
}

extern "C" fn request_copy(this: Id, _: Sel, _zone: usize) -> Id {
    copy_request(this, "NSURLRequest")
}

extern "C" fn request_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    copy_request(this, "NSMutableURLRequest")
}

property_id!(request_body, request_set_body, "body");
property_id!(request_main_document_url, request_set_main_document_url, "mainDocumentURL");

extern "C" fn request_timeout(this: Id, _: Sel) -> f64 {
    property_get(this, "timeout").map(f64::from_bits).unwrap_or(60.0)
}

extern "C" fn request_set_timeout(this: Id, _: Sel, seconds: f64) {
    property_set(this, "timeout", seconds.to_bits());
}

extern "C" fn request_cache_policy(this: Id, _: Sel) -> usize {
    property_get(this, "cachePolicy").unwrap_or(0) as usize
}

extern "C" fn request_set_cache_policy(this: Id, _: Sel, policy: usize) {
    property_set(this, "cachePolicy", policy as u64);
}

/// Header names are matched without regard to case, as in HTTP.
fn headers(this: Id) -> Vec<(String, Id)> {
    let dict = property_get(this, "headers").unwrap_or(0) as Id;
    collections::dictionary_pairs(dict).into_iter().map(|(k, v)| (string::get(k), v)).collect()
}

fn set_headers(this: Id, list: &[(String, Id)]) {
    let pairs: Vec<(Id, Id)> = list.iter().map(|(k, v)| (string::make(k), *v)).collect();
    property_set(this, "headers", collections::dictionary_from(&pairs) as u64);
}

extern "C" fn request_all_headers(this: Id, _: Sel) -> Id {
    property_get(this, "headers").unwrap_or(0) as Id
}

extern "C" fn request_set_all_headers(this: Id, _: Sel, fields: Id) {
    let list: Vec<(String, Id)> = collections::dictionary_pairs(fields).into_iter().map(|(k, v)| (string::get(k), v)).collect();
    set_headers(this, &list);
}

extern "C" fn request_header(this: Id, _: Sel, name: Id) -> Id {
    let name = string::get(name);
    headers(this).into_iter().find(|(k, _)| k.eq_ignore_ascii_case(&name)).map(|(_, v)| v).unwrap_or(0)
}

extern "C" fn request_set_header(this: Id, _: Sel, value: Id, name: Id) {
    let name = string::get(name);
    let mut list = headers(this);
    list.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
    if value != 0 {
        list.push((name, value));
    }
    set_headers(this, &list);
}

extern "C" fn request_add_header(this: Id, _: Sel, value: Id, name: Id) {
    let name = string::get(name);
    let mut list = headers(this);
    match list.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(&name)) {
        Some(entry) => entry.1 = string::make(&format!("{},{}", string::get(entry.1), string::get(value))),
        None => list.push((name, value)),
    }
    set_headers(this, &list);
}

extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}
extern "C" fn ignore_id_id(_this: Id, _: Sel, _a: Id, _b: Id) {}
extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}

extern "C" fn request_method(this: Id, _: Sel) -> Id {
    let method = property_get(this, "method").unwrap_or(0) as Id;
    if method == 0 { string::make("GET") } else { method }
}

extern "C" fn request_set_method(this: Id, _: Sel, method: Id) {
    property_set(this, "method", method as u64);
}

// ----- NSURLConnection: every connection fails as offline -----

fn report_failure(connection: Id, delegate: Id) {
    let url = property_get(property_get(connection, "request").unwrap_or(0) as Id, "url").unwrap_or(0) as Id;
    crate::log!("web request {} refused: offline", string::describe(url));
    let pair = Box::into_raw(Box::new((connection, delegate))) as usize;
    let block = crate::fw::media::make_callback(pair, |data| {
        let (connection, delegate) = *unsafe { Box::from_raw(data as *mut (Id, Id)) };
        if delegate != 0 && runtime::responds(delegate, "connection:didFailWithError:") {
            runtime::send(delegate, "connection:didFailWithError:", &[connection, offline_error()]);
        }
    });
    crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
}

extern "C" fn connection_init(this: Id, _: Sel, request: Id, delegate: Id) -> Id {
    property_set(this, "request", request as u64);
    report_failure(this, delegate);
    this
}

extern "C" fn connection_init_start(this: Id, _: Sel, request: Id, delegate: Id, start: u8) -> Id {
    property_set(this, "request", request as u64);
    property_set(this, "delegate", delegate as u64);
    if start != 0 {
        report_failure(this, delegate);
    }
    this
}

extern "C" fn connection_with_request(class: Class, s: Sel, request: Id, delegate: Id) -> Id {
    let object = runtime::alloc_instance(class);
    connection_init(object, s, request, delegate)
}

extern "C" fn connection_start(this: Id, _: Sel) {
    let delegate = property_get(this, "delegate").unwrap_or(0) as Id;
    report_failure(this, delegate);
}

extern "C" fn connection_send_sync(_: Class, _: Sel, request: Id, response: *mut Id, error: *mut Id) -> Id {
    let url = request_url(request, 0);
    crate::log!("web request {} refused: offline", string::describe(url));
    unsafe {
        if !response.is_null() {
            *response = 0;
        }
        if !error.is_null() {
            *error = offline_error();
        }
    }
    0
}

extern "C" fn connection_send_async(_: Class, _: Sel, request: Id, _queue: Id, handler: usize) {
    let url = request_url(request, 0);
    crate::log!("web request {} refused: offline", string::describe(url));
    let handler = crate::libsystem::mach::block_copy(handler as *mut _) as usize;
    let block = crate::fw::media::make_callback(handler, |handler| {
        let header = handler as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, Id, Id, Id) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(handler, 0, 0, offline_error());
    });
    crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
}

extern "C" fn cookie_storage(_: Class, _: Sel) -> Id {
    static STORAGE: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *STORAGE.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSHTTPCookieStorage")))
}

extern "C" fn no_cookies(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

extern "C" fn no_cookies_for(_this: Id, _: Sel, _url: Id) -> Id {
    collections::array_from(&[])
}

pub fn register(r: &mut Registry) {
    let ns_string = runtime::class_named("NSString");
    methods(
        ns_string,
        &[
            ("stringByAddingPercentEscapesUsingEncoding:", adding_percent_escapes as usize),
            ("stringByReplacingPercentEscapesUsingEncoding:", replacing_percent_escapes as usize),
            ("stringByRemovingPercentEncoding", removing_percent_encoding as usize),
        ],
    );
    r.function("CFURLCreateStringByAddingPercentEscapes", cf_adding_percent_escapes as usize);
    r.function("CFURLCreateStringByReplacingPercentEscapesUsingEncoding", cf_replacing_percent_escapes as usize);

    let request = define_class("NSURLRequest", "NSObject", 8);
    let mutable = define_class("NSMutableURLRequest", "NSURLRequest", 8);
    class_methods(
        request,
        &[
            ("requestWithURL:", request_with_url as usize),
            ("requestWithURL:cachePolicy:timeoutInterval:", request_with_url_policy as usize),
        ],
    );
    methods(
        request,
        &[
            ("initWithURL:", request_init_with_url as usize),
            ("initWithURL:cachePolicy:timeoutInterval:", request_init_with_url_policy as usize),
            ("URL", request_url as usize),
            ("HTTPMethod", request_method as usize),
            ("HTTPBody", request_body as usize),
            ("allHTTPHeaderFields", request_all_headers as usize),
            ("valueForHTTPHeaderField:", request_header as usize),
            ("timeoutInterval", request_timeout as usize),
            ("cachePolicy", request_cache_policy as usize),
            ("mainDocumentURL", request_main_document_url as usize),
            ("mutableCopyWithZone:", request_mutable_copy as usize),
        ],
    );
    methods(
        mutable,
        &[
            ("copyWithZone:", request_copy as usize),
            ("setURL:", request_set_url as usize),
            ("setHTTPMethod:", request_set_method as usize),
            ("setHTTPBody:", request_set_body as usize),
            ("setValue:forHTTPHeaderField:", request_set_header as usize),
            ("addValue:forHTTPHeaderField:", request_add_header as usize),
            ("setAllHTTPHeaderFields:", request_set_all_headers as usize),
            ("setCachePolicy:", request_set_cache_policy as usize),
            ("setTimeoutInterval:", request_set_timeout as usize),
            ("setMainDocumentURL:", request_set_main_document_url as usize),
            ("setHTTPShouldHandleCookies:", ignore_bool as usize),
        ],
    );

    let connection = define_class("NSURLConnection", "NSObject", 8);
    class_methods(
        connection,
        &[
            ("connectionWithRequest:delegate:", connection_with_request as usize),
            ("sendSynchronousRequest:returningResponse:error:", connection_send_sync as usize),
            ("sendAsynchronousRequest:queue:completionHandler:", connection_send_async as usize),
            ("canHandleRequest:", no_with as usize),
        ],
    );
    methods(
        connection,
        &[
            ("initWithRequest:delegate:", connection_init as usize),
            ("initWithRequest:delegate:startImmediately:", connection_init_start as usize),
            ("start", connection_start as usize),
            ("cancel", nothing as usize),
            ("scheduleInRunLoop:forMode:", ignore_id_id as usize),
            ("unscheduleFromRunLoop:forMode:", ignore_id_id as usize),
        ],
    );
    let cookies = define_class("NSHTTPCookieStorage", "NSObject", 8);
    class_methods(cookies, &[("sharedHTTPCookieStorage", cookie_storage as usize)]);
    methods(
        cookies,
        &[
            ("cookies", no_cookies as usize),
            ("cookiesForURL:", no_cookies_for as usize),
            ("deleteCookie:", ignore_id as usize),
            ("setCookie:", ignore_id as usize),
        ],
    );
    let _ = define_class("NSURLResponse", "NSObject", 8);
    let _ = define_class("NSHTTPURLResponse", "NSURLResponse", 8);
}

extern "C" fn nothing(_this: Id, _: Sel) {}
extern "C" fn no_with(_: Class, _: Sel, _value: Id) -> bool {
    false
}
