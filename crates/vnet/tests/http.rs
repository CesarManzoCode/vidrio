//! inflate/deflate and HTTP/1.1 client and server pieces.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use vnet::deflate::{Level, deflate, gzip_compress, zlib_compress};
use vnet::http::{
    Client, Event, FetchMessage, Fetcher, Request, RequestReader, ResponseEvent, ResponseParser,
    ServerResponse, Url, encode_chunk,
};
use vnet::inflate::{Decoder, Format, InflateError, gzip_decompress, inflate, zlib_decompress};
use vnet::resolve::{HostsFile, Resolver};

// Vectors produced by zlib 1.3 (Python `zlib`/`gzip`, windowBits -15 for raw).

/// Stored block (BTYPE 00).
const STORED: &[u8] = &[
    0x01, 0x0e, 0x00, 0xf1, 0xff, 0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x73, 0x74, 0x6f, 0x72,
    0x65, 0x64, 0x21,
];
/// Fixed Huffman block (BTYPE 01) with back-references.
const FIXED: &[u8] = &[
    0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xf0, 0x40, 0xa1, 0xd2, 0x32, 0x2b, 0x52, 0x53, 0x14,
    0x01,
];
/// Dynamic Huffman block (BTYPE 10).
const DYNAMIC: &[u8] = &[
    0xb5, 0xca, 0xc7, 0x11, 0x80, 0x20, 0x10, 0x00, 0xc0, 0x56, 0xae, 0x02, 0xc7, 0x1c, 0xfa, 0xa0,
    0x01, 0x89, 0x82, 0xe1, 0x14, 0x24, 0x48, 0xf5, 0xda, 0x84, 0xef, 0x5d, 0xb2, 0x08, 0xb8, 0xbc,
    0x66, 0x2b, 0x50, 0x8b, 0xf1, 0x00, 0x89, 0x09, 0x8c, 0xdf, 0x4f, 0x07, 0x18, 0x84, 0x85, 0xfb,
    0xe3, 0x6d, 0xce, 0x0f, 0x70, 0x54, 0x05, 0x90, 0xdf, 0xf2, 0x4c, 0x19, 0x17, 0x52, 0x2d, 0xda,
    0xac, 0xdb, 0x7e, 0xe0, 0x79, 0x59, 0x77, 0xfb, 0x10, 0xd3, 0x93, 0xcb, 0xaa, 0x6e, 0xda, 0xae,
    0x1f, 0xc6, 0xe9, 0x05,
];
const ZLIB_EMPTY: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
const ZLIB_ABC: &[u8] = &[
    0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0x06, 0x00, 0x02, 0x4d, 0x01, 0x27,
];
const GZIP: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x4b, 0xaf, 0xca, 0x2c, 0x50, 0x28,
    0x48, 0xac, 0xcc, 0xc9, 0x4f, 0x4c, 0xe1, 0x02, 0x00, 0xec, 0x4d, 0xc2, 0xd7, 0x0d, 0x00, 0x00,
    0x00,
];

fn dynamic_plain() -> Vec<u8> {
    let mut v = b"The quick brown fox jumps over the lazy dog. ".repeat(3);
    v.extend_from_slice(b"abcdefghijklmnopqrstuvwxyz0123456789");
    v
}

/// Decode `data` feeding it in pieces of `step` bytes.
fn decode_in_steps(format: Format, data: &[u8], step: usize) -> Result<Vec<u8>, InflateError> {
    let mut d = Decoder::new(format);
    let mut out = Vec::new();
    for piece in data.chunks(step) {
        d.push(piece, &mut out)?;
    }
    d.finish()?;
    Ok(out)
}

