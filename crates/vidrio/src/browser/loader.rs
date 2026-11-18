//! Fetching for the user agent and the loading of documents.
//!
//! [`Network`] runs HTTP(S) requests on the thread pool of vnet
//! (vault/CONTRACT.md §3.2) and answers `file:`, `data:` and `about:` URLs
//! itself. Responses come back to the main thread over a channel;
//! [`poll_network`] turns every response event into a task of the
//! networking task source, so callbacks always run on the event loop.
//! Redirects are followed here (at most 20).
//!
//! [`start_navigation`] loads the document of a navigation: it decides the
//! character encoding (BOM, the `charset` of `Content-Type`, the prescan of
//! the first 1024 bytes for `<meta>`, the parent's encoding, then
//! windows-1252; HTML §13.2.3), creates the document and its `Window` when
//! the response head arrives, and feeds the parser as bytes arrive.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use vjs::Agent;
use vnet::http::{Client, Event as NetEvent, FetchId, FetchMessage, Fetcher, Request};

use super::UserAgent;
use super::document::{self, Readiness};
use super::event_loop::{TaskSource, queue_task};
use super::navigable::ContextId;
use crate::dom::{DocRef, Document};
use crate::encoding::{BomHandling, Decoder, DecoderErrorMode, Encoding, bom_sniff};
use crate::mime::MimeType;
use crate::url::Url;

/// The head of a response.
#[derive(Clone, Debug)]
pub struct ResponseInfo {
    /// The URL after redirects.
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// What a fetch reports, in order: `Head`, `Data`…, then `Done` or
/// `Failed`.
#[derive(Clone, Debug)]
pub enum FetchEvent {
    Head(ResponseInfo),
    Data(Vec<u8>),
    Done,
    Failed(String),
}

/// A response read in full.
#[derive(Clone, Debug)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

type Callback = Rc<RefCell<Box<dyn FnMut(&mut Agent, FetchEvent)>>>;

struct Pending {
    url: Url,
    redirects: u32,
    document: Option<DocRef>,
    callback: Callback,
}

/// The network of the user agent.
pub struct Network {
    ca_cert: Option<PathBuf>,
    fetcher: RefCell<Option<Fetcher>>,
    tx: Sender<FetchMessage>,
    rx: Receiver<FetchMessage>,
    pending: RefCell<HashMap<u64, Pending>>,
    by_net: RefCell<HashMap<FetchId, u64>>,
    local: RefCell<VecDeque<(u64, FetchEvent)>>,
    next: Cell<u64>,
}

/// Redirects followed before a fetch fails.
pub const MAX_REDIRECTS: u32 = 20;

impl Network {
    pub fn new(ca_cert: Option<&Path>) -> Network {
        let (tx, rx) = mpsc::channel();
        Network {
            ca_cert: ca_cert.map(Path::to_path_buf),
            fetcher: RefCell::new(None),
            tx,
            rx,
            pending: RefCell::new(HashMap::new()),
            by_net: RefCell::new(HashMap::new()),
            local: RefCell::new(VecDeque::new()),
            next: Cell::new(1),
        }
    }

    /// Whether a fetch has not finished.
    pub fn has_pending(&self) -> bool {
        !self.pending.borrow().is_empty()
    }

    fn start_net(&self, request: Request) -> FetchId {
        let mut f = self.fetcher.borrow_mut();
        if f.is_none() {
            let mut client = Client::from_env();
            // Without a usable trust store `https` fails per request.
            let _ = client.enable_https(self.ca_cert.as_deref());
            *f = Some(Fetcher::new(
                Arc::new(client),
                vnet::http::MAX_NETWORK_THREADS,
            ));
        }
        f.as_ref()
            .expect("the fetcher exists")
            .start(request, self.tx.clone())
    }

    fn cancel_net(&self, id: FetchId) {
        if let Some(f) = self.fetcher.borrow().as_ref() {
            f.cancel(id);
        }
    }

    fn local_event(&self, id: u64, event: FetchEvent) {
        self.local.borrow_mut().push_back((id, event));
    }

