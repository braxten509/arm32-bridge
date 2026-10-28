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
// As Apple's NSURLConnectionInternal: -initWithInfo: retains the delegate
// and the connection (CFRetain), so a delegate only its connection holds
// lives until the last callback; the block of
// -_withConnectionDisconnectFromConnection: lets go of both after it (or at
// -cancel, after which the delegate hears nothing more).

fn hold_delegate(connection: Id, delegate: Id) {
    property_set(connection, "delegate", delegate as u64);
    if delegate != 0 {
        runtime::send(delegate, "retain", &[]);
        runtime::send(connection, "retain", &[]);
    }
}

/// Lets go of the delegate (once: a second failure or cancel finds none).
fn disconnect(connection: Id) {
    let delegate = property_get(connection, "delegate").unwrap_or(0) as Id;
    if delegate != 0 {
        property_set(connection, "delegate", 0);
        runtime::send(delegate, "release", &[]);
        runtime::send(connection, "release", &[]);
    }
}

fn report_failure(connection: Id) {
    let url = property_get(property_get(connection, "request").unwrap_or(0) as Id, "url").unwrap_or(0) as Id;
    crate::log!("web request {} refused: offline", string::describe(url));
    let block = crate::fw::media::make_callback(connection, |connection| {
        let delegate = property_get(connection, "delegate").unwrap_or(0) as Id;
        if delegate != 0 && runtime::responds(delegate, "connection:didFailWithError:") {
            runtime::send(delegate, "connection:didFailWithError:", &[connection, offline_error()]);
        }
        disconnect(connection);
    });
    crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
}

extern "C" fn connection_init(this: Id, _: Sel, request: Id, delegate: Id) -> Id {
    property_set(this, "request", request as u64);
    hold_delegate(this, delegate);
    report_failure(this);
    this
}

extern "C" fn connection_init_start(this: Id, _: Sel, request: Id, delegate: Id, start: u8) -> Id {
    property_set(this, "request", request as u64);
    hold_delegate(this, delegate);
    if start != 0 {
        report_failure(this);
    }
    this
}

extern "C" fn connection_cancel(this: Id, _: Sel) {
    disconnect(this);
}

extern "C" fn connection_with_request(class: Class, s: Sel, request: Id, delegate: Id) -> Id {
    let object = runtime::alloc_instance(class);
    connection_init(object, s, request, delegate)
}

/// -originalRequest / -currentRequest: the request it was made with (no
/// redirect is ever followed here).
extern "C" fn connection_request(this: Id, _: Sel) -> Id {
    property_get(this, "request").unwrap_or(0) as Id
}

extern "C" fn connection_start(this: Id, _: Sel) {
    // A cancelled (or finished) connection has let go of its delegate.
    if property_get(this, "delegate").unwrap_or(0) != 0 {
        report_failure(this);
    }
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
        // NSURLConnection lets go of its copy once the handler has run.
        crate::libsystem::mach::block_release(handler as *mut _);
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

// ----- NSURLResponse / NSHTTPURLResponse: no server answers here, but a game
// (or its test code) may make one; it keeps what it was made with -----

fn object_of(this: Id, key: &'static str) -> Id {
    property_get(this, key).unwrap_or(0) as Id
}

fn keep_copy(this: Id, key: &'static str, value: Id) {
    let value = if value == 0 { 0 } else { runtime::send(value, "copy", &[]) };
    crate::fw::uikit::property_set_object(this, key, value);
}

extern "C" fn response_init(this: Id, _: Sel, url: Id, mime: Id, length: isize, encoding: Id) -> Id {
    keep_copy(this, "URL", url);
    keep_copy(this, "MIMEType", mime);
    keep_copy(this, "textEncodingName", encoding);
    property_set(this, "expectedContentLength", length as u64);
    this
}

extern "C" fn response_url(this: Id, _: Sel) -> Id {
    object_of(this, "URL")
}

extern "C" fn response_mime_type(this: Id, _: Sel) -> Id {
    object_of(this, "MIMEType")
}

extern "C" fn response_text_encoding(this: Id, _: Sel) -> Id {
    object_of(this, "textEncodingName")
}

/// NSURLResponseUnknownLength (-1) unless it was given.
extern "C" fn response_expected_length(this: Id, _: Sel) -> i64 {
    property_get(this, "expectedContentLength").map_or(-1, |v| v as i64)
}

extern "C" fn response_suggested_filename(this: Id, _: Sel) -> Id {
    let url = object_of(this, "URL");
    let name = if url == 0 { String::new() } else { string::get(runtime::send(url, "lastPathComponent", &[])) };
    string::make(if name.is_empty() || name == "/" { "Unknown" } else { &name })
}

/// The HTTP form: status and header fields (MIME type and text encoding
/// read from its Content-Type, as Apple does).
extern "C" fn http_response_init(this: Id, _: Sel, url: Id, status: isize, _version: Id, headers: Id) -> Id {
    keep_copy(this, "URL", url);
    property_set(this, "statusCode", status as u64);
    let headers = if headers == 0 { collections::dictionary_from(&[]) } else { runtime::send(headers, "copy", &[]) };
    crate::fw::uikit::property_set_object(this, "allHeaderFields", headers);
    let content_type = collections::dictionary_pairs(headers)
        .into_iter()
        .find(|(k, _)| string::get(*k).eq_ignore_ascii_case("Content-Type"))
        .map(|(_, v)| string::get(v));
    if let Some(content_type) = content_type {
        let mut parts = content_type.split(';');
        let mime = parts.next().unwrap_or("").trim().to_string();
        crate::fw::uikit::property_set_object(this, "MIMEType", string::make(&mime));
        let charset = parts.find_map(|p| p.trim().strip_prefix("charset=").map(|c| c.trim_matches('"').to_string()));
        if let Some(charset) = charset {
            crate::fw::uikit::property_set_object(this, "textEncodingName", string::make(&charset));
        }
    }
    if let Some(length) = collections::dictionary_pairs(headers)
        .into_iter()
        .find(|(k, _)| string::get(*k).eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, v)| string::get(v).trim().parse::<i64>().ok())
    {
        property_set(this, "expectedContentLength", length as u64);
    }
    this
}

