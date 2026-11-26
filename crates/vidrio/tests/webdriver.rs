//! The WebDriver server over real HTTP: a server on a local port receives
//! the sequence wptrunner's WebDriverTestharnessExecutor sends for a
//! testharness test (new session, runner window, new window, navigation
//! to a local server, `execute/async` with its callback, close window,
//! delete session), plus the `no such window` and `unknown command`
//! errors and the serialization of element and window references.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::rc::Rc;
use std::thread;

use vidrio::browser::{Browser, BrowserOptions};
use vidrio::paint::DocumentPainter;
use vidrio::webdriver::json::Json;
use vidrio::webdriver::script::{ELEMENT_KEY, WINDOW_KEY};

const RUNNER: &str = "<!doctype html><title>runner page</title><body>runner</body>";
const TEST_PAGE: &str = "<!doctype html><title>wd test</title><body>\
<div id=target class=x>hi</div><div id=other></div><input id=field>\
<script>window.loaded = true; window.keys = [];\
document.getElementById('field').addEventListener('keydown', e => keys.push(e.type));\
document.documentElement.addEventListener('click', e => { window.clicked = e.isTrusted; });\
</script></body>";

/// A tiny HTTP server for the pages: `/runner.html` and anything else
/// (the test page).
fn page_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let path = head.split(' ').nth(1).unwrap_or("/").to_string();
            let body = if path.starts_with("/runner.html") {
                RUNNER
            } else {
                TEST_PAGE
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    addr
}

/// A WebDriver client on one keep-alive connection, like wptrunner's.
struct Client {
    stream: TcpStream,
    session: String,
}

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        Client {
            stream,
            session: String::new(),
        }
    }

    fn raw(&mut self, method: &str, path: &str, body: Option<&str>) -> (u16, Json) {
        let body = body.unwrap_or("");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        self.stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let n = self.stream.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed before the response");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8(buf[..head_end].to_vec()).unwrap();
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        let length: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().unwrap())
            })
            .expect("a Content-Length");
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < length {
            let n = self.stream.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed inside the body");
            body.extend_from_slice(&chunk[..n]);
        }
        let json = Json::parse(std::str::from_utf8(&body[..length]).unwrap()).unwrap();
        (status, json)
    }

    /// A session command; returns the status and the `value`.
    fn cmd(&mut self, method: &str, path: &str, body: Option<&str>) -> (u16, Json) {
        let full = format!("/session/{}{path}", self.session);
        let (status, json) = self.raw(method, &full, body);
        (status, json.get("value").cloned().unwrap_or(Json::Null))
    }

    /// A session command that must succeed.
    fn ok(&mut self, method: &str, path: &str, body: Option<&str>) -> Json {
        let (status, value) = self.cmd(method, path, body);
        assert_eq!(status, 200, "{method} {path}: {}", value.serialize());
        value
    }

    /// A session command that must fail with `code`.
    fn fails(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&str>,
        status: u16,
        code: &str,
    ) -> Json {
        let (s, value) = self.cmd(method, path, body);
        assert_eq!(s, status, "{method} {path}: {}", value.serialize());
        assert_eq!(
            value.get("error").and_then(Json::as_str),
            Some(code),
            "{method} {path}"
        );
        value
    }

    fn execute(&mut self, script: &str, args: &str) -> Json {
        let body = format!(
            r#"{{"script":{},"args":{args}}}"#,
            Json::str(script).serialize()
        );
        self.ok("POST", "/execute/sync", Some(&body))
    }

    fn execute_async(&mut self, script: &str, args: &str) -> Json {
        let body = format!(
            r#"{{"script":{},"args":{args}}}"#,
            Json::str(script).serialize()
        );
        self.ok("POST", "/execute/async", Some(&body))
    }
}

fn element_id(v: &Json) -> String {
    v.get(ELEMENT_KEY)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("not an element reference: {}", v.serialize()))
        .to_string()
}