    /// Starts (or restarts, after a redirect) request `id` for `url`.
    fn issue(&self, user_agent: &str, id: u64, url: &Url) {
        match url.scheme.as_str() {
            "http" | "https" => match vnet::http::Url::parse(&url.serialize(true)) {
                Ok(u) => {
                    let mut request = Request::new("GET", u);
                    request.headers.set("User-Agent", user_agent);
                    request.headers.set("Accept", "*/*");
                    let fid = self.start_net(request);
                    self.by_net.borrow_mut().insert(fid, id);
                }
                Err(e) => self.local_event(id, FetchEvent::Failed(e.to_string())),
            },
            "file" => self.local_file(id, url),
            "data" => match parse_data_url(url) {
                Some((mime, body)) => {
                    self.local_event(
                        id,
                        FetchEvent::Head(ResponseInfo {
                            url: url.clone(),
                            status: 200,
                            content_type: Some(mime.clone()),
                            headers: vec![("Content-Type".to_string(), mime)],
                        }),
                    );
                    self.local_event(id, FetchEvent::Data(body));
                    self.local_event(id, FetchEvent::Done);
                }
                None => self.local_event(id, FetchEvent::Failed("invalid data: URL".to_string())),
            },
            "about" => {
                let ok = matches!(url.serialize(true).as_str(), "about:blank" | "about:srcdoc");
                if ok {
                    self.local_event(
                        id,
                        FetchEvent::Head(ResponseInfo {
                            url: url.clone(),
                            status: 200,
                            content_type: Some("text/html;charset=utf-8".to_string()),
                            headers: Vec::new(),
                        }),
                    );
                    self.local_event(id, FetchEvent::Done);
                } else {
                    self.local_event(id, FetchEvent::Failed("unknown about: URL".to_string()));
                }
            }
            other => self.local_event(
                id,
                FetchEvent::Failed(format!("unsupported scheme {other}")),
            ),
        }
    }