extern "C" fn http_status(this: Id, _: Sel) -> isize {
    property_get(this, "statusCode").unwrap_or(0) as isize
}

extern "C" fn http_all_headers(this: Id, _: Sel) -> Id {
    match object_of(this, "allHeaderFields") {
        0 => collections::dictionary_from(&[]),
        headers => headers,
    }
}

extern "C" fn request_set_pipelining(this: Id, _: Sel, value: u8) {
    property_set(this, "pipelining", u64::from(value != 0));
}

extern "C" fn request_pipelining(this: Id, _: Sel) -> bool {
    property_get(this, "pipelining").unwrap_or(0) != 0
}

// ----- NSURLAuthenticationChallenge: never sent here (connections fail
// offline before reaching a server); one a game makes keeps its parts -----

extern "C" fn challenge_init(this: Id, _: Sel, space: Id, credential: Id, failures: isize, response: Id, error: Id, sender: Id) -> Id {
    for (key, value) in [("protectionSpace", space), ("proposedCredential", credential), ("failureResponse", response), ("error", error), ("sender", sender)] {
        crate::fw::uikit::property_set_object(this, key, value);
    }
    property_set(this, "previousFailureCount", failures as u64);
    this
}

extern "C" fn challenge_space(this: Id, _: Sel) -> Id {
    object_of(this, "protectionSpace")
}
extern "C" fn challenge_proposed(this: Id, _: Sel) -> Id {
    object_of(this, "proposedCredential")
}
extern "C" fn challenge_response(this: Id, _: Sel) -> Id {
    object_of(this, "failureResponse")
}
extern "C" fn challenge_error(this: Id, _: Sel) -> Id {
    object_of(this, "error")
}
extern "C" fn challenge_sender(this: Id, _: Sel) -> Id {
    object_of(this, "sender")
}
extern "C" fn challenge_failures(this: Id, _: Sel) -> isize {
    property_get(this, "previousFailureCount").unwrap_or(0) as isize
}

