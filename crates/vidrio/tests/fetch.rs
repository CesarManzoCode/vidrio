//! The data APIs of k3-11 against two local HTTP servers (two origins):
//! `fetch` with redirects (`follow`, `manual`, `error`, 303), CORS with
//! and without a preflight, `no-cors` and `same-origin` modes, cookies
//! and the `Referer`; consuming bodies (`text`, `json`, `arrayBuffer`,
//! `blob`, `formData`, `bytes` and a stream reader); synchronous and
//! asynchronous `XMLHttpRequest`; `blob:` and `data:` URLs with
//! `FileReader`; `URL`, `URLSearchParams` and the encoders; XML parsing
//! (`DOMParser`, `responseXML`, an XHTML navigation) and serialization
//! with namespaces.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vidrio::browser::{Browser, BrowserOptions, ClockMode, ContextId};

/// A request as the test server read it.
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> String {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

/// A response of the test server.
struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn resp(status: u16, headers: &[(&str, &str)], body: impl Into<Vec<u8>>) -> Resp {
    Resp {
        status,
        headers: headers
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        body: body.into(),
    }
}

type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = data[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    Some(Req {
        method,
        path,
        headers,
        body,
    })
}

fn answer(mut stream: TcpStream, handler: Handler) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    let r = handler(&req);
    let reason = match r.status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        404 => "Not Found",
        _ => "Status",
    };
    let mut out = format!("HTTP/1.1 {} {reason}\r\n", r.status).into_bytes();
    for (k, v) in &r.headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            r.body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&r.body);
    let _ = stream.write_all(&out);
    let _ = stream.flush();
}

/// Starts an HTTP/1.1 server on a free port of 127.0.0.1; returns its
/// origin.
fn serve(handler: Handler) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("local address").port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let h = handler.clone();
            thread::spawn(move || answer(stream, h));
        }
    });
    format!("http://127.0.0.1:{port}")
}

const XHTML: &str = "<?xml version=\"1.0\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>x</title></head><body><p id=\"x\">hi</p><script>document.getElementById('x').textContent += '!';</script></body></html>";

/// The page origin: documents, redirects, echoes and cookies.
fn page_server() -> String {
    serve(Arc::new(|r: &Req| match r.path.as_str() {
        "/page.html" => resp(
            200,
            &[("Content-Type", "text/html")],
            "<!DOCTYPE html><title>t</title><p>page</p>",
        ),
        "/redirect" => resp(302, &[("Location", "/final")], ""),
        "/final" => resp(200, &[("Content-Type", "text/plain")], "final body"),
        "/json" => resp(200, &[("Content-Type", "application/json")], "{\"a\":1}"),
        "/see-other" => resp(303, &[("Location", "/echo-method")], ""),
        "/echo-method" => resp(200, &[("Content-Type", "text/plain")], r.method.clone()),
        "/echo-body" => {
            let mut body =
                format!("{}:{}:", r.header("x-test"), r.header("content-type")).into_bytes();
            body.extend_from_slice(&r.body);
            resp(200, &[("Content-Type", "text/plain")], body)
        }
        "/set-cookie" => resp(200, &[("Set-Cookie", "k=v; Path=/")], "set"),
        "/echo-cookie" => resp(
            200,
            &[("Content-Type", "text/plain")],
            format!("{}|{}", r.header("cookie"), r.header("referer")),
        ),
        "/doc.xml" => resp(
            200,
            &[("Content-Type", "application/xml")],
            "<?xml version=\"1.0\"?><r xmlns=\"urn:test\"><i>1</i></r>",
        ),
        "/doc.xhtml" => resp(200, &[("Content-Type", "application/xhtml+xml")], XHTML),
        "/latin1" => resp(
            200,
            &[("Content-Type", "application/octet-stream")],
            vec![0xE9u8],
        ),
        _ => resp(404, &[], ""),
    }))
}