    fn local_file(&self, id: u64, url: &Url) {
        let raw = crate::url::percent::percent_decode_str(&url.path_string());
        let path = PathBuf::from(String::from_utf8_lossy(&raw).into_owned());
        match std::fs::read(&path) {
            Ok(body) => {
                let mime = mime_for_path(&path).to_string();
                self.local_event(
                    id,
                    FetchEvent::Head(ResponseInfo {
                        url: url.clone(),
                        status: 200,
                        content_type: Some(mime.clone()),
                        headers: vec![("Content-Type".to_string(), mime)],
                    }),
                );
                self.local_event(id, FetchEvent::Data(body));
                self.local_event(id, FetchEvent::Done);
            }
            Err(e) => self.local_event(id, FetchEvent::Failed(format!("{}: {e}", path.display()))),
        }
    }
}

/// The MIME type of a local file, by its extension.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "xhtml" | "xht" => "application/xhtml+xml",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "css" => "text/css",
        "txt" => "text/plain",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The `data:` URL processor (Fetch §4.2): the MIME type and the body.
pub fn parse_data_url(url: &Url) -> Option<(String, Vec<u8>)> {
    let input = url.serialize(true);
    let rest = input.strip_prefix("data:")?;
    let (mime, body) = rest.split_once(',')?;
    let mut mime = mime
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_string();
    let bytes = crate::url::percent::percent_decode_str(body);
    let lower = mime.to_ascii_lowercase();
    let base64 = {
        let t = lower.trim_end_matches(' ');
        t.ends_with(";base64") || {
            // `;base64` may have spaces before it.
            t.strip_suffix("base64")
                .map(|s| s.trim_end_matches(' '))
                .is_some_and(|s| s.ends_with(';'))
        }
    };
    let body = if base64 {
        let cut = lower.rfind(';').unwrap_or(mime.len());
        mime.truncate(cut);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        super::members::forgiving_base64_decode(&text)?
    } else {
        bytes
    };
    let mime = if mime.starts_with(';') {
        format!("text/plain{mime}")
    } else {
        mime
    };
    let mime = match MimeType::parse(&mime) {
        Some(m) => m.serialize(),
        None => "text/plain;charset=US-ASCII".to_string(),
    };
    Some((mime, body))
}

/// Starts a fetch of `url` whose events go to `callback` as networking
/// tasks of `document` (`None`: tasks of no document).
pub fn fetch_streaming(
    ua: &UserAgent,
    url: Url,
    document: Option<DocRef>,
    callback: Box<dyn FnMut(&mut Agent, FetchEvent)>,
) -> u64 {
    let net = &ua.net;
    let id = net.next.get();
    net.next.set(id + 1);
    net.pending.borrow_mut().insert(
        id,
        Pending {
            url: url.clone(),
            redirects: 0,
            document,
            callback: Rc::new(RefCell::new(callback)),
        },
    );
    net.issue(&ua.options.user_agent, id, &url);
    id
}

/// Fetches `url` in full and calls `done` with the response (any status)
/// or the network error.
pub fn fetch(
    ua: &UserAgent,
    url: Url,
    document: Option<DocRef>,
    done: impl FnOnce(&mut Agent, Result<Response, String>) + 'static,
) {
    let mut head: Option<ResponseInfo> = None;
    let mut body = Vec::new();
    let mut done = Some(done);
    fetch_streaming(
        ua,
        url,
        document,
        Box::new(move |agent, event| match event {
            FetchEvent::Head(h) => head = Some(h),
            FetchEvent::Data(d) => body.extend_from_slice(&d),
            FetchEvent::Done => {
                if let Some(f) = done.take() {
                    let result = match head.take() {
                        Some(h) => Ok(Response {
                            url: h.url,
                            status: h.status,
                            content_type: h.content_type,
                            headers: h.headers,
                            body: std::mem::take(&mut body),
                        }),
                        None => Err("no response".to_string()),
                    };
                    f(agent, result);
                }
            }
            FetchEvent::Failed(e) => {
                if let Some(f) = done.take() {
                    f(agent, Err(e));
                }
            }
        }),
    );
}

/// Queues the task that hands `event` of request `id` to its callback.
fn deliver(ua: &UserAgent, id: u64, event: FetchEvent) {
    let net = &ua.net;
    let last = matches!(event, FetchEvent::Done | FetchEvent::Failed(_));
    let (callback, document) = {
        let mut pending = net.pending.borrow_mut();
        let Some(p) = pending.get(&id) else {
            return;
        };
        let r = (p.callback.clone(), p.document.clone());
        if last {
            pending.remove(&id);
        }
        r
    };
    queue_task(TaskSource::Networking, document, move |agent| {
        let mut cb = callback.borrow_mut();
        (*cb)(agent, event);
    });
}

/// Handles one message of the network threads.
fn handle_message(ua: &UserAgent, message: FetchMessage) {
    let net = &ua.net;
    let Some(id) = net.by_net.borrow().get(&message.id).copied() else {
        return;
    };
    match message.event {
        NetEvent::Head(head) => {
            let redirect = matches!(head.status, 301 | 302 | 303 | 307 | 308);
            let location = head.headers.get("location").map(str::to_string);
            let (url, redirects) = match net.pending.borrow().get(&id) {
                Some(p) => (p.url.clone(), p.redirects),
                None => return,
            };
            if let (true, Some(location)) = (redirect, location) {
                net.by_net.borrow_mut().remove(&message.id);
                net.cancel_net(message.id);
                if redirects >= MAX_REDIRECTS {
                    deliver(ua, id, FetchEvent::Failed("too many redirects".to_string()));
                    return;
                }
                match Url::parse(&location, Some(&url)) {
                    Ok(next) => {
                        if let Some(p) = net.pending.borrow_mut().get_mut(&id) {
                            p.url = next.clone();
                            p.redirects += 1;
                        }
                        net.issue(&ua.options.user_agent, id, &next);
                    }
                    Err(_) => deliver(ua, id, FetchEvent::Failed("bad redirect".to_string())),
                }
                return;
            }
            let headers: Vec<(String, String)> = head
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let content_type = head.headers.get("content-type").map(str::to_string);
            deliver(
                ua,
                id,
                FetchEvent::Head(ResponseInfo {
                    url,
                    status: head.status,
                    content_type,
                    headers,
                }),
            );
        }
        NetEvent::Data(bytes) => deliver(ua, id, FetchEvent::Data(bytes)),
        NetEvent::Done => {
            net.by_net.borrow_mut().remove(&message.id);
            deliver(ua, id, FetchEvent::Done);
        }
        NetEvent::Error(e) => {
            net.by_net.borrow_mut().remove(&message.id);
            deliver(ua, id, FetchEvent::Failed(e.to_string()));
        }
    }
}

/// Turns what the network answered into networking tasks. With
/// `timeout`, waits up to that long for a network message when nothing
/// was ready.
pub fn poll_network(ua: &UserAgent, timeout: Option<Duration>) {
    let net = &ua.net;
    let mut delivered = false;
    loop {
        let item = net.local.borrow_mut().pop_front();
        match item {
            Some((id, event)) => {
                delivered = true;
                deliver(ua, id, event);
            }
            None => break,
        }
    }
    let mut first = true;
    loop {
        let wait = timeout.filter(|_| first && !delivered && !net.by_net.borrow().is_empty());
        first = false;
        let message = match wait {
            Some(t) => net.rx.recv_timeout(t).ok(),
            None => net.rx.try_recv().ok(),
        };
        let Some(message) = message else {
            break;
        };
        handle_message(ua, message);
        // Redirects to local URLs answer at once.
        loop {
            let item = net.local.borrow_mut().pop_front();
            match item {
                Some((id, event)) => deliver(ua, id, event),
                None => break,
            }
        }
    }
}

// ----- encodings ---------------------------------------------------------------------

/// The `charset` parameter of a `Content-Type` value, as an encoding.
pub fn charset_of(content_type: Option<&str>) -> Option<Encoding> {
    let m = MimeType::parse(content_type?)?;
    Encoding::for_label(m.parameter("charset")?)
}

/// Decodes a script body: a BOM wins, then the `charset` of its
/// `Content-Type`, then `fallback`.
pub fn decode_script(body: &[u8], content_type: Option<&str>, fallback: Encoding) -> String {
    let encoding = charset_of(content_type).unwrap_or(fallback);
    crate::encoding::decode(body, encoding).0
}

fn is_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn starts_with_ci(b: &[u8], prefix: &[u8]) -> bool {
    b.len() >= prefix.len() && b[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// "Get an attribute" of the prescan (HTML §13.2.3.3).
fn get_attribute(b: &[u8], pos: &mut usize) -> Option<(String, String)> {
    let len = b.len();
    while *pos < len && (is_ws(b[*pos]) || b[*pos] == b'/') {
        *pos += 1;
    }
    if *pos >= len || b[*pos] == b'>' {
        return None;
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    loop {
        if *pos >= len {
            return Some((String::from_utf8_lossy(&name).into_owned(), String::new()));
        }
        let c = b[*pos];
        if c == b'=' && !name.is_empty() {
            *pos += 1;
            break;
        }
        if is_ws(c) {
            while *pos < len && is_ws(b[*pos]) {
                *pos += 1;
            }
            if *pos < len && b[*pos] == b'=' {
                *pos += 1;
                break;
            }
            return Some((String::from_utf8_lossy(&name).into_owned(), String::new()));
        }
        if c == b'/' || c == b'>' {
            return Some((String::from_utf8_lossy(&name).into_owned(), String::new()));
        }
        name.push(c.to_ascii_lowercase());
        *pos += 1;
    }
    while *pos < len && is_ws(b[*pos]) {
        *pos += 1;
    }
    if *pos >= len {
        return Some((String::from_utf8_lossy(&name).into_owned(), String::new()));
    }
    let c = b[*pos];
    if c == b'"' || c == b'\'' {
        *pos += 1;
        loop {
            if *pos >= len {
                return None;
            }
            let d = b[*pos];
            *pos += 1;
            if d == c {
                break;
            }
            value.push(d.to_ascii_lowercase());
        }
    } else if c == b'>' {
        return Some((String::from_utf8_lossy(&name).into_owned(), String::new()));
    } else {
        while *pos < len && !is_ws(b[*pos]) && b[*pos] != b'>' {
            value.push(b[*pos].to_ascii_lowercase());
            *pos += 1;
        }
    }
    Some((
        String::from_utf8_lossy(&name).into_owned(),
        String::from_utf8_lossy(&value).into_owned(),
    ))
}

/// "Extract a character encoding from a meta element" (HTML §2.5.4.4)
/// from the value of `content`.
pub fn extract_meta_charset(content: &str) -> Option<String> {
    let b = content.as_bytes();
    let mut pos = 0;
    loop {
        let rest = &b[pos..];
        let i =
            (0..rest.len().saturating_sub(6)).find(|&i| starts_with_ci(&rest[i..], b"charset"))?;
        pos += i + 7;
        while pos < b.len() && is_ws(b[pos]) {
            pos += 1;
        }
        if pos >= b.len() || b[pos] != b'=' {
            continue;
        }
        pos += 1;
        while pos < b.len() && is_ws(b[pos]) {
            pos += 1;
        }
        if pos >= b.len() {
            return None;
        }
        let q = b[pos];
        if q == b'"' || q == b'\'' {
            let end = b[pos + 1..].iter().position(|&c| c == q)?;
            return Some(String::from_utf8_lossy(&b[pos + 1..pos + 1 + end]).into_owned());
        }
        let end = b[pos..]
            .iter()
            .position(|&c| is_ws(c) || c == b';')
            .unwrap_or(b.len() - pos);
        return Some(String::from_utf8_lossy(&b[pos..pos + end]).into_owned());
    }
}

/// The encoding a `<meta>` names, as the prescan returns it.
fn meta_encoding(e: Encoding) -> Encoding {
    match e {
        Encoding::Utf16Be | Encoding::Utf16Le => Encoding::Utf8,
        Encoding::XUserDefined => Encoding::Windows1252,
        e => e,
    }
}

/// "Prescan a byte stream to determine its encoding" over the first 1024
/// bytes of `input`.
pub fn prescan(input: &[u8]) -> Option<Encoding> {
    let b = &input[..input.len().min(1024)];
    let len = b.len();
    let mut pos = 0;
    while pos < len {
        let rest = &b[pos..];
        if rest.starts_with(b"<!--") {
            pos = find(b, pos + 2, b"-->")? + 3;
            continue;
        }
        if starts_with_ci(rest, b"<meta") && rest.get(5).is_some_and(|&c| is_ws(c) || c == b'/') {
            pos += 6;
            let mut seen: Vec<String> = Vec::new();
            let mut got_pragma = false;
            let mut need_pragma: Option<bool> = None;
            let mut charset: Option<Encoding> = None;
            while let Some((name, value)) = get_attribute(b, &mut pos) {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                match name.as_str() {
                    "http-equiv" => {
                        if value == "content-type" {
                            got_pragma = true;
                        }
                    }
                    "content" => {
                        if charset.is_none() {
                            if let Some(c) = extract_meta_charset(&value) {
                                charset = Encoding::for_label(&c);
                                need_pragma = Some(true);
                            }
                        }
                    }
                    "charset" => {
                        charset = Encoding::for_label(&value);
                        need_pragma = Some(false);
                    }
                    _ => {}
                }
            }
            let decided = match need_pragma {
                None => false,
                Some(true) => got_pragma,
                Some(false) => true,
            };
            if decided {
                if let Some(c) = charset {
                    return Some(meta_encoding(c));
                }
            }
            continue;
        }
        let tag = (rest.len() > 1 && rest[0] == b'<' && rest[1].is_ascii_alphabetic())
            || (rest.len() > 2 && rest.starts_with(b"</") && rest[2].is_ascii_alphabetic());
        if tag {
            pos += 1;
            while pos < len && !is_ws(b[pos]) && b[pos] != b'>' {
                pos += 1;
            }
            while get_attribute(b, &mut pos).is_some() {}
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            pos = find(b, pos + 1, b">")? + 1;
            continue;
        }
        pos += 1;
    }
    None
}

/// The encoding of an HTML document (HTML §13.2.3.2) from its first
/// bytes, the `Content-Type` and the parent document's encoding.
pub fn sniff_encoding(
    bytes: &[u8],
    content_type: Option<&str>,
    parent: Option<Encoding>,
) -> Encoding {
    if let Some((e, _)) = bom_sniff(bytes) {
        return e;
    }
    if let Some(e) = charset_of(content_type) {
        return e;
    }
    if let Some(e) = prescan(bytes) {
        return e;
    }
    if let Some(e) = parent {
        return e;
    }
    Encoding::Windows1252
}

// ----- navigation ----------------------------------------------------------------------

/// A navigation waiting for its response.
pub struct NavigationRequest {
    pub context: ContextId,
    pub navigation: u64,
    pub url: Url,
    pub replace: bool,
    pub srcdoc: Option<String>,
}

/// How a response body becomes the parser's input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Html,
    /// Text shown in a `pre` element.
    Text,
    /// Not shown (images, binaries): an empty document.
    Empty,
}

/// The loading state of a navigation's document.
struct DocumentLoad {
    context: ContextId,
    navigation: u64,
    replace: bool,
    doc: Option<DocRef>,
    generation: u64,
    kind: BodyKind,
    content_type: Option<String>,
    buffered: Vec<u8>,
    decoder: Option<Decoder>,
    cancelled: bool,
}

/// Whether the navigation `nav` of context `id` is still the ongoing one.
fn still_ongoing(ua: &UserAgent, id: ContextId, nav: u64) -> bool {
    ua.context(id).is_some_and(|c| c.navigation.get() == nav)
}

/// Creates the document of a navigation response and makes it the active
/// document of the context; returns it with its parser's generation.
fn create_document(
    agent: &mut Agent,
    ua: &UserAgent,
    context: ContextId,
    url: &Url,
    content_type: &str,
    replace: bool,
    srcdoc: bool,
) -> Option<(DocRef, u64)> {
    let ctx = ua.context(context)?;
    ctx.navigation.set(0);
    let mut d = Document::new_html();
    d.url = url.clone();
    d.origin = if srcdoc || url.href() == "about:blank" {
        ctx.container_document()
            .map(|p| p.borrow().origin.clone())
            .unwrap_or_else(|| url.origin())
    } else {
        url.origin()
    };
    d.content_type = content_type.to_string();
    d.is_srcdoc = srcdoc;
    let doc: DocRef = Rc::new(RefCell::new(d));
    super::navigable::replace_document(agent, ua, &ctx, doc.clone(), replace);
    let info = ua.doc_info(&doc)?;
    info.readiness.set(Readiness::Loading);
    if let Some(parent) = ctx.container_document() {
        *info.referrer.borrow_mut() = parent.borrow().url.serialize(true);
    }
    document::create_parser(ua, &doc, &info, false);
    Some((doc.clone(), info.parser_generation.get()))
}

/// Escapes text for the `pre` of a text document.
fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

impl DocumentLoad {
    /// Decides the encoding once enough bytes arrived (or the body ended)
    /// and feeds what was buffered.
    fn pump(&mut self, agent: &mut Agent, last: bool) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        if self.kind == BodyKind::Empty {
            self.buffered.clear();
            if last {
                document::end_parser_input(agent, &doc, self.generation);
            }
            return;
        }
        if self.decoder.is_none() {
            if self.buffered.len() < 1024 && !last {
                return;
            }
            let parent = super::ua()
                .and_then(|ua| ua.context(self.context))
                .and_then(|c| c.container_document())
                .filter(|p| p.borrow().origin.same_origin(&doc.borrow().origin))
                .map(|p| p.borrow().encoding);
            let encoding = sniff_encoding(&self.buffered, self.content_type.as_deref(), parent);
            self.decoder = Some(Decoder::new(
                encoding,
                DecoderErrorMode::Replacement,
                BomHandling::Sniff,
            ));
        }
        let bytes = std::mem::take(&mut self.buffered);
        let mut text = String::new();
        if let Some(decoder) = &mut self.decoder {
            let _ = decoder.decode(&bytes, last, &mut text);
            doc.borrow_mut().encoding = decoder.encoding();
        }
        if self.kind == BodyKind::Text {
            text = escape_text(&text);
        }
        if !text.is_empty() {
            document::feed_parser(agent, &doc, self.generation, &text);
        }
        if last {
            document::end_parser_input(agent, &doc, self.generation);
        }
    }
}

/// The body kind of a response with `Content-Type` `content_type`.
fn body_kind(content_type: Option<&str>) -> (BodyKind, String) {
    let mime = content_type.and_then(MimeType::parse);
    let essence = mime
        .as_ref()
        .map_or("text/html".to_string(), MimeType::essence);
    let kind = match essence.as_str() {
        "text/html" => BodyKind::Html,
        // There is no XML parser yet: XML documents parse as HTML
        // (LIMITATIONS.md).
        e if e.ends_with("+xml") || e == "application/xml" || e == "text/xml" => BodyKind::Html,
        e if e.starts_with("text/") || e == "application/json" || e == "application/javascript" => {
            BodyKind::Text
        }
        _ => BodyKind::Empty,
    };
    (kind, essence)
}

/// Starts loading the document of a navigation: `srcdoc` and
/// `about:blank` documents in a task, everything else from the network.
pub fn start_navigation(_agent: &mut Agent, ua: &UserAgent, request: NavigationRequest) {
    let NavigationRequest {
        context,
        navigation,
        url,
        replace,
        srcdoc,
    } = request;
    if let Some(text) = srcdoc {
        queue_task(TaskSource::Navigation, None, move |agent| {
            let Some(ua) = super::ua() else {
                return;
            };
            if !still_ongoing(&ua, context, navigation) {
                return;
            }
            if let Some((doc, generation)) =
                create_document(agent, &ua, context, &url, "text/html", replace, true)
            {
                doc.borrow_mut().encoding = Encoding::Utf8;
                document::feed_parser(agent, &doc, generation, &text);
                document::end_parser_input(agent, &doc, generation);
            }
        });
        return;
    }
    if url.href() == "about:blank" {
        queue_task(TaskSource::Navigation, None, move |agent| {
            let Some(ua) = super::ua() else {
                return;
            };
            if !still_ongoing(&ua, context, navigation) {
                return;
            }
            if let Some((doc, generation)) =
                create_document(agent, &ua, context, &url, "text/html", replace, false)
            {
                doc.borrow_mut().encoding = Encoding::Utf8;
                document::end_parser_input(agent, &doc, generation);
            }
        });
        return;
    }
    let mut load = DocumentLoad {
        context,
        navigation,
        replace,
        doc: None,
        generation: 0,
        kind: BodyKind::Html,
        content_type: None,
        buffered: Vec::new(),
        decoder: None,
        cancelled: false,
    };
    let fallback_url = url.clone();
    fetch_streaming(
        ua,
        url,
        None,
        Box::new(move |agent, event| {
            if load.cancelled {
                return;
            }
            let Some(ua) = super::ua() else {
                return;
            };
            match event {
                FetchEvent::Head(head) => {
                    if !still_ongoing(&ua, load.context, load.navigation) {
                        load.cancelled = true;
                        return;
                    }
                    let (kind, essence) = body_kind(head.content_type.as_deref());
                    load.kind = kind;
                    load.content_type = head.content_type.clone();
                    match create_document(
                        agent,
                        &ua,
                        load.context,
                        &head.url,
                        &essence,
                        load.replace,
                        false,
                    ) {
                        Some((doc, generation)) => {
                            if kind == BodyKind::Text {
                                document::feed_parser(agent, &doc, generation, "<pre>");
                            }
                            load.doc = Some(doc);
                            load.generation = generation;
                        }
                        None => load.cancelled = true,
                    }
                }
                FetchEvent::Data(bytes) => {
                    load.buffered.extend_from_slice(&bytes);
                    load.pump(agent, false);
                }
                FetchEvent::Done => load.pump(agent, true),
                FetchEvent::Failed(_) => {
                    if load.doc.is_none() {
                        // A network error: an empty document in its place.
                        if !still_ongoing(&ua, load.context, load.navigation) {
                            return;
                        }
                        if let Some((doc, generation)) = create_document(
                            agent,
                            &ua,
                            load.context,
                            &fallback_url,
                            "text/html",
                            load.replace,
                            false,
                        ) {
                            load.doc = Some(doc);
                            load.generation = generation;
                            load.kind = BodyKind::Empty;
                        }
                    }
                    load.pump(agent, true);
                }
            }
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prescan_finds_meta_charset() {
        assert_eq!(prescan(b"<meta charset=utf-8>"), Some(Encoding::Utf8));
        assert_eq!(
            prescan(b"<!-- <meta charset=koi8-r> --><META CHARSET='iso-8859-2'>"),
            Some(Encoding::Iso8859_2)
        );
        assert_eq!(
            prescan(b"<meta http-equiv=Content-Type content='text/html; charset=windows-1251'>"),
            Some(Encoding::Windows1251)
        );
        // `content` without the pragma does not count.
        assert_eq!(
            prescan(b"<meta content='text/html; charset=windows-1251'>"),
            None
        );
        assert_eq!(prescan(b"<meta charset=utf-16le>"), Some(Encoding::Utf8));
        assert_eq!(prescan(b"<p title='<meta charset=utf-8>'>"), None);
    }

    #[test]
    fn meta_content_charset_extraction() {
        assert_eq!(
            extract_meta_charset("text/html; charset=utf-8").as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            extract_meta_charset("charset = \"x\"").as_deref(),
            Some("x")
        );
        assert_eq!(
            extract_meta_charset("charsetx; charset=y;z").as_deref(),
            Some("y")
        );
        assert_eq!(extract_meta_charset("text/html"), None);
    }

    #[test]
    fn encoding_sniffing_order() {
        assert_eq!(
            sniff_encoding(
                b"\xEF\xBB\xBF<meta charset=koi8-r>",
                Some("text/html; charset=gbk"),
                None
            ),
            Encoding::Utf8
        );
        assert_eq!(
            sniff_encoding(
                b"<meta charset=koi8-r>",
                Some("text/html; charset=gbk"),
                None
            ),
            Encoding::Gbk
        );
        assert_eq!(
            sniff_encoding(b"<meta charset=koi8-r>", None, None),
            Encoding::Koi8R
        );
        assert_eq!(
            sniff_encoding(b"<p>", None, Some(Encoding::ShiftJis)),
            Encoding::ShiftJis
        );
        assert_eq!(sniff_encoding(b"<p>", None, None), Encoding::Windows1252);
    }

    #[test]
    fn data_urls() {
        let u = Url::parse("data:text/html;base64,PHA+", None).unwrap();
        assert_eq!(
            parse_data_url(&u),
            Some(("text/html".to_string(), b"<p>".to_vec()))
        );
        let u = Url::parse("data:,a%20b", None).unwrap();
        assert_eq!(
            parse_data_url(&u),
            Some(("text/plain;charset=US-ASCII".to_string(), b"a b".to_vec()))
        );
    }

    #[test]
    fn local_mime_types() {
        assert_eq!(mime_for_path(Path::new("/a/b.HTML")), "text/html");
        assert_eq!(mime_for_path(Path::new("m.mjs")), "text/javascript");
        assert_eq!(mime_for_path(Path::new("x")), "application/octet-stream");
    }
}
