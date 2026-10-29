//! Offline answers: a game's requests to its servers are answered on the phone
//! (docs/offline-plan.md). The converter turns the game's Apache HTTP client into Game
//! Bridge's (fix 7, offline/java) and adds the game's offline pack next to the bridge
//! (`libarm32_bridge_offline.so`). Each request comes here through
//! `Offline.nativeAnswer` (jni.rs); the pack's `pack.txt` says which handler answers which
//! host:
//!
//! ```text
//! # host                               handler  argument
//! fs.teamlava.com                      storm8   storm8/
//! cdn-content-https-s8.akamaized.net   files    cdn/cdn-content-https-s8.akamaized.net
//! ```
//!
//! - `files`: the request's path (query left out) is a file in the pack under the argument.
//! - `storm8`: the Storm8/TeamLava game server (storm8.rs), its recorded data under the
//!   argument, the player's progress in the game's files folder.
//!
//! A host without a line gets no answer: the game sees the network as off.

#[cfg(target_os = "android")]
mod jni;
pub mod storm8;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The pack file's first bytes.
pub const PACK_MAGIC: &[u8; 8] = b"GBOFFLN1";
/// The pack's file name next to the bridge (named lib*.so so Android unpacks it).
pub const PACK_FILE: &str = "libarm32_bridge_offline.so";

/// One request, as the game's HTTP client made it.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub method: String,
    /// Path and query ("/api.php?method=login"), or a whole URL for clients that send one.
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The path without the query ("/api.php"), also when the client sent a whole URL.
    pub fn path(&self) -> &str {
        let uri = self.uri.split_once("://").map_or(self.uri.as_str(), |(_, rest)| rest.find('/').map_or("/", |at| &rest[at..]));
        uri.split(['?', '#']).next().unwrap_or("/")
    }

    /// The query's parameters, decoded.
    pub fn query(&self) -> HashMap<String, String> {
        self.uri.split_once('?').map(|(_, query)| form_decode(query.split('#').next().unwrap_or(""))).unwrap_or_default()
    }

    /// The body's parameters when it is a form (application/x-www-form-urlencoded).
    pub fn form(&self) -> HashMap<String, String> {
        form_decode(&String::from_utf8_lossy(&self.body))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

/// One answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(content_type: &str, body: Vec<u8>) -> Response {
        Response { status: 200, content_type: content_type.into(), body }
    }

    pub fn json(text: String) -> Response {
        Response::ok("application/json", text.into_bytes())
    }

    pub fn not_found() -> Response {
        Response { status: 404, content_type: "text/plain".into(), body: b"not in the offline pack".to_vec() }
    }

    /// The whole answer as HTTP/1.1 bytes, what Offline.answer hands to Java.
    pub fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            404 => "Not Found",
            _ => "Error",
        };
        let mut out = format!("HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n", self.status, self.content_type, self.body.len()).into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// `a=1&b=x%20y` -> {a: 1, b: x y} (`+` is a space).