/// The other origin: CORS answers; every request is logged.
fn cross_server(log: Arc<Mutex<Vec<String>>>) -> String {
    serve(Arc::new(move |r: &Req| {
        log.lock().unwrap().push(format!(
            "{} {} origin={} acrm={} acrh={}",
            r.method,
            r.path,
            r.header("origin"),
            r.header("access-control-request-method"),
            r.header("access-control-request-headers"),
        ));
        let origin = r.header("origin");
        match (r.method.as_str(), r.path.as_str()) {
            (_, "/cors") => resp(
                200,
                &[
                    ("Access-Control-Allow-Origin", origin.as_str()),
                    ("Access-Control-Expose-Headers", "X-Exposed"),
                    ("X-Secret", "1"),
                    ("X-Exposed", "yes"),
                    ("Content-Type", "text/plain"),
                ],
                "cors ok",
            ),
            (_, "/nocors") => resp(200, &[("Content-Type", "text/plain")], "secret"),
            ("OPTIONS", "/preflight") => resp(
                204,
                &[
                    ("Access-Control-Allow-Origin", origin.as_str()),
                    ("Access-Control-Allow-Methods", "PUT"),
                    ("Access-Control-Allow-Headers", "X-Custom"),
                ],
                "",
            ),
            ("PUT", "/preflight") => resp(
                200,
                &[("Access-Control-Allow-Origin", origin.as_str())],
                format!("put {}", r.header("x-custom")),
            ),
            _ => resp(404, &[], ""),
        }
    }))
}

fn browser() -> Browser {
    Browser::new(BrowserOptions {
        clock: ClockMode::Virtual,
        ..BrowserOptions::default()
    })
}

/// Runs the event loop of `b` until `condition` (a script evaluated in the
/// active window of `id`) is true, for at most 20 s.
fn wait_for(b: &mut Browser, id: ContextId, condition: &str) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if b.evaluate_to_string(id, condition) == "true" {
            return true;
        }
        b.run_until_idle(Duration::from_millis(100));
    }
    false
}

/// A browser with the page of `origin` loaded.
fn open_page(origin: &str) -> (Browser, ContextId) {
    let mut b = browser();
    let id = b
        .open(&format!("{origin}/page.html"))
        .expect("the page URL");
    assert!(
        wait_for(&mut b, id, "document.readyState == 'complete'"),
        "the page loads"
    );
    (b, id)
}

/// Evaluates `script` (which sets `window.result`) and waits for it.
fn run_async(b: &mut Browser, id: ContextId, script: &str) -> String {
    b.evaluate(id, &format!("window.result = undefined; {script}"))
        .expect("the script runs");
    assert!(
        wait_for(b, id, "window.result !== undefined"),
        "the result arrives for: {script}"
    );
    b.evaluate_to_string(id, "String(window.result)")
}

#[test]
fn fetch_follows_redirect_modes_and_consumes_bodies() {
    let origin = page_server();
    let (mut b, id) = open_page(&origin);

    let first = run_async(
        &mut b,
        id,
        "fetch('/redirect').then(r => Promise.all([r.status, r.redirected, r.url.endsWith('/final'),
           r.headers.get('content-type'), r.type, r.ok, r.text()]))
         .then(v => result = v.join('|'), e => result = 'failed: ' + e);",
    );
    assert_eq!(first, "200|true|true|text/plain|basic|true|final body");

    let modes = run_async(
        &mut b,
        id,
        "Promise.all([
           fetch('/redirect', {redirect: 'manual'}).then(r => r.type + ':' + r.status),
           fetch('/redirect', {redirect: 'error'}).then(() => 'resolved', e => e.name),
           fetch('/see-other', {method: 'POST', body: 'x'}).then(r => r.text()),
           fetch('/missing').then(r => r.status + ':' + r.ok),
         ]).then(v => result = v.join('|'), e => result = 'failed: ' + e);",
    );
    assert_eq!(modes, "opaqueredirect:0|TypeError|GET|404:false");

    let bodies = run_async(
        &mut b,
        id,
        "Promise.all([
           fetch('/json').then(r => r.json()).then(j => j.a),
           fetch('/final').then(r => r.arrayBuffer()).then(b => b.byteLength),
           fetch('/final').then(r => r.blob()).then(b => b.size + ':' + b.type),
           new Response('a=1&b=2', {headers: {'Content-Type': 'application/x-www-form-urlencoded'}})
             .formData().then(f => f.get('b')),
           fetch('/final').then(r => r.bytes()).then(u => u[0]),
           fetch('/final').then(async r => {
             const reader = r.body.getReader();
             let n = 0;
             for (;;) {
               const {value, done} = await reader.read();
               if (done) break;
               n += value.length;
             }
             return n + ':' + r.bodyUsed;
           }),
           fetch('/final').then(async r => { await r.text(); return r.text().then(() => 'twice', e => e.name); }),
         ]).then(v => result = v.join(','), e => result = 'failed: ' + e);",
    );
    assert_eq!(bodies, "1,10,10:text/plain,2,102,10:true,TypeError");

    let posted = run_async(
        &mut b,
        id,
        "const fd = new FormData(); fd.append('k', 'v');
         Promise.all([
           fetch('/echo-body', {method: 'POST', body: 'hello', headers: {'X-Test': 't'}}).then(r => r.text()),
           fetch('/echo-body', {method: 'POST', body: new URLSearchParams('a=b c')}).then(r => r.text()),
           fetch('/echo-body', {method: 'POST', body: fd}).then(r => r.text())
             .then(t => t.includes('name=\"k\"') && t.includes('multipart/form-data; boundary=')),
           new Request('/x', {method: 'post'}).method,
           new Headers([['a', '1'], ['A', '2']]).get('a'),
         ]).then(v => result = v.join('|'), e => result = 'failed: ' + e);",
    );
    assert_eq!(
        posted,
        "t:text/plain;charset=UTF-8:hello|:application/x-www-form-urlencoded;charset=UTF-8:a=b+c|true|POST|1, 2"
    );
}

