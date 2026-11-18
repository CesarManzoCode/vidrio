//! Blobs and files (File API §3–§4), the blob URL store (File API §8)
//! and the entry lists of `FormData` with their `multipart/form-data`
//! encoding (HTML §4.10.21.8) and parsing (Fetch §5.2, `formData()`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::url::Origin;

/// The bytes and type of a `Blob`; a `File` also has a name and a
/// modification time.
#[derive(Debug)]
pub struct BlobData {
    pub bytes: Rc<[u8]>,
    pub type_: String,
    pub file: Option<FileInfo>,
}

#[derive(Clone, Debug)]
pub struct FileInfo {
    pub name: String,
    pub last_modified: i64,
}

impl BlobData {
    pub fn new(bytes: Vec<u8>, type_: &str) -> BlobData {
        BlobData {
            bytes: bytes.into(),
            type_: type_.to_string(),
            file: None,
        }
    }

    /// `slice(start, end, contentType)` with the relative positions
    /// already applied by [`relative_range`].
    pub fn slice(&self, start: usize, end: usize, type_: &str) -> BlobData {
        BlobData::new(self.bytes[start..end.max(start)].to_vec(), type_)
    }
}

/// The type of a `Blob` from `BlobPropertyBag.type`: lowercase, or empty
/// if it has a character outside U+0020–U+007E.
pub fn normalize_type(t: &str) -> String {
    if t.chars().all(|c| (' '..='~').contains(&c)) {
        t.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// The byte range of `slice(start, end)` on a blob of `size` bytes.
pub fn relative_range(size: usize, start: Option<i64>, end: Option<i64>) -> (usize, usize) {
    let size_i = size as i64;
    let rel = |v: i64| {
        if v < 0 {
            (size_i + v).max(0)
        } else {
            v.min(size_i)
        }
    };
    let from = start.map_or(0, rel);
    let to = end.map_or(size_i, rel);
    (from as usize, to.max(from) as usize)
}

/// "Convert line endings to native" (`endings: "native"`): `\n` on
/// Linux.
pub fn native_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

// ----- the blob URL store ----------------------------------------------------------

struct BlobUrlEntry {
    blob: Rc<BlobData>,
    #[allow(dead_code)]
    origin: Origin,
}

thread_local! {
    static STORE: RefCell<HashMap<String, BlobUrlEntry>> = RefCell::new(HashMap::new());
    static RANDOM: Cell<u64> = const { Cell::new(0) };
}

/// A pseudo-random number (xorshift seeded from the clock and the
/// address of a local): enough for UUIDs and multipart boundaries.
pub fn random_u64() -> u64 {
    RANDOM.with(|r| {
        let mut x = r.get();
        if x == 0 {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64);
            let local = 0u8;
            x = t ^ (&local as *const u8 as u64).rotate_left(32) | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        x
    })
}

/// A version 4 UUID.
pub fn uuid() -> String {
    let a = random_u64();
    let b = random_u64();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..].concat()
    )
}

/// `URL.createObjectURL(blob)`: a new `blob:` URL of `origin`.
pub fn add_url(blob: Rc<BlobData>, origin: &Origin) -> String {
    let serialized = origin.serialize();
    let url = format!("blob:{serialized}/{}", uuid());
    STORE.with(|s| {
        s.borrow_mut().insert(
            url.clone(),
            BlobUrlEntry {
                blob,
                origin: origin.clone(),
            },
        )
    });
    url
}

/// `URL.revokeObjectURL(url)`.
pub fn revoke_url(url: &str) {
    let key = url.split('#').next().unwrap_or(url);
    STORE.with(|s| s.borrow_mut().remove(key));
}

/// The blob of a `blob:` URL (without its fragment), if it is in the
/// store.
pub fn resolve_url(url: &str) -> Option<Rc<BlobData>> {
    let key = url.split('#').next().unwrap_or(url);
    STORE.with(|s| s.borrow().get(key).map(|e| e.blob.clone()))
}

// ----- entry lists -------------------------------------------------------------------

/// The value of a `FormData` entry.
#[derive(Clone, Debug)]
pub enum FormValue {
    Text(String),
    File(Rc<BlobData>),
}

pub type EntryList = Vec<(String, FormValue)>;

/// A file entry value for `append(name, blob, filename)`: a `File` named
/// `filename`, or `blob` itself if it already is a `File` and no name is
/// given ("create an entry", XHR §5.2).
pub fn file_entry(blob: &Rc<BlobData>, filename: Option<&str>) -> Rc<BlobData> {
    match (&blob.file, filename) {
        (Some(_), None) => blob.clone(),
        _ => Rc::new(BlobData {
            bytes: blob.bytes.clone(),
            type_: blob.type_.clone(),
            file: Some(FileInfo {
                name: filename.unwrap_or("blob").to_string(),
                last_modified: now_ms(),
            }),
        }),
    }
}