pub fn form_decode(text: &str) -> HashMap<String, String> {
    text.split('&').filter(|pair| !pair.is_empty()).map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(key), percent_decode(value))
    }).collect()
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                Some(byte) => {
                    out.push(byte);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The offline pack: files by name ("pack.txt", "cdn/host/fs/images/x.png", ...).
///
/// Layout: PACK_MAGIC, u32 file count, then per file u32 name length, the name (UTF-8),
/// u64 offset and u64 size (from the start of the pack); then the files' bytes. Little-endian.
pub struct Pack {
    data: Vec<u8>,
    files: HashMap<String, (usize, usize)>,
}

impl Pack {
    pub fn parse(data: Vec<u8>) -> Result<Pack, String> {
        if data.len() < 12 || &data[..8] != PACK_MAGIC {
            return Err("not an offline pack".into());
        }
        let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        let u64_at = |at: usize| data.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize);
        let count = u32_at(8).unwrap();
        let mut files = HashMap::new();
        let mut at = 12;
        for _ in 0..count {
            let length = u32_at(at).ok_or("cut short")?;
            let name = data.get(at + 4..at + 4 + length).ok_or("cut short")?;
            let name = String::from_utf8(name.to_vec()).map_err(|_| "file name isn't UTF-8")?;
            at += 4 + length;
            let (offset, size) = (u64_at(at).ok_or("cut short")?, u64_at(at + 8).ok_or("cut short")?);
            at += 16;
            if offset.checked_add(size).is_none_or(|end| end > data.len()) {
                return Err(format!("{name} lies outside the pack"));
            }
            files.insert(name, (offset, size));
        }
        Ok(Pack { data, files })
    }

    pub fn file(&self, name: &str) -> Option<&[u8]> {
        self.files.get(name).map(|&(offset, size)| &self.data[offset..offset + size])
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Which handler answers a host.
#[derive(Debug, Clone, PartialEq)]
enum Handler {
    Files(String),
    Storm8(String),
}

/// A game's offline answers: its pack and where its progress is kept.
pub struct Offline {
    pack: Pack,
    hosts: HashMap<String, Handler>,
    state_dir: PathBuf,
    storm8: std::sync::Mutex<HashMap<String, storm8::Server>>,
}

impl Offline {
    pub fn new(pack: Pack, state_dir: PathBuf) -> Result<Offline, String> {
        let config = String::from_utf8_lossy(pack.file("pack.txt").ok_or("the pack has no pack.txt")?).into_owned();
        let mut hosts = HashMap::new();
        for line in config.lines().map(|line| line.split('#').next().unwrap().trim()).filter(|line| !line.is_empty()) {
            let words: Vec<&str> = line.split_whitespace().collect();
            let argument = words.get(2).map_or(String::new(), |word| word.to_string());
            let handler = match words.get(1) {
                Some(&"files") => Handler::Files(argument),
                Some(&"storm8") => Handler::Storm8(argument),
                other => return Err(format!("pack.txt: unknown handler {other:?} in {line:?}")),
            };
            hosts.insert(words[0].to_ascii_lowercase(), handler);
        }
        Ok(Offline { pack, hosts, state_dir, storm8: Default::default() })
    }

    pub fn hosts(&self) -> usize {
        self.hosts.len()
    }

    pub fn files(&self) -> usize {
        self.pack.len()
    }

    /// The answer to `request`, or None when its host has none (no network, as far as the
    /// game can tell).
    pub fn answer(&self, request: &Request) -> Option<Response> {
        match self.hosts.get(&request.host.to_ascii_lowercase())? {
            Handler::Files(folder) => {
                // Empty path parts dropped: Farm Story asks for /fs/images/items//item1234.s8i.
                let path: Vec<&str> = request.path().split('/').filter(|part| !part.is_empty()).collect();
                let name = format!("{}/{}", folder.trim_end_matches('/'), path.join("/"));
                Some(self.pack.file(&name).map_or_else(Response::not_found, |bytes| Response::ok(content_type(&name), bytes.to_vec())))
            }
            Handler::Storm8(folder) => {
                let mut servers = self.storm8.lock().unwrap_or_else(|e| e.into_inner());
                let key = request.host.to_ascii_lowercase();
                if !servers.contains_key(&key) {
                    let state = self.state_dir.join(format!("{key}.json"));
                    servers.insert(key.clone(), storm8::Server::new(&self.pack, folder, state).map_err(|e| crate::log::line(&format!("[offline] {key}: {e}"))).ok()?);
                }
                Some(servers.get_mut(&key).unwrap().answer(request))
            }
        }
    }
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json",
        "txt" => "text/plain",
        "html" => "text/html",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A pack in the layout Pack::parse reads (the converter writes the same).
    pub fn pack_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut index = Vec::new();
        let header = 12 + files.iter().map(|(name, _)| 4 + name.len() + 16).sum::<usize>();
        let mut offset = header;
        for (name, bytes) in files {
            index.extend_from_slice(&(name.len() as u32).to_le_bytes());
            index.extend_from_slice(name.as_bytes());
            index.extend_from_slice(&(offset as u64).to_le_bytes());
            index.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            offset += bytes.len();
        }
        let mut out = PACK_MAGIC.to_vec();
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        out.extend(index);
        for (_, bytes) in files {
            out.extend_from_slice(bytes);
        }
        out
    }

    fn request(host: &str, uri: &str) -> Request {
        Request { scheme: "http".into(), host: host.into(), port: -1, method: "GET".into(), uri: uri.into(), ..Default::default() }
    }

    #[test]
    fn files_come_from_the_pack_by_path() {
        let pack = pack_of(&[("pack.txt", b"cdn.example.com files cdn/cdn.example.com\n"), ("cdn/cdn.example.com/img/a.png", b"PNG")]);
        let offline = Offline::new(Pack::parse(pack).unwrap(), std::env::temp_dir()).unwrap();
        let found = offline.answer(&request("CDN.example.com", "/img/a.png?v=1020")).unwrap();
        assert_eq!((found.status, found.content_type.as_str(), found.body.as_slice()), (200, "image/png", &b"PNG"[..]));
        let whole_url = offline.answer(&request("cdn.example.com", "http://cdn.example.com/img/a.png")).unwrap();
        assert_eq!(whole_url.status, 200, "a whole URL as the request target");
        let doubled = offline.answer(&request("cdn.example.com", "/img//a.png?v=1020")).unwrap();
        assert_eq!(doubled.status, 200, "a doubled slash (Farm Story's texture links)");
        assert_eq!(offline.answer(&request("cdn.example.com", "/img/b.png")).unwrap().status, 404);
        assert!(offline.answer(&request("other.example.com", "/img/a.png")).is_none(), "unknown host: no network");
    }

    #[test]
    fn broken_packs_are_refused() {
        assert!(Pack::parse(b"GBOFFLN1\x01\0\0\0".to_vec()).is_err(), "cut short");
        let mut outside = pack_of(&[("a", b"xyz")]);
        let size_at = 12 + 4 + 1 + 8;
        outside[size_at..size_at + 8].copy_from_slice(&1000u64.to_le_bytes());
        assert!(Pack::parse(outside).is_err(), "a file past the end");
        assert!(Offline::new(Pack::parse(pack_of(&[("pack.txt", b"h nonsense")])).unwrap(), std::env::temp_dir()).is_err());
    }

    #[test]
    fn forms_and_answers() {
        let form = form_decode("p=%7B%22a%22%3A1%7D&x=a+b&empty=");
        assert_eq!((form["p"].as_str(), form["x"].as_str(), form["empty"].as_str()), ("{\"a\":1}", "a b", ""));
        assert_eq!(percent_decode("100%"), "100%", "a lone percent stays");
        let mut get = request("h", "/api.php?cv=1&method=login");
        assert_eq!((get.path(), get.query()["method"].as_str()), ("/api.php", "login"));
        get.headers.push(("Accept-Encoding".into(), "gzip".into()));
        assert_eq!(get.header("accept-encoding"), Some("gzip"));
        let bytes = Response::json("{}".into()).to_bytes();
        assert_eq!(bytes, b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
    }
}