#[test]
fn cors_preflight_cookies_and_referrer() {
    let origin = page_server();
    let log = Arc::new(Mutex::new(Vec::new()));
    let cross = cross_server(log.clone());
    let (mut b, id) = open_page(&origin);

    let cors = run_async(
        &mut b,
        id,
        &format!(
            "const B = '{cross}';
             Promise.all([
               fetch(B + '/cors').then(r => [r.type, String(r.headers.get('x-secret')),
                 r.headers.get('x-exposed'), r.status].join('/')),
               fetch(B + '/nocors').then(() => 'ok', e => e.name),
               fetch(B + '/nocors', {{mode: 'no-cors'}}).then(r => r.type + r.status),
               fetch(B + '/preflight', {{method: 'PUT', headers: {{'X-Custom': 'v'}}}}).then(r => r.text()),
               fetch(B + '/cors', {{mode: 'same-origin'}}).then(() => 'ok', e => e.name),
             ]).then(v => result = v.join('|'), e => result = 'failed: ' + e);"
        ),
    );
    assert_eq!(cors, "cors/null/yes/200|TypeError|opaque0|put v|TypeError");
    let log = log.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|l| l == &format!("GET /cors origin={origin} acrm= acrh=")),
        "a simple CORS request sends Origin: {log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l == &format!("OPTIONS /preflight origin={origin} acrm=PUT acrh=x-custom")),
        "the preflight asks for the method and header: {log:?}"
    );
    assert!(
        log.iter().any(|l| l.starts_with("PUT /preflight origin=")),
        "the actual request follows the preflight: {log:?}"
    );

    let cookies = run_async(
        &mut b,
        id,
        "fetch('/set-cookie').then(() => fetch('/echo-cookie')).then(r => r.text())
           .then(t => result = t, e => result = 'failed: ' + e);",
    );
    assert_eq!(cookies, format!("k=v|{origin}/page.html"));
}

