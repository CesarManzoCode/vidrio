//! The HTTP/1.1 side of the WebDriver server: one thread (vault/CONTRACT.md
//! §3.2) that accepts connections on a local port, reads requests from
//! every open connection without blocking, hands each one to the main
//! thread through a channel and writes back the answer. Commands run one
//! at a time: while the main thread works on one, the server waits.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// The largest request head the server reads.
const MAX_HEAD: usize = 64 * 1024;
/// The largest request body the server reads.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// A request as the main thread sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: String,
    /// The path, without the query.
    pub path: String,
    pub body: Vec<u8>,
}

/// An answer: status and JSON body.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// A request and where its answer goes.
pub struct Message {
    pub request: Request,
    pub reply: Sender<Response>,
}

/// The receiving end on the main thread: messages, and a flag the server
/// raises when it sends one (so the event loop can stop early).
pub struct Commands {
    pub rx: Receiver<Message>,
    pub wake: Arc<AtomicBool>,
}

/// A running server thread.
pub struct Server {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Stops the thread; the main thread's [`Commands`] then disconnects.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Binds `addr` (for example `127.0.0.1:4444`, or port 0 for any) and
/// starts the server thread.
pub fn start(addr: &str) -> io::Result<(Server, Commands)> {
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let (tx, rx) = mpsc::channel();
    let wake = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let wake = wake.clone();
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("webdriver".to_string())
            .spawn(move || serve(listener, tx, wake, stop))?
    };
    Ok((
        Server {
            addr: local,
            stop,
            thread: Some(thread),
        },
        Commands { rx, wake },
    ))
}

struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
    closed: bool,
}

/// What [`parse_request`] found at the start of a buffer.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// More bytes are needed.
    Incomplete,
    /// A request, the bytes it used and whether the client asked to close.
    Request(Request, usize, bool),
    /// A request that cannot be read: the status to answer before closing.
    Bad(u16),
}

/// Reads one request from the start of `buf`.
pub fn parse_request(buf: &[u8]) -> Parsed {
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > MAX_HEAD {
            Parsed::Bad(431)
        } else {
            Parsed::Incomplete
        };
    };
    let Ok(head) = std::str::from_utf8(&buf[..head_end]) else {
        return Parsed::Bad(400);
    };
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or("");
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Parsed::Bad(400);
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() {
        return Parsed::Bad(400);
    }
    let mut length = 0usize;
    let mut close = version == "HTTP/1.0";
    for h in lines {
        let Some((name, value)) = h.split_once(':') else {
            return Parsed::Bad(400);
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            match value.parse::<usize>() {
                Ok(n) if n <= MAX_BODY => length = n,
                Ok(_) => return Parsed::Bad(413),
                Err(_) => return Parsed::Bad(400),
            }
        } else if name.eq_ignore_ascii_case("connection") {
            close = value.eq_ignore_ascii_case("close");
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            // WebDriver clients send a Content-Length.
            return Parsed::Bad(411);
        }
    }
    let body_start = head_end + 4;
    if buf.len() < body_start + length {
        return Parsed::Incomplete;
    }
    let path = target.split(['?', '#']).next().unwrap_or("").to_string();
    Parsed::Request(
        Request {
            method: method.to_string(),
            path,
            body: buf[body_start..body_start + length].to_vec(),
        },
        body_start + length,
        close,
    )
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

/// The bytes of an answer.
pub fn format_response(r: &Response, close: bool) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n",
        r.status,
        reason(r.status),
        r.body.len()
    );
    if close {
        out.push_str("Connection: close\r\n");
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(r.body.as_bytes());
    bytes
}

fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let r = stream.write_all(bytes).and_then(|_| stream.flush());
    stream.set_nonblocking(true)?;
    r
}

fn serve(listener: TcpListener, tx: Sender<Message>, wake: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    let mut conns: Vec<Conn> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut busy = false;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() {
                        let _ = stream.set_nodelay(true);
                        conns.push(Conn {
                            stream,
                            buf: Vec::new(),
                            closed: false,
                        });
                    }
                    busy = true;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        for c in conns.iter_mut() {
            loop {
                match c.stream.read(&mut chunk) {
                    Ok(0) => {
                        c.closed = true;
                        break;
                    }
                    Ok(n) => {
                        c.buf.extend_from_slice(&chunk[..n]);
                        busy = true;
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        c.closed = true;
                        break;
                    }
                }
            }
            // Answer every complete request in order.
            loop {
                match parse_request(&c.buf) {
                    Parsed::Incomplete => break,
                    Parsed::Bad(status) => {
                        let r = Response {
                            status,
                            body: String::new(),
                        };
                        let _ = write_all(&mut c.stream, &format_response(&r, true));
                        c.closed = true;
                        break;
                    }
                    Parsed::Request(request, used, close) => {
                        c.buf.drain(..used);
                        busy = true;
                        let (reply_tx, reply_rx) = mpsc::channel();
                        let answer = if tx
                            .send(Message {
                                request,
                                reply: reply_tx,
                            })
                            .is_ok()
                        {
                            wake.store(true, Ordering::SeqCst);
                            reply_rx.recv().ok()
                        } else {
                            None
                        };
                        let r = answer.unwrap_or(Response {
                            status: 500,
                            body: r#"{"value":{"error":"unknown error","message":"the browser is gone","stacktrace":""}}"#.to_string(),
                        });
                        if write_all(&mut c.stream, &format_response(&r, close)).is_err() || close {
                            c.closed = true;
                            break;
                        }
                    }
                }
            }
        }
        conns.retain(|c| !c.closed);
        if !busy {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_request_with_a_body() {
        let raw = b"POST /session HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}GET";
        match parse_request(raw) {
            Parsed::Request(r, used, close) => {
                assert_eq!(r.method, "POST");
                assert_eq!(r.path, "/session");
                assert_eq!(r.body, b"{}");
                assert_eq!(used, raw.len() - 3);
                assert!(!close);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn waits_for_the_whole_body_and_strips_the_query() {
        assert_eq!(
            parse_request(b"POST /a HTTP/1.1\r\nContent-Length: 5\r\n\r\n{}"),
            Parsed::Incomplete
        );
        match parse_request(b"GET /status?x=1 HTTP/1.1\r\nConnection: close\r\n\r\n") {
            Parsed::Request(r, _, close) => {
                assert_eq!(r.path, "/status");
                assert!(close);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_request(b"nonsense\r\n\r\n"), Parsed::Bad(400));
    }

    #[test]
    fn formats_a_json_response() {
        let r = Response {
            status: 404,
            body: "{}".to_string(),
        };
        let text = String::from_utf8(format_response(&r, false)).unwrap();
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.ends_with("\r\n\r\n{}"));
    }
}