/// The connection's answer to a challenge: its request has already failed
/// (offline), so there is nothing left to send the credential to.
extern "C" fn connection_answer_challenge(_this: Id, _: Sel, _credential: Id, _challenge: Id) {}

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
            ("setHTTPShouldUsePipelining:", request_set_pipelining as usize),
        ],
    );
    methods(request, &[("HTTPShouldUsePipelining", request_pipelining as usize)]);

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
            ("currentRequest", connection_request as usize),
            ("originalRequest", connection_request as usize),
            ("cancel", connection_cancel as usize),
            ("scheduleInRunLoop:forMode:", ignore_id_id as usize),
            ("unscheduleFromRunLoop:forMode:", ignore_id_id as usize),
            ("useCredential:forAuthenticationChallenge:", connection_answer_challenge as usize),
            ("continueWithoutCredentialForAuthenticationChallenge:", ignore_id as usize),
            ("cancelAuthenticationChallenge:", ignore_id as usize),
            ("performDefaultHandlingForAuthenticationChallenge:", ignore_id as usize),
            ("rejectProtectionSpaceAndContinueWithChallenge:", ignore_id as usize),
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
    let response = define_class("NSURLResponse", "NSObject", 8);
    methods(
        response,
        &[
            ("initWithURL:MIMEType:expectedContentLength:textEncodingName:", response_init as usize),
            ("URL", response_url as usize),
            ("MIMEType", response_mime_type as usize),
            ("textEncodingName", response_text_encoding as usize),
            ("expectedContentLength", response_expected_length as usize),
            ("suggestedFilename", response_suggested_filename as usize),
        ],
    );
    let http = define_class("NSHTTPURLResponse", "NSURLResponse", 8);
    methods(
        http,
        &[
            ("initWithURL:statusCode:HTTPVersion:headerFields:", http_response_init as usize),
            ("statusCode", http_status as usize),
            ("allHeaderFields", http_all_headers as usize),
        ],
    );
    let cache = define_class("NSURLCache", "NSObject", 8);
    class_methods(cache, &[("sharedURLCache", shared_url_cache as usize), ("setSharedURLCache:", set_shared_url_cache as usize)]);
    methods(
        cache,
        &[
            ("initWithMemoryCapacity:diskCapacity:diskPath:", url_cache_init as usize),
            ("memoryCapacity", url_cache_memory as usize),
            ("diskCapacity", url_cache_disk as usize),
            ("setMemoryCapacity:", url_cache_set_memory as usize),
            ("setDiskCapacity:", url_cache_set_disk as usize),
            ("currentMemoryUsage", url_cache_usage as usize),
            ("currentDiskUsage", url_cache_usage as usize),
            ("removeAllCachedResponses", nothing as usize),
        ],
    );
    let challenge = define_class("NSURLAuthenticationChallenge", "NSObject", 8);
    methods(
        challenge,
        &[
            ("initWithProtectionSpace:proposedCredential:previousFailureCount:failureResponse:error:sender:", challenge_init as usize),
            ("protectionSpace", challenge_space as usize),
            ("proposedCredential", challenge_proposed as usize),
            ("previousFailureCount", challenge_failures as usize),
            ("failureResponse", challenge_response as usize),
            ("error", challenge_error as usize),
            ("sender", challenge_sender as usize),
        ],
    );
}

extern "C" fn nothing(_this: Id, _: Sel) {}

// NSURLCache: capacities only. Nothing is ever loaded from the network
// (see the offline answers above), so no response is stored and usage is 0.
static SHARED_URL_CACHE: std::sync::Mutex<Id> = std::sync::Mutex::new(0);

extern "C" fn shared_url_cache(class: Class, _: Sel) -> Id {
    let mut shared = SHARED_URL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if *shared == 0 {
        // iOS's own: 512 KB in memory, 10 MB on disk.
        let cache = runtime::alloc_instance(class);
        *shared = url_cache_init(cache, 0, 512 * 1024, 10 * 1024 * 1024, 0);
    }
    *shared
}

extern "C" fn set_shared_url_cache(_: Class, _: Sel, cache: Id) {
    if cache != 0 {
        runtime::send(cache, "retain", &[]);
    }
    let old = std::mem::replace(&mut *SHARED_URL_CACHE.lock().unwrap_or_else(|e| e.into_inner()), cache);
    if old != 0 {
        runtime::send(old, "release", &[]);
    }
}

extern "C" fn url_cache_init(this: Id, _: Sel, memory: usize, disk: usize, _path: Id) -> Id {
    property_set(this, "memoryCapacity", memory as u64);
    property_set(this, "diskCapacity", disk as u64);
    this
}

extern "C" fn url_cache_memory(this: Id, _: Sel) -> usize {
    property_get(this, "memoryCapacity").unwrap_or(0) as usize
}

extern "C" fn url_cache_disk(this: Id, _: Sel) -> usize {
    property_get(this, "diskCapacity").unwrap_or(0) as usize
}

extern "C" fn url_cache_set_memory(this: Id, _: Sel, value: usize) {
    property_set(this, "memoryCapacity", value as u64);
}

extern "C" fn url_cache_set_disk(this: Id, _: Sel, value: usize) {
    property_set(this, "diskCapacity", value as u64);
}