#[test]
fn synchronous_and_asynchronous_xhr() {
    let origin = page_server();
    let cross = cross_server(Arc::new(Mutex::new(Vec::new())));
    let (mut b, id) = open_page(&origin);

    let sync = b.evaluate_to_string(
        id,
        "var x = new XMLHttpRequest(); var states = [];
         x.onreadystatechange = () => states.push(x.readyState);
         x.open('GET', '/json', false); x.send();
         [x.status, x.readyState, x.responseText, x.getResponseHeader('Content-Type'),
          states.join(''), x.responseURL.endsWith('/json')].join('|')",
    );
    assert_eq!(sync, "200|4|{\"a\":1}|application/json|14|true");

    let posted = b.evaluate_to_string(
        id,
        "var p = new XMLHttpRequest(); p.open('POST', '/echo-body', false);
         p.setRequestHeader('X-Test', '1'); p.send('hello'); p.responseText",
    );
    assert_eq!(posted, "1:text/plain;charset=UTF-8:hello");

    let redirected = b.evaluate_to_string(
        id,
        "var r = new XMLHttpRequest(); r.open('GET', '/redirect', false); r.send();
         r.responseText + '|' + r.responseURL.endsWith('/final')",
    );
    assert_eq!(redirected, "final body|true");

    let error = b.evaluate_to_string(
        id,
        &format!(
            "var e = new XMLHttpRequest(); e.open('GET', '{cross}/nocors', false);
             try {{ e.send(); 'sent' }} catch (err) {{ err.name + ':' + e.readyState + ':' + e.status }}"
        ),
    );
    assert_eq!(error, "NetworkError:4:0");

    let xml = b.evaluate_to_string(
        id,
        "var d = new XMLHttpRequest(); d.open('GET', '/doc.xml', false); d.send();
         var root = d.responseXML.documentElement;
         [root.namespaceURI, root.localName, root.firstChild.textContent].join('|')",
    );
    assert_eq!(xml, "urn:test|r|1");

    let latin1 = b.evaluate_to_string(
        id,
        "var l = new XMLHttpRequest(); l.open('GET', '/latin1', false);
         l.overrideMimeType('text/plain; charset=windows-1252'); l.send(); l.responseText",
    );
    assert_eq!(latin1, "\u{e9}");

    let events = run_async(
        &mut b,
        id,
        "var a = new XMLHttpRequest(); var ev = [];
         a.responseType = 'arraybuffer';
         a.onloadstart = () => ev.push('loadstart');
         a.onprogress = e => ev.push('progress');
         a.onload = e => ev.push('load:' + a.response.byteLength + ':' + e.loaded + ':' + (e instanceof ProgressEvent));
         a.onloadend = () => { ev.push('loadend'); result = ev.filter(x => x != 'progress').join(',') + ':' + a.readyState; };
         a.open('GET', '/final'); a.send();",
    );
    assert_eq!(events, "loadstart,load:10:10:true,loadend:4");

    let json = run_async(
        &mut b,
        id,
        "var j = new XMLHttpRequest(); j.responseType = 'json';
         j.onload = () => result = j.response.a; j.open('GET', '/json'); j.send();",
    );
    assert_eq!(json, "1");
}

#[test]
fn blob_and_data_urls() {
    let origin = page_server();
    let (mut b, id) = open_page(&origin);

    let blobs = run_async(
        &mut b,
        id,
        "var blob = new Blob(['hello ', new Uint8Array([119, 111, 114, 108, 100])], {type: 'Text/Plain'});
         var u = URL.createObjectURL(blob);
         Promise.all([
           fetch(u).then(r => r.text().then(t => t + '|' + r.headers.get('content-type') + '|' + r.status)),
           fetch('data:text/plain;base64,aGk=').then(r => r.text()),
           fetch('data:,a%20b').then(r => r.text().then(t => t + '|' + r.headers.get('content-type'))),
           blob.slice(-5).text(),
           new Promise(res => {
             const fr = new FileReader();
             fr.onload = () => res(fr.result + ':' + fr.readyState);
             fr.readAsDataURL(new Blob(['hi']));
           }),
           new Promise(res => {
             const fr = new FileReader();
             fr.onloadend = () => res(fr.result);
             fr.readAsText(new Blob([new Uint8Array([0xEF, 0xBB, 0xBF, 0x61])]));
           }),
           new File(['x'], 'f.txt', {type: 'text/plain'}).name,
         ]).then(v => {
           URL.revokeObjectURL(u);
           return fetch(u).then(() => 'ok', e => e.name).then(after => result = v.join(',') + ',' + after);
         }, e => result = 'failed: ' + e);",
    );
    assert_eq!(
        blobs,
        "hello world|text/plain|200,hi,a b|text/plain;charset=US-ASCII,world,\
         data:application/octet-stream;base64,aGk=:2,a,f.txt,TypeError"
    );

    let sync_blob = b.evaluate_to_string(
        id,
        "var u2 = URL.createObjectURL(new Blob(['from blob']));
         var s = new XMLHttpRequest(); s.open('GET', u2, false); s.send();
         s.responseText + '|' + u2.startsWith('blob:' + location.origin + '/')",
    );
    assert_eq!(sync_blob, "from blob|true");

    let urls = b.evaluate_to_string(
        id,
        "var url = new URL('https://u:p@example.com:8080/a/b?x=1&y=2#h');
         url.searchParams.append('z', '3 4');
         var sp = new URLSearchParams('?b=2&a=1&b=3'); sp.sort();
         [url.href, url.origin, url.searchParams.get('y'),
          new URLSearchParams({a: '1', b: '2'}).toString(), sp.toString(),
          new TextDecoder().decode(new TextEncoder().encode('ñ')),
          new TextEncoder().encode('ñ').length,
          new TextDecoder('windows-1252').decode(new Uint8Array([0x80])),
          URL.canParse('nope'), new URL('../c', 'http://h/a/b').href].join('|')",
    );
    assert_eq!(
        urls,
        "https://u:p@example.com:8080/a/b?x=1&y=2&z=3+4#h|https://example.com:8080|2|a=1&b=2|\
         a=1&b=2&b=3|\u{f1}|2|\u{20ac}|false|http://h/c"
    );
}

