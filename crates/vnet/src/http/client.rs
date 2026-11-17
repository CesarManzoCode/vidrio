//! HTTP/1.1 client over `std::net` with persistent connections per origin.

use super::parse::{ResponseEvent, ResponseHead, ResponseParser};
use super::{HttpError, Origin, Request, Scheme};
use crate::inflate::{Decoder, Format};
use crate::resolve::Resolver;
use crate::transport::{TlsConnector, Transport};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Idle connections kept per origin.
const MAX_IDLE_PER_ORIGIN: usize = 6;
/// Idle connections older than this are not reused.
const IDLE_LIMIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Per connection attempt.
    pub connect: Duration,
    /// Longest wait for any single read or write.
    pub idle: Duration,
    /// Whole request, from connecting to the last body byte.
    pub total: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            total: None,
        }
    }
}

/// Progress of one request, in order: `Head`, any number of `Data`, then
/// `Done` or `Error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Head(ResponseHead),
    Data(Vec<u8>),
    Done,
    Error(HttpError),
}

/// A response read in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub head: ResponseHead,
    pub body: Vec<u8>,
}

struct Idle {
    conn: Box<dyn Transport>,
    since: Instant,
}

pub struct Client {
    resolver: Resolver,
    tls: Option<Arc<dyn TlsConnector>>,
    timeouts: Timeouts,
    pool: Mutex<HashMap<Origin, Vec<Idle>>>,
}

enum Failure {
    /// Nothing came back on a connection that may have been closed while idle.
    Stale(HttpError),
    Fatal(HttpError),
}

/// Content-Encoding decoders, applied in reverse header order.
struct ContentDecoder {
    stages: Vec<Decoder>,
    fed: bool,
}

impl ContentDecoder {
    fn for_head(head: &ResponseHead) -> Option<ContentDecoder> {
        let mut stages = Vec::new();
        for coding in head.headers.tokens("content-encoding") {
            match coding.to_ascii_lowercase().as_str() {
                "identity" => {}
                "gzip" | "x-gzip" => stages.push(Decoder::new(Format::Gzip)),
                "deflate" => stages.push(Decoder::new(Format::ZlibOrRaw)),
                // Unknown coding: hand the body over untouched.
                _ => return None,
            }
        }
        if stages.is_empty() {
            return None;
        }
        stages.reverse();
        Some(ContentDecoder { stages, fed: false })
    }

    fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, HttpError> {
        self.fed |= !data.is_empty();
        let mut cur = data.to_vec();
        for stage in &mut self.stages {
            let mut out = Vec::new();
            stage.push(&cur, &mut out)?;
            cur = out;
        }
        Ok(cur)
    }

    fn finish(&mut self) -> Result<(), HttpError> {
        if !self.fed {
            return Ok(());
        }
        for stage in &mut self.stages {
            stage.finish()?;
        }
        Ok(())
    }
}

impl Client {
    pub fn new(resolver: Resolver) -> Client {
        Client {
            resolver,
            tls: None,
            timeouts: Timeouts::default(),
            pool: Mutex::new(HashMap::new()),
        }
    }

    /// Client resolving names with `HOST_FILE` first.
    pub fn from_env() -> Client {
        Client::new(Resolver::from_env())
    }

    /// Enable `https` URLs.
    pub fn set_tls(&mut self, tls: Arc<dyn TlsConnector>) {
        self.tls = Some(tls);
    }

    pub fn set_timeouts(&mut self, timeouts: Timeouts) {
        self.timeouts = timeouts;
    }

    pub fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    /// Idle connections currently pooled for `origin`.
    pub fn idle_connections(&self, origin: &Origin) -> usize {
        self.pool.lock().unwrap().get(origin).map_or(0, |v| v.len())
    }

    /// Send `req` and read the whole response.
    pub fn fetch(&self, req: &Request) -> Result<Response, HttpError> {
        let mut head = None;
        let mut body = Vec::new();
        self.send(req, &|| false, &mut |e| match e {
            Event::Head(h) => head = Some(h),
            Event::Data(d) => body.extend(d),
            Event::Done | Event::Error(_) => {}
        })?;
        let head = head.ok_or(HttpError::ConnectionClosed)?;
        Ok(Response { head, body })
    }

    /// Send `req`, reporting `Head`, `Data` and `Done` to `sink` as they
    /// arrive. Errors are returned, not reported. `cancel` is polled between
    /// reads.
    pub fn send(
        &self,
        req: &Request,
        cancel: &dyn Fn() -> bool,
        sink: &mut dyn FnMut(Event),
    ) -> Result<(), HttpError> {
        let origin = req.url.origin();
        if origin.scheme == Scheme::Https && self.tls.is_none() {
            return Err(HttpError::UnsupportedScheme("https".to_string()));
        }
        let deadline = self.timeouts.total.map(|t| Instant::now() + t);
        let bytes = req.to_bytes();
        loop {
            if cancel() {
                return Err(HttpError::Cancelled);
            }
            let (mut conn, reused) = match self.take_idle(&origin) {
                Some(c) => (c, true),
                None => (self.connect(&origin, deadline)?, false),
            };
            match self.exchange(&mut *conn, req, &bytes, deadline, cancel, sink) {
                Ok(reusable) => {
                    if reusable {
                        self.put_idle(origin, conn);
                    }
                    return Ok(());
                }
                Err(Failure::Stale(_)) if reused && req.is_idempotent() => continue,
                Err(Failure::Stale(e)) | Err(Failure::Fatal(e)) => return Err(e),
            }
        }
    }