#[test]
fn inflate_reference_vectors() {
    assert_eq!(inflate(STORED).unwrap(), b"Hello, stored!");
    assert_eq!(inflate(FIXED).unwrap(), b"Hello, Hello, Hello, fixed!");
    assert_eq!(inflate(DYNAMIC).unwrap(), dynamic_plain());
    assert_eq!(zlib_decompress(ZLIB_EMPTY).unwrap(), b"");
    assert_eq!(zlib_decompress(ZLIB_ABC).unwrap(), b"abc");
    assert_eq!(gzip_decompress(GZIP).unwrap(), b"gzip payload\n");
    // Block types as stored in the first byte.
    assert_eq!((STORED[0] >> 1) & 3, 0);
    assert_eq!((FIXED[0] >> 1) & 3, 1);
    assert_eq!((DYNAMIC[0] >> 1) & 3, 2);
}

#[test]
fn inflate_streams_split_anywhere() {
    let cases: [(Format, &[u8], Vec<u8>); 6] = [
        (Format::Raw, STORED, b"Hello, stored!".to_vec()),
        (Format::Raw, FIXED, b"Hello, Hello, Hello, fixed!".to_vec()),
        (Format::Raw, DYNAMIC, dynamic_plain()),
        (Format::Zlib, ZLIB_ABC, b"abc".to_vec()),
        (Format::Gzip, GZIP, b"gzip payload\n".to_vec()),
        (Format::ZlibOrRaw, ZLIB_ABC, b"abc".to_vec()),
    ];
    for (format, data, plain) in &cases {
        for step in 1..=data.len() {
            assert_eq!(
                &decode_in_steps(*format, data, step).unwrap(),
                plain,
                "{format:?} step {step}"
            );
        }
        // Two pieces split at every position.
        for cut in 0..=data.len() {
            let mut d = Decoder::new(*format);
            let mut out = Vec::new();
            d.push(&data[..cut], &mut out).unwrap();
            d.push(&data[cut..], &mut out).unwrap();
            d.finish().unwrap();
            assert_eq!(&out, plain, "{format:?} cut {cut}");
        }
    }
    // HTTP `deflate` sent without the zlib wrapper.
    assert_eq!(
        decode_in_steps(Format::ZlibOrRaw, FIXED, 3).unwrap(),
        b"Hello, Hello, Hello, fixed!"
    );
}

#[test]
fn inflate_gzip_optional_fields_and_members() {
    // Same member with FNAME and FCOMMENT set.
    let mut named = vec![0x1f, 0x8b, 0x08, 0x18, 0, 0, 0, 0, 0, 0xff];
    named.extend_from_slice(b"payload.txt\0a comment\0");
    named.extend_from_slice(&GZIP[10..]);
    assert_eq!(gzip_decompress(&named).unwrap(), b"gzip payload\n");
    assert_eq!(
        decode_in_steps(Format::Gzip, &named, 1).unwrap(),
        b"gzip payload\n"
    );
    // Concatenated members decode to the concatenation.
    let mut two = GZIP.to_vec();
    two.extend_from_slice(&named);
    assert_eq!(
        gzip_decompress(&two).unwrap(),
        b"gzip payload\ngzip payload\n"
    );
}

#[test]
fn inflate_rejects_bad_input() {
    // BFINAL=1, BTYPE=11.
    assert_eq!(inflate(&[0x07]), Err(InflateError::InvalidBlockType));
    // Stored block whose NLEN is not the complement of LEN.
    assert_eq!(
        inflate(&[0x01, 0x05, 0x00, 0x00, 0x00]),
        Err(InflateError::StoredLengthMismatch)
    );
    // Fixed block starting with a match and no history: BFINAL=1, BTYPE=01,
    // symbol 257 (0000001, length 3), distance code 0 (distance 1).
    assert_eq!(inflate(&[0x03, 0x02]), Err(InflateError::DistanceTooFar));
    let mut bad = ZLIB_ABC.to_vec();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(zlib_decompress(&bad), Err(InflateError::ChecksumMismatch));
    assert_eq!(
        zlib_decompress(&ZLIB_ABC[..ZLIB_ABC.len() - 2]),
        Err(InflateError::Truncated)
    );
    assert_eq!(
        inflate(&FIXED[..FIXED.len() - 1]),
        Err(InflateError::Truncated)
    );
    let mut bad_gzip = GZIP.to_vec();
    let len = bad_gzip.len();
    bad_gzip[len - 1] = 1;
    assert_eq!(
        gzip_decompress(&bad_gzip),
        Err(InflateError::LengthMismatch)
    );
}