/// The current time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Escapes a name or filename of a `Content-Disposition` header.
fn escape_disposition(s: &str) -> String {
    s.replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('"', "%22")
}

/// CR, LF and CRLF become CRLF.
fn crlf(s: &str) -> String {
    native_line_endings(s).replace('\n', "\r\n")
}

/// The `multipart/form-data` encoding of `entries`: the body and its
/// `Content-Type`.
pub fn multipart_encode(entries: &EntryList) -> (Vec<u8>, String) {
    let boundary = format!("----vidrioFormBoundary{:016x}", random_u64());
    let mut body = Vec::new();
    for (name, value) in entries {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let name = escape_disposition(&crlf(name));
        match value {
            FormValue::Text(t) => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
                body.extend_from_slice(crlf(t).as_bytes());
            }
            FormValue::File(f) => {
                let filename = f.file.as_ref().map_or("blob", |i| i.name.as_str());
                let type_ = if f.type_.is_empty() {
                    "application/octet-stream"
                } else {
                    f.type_.as_str()
                };
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: {type_}\r\n\r\n",
                        escape_disposition(filename)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&f.bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// A parameter of a `Content-Disposition` header value.
fn disposition_param(header: &str, param: &str) -> Option<String> {
    for part in header.split(';').skip(1) {
        let (k, v) = part.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(param) {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            return Some(v.to_string());
        }
    }
    None
}

/// Parses a `multipart/form-data` body with `boundary`; `None` if it is
/// malformed.
pub fn multipart_parse(body: &[u8], boundary: &str) -> Option<EntryList> {
    let delimiter = format!("--{boundary}").into_bytes();
    let mut entries = Vec::new();
    let mut pos = find(body, &delimiter, 0)? + delimiter.len();
    loop {
        if body[pos..].starts_with(b"--") {
            return Some(entries);
        }
        if !body[pos..].starts_with(b"\r\n") {
            return None;
        }
        pos += 2;
        let head_end = find(body, b"\r\n\r\n", pos)?;
        let head = String::from_utf8_lossy(&body[pos..head_end]).into_owned();
        let content_start = head_end + 4;
        let mut next_delimiter = b"\r\n".to_vec();
        next_delimiter.extend_from_slice(&delimiter);
        let content_end = find(body, &next_delimiter, content_start)?;
        let content = &body[content_start..content_end];
        let mut name = None;
        let mut filename = None;
        let mut content_type = None;
        for line in head.split("\r\n") {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            match k.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => {
                    name = disposition_param(v, "name");
                    filename = disposition_param(v, "filename");
                }
                "content-type" => content_type = Some(v.trim().to_string()),
                _ => {}
            }
        }
        let name = name?;
        let value = match filename {
            Some(f) => FormValue::File(Rc::new(BlobData {
                bytes: content.to_vec().into(),
                type_: content_type.unwrap_or_else(|| "text/plain".to_string()),
                file: Some(FileInfo {
                    name: f,
                    last_modified: now_ms(),
                }),
            })),
            None => FormValue::Text(crate::encoding::utf8_decode_without_bom(content)),
        };
        entries.push((name, value));
        pos = content_end + next_delimiter.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_use_relative_positions() {
        assert_eq!(relative_range(10, Some(-3), None), (7, 10));
        assert_eq!(relative_range(10, Some(2), Some(-2)), (2, 8));
        assert_eq!(relative_range(10, Some(8), Some(2)), (8, 8));
        assert_eq!(relative_range(10, Some(20), None), (10, 10));
    }

    #[test]
    fn multipart_round_trip() {
        let file = Rc::new(BlobData {
            bytes: b"\x00bin".to_vec().into(),
            type_: "application/x-test".to_string(),
            file: Some(FileInfo {
                name: "f\".bin".to_string(),
                last_modified: 0,
            }),
        });
        let entries = vec![
            ("a".to_string(), FormValue::Text("x\ny".to_string())),
            ("f".to_string(), FormValue::File(file)),
        ];
        let (body, content_type) = multipart_encode(&entries);
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        let parsed = multipart_parse(&body, boundary).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(&parsed[0].1, FormValue::Text(t) if t == "x\r\ny"));
        match &parsed[1].1 {
            FormValue::File(f) => {
                assert_eq!(&*f.bytes, b"\x00bin");
                assert_eq!(f.type_, "application/x-test");
                assert_eq!(f.file.as_ref().unwrap().name, "f%22.bin");
            }
            _ => panic!("a file"),
        }
    }

    #[test]
    fn uuids_are_version_4() {
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(u, uuid());
    }
}