extern "C" fn url_cache_usage(_this: Id, _: Sel) -> usize {
    0
}
extern "C" fn no_with(_: Class, _: Sel, _value: Id) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static COUNTS: Mutex<Vec<(Id, isize)>> = Mutex::new(Vec::new());
    static COUNTS_IN_CALLBACK: Mutex<Vec<(Id, isize)>> = Mutex::new(Vec::new());

    fn count_of(object: Id) -> isize {
        COUNTS.lock().unwrap().iter().find(|(o, _)| *o == object).map_or(0, |e| e.1)
    }

    fn change(object: Id, by: isize) {
        let mut counts = COUNTS.lock().unwrap();
        match counts.iter_mut().find(|(o, _)| *o == object) {
            Some(entry) => entry.1 += by,
            None => counts.push((object, by)),
        }
    }

    extern "C" fn counted_retain(this: Id, _: Sel) -> Id {
        change(this, 1);
        this
    }

    extern "C" fn counted_release(this: Id, _: Sel) {
        change(this, -1);
    }

    extern "C" fn failed(this: Id, _: Sel, _connection: Id, _error: Id) {
        COUNTS_IN_CALLBACK.lock().unwrap().push((this, count_of(this)));
    }

    /// As Apple's NSURLConnectionInternal (-initWithInfo: retains the
    /// delegate; the block of _withConnectionDisconnectFromConnection
    /// releases it after the last callback): a delegate only its connection
    /// holds hears the failure and is let go of after it; a cancelled
    /// connection tells its delegate nothing more and lets go of it.
    #[test]
    fn a_connection_holds_its_delegate_until_its_last_callback() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("WebHeldDelegate", "NSObject", 8);
        methods(
            class,
            &[("retain", counted_retain as usize), ("release", counted_release as usize), ("connection:didFailWithError:", failed as usize)],
        );
        let url = runtime::send(runtime::class_named("NSURL"), "URLWithString:", &[string::make("http://example.com/")]);
        let request = runtime::send(runtime::class_named("NSURLRequest"), "requestWithURL:", &[url]);
        let connect = |start: bool| {
            let delegate = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(delegate, 1);
            let connection = runtime::send(runtime::class_named("NSURLConnection"), "alloc", &[]);
            runtime::send(connection, "initWithRequest:delegate:startImmediately:", &[request, delegate, start as usize]);
            runtime::send(delegate, "release", &[]);
            (delegate, connection)
        };
        let (failing, _) = connect(true);
        let (cancelled, connection) = connect(false);
        // (Cancelled before its start: other tests run the main loop too.)
        runtime::send(connection, "cancel", &[]);
        runtime::send(connection, "start", &[]);
        for _ in 0..10 {
            crate::fw::runloop::run_main_loop_briefly();
            if COUNTS_IN_CALLBACK.lock().unwrap().iter().any(|e| e.0 == failing) {
                break;
            }
        }
        let heard = COUNTS_IN_CALLBACK.lock().unwrap().clone();
        assert_eq!(heard.iter().filter(|e| e.0 == failing).map(|e| e.1).collect::<Vec<_>>(), [1], "heard once, still held");
        assert!(!heard.iter().any(|e| e.0 == cancelled), "a cancelled connection says nothing");
        assert_eq!((count_of(failing), count_of(cancelled)), (0, 0), "both let go of");
    }

    #[test]
    fn url_caches_keep_their_capacities_and_the_shared_one_can_change() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSURLCache");
        let shared = runtime::send(class, "sharedURLCache", &[]);
        assert_ne!(shared, 0);
        assert_eq!(runtime::send(shared, "memoryCapacity", &[]), 512 * 1024, "iOS's default");
        let path = string::make("Cache.db");
        let cache = runtime::send(runtime::send(class, "alloc", &[]), "initWithMemoryCapacity:diskCapacity:diskPath:", &[4096, 1 << 20, path]);
        assert_eq!((runtime::send(cache, "memoryCapacity", &[]), runtime::send(cache, "diskCapacity", &[])), (4096, 1 << 20));
        runtime::send(class, "setSharedURLCache:", &[cache]);
        assert_eq!(runtime::send(class, "sharedURLCache", &[]), cache);
        runtime::send(cache, "removeAllCachedResponses", &[]);
        assert_eq!(runtime::send(cache, "currentMemoryUsage", &[]), 0);
        runtime::send(class, "setSharedURLCache:", &[shared]);
    }
}