/// Deterministic pseudo-random bytes.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

fn samples() -> Vec<Vec<u8>> {
    let text =
        b"<p class=\"note\">vidrio renders this paragraph again and again.</p>\n".repeat(2000);
    let mut mixed = noise(5000, 7);
    mixed.extend_from_slice(&text[..40000]);
    mixed.extend(noise(3000, 9));
    mixed.extend(std::iter::repeat_n(0u8, 300_000));
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"abc".to_vec(),
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
        text,
        noise(70_000, 1),
        mixed,
    ]
}

#[test]
fn deflate_round_trips() {
    for data in samples() {
        for level in [0u8, 1, 6, 9] {
            let raw = deflate(&data, Level(level));
            assert_eq!(
                inflate(&raw).unwrap(),
                data,
                "raw level {level} len {}",
                data.len()
            );
            let z = zlib_compress(&data, Level(level));
            assert_eq!(zlib_decompress(&z).unwrap(), data, "zlib level {level}");
            let g = gzip_compress(&data, Level(level));
            assert_eq!(gzip_decompress(&g).unwrap(), data, "gzip level {level}");
            assert_eq!(
                decode_in_steps(Format::Zlib, &z, 7).unwrap(),
                data,
                "zlib level {level} in pieces"
            );
        }
    }
}

#[test]
fn deflate_compresses() {
    let text = b"The same sentence, over and over, compresses well. ".repeat(1000);
    let out = deflate(&text, Level(6));
    assert!(
        out.len() * 20 < text.len(),
        "{} -> {}",
        text.len(),
        out.len()
    );
    // Incompressible input costs little more than stored blocks.
    let n = noise(100_000, 3);
    assert!(deflate(&n, Level(9)).len() <= n.len() + n.len() / 100);
    // Level 0 is stored blocks only.
    let stored = deflate(&text, Level(0));
    assert_eq!(stored.len(), text.len() + 5);
}

const CHUNKED_BODY: &[u8] = b"hello, chunked world: abcdefghijklmnopqrstuvwxyz";

fn chunked_wire() -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
Transfer-Encoding: chunked\r\nX-Folded: a\r\n b\r\n\r\n",
    );
    w.extend_from_slice(b"5\r\nhello\r\n");
    w.extend_from_slice(b"10;name=value\r\n, chunked world:\r\n");
    w.extend_from_slice(b"1B\r\n abcdefghijklmnopqrstuvwxyz\r\n");
    w.extend_from_slice(b"0\r\nX-Trailer: done\r\n\r\n");
    w
}

/// Feed the response in the given pieces; returns body, head and consumed count.
fn parse_pieces(pieces: &[&[u8]]) -> (Vec<u8>, u16, usize, bool) {
    let mut p = ResponseParser::new(false);
    let mut events = Vec::new();
    let mut consumed = 0;
    for piece in pieces {
        consumed += p.feed(piece, &mut events).unwrap();
    }
    let mut body = Vec::new();
    let mut status = 0;
    let mut ended = false;
    for e in events {
        match e {
            ResponseEvent::Head(h) => {
                assert_eq!(h.headers.get("x-folded"), Some("a b"));
                status = h.status;
            }
            ResponseEvent::Data(d) => body.extend(d),
            ResponseEvent::End => ended = true,
        }
    }
    (body, status, consumed, ended && p.keep_alive())
}