#[test]
fn xml_parsing_and_serialization_with_namespaces() {
    let origin = page_server();
    let (mut b, id) = open_page(&origin);

    let parsed = b.evaluate_to_string(
        id,
        "var d = new DOMParser().parseFromString(
           '<r xmlns=\"urn:a\" xmlns:b=\"urn:b\"><b:c b:x=\"1\">t&amp;</b:c><d xmlns=\"\"/><![CDATA[<x>]]></r>',
           'application/xml');
         var r = d.documentElement, c = r.firstChild;
         [r.namespaceURI, c.namespaceURI, c.prefix, c.localName, c.getAttributeNS('urn:b', 'x'),
          String(r.childNodes[1].namespaceURI), r.lastChild.nodeType,
          new XMLSerializer().serializeToString(d)].join('|')",
    );
    assert_eq!(
        parsed,
        "urn:a|urn:b|b|c|1|null|4|<r xmlns=\"urn:a\" xmlns:b=\"urn:b\"><b:c b:x=\"1\">t&amp;</b:c><d xmlns=\"\"/><![CDATA[<x>]]></r>"
    );

    let bad = b.evaluate_to_string(
        id,
        "new DOMParser().parseFromString('<a><b></a>', 'text/xml').documentElement.localName",
    );
    assert_eq!(bad, "parsererror");

    let built = b.evaluate_to_string(
        id,
        "var doc = document.implementation.createDocument('urn:x', 'p:root', null);
         doc.documentElement.appendChild(doc.createElementNS('urn:y', 'c'));
         doc.documentElement.appendChild(doc.createElementNS('urn:x', 'p:d'));
         new XMLSerializer().serializeToString(doc)",
    );
    assert_eq!(
        built,
        "<p:root xmlns:p=\"urn:x\"><c xmlns=\"urn:y\"/><p:d/></p:root>"
    );

    let html = b.evaluate_to_string(
        id,
        "var h = document.createElement('div'); h.innerHTML = '<br><span>a</span>';
         new XMLSerializer().serializeToString(h)",
    );
    assert_eq!(
        html,
        "<div xmlns=\"http://www.w3.org/1999/xhtml\"><br /><span>a</span></div>"
    );

    // An XHTML document from the network: the XML parser builds it and its
    // script runs.
    let mut b2 = browser();
    let id2 = b2
        .open(&format!("{origin}/doc.xhtml"))
        .expect("the XHTML URL");
    assert!(wait_for(&mut b2, id2, "document.readyState == 'complete'"));
    let xhtml = b2.evaluate_to_string(
        id2,
        "[document.contentType, document.documentElement.namespaceURI,
          document.getElementById('x').textContent].join('|')",
    );
    assert_eq!(
        xhtml,
        "application/xhtml+xml|http://www.w3.org/1999/xhtml|hi!"
    );
}