    fn take_idle(&self, origin: &Origin) -> Option<Box<dyn Transport>> {
        let mut pool = self.pool.lock().unwrap();
        let list = pool.get_mut(origin)?;
        while let Some(idle) = list.pop() {
            if idle.since.elapsed() < IDLE_LIMIT {
                return Some(idle.conn);
            }
        }
        None
    }

    fn put_idle(&self, origin: Origin, conn: Box<dyn Transport>) {
        let mut pool = self.pool.lock().unwrap();
        let list = pool.entry(origin).or_default();
        if list.len() < MAX_IDLE_PER_ORIGIN {
            list.push(Idle {
                conn,
                since: Instant::now(),
            });
        }
    }

    fn connect(
        &self,
        origin: &Origin,
        deadline: Option<Instant>,
    ) -> Result<Box<dyn Transport>, HttpError> {
        let addrs = self
            .resolver
            .resolve(&origin.host, origin.port)
            .map_err(|e| HttpError::Resolve(format!("{}: {e}", origin.host)))?;
        let mut last = HttpError::Resolve(origin.host.clone());
        for addr in addrs {
            let wait = limit(deadline, self.timeouts.connect)?;
            match TcpStream::connect_timeout(&addr, wait) {
                Ok(tcp) => {
                    let _ = tcp.set_nodelay(true);
                    return match origin.scheme {
                        Scheme::Http => Ok(Box::new(tcp) as Box<dyn Transport>),
                        Scheme::Https => {
                            let tls = self
                                .tls
                                .as_ref()
                                .ok_or_else(|| HttpError::UnsupportedScheme("https".into()))?;
                            tcp.set_read_timeout(Some(wait))?;
                            tcp.set_write_timeout(Some(wait))?;
                            tls.connect(tcp, &origin.host)
                                .map_err(|e| HttpError::Tls(e.to_string()))
                        }
                    };
                }
                Err(e) => last = e.into(),
            }
        }
        Err(last)
    }

    /// One request/response on `conn`; `Ok(true)` if the connection can be reused.
    fn exchange(
        &self,
        conn: &mut dyn Transport,
        req: &Request,
        bytes: &[u8],
        deadline: Option<Instant>,
        cancel: &dyn Fn() -> bool,
        sink: &mut dyn FnMut(Event),
    ) -> Result<bool, Failure> {
        let fatal = Failure::Fatal;
        let wait = limit(deadline, self.timeouts.idle).map_err(fatal)?;
        conn.set_write_timeout(Some(wait))
            .map_err(|e| fatal(e.into()))?;
        if let Err(e) = conn.write_all(bytes).and_then(|_| conn.flush()) {
            return Err(Failure::Stale(e.into()));
        }
        let mut parser = ResponseParser::new(req.method.eq_ignore_ascii_case("HEAD"));
        let mut decoder: Option<ContentDecoder> = None;
        let mut events = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        let mut received = false;
        loop {
            if cancel() {
                return Err(fatal(HttpError::Cancelled));
            }
            let wait = limit(deadline, self.timeouts.idle).map_err(fatal)?;
            conn.set_read_timeout(Some(wait))
                .map_err(|e| fatal(e.into()))?;
            let n = match conn.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let e = HttpError::from(e);
                    return Err(if received || e == HttpError::Timeout {
                        fatal(e)
                    } else {
                        Failure::Stale(e)
                    });
                }
            };
            if n == 0 {
                if !received {
                    return Err(Failure::Stale(HttpError::ConnectionClosed));
                }
                parser.feed_eof(&mut events).map_err(fatal)?;
                dispatch(&mut events, &mut decoder, sink).map_err(fatal)?;
                return Ok(false);
            }
            received = true;
            let used = parser.feed(&buf[..n], &mut events).map_err(fatal)?;
            dispatch(&mut events, &mut decoder, sink).map_err(fatal)?;
            if parser.is_done() {
                // Bytes past the response mean the server misbehaved; do not reuse.
                return Ok(parser.keep_alive() && used == n);
            }
        }
    }
}

/// Time allowed for the next operation: `step`, cut short by the deadline.
fn limit(deadline: Option<Instant>, step: Duration) -> Result<Duration, HttpError> {
    match deadline {
        None => Ok(step),
        Some(d) => {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                Err(HttpError::Timeout)
            } else {
                Ok(left.min(step))
            }
        }
    }
}

fn dispatch(
    events: &mut Vec<ResponseEvent>,
    decoder: &mut Option<ContentDecoder>,
    sink: &mut dyn FnMut(Event),
) -> Result<(), HttpError> {
    for event in events.drain(..) {
        match event {
            ResponseEvent::Head(mut head) => {
                *decoder = ContentDecoder::for_head(&head);
                head.content_decoded = decoder.is_some();
                sink(Event::Head(head));
            }
            ResponseEvent::Data(data) => {
                let data = match decoder {
                    Some(d) => d.push(&data)?,
                    None => data,
                };
                if !data.is_empty() {
                    sink(Event::Data(data));
                }
            }
            ResponseEvent::End => {
                if let Some(d) = decoder {
                    d.finish()?;
                }
                sink(Event::Done);
            }
        }
    }
    Ok(())
}