#[test]
fn chunked_response_split_anywhere() {
    let wire = chunked_wire();
    let (body, status, consumed, ok) = parse_pieces(&[&wire]);
    assert_eq!(
        (body.as_slice(), status, consumed, ok),
        (CHUNKED_BODY, 200, wire.len(), true)
    );
    for cut in 0..=wire.len() {
        let (body, status, _, ok) = parse_pieces(&[&wire[..cut], &wire[cut..]]);
        assert_eq!(
            (body.as_slice(), status, ok),
            (CHUNKED_BODY, 200, true),
            "cut {cut}"
        );
    }
    for a in (0..wire.len()).step_by(7) {
        for b in (a..=wire.len()).step_by(5) {
            let (body, _, _, ok) = parse_pieces(&[&wire[..a], &wire[a..b], &wire[b..]]);
            assert_eq!((body.as_slice(), ok), (CHUNKED_BODY, true), "cuts {a} {b}");
        }
    }
    let bytes: Vec<&[u8]> = wire.chunks(1).collect();
    assert_eq!(parse_pieces(&bytes).0, CHUNKED_BODY);
    // Bytes after the last chunk are left for the next response.
    let mut more = wire.clone();
    more.extend_from_slice(b"HTTP/1.1 204 No Content\r\n\r\n");
    let (_, _, consumed, _) = parse_pieces(&[&more]);
    assert_eq!(consumed, wire.len());
}

#[test]
fn response_framing() {
    let run = |head_request: bool, wire: &[u8], eof: bool| {
        let mut p = ResponseParser::new(head_request);
        let mut events = Vec::new();
        p.feed(wire, &mut events).unwrap();
        if eof {
            p.feed_eof(&mut events).unwrap();
        }
        let body: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::Data(d) => Some(d.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        (body, p.is_done(), p.keep_alive())
    };
    assert_eq!(
        run(
            false,
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef",
            false
        ),
        (b"abc".to_vec(), true, true)
    );
    assert_eq!(
        run(false, b"HTTP/1.0 200 OK\r\n\r\nuntil close", true),
        (b"until close".to_vec(), true, false)
    );
    assert_eq!(
        run(
            true,
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n",
            false
        ),
        (Vec::new(), true, true)
    );
    assert_eq!(
        run(
            false,
            b"HTTP/1.1 304 Not Modified\nConnection: close\n\n",
            false
        ),
        (Vec::new(), true, false)
    );
    let mut p = ResponseParser::new(false);
    let mut events = Vec::new();
    assert!(
        p.feed(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
            &mut events
        )
        .is_err()
    );
    let mut p = ResponseParser::new(false);
    p.feed(
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc",
        &mut events,
    )
    .unwrap();
    assert!(p.feed_eof(&mut events).is_err());
}

fn page_text() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..400 {
        writeln!(v, "<li>item {i} of the list served by the test server</li>").unwrap();
    }
    v
}

/// Test server: counts connections and serves `/gzip` (chunked, gzip),
/// `/deflate` (zlib), `/raw-deflate`, `/plain` and `/close`.
fn start_server() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&connections);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || serve(stream));
        }
    });
    (port, connections)
}

fn serve(stream: TcpStream) {
    let mut reader = RequestReader::new(stream);
    while let Ok(Some(req)) = reader.read_request() {
        let head = req.method == "HEAD";
        let text = page_text();
        let w = reader.get_mut();
        let close = !req.keep_alive() || req.path() == "/close";
        match req.path() {
            "/gzip" => {
                assert!(
                    req.headers.has_token("accept-encoding", "gzip"),
                    "client must ask for gzip"
                );
                let gz = gzip_compress(&text, Level(6));
                let resp = ServerResponse::new(200)
                    .header("Content-Encoding", "gzip")
                    .header("Transfer-Encoding", "chunked");
                w.write_all(&resp.head_bytes()).unwrap();
                for part in gz.chunks(97) {
                    w.write_all(&encode_chunk(part)).unwrap();
                    w.flush().unwrap();
                }
                w.write_all(&encode_chunk(&[])).unwrap();
                w.flush().unwrap();
            }
            "/deflate" => ServerResponse::new(200)
                .header("Content-Encoding", "deflate")
                .body(zlib_compress(&text, Level(9)))
                .write_to(w, head)
                .unwrap(),
            "/raw-deflate" => ServerResponse::new(200)
                .header("Content-Encoding", "deflate")
                .body(deflate(&text, Level(1)))
                .write_to(w, head)
                .unwrap(),
            "/plain" => ServerResponse::new(200)
                .header("Content-Type", "text/plain")
                .body(req.body.clone())
                .write_to(w, head)
                .unwrap(),
            "/close" => ServerResponse::new(200)
                .header("Connection", "close")
                .body("bye")
                .write_to(w, head)
                .unwrap(),
            _ => ServerResponse::new(404).write_to(w, head).unwrap(),
        }
        if close {
            break;
        }
    }
}