fn strings(v: &Json) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn wptrunner_testharness_sequence_over_http() {
    let pages = page_server();
    let (mut server, commands) = vidrio::webdriver::http::start("127.0.0.1:0").unwrap();
    let addr = server.addr;
    let browser = thread::spawn(move || {
        let mut browser = Browser::new(BrowserOptions::default());
        vidrio::webdriver::run(
            &mut browser,
            commands,
            Rc::new(DocumentPainter::new()),
            true,
        );
    });

    let mut c = Client::connect(addr);
    let (status, v) = c.raw("GET", "/status", None);
    assert_eq!(status, 200);
    assert_eq!(
        v.get("value").and_then(|v| v.get("ready")),
        Some(&Json::Bool(true))
    );

    // New session with the capabilities of wptrunner's vidrio product.
    let (status, v) = c.raw(
        "POST",
        "/session",
        Some(r#"{"capabilities":{"alwaysMatch":{}}}"#),
    );
    assert_eq!(status, 200, "{}", v.serialize());
    let value = v.get("value").unwrap();
    c.session = value
        .get("sessionId")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    let caps = value.get("capabilities").unwrap();
    assert_eq!(caps.get("browserName"), Some(&Json::str("vidrio")));
    assert_eq!(caps.get("pageLoadStrategy"), Some(&Json::str("normal")));
    let (_, v) = c.raw("GET", "/status", None);
    assert_eq!(
        v.get("value").and_then(|v| v.get("ready")),
        Some(&Json::Bool(false))
    );

    // A second session is refused.
    let (status, v) = c.raw("POST", "/session", Some(r#"{"capabilities":{}}"#));
    assert_eq!(status, 500);
    assert_eq!(
        v.get("value")
            .and_then(|v| v.get("error"))
            .and_then(Json::as_str),
        Some("session not created")
    );

    // Executor setup: the window rect, then the runner page.
    let rect = c.ok("GET", "/window/rect", None);
    assert_eq!(rect.get("width"), Some(&Json::Number(800.0)));
    assert_eq!(rect.get("height"), Some(&Json::Number(600.0)));
    c.ok("POST", "/timeouts", Some(r#"{"script":10000}"#));
    let runner = c.ok("GET", "/window", None).as_str().unwrap().to_string();
    let url = format!("http://{pages}/runner.html");
    assert_eq!(
        c.ok("POST", "/url", Some(&format!(r#"{{"url":"{url}"}}"#))),
        Json::Null
    );
    assert_eq!(c.ok("GET", "/url", None), Json::String(url));
    assert_eq!(c.ok("GET", "/title", None), Json::str("runner page"));
    assert_eq!(
        c.execute("document.title = 'runner'; return document.title;", "[]"),
        Json::str("runner")
    );

    // do_testharness: close_old_windows (release actions, no other window).
    assert_eq!(c.ok("DELETE", "/actions", None), Json::Null);
    assert_eq!(
        strings(&c.ok("GET", "/window/handles", None)),
        [runner.clone()]
    );

    // create_test_window: a new tab, switch, wait for about:blank, click
    // the root element.
    let new = c.ok("POST", "/window/new", Some(r#"{"type":"tab"}"#));
    let test_window = new
        .get("handle")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert_ne!(test_window, runner);
    let handles = strings(&c.ok("GET", "/window/handles", None));
    assert_eq!(handles.len(), 2);
    assert!(handles.contains(&test_window));
    c.ok(
        "POST",
        "/window",
        Some(&format!(r#"{{"handle":"{test_window}"}}"#)),
    );
    assert_eq!(
        c.ok("GET", "/window", None),
        Json::String(test_window.clone())
    );
    let loaded = c.execute_async(
        "const [resolve] = arguments;\n\
         if (document.readyState != 'complete') {\n\
           window.addEventListener('load', () => { resolve(); }, { once: true });\n\
         } else { resolve(); }",
        "[]",
    );
    assert_eq!(loaded, Json::Null);
    let root = c.execute("return document.documentElement;", "[]");
    let root_id = element_id(&root);
    assert_eq!(
        c.ok("POST", &format!("/element/{root_id}/click"), Some("{}")),
        Json::Null
    );

    // testdriver.run: navigate the test window to the test, then the
    // resume script with its callback (here answered from a timer).
    let test_url = format!("http://{pages}/dom/test.html");
    c.ok("POST", "/url", Some(&format!(r#"{{"url":"{test_url}"}}"#)));
    assert_eq!(
        c.execute("return document.readyState;", "[]"),
        Json::str("complete")
    );
    let message = c.execute_async(
        "const callback = arguments[arguments.length - 1];\n\
         setTimeout(() => callback({status: 'complete', n: arguments[0] + 1, loaded: window.loaded}), 10);",
        "[41]",
    );
    assert_eq!(message.get("status"), Some(&Json::str("complete")));
    assert_eq!(message.get("n"), Some(&Json::Number(42.0)));
    assert_eq!(message.get("loaded"), Some(&Json::Bool(true)));
    // A returned promise settles a synchronous script too.
    assert_eq!(
        c.execute("return Promise.resolve(arguments[0] * 2);", "[21]"),
        Json::Number(42.0)
    );

    // Element references: the same node keeps its reference, arrays and
    // NodeLists become arrays, references come back as the element.
    let v = c.execute(
        "return [document.getElementById('target'), document.querySelectorAll('div'), null, 'x'];",
        "[]",
    );
    let items = v.as_array().unwrap();
    let target = element_id(&items[0]);
    let list = items[1].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(element_id(&list[0]), target);
    assert_ne!(element_id(&list[1]), target);
    assert_eq!(items[2], Json::Null);
    assert_eq!(items[3], Json::str("x"));
    let arg = format!(r#"[{{"{ELEMENT_KEY}":"{target}"}}, {{"k":[1,true]}}]"#);
    assert_eq!(
        c.execute(
            "return arguments[0].id + ':' + JSON.stringify(arguments[1]);",
            &arg
        ),
        Json::str("target:{\"k\":[1,true]}")
    );
    let found = c.ok(
        "POST",
        "/element",
        Some(r##"{"using":"css selector","value":"#target"}"##),
    );
    assert_eq!(element_id(&found), target);
    c.fails(
        "POST",
        "/element",
        Some(r##"{"using":"css selector","value":"#missing"}"##),
        404,
        "no such element",
    );
    c.fails(
        "POST",
        "/element",
        Some(r#"{"using":"css selector","value":"[["}"#),
        400,
        "invalid selector",
    );
    let unknown = format!(r#"[{{"{ELEMENT_KEY}":"not-a-known-element"}}]"#);
    c.fails(
        "POST",
        "/execute/sync",
        Some(&format!(r#"{{"script":"return 1","args":{unknown}}}"#)),
        404,
        "no such element",
    );
    // The root element of the replaced about:blank document is stale;
    // the new one receives a trusted click.
    c.fails(
        "POST",
        &format!("/element/{root_id}/click"),
        Some("{}"),
        404,
        "stale element reference",
    );
    let new_root = element_id(&c.execute("return document.documentElement;", "[]"));
    assert_ne!(new_root, root_id);
    c.ok("POST", &format!("/element/{new_root}/click"), Some("{}"));
    assert_eq!(c.execute("return window.clicked;", "[]"), Json::Bool(true));
    // Typing into the input.
    let field = element_id(&c.ok(
        "POST",
        "/element",
        Some(r##"{"using":"css selector","value":"#field"}"##),
    ));
    c.ok(
        "POST",
        &format!("/element/{field}/value"),
        Some(r#"{"text":"ab"}"#),
    );
    assert_eq!(
        c.execute(
            "return [window.keys.length, document.getElementById('field').getAttribute('value')];",
            "[]"
        ),
        Json::Array(vec![Json::Number(2.0), Json::str("ab")])
    );

    // Window references and cycles.
    let w = c.execute("return window;", "[]");
    assert_eq!(w.get(WINDOW_KEY), Some(&Json::String(test_window.clone())));
    let v = c.fails(
        "POST",
        "/execute/sync",
        Some(r#"{"script":"let a = {}; a.self = a; return a;","args":[]}"#),
        500,
        "javascript error",
    );
    assert!(
        v.get("message")
            .and_then(Json::as_str)
            .unwrap()
            .contains("cyclic")
    );
    let v = c.fails(
        "POST",
        "/execute/sync",
        Some(r#"{"script":"throw new TypeError('boom');","args":[]}"#),
        500,
        "javascript error",
    );
    assert!(
        v.get("message")
            .and_then(Json::as_str)
            .unwrap()
            .contains("boom")
    );
    c.fails(
        "POST",
        "/execute/sync",
        Some(r#"{"script":"return 1","args":"nope"}"#),
        400,
        "invalid argument",
    );

    // Script timeout of an async script that never calls back.
    c.ok("POST", "/timeouts", Some(r#"{"script":50}"#));
    c.fails(
        "POST",
        "/execute/async",
        Some(r#"{"script":"","args":[]}"#),
        500,
        "script timeout",
    );
    c.ok("POST", "/timeouts", Some(r#"{"script":10000}"#));
    let t = c.ok("GET", "/timeouts", None);
    assert_eq!(t.get("script"), Some(&Json::Number(10000.0)));

    // The screenshot is a PNG of the viewport.
    let shot = c.ok("GET", "/screenshot", None);
    assert!(shot.as_str().unwrap().starts_with("iVBORw0KGgo"));

    // Actions: a pointer source and a key source.
    c.ok(
        "POST",
        "/actions",
        Some(
            r#"{"actions":[{"type":"pointer","id":"mouse","parameters":{"pointerType":"mouse"},"actions":[{"type":"pointerMove","x":5,"y":5},{"type":"pointerDown","button":0},{"type":"pointerUp","button":0}]},{"type":"key","id":"kbd","actions":[{"type":"pause"},{"type":"keyDown","value":"a"},{"type":"keyUp","value":"a"}]}]}"#,
        ),
    );
    c.fails(
        "POST",
        "/actions",
        Some(r#"{"actions":[{"type":"pointer","id":"m","actions":[{"type":"pointerMove","x":5000,"y":5}]}]}"#),
        500,
        "move target out of bounds",
    );
    c.ok("DELETE", "/actions", None);

    // Cookies of the test document.
    c.execute("document.cookie = 'a=1';", "[]");
    let cookies = c.ok("GET", "/cookie", None);
    assert!(
        cookies
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k.get("name") == Some(&Json::str("a")))
    );
    c.ok("DELETE", "/cookie", None);
    assert_eq!(c.ok("GET", "/cookie", None), Json::Array(Vec::new()));

    // close_old_windows: the test window closes; the runner remains.
    let remaining = c.ok("DELETE", "/window", None);
    assert_eq!(strings(&remaining), [runner.clone()]);

    // The current window is gone: `no such window`.
    c.fails("GET", "/url", None, 404, "no such window");
    c.fails("GET", "/window", None, 404, "no such window");
    c.fails(
        "POST",
        "/execute/sync",
        Some(r#"{"script":"return 1","args":[]}"#),
        404,
        "no such window",
    );
    c.fails(
        "POST",
        "/window",
        Some(&format!(r#"{{"handle":"{test_window}"}}"#)),
        404,
        "no such window",
    );
    c.fails(
        "POST",
        "/window",
        Some(r#"{"handle":"no-such-handle"}"#),
        404,
        "no such window",
    );

    // Unknown commands.
    c.fails(
        "GET",
        &format!("/element/{target}/text"),
        None,
        404,
        "unknown command",
    );
    c.fails("POST", "/print", Some("{}"), 404, "unknown command");
    let (status, v) = c.raw("GET", "/nonsense", None);
    assert_eq!(status, 404);
    assert_eq!(
        v.get("value")
            .and_then(|v| v.get("error"))
            .and_then(Json::as_str),
        Some("unknown command")
    );
    let (status, v) = c.raw("GET", "/session/bogus/url", None);
    assert_eq!(status, 404);
    assert_eq!(
        v.get("value")
            .and_then(|v| v.get("error"))
            .and_then(Json::as_str),
        Some("invalid session id")
    );

    // Back to the runner, then the end of the session.
    c.ok(
        "POST",
        "/window",
        Some(&format!(r#"{{"handle":"{runner}"}}"#)),
    );
    assert_eq!(c.ok("GET", "/title", None), Json::str("runner"));
    assert_eq!(c.ok("DELETE", "", None), Json::Null);
    let (_, v) = c.raw("GET", "/status", None);
    assert_eq!(
        v.get("value").and_then(|v| v.get("ready")),
        Some(&Json::Bool(true))
    );
    let (status, _) = c.raw("GET", &format!("/session/{}/url", c.session), None);
    assert_eq!(status, 404);

    drop(c);
    server.shutdown();
    browser.join().unwrap();
}

#[test]
fn commands_are_not_delayed_by_a_pending_timer() {
    // wptrunner sends several commands between tests while the page still
    // has timers; each one used to wait for the 10 ms slice of the loop.
    let pages = page_server();
    let (mut server, commands) = vidrio::webdriver::http::start("127.0.0.1:0").unwrap();
    let addr = server.addr;
    let browser = thread::spawn(move || {
        let mut browser = Browser::new(BrowserOptions::default());
        vidrio::webdriver::run(
            &mut browser,
            commands,
            Rc::new(DocumentPainter::new()),
            true,
        );
    });
    let mut c = Client::connect(addr);
    let (_, v) = c.raw("POST", "/session", Some(r#"{"capabilities":{}}"#));
    c.session = v
        .get("value")
        .and_then(|v| v.get("sessionId"))
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    let url = format!("http://{pages}/runner.html");
    c.ok("POST", "/url", Some(&format!(r#"{{"url":"{url}"}}"#)));
    c.execute("setTimeout(function () {}, 60000);", "[]");
    let start = std::time::Instant::now();
    for _ in 0..50 {
        c.ok("GET", "/window/handles", None);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "50 commands took {elapsed:?}"
    );
    assert_eq!(c.ok("DELETE", "", None), Json::Null);
    drop(c);
    server.shutdown();
    browser.join().unwrap();
}