fn test_client() -> Client {
    Client::new(Resolver::new(Some(HostsFile::parse(
        "# HOST_FILE format\n127.0.0.1 vidrio.test www.vidrio.test\n",
    ))))
}

#[test]
fn client_server_persistent_gzip() {
    let (port, connections) = start_server();
    let client = test_client();
    let base = format!("http://vidrio.test:{port}");
    let text = page_text();

    let r = client
        .fetch(&Request::get(&format!("{base}/gzip")).unwrap())
        .unwrap();
    assert_eq!(r.head.status, 200);
    assert!(r.head.content_decoded);
    assert_eq!(r.head.headers.get("content-encoding"), Some("gzip"));
    assert_eq!(r.body, text);

    for path in ["/deflate", "/raw-deflate", "/gzip"] {
        let r = client
            .fetch(&Request::get(&format!("{base}{path}")).unwrap())
            .unwrap();
        assert_eq!(r.body, text, "{path}");
    }
    let mut post = Request::new("POST", Url::parse(&format!("{base}/plain")).unwrap());
    post.body = b"echo me".to_vec();
    assert_eq!(client.fetch(&post).unwrap().body, b"echo me");
    let missing = client
        .fetch(&Request::get(&format!("{base}/nothing")).unwrap())
        .unwrap();
    assert_eq!((missing.head.status, missing.body.len()), (404, 0));

    // Every request so far went over one persistent connection.
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let origin = Url::parse(&base).unwrap().origin();
    assert_eq!(client.idle_connections(&origin), 1);

    // `Connection: close` retires it; the next request opens a new one.
    let bye = client
        .fetch(&Request::get(&format!("{base}/close")).unwrap())
        .unwrap();
    assert_eq!(bye.body, b"bye");
    assert_eq!(client.idle_connections(&origin), 0);
    let r = client
        .fetch(&Request::get(&format!("{base}/gzip")).unwrap())
        .unwrap();
    assert_eq!(r.body, text);
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[test]
fn fetcher_streams_over_channel() {
    let (port, _) = start_server();
    let fetcher = Fetcher::new(Arc::new(test_client()), 8);
    let (tx, rx) = mpsc::channel::<FetchMessage>();
    let paths = ["/gzip", "/deflate", "/plain", "/gzip", "/nothing"];
    let ids: Vec<u64> = paths
        .iter()
        .map(|p| {
            let req = Request::get(&format!("http://www.vidrio.test:{port}{p}")).unwrap();
            fetcher.start(req, tx.clone())
        })
        .collect();
    let mut bodies = vec![Vec::new(); ids.len()];
    let mut status = vec![0u16; ids.len()];
    let mut done = 0;
    while done < ids.len() {
        let msg = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("fetch event");
        let i = ids.iter().position(|&id| id == msg.id).unwrap();
        match msg.event {
            Event::Head(h) => status[i] = h.status,
            Event::Data(d) => {
                assert_ne!(status[i], 0, "data before head");
                bodies[i].extend(d);
            }
            Event::Done => done += 1,
            Event::Error(e) => panic!("{} failed: {e}", paths[i]),
        }
    }
    let text = page_text();
    assert_eq!(status, vec![200, 200, 200, 200, 404]);
    assert_eq!(bodies[0], text);
    assert_eq!(bodies[1], text);
    assert!(bodies[2].is_empty());
    assert_eq!(bodies[3], text);

    // `https` without a TLS connector fails through the channel.
    let id = fetcher.start(Request::get("https://vidrio.test/").unwrap(), tx.clone());
    let msg = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(msg.id, id);
    assert!(matches!(msg.event, Event::Error(_)));
}
