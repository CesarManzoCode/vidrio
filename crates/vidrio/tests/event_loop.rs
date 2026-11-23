//! The HTML event loop of k3-10: the order of tasks and microtasks with
//! nested timers, `defer`/`async`/module scripts of a document served by a
//! local HTTP server, `document.write` while parsing, and `postMessage`
//! (with a transferred `MessagePort`) between a window and its iframe.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use vidrio::browser::{Browser, BrowserOptions, ClockMode, ContextId};

/// A route of the test server: path, content type, body and a delay
/// before the response.
struct Route {
    path: &'static str,
    content_type: &'static str,
    body: &'static str,
    delay_ms: u64,
}

fn route(path: &'static str, content_type: &'static str, body: &'static str) -> Route {
    Route {
        path,
        content_type,
        body,
        delay_ms: 0,
    }
}

fn answer(mut stream: TcpStream, routes: &[Route]) {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&request);
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_string();
    let response = match routes.iter().find(|r| r.path == path) {
        Some(r) => {
            if r.delay_ms > 0 {
                thread::sleep(Duration::from_millis(r.delay_ms));
            }
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                r.content_type,
                r.body.len(),
                r.body
            )
        }
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Starts an HTTP/1.1 server on a free port of 127.0.0.1 that answers
/// `routes`; returns its origin.
fn serve(routes: Vec<Route>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("local address").port();
    let routes: &'static [Route] = Box::leak(routes.into_boxed_slice());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || answer(stream, routes));
        }
    });
    format!("http://127.0.0.1:{port}")
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

#[test]
fn tasks_microtasks_and_nested_timers_run_in_order() {
    let mut b = browser();
    let id = b.open("about:blank").expect("about:blank");
    b.evaluate(
        id,
        "var log = [];
         setTimeout(function () {
           log.push('t1');
           Promise.resolve().then(function () { log.push('m-after-t1'); });
           setTimeout(function () { log.push('t3'); }, 0);
         }, 0);
         setTimeout(function () { log.push('t2'); }, 0);
         Promise.resolve().then(function () { log.push('m1'); });
         queueMicrotask(function () { log.push('m2'); });
         log.push('sync');",
    )
    .expect("the script runs");
    // The microtask checkpoint after the script ran its microtasks.
    assert_eq!(b.evaluate_to_string(id, "log.join(',')"), "sync,m1,m2");
    assert!(b.run_until_idle(Duration::from_secs(5)));
    assert_eq!(
        b.evaluate_to_string(id, "log.join(',')"),
        "sync,m1,m2,t1,m-after-t1,t2,t3"
    );
}

#[test]
fn nested_timers_are_clamped_after_five_levels() {
    let mut b = browser();
    let id = b.open("about:blank").expect("about:blank");
    b.evaluate(
        id,
        "var times = [];
         function step() {
           times.push(performance.now());
           if (times.length < 10) setTimeout(step, 0);
         }
         setTimeout(step, 0);",
    )
    .expect("the script runs");
    assert!(b.run_until_idle(Duration::from_secs(5)));
    // Timers 1 to 6 wait 0 ms; from the seventh nesting level on, 4 ms.
    assert_eq!(
        b.evaluate_to_string(
            id,
            "times.slice(1).map(function (t, i) { return Math.round(t - times[i]); }).join(',')"
        ),
        "0,0,0,0,0,4,4,4,4"
    );
}

#[test]
fn intervals_strings_and_animation_frames() {
    let mut b = browser();
    let id = b.open("about:blank").expect("about:blank");
    b.evaluate(
        id,
        "var n = 0, frames = [], fromString = false;
         var h = setInterval(function () { if (++n === 3) clearInterval(h); }, 10);
         setTimeout('fromString = true', 1);
         requestAnimationFrame(function (t) { frames.push(typeof t); });
         var cancelled = requestAnimationFrame(function () { frames.push('cancelled'); });
         cancelAnimationFrame(cancelled);",
    )
    .expect("the script runs");
    assert!(b.run_until_idle(Duration::from_secs(5)));
    assert_eq!(b.evaluate_to_string(id, "n"), "3");
    assert_eq!(b.evaluate_to_string(id, "fromString"), "true");
    assert_eq!(b.evaluate_to_string(id, "frames.join(',')"), "number");
}

#[test]
fn defer_async_and_module_scripts_of_a_served_document() {
    let origin = serve(vec![
        route(
            "/scripts.html",
            "text/html; charset=utf-8",
            r#"<!doctype html>
<script>
var log = [];
document.addEventListener('DOMContentLoaded', function () { log.push('DOMContentLoaded'); });
window.addEventListener('load', function () { log.push('load:' + document.readyState); });
</script>
<script defer src="/defer.js"></script>
<script async src="/async.js"></script>
<script type="module">log.push('inline-module');</script>
<script type="module" src="/module.js"></script>
<script>log.push('inline-' + document.readyState);</script>
<script src="/blocking.js"></script>
<script>log.push('after-blocking');</script>
<script nomodule>log.push('nomodule');</script>
<script type="text/plain">log.push('data block');</script>
<p id="end">end</p>
"#,
        ),
        route(
            "/defer.js",
            "text/javascript",
            "log.push('defer:' + document.readyState + ':' + !!document.getElementById('end'));",
        ),
        Route {
            path: "/async.js",
            content_type: "text/javascript",
            body: "log.push('async');",
            delay_ms: 500,
        },
        route(
            "/module.js",
            "text/javascript",
            "import { value } from './dep.js'; log.push('module:' + value + ':' + (document.currentScript === null));",
        ),
        route("/dep.js", "text/javascript", "export const value = 'dep';"),
        route(
            "/blocking.js",
            "text/javascript",
            "log.push('blocking:' + !!document.getElementById('end') + ':' + document.currentScript.src.endsWith('/blocking.js'));",
        ),
    ]);
    let mut b = browser();
    let id = b.open(&format!("{origin}/scripts.html")).expect("open");
    assert!(
        wait_for(
            &mut b,
            id,
            "typeof log === 'object' && log.indexOf('load:complete') >= 0"
        ),
        "the document never loaded"
    );
    assert_eq!(
        b.evaluate_to_string(id, "log.join(',')"),
        "inline-loading,blocking:false:true,after-blocking,defer:interactive:true,\
         inline-module,module:dep:true,DOMContentLoaded,async,load:complete"
    );
}

#[test]
fn document_write_while_parsing() {
    let origin = serve(vec![
        route(
            "/write.html",
            "text/html; charset=utf-8",
            r#"<!doctype html><body><script>var log = [];
document.write('<p id=a>written</p><script>document.write("<i id=b>nested</i>")<\/script>');
</script><p id=c>after</p><script>
document.write('<script src="/written.js"><\/script>');
log.push('after-write-call');
</script><script>log.push('next-inline:' + !!document.getElementById('w'));</script><p id=end></p></body>"#,
        ),
        route(
            "/written.js",
            "text/javascript",
            "log.push('written-script'); document.write('<span id=w></span>');",
        ),
    ]);
    let mut b = browser();
    let id = b.open(&format!("{origin}/write.html")).expect("open");
    assert!(
        wait_for(
            &mut b,
            id,
            "typeof log === 'object' && document.readyState === 'complete'"
        ),
        "the document never loaded"
    );
    assert_eq!(
        b.evaluate_to_string(id, "log.join(',')"),
        "after-write-call,written-script,next-inline:true"
    );
    assert_eq!(
        b.evaluate_to_string(
            id,
            "Array.prototype.map.call(document.body.children, function (e) { return e.tagName + (e.id ? '#' + e.id : ''); }).join(',')"
        ),
        "SCRIPT,P#a,SCRIPT,I#b,P#c,SCRIPT,SCRIPT,SPAN#w,SCRIPT,P#end"
    );
    // Once the document is loaded, write() opens a new one.
    b.evaluate(
        id,
        "document.write('<p id=fresh>fresh</p>'); document.close();",
    )
    .expect("write after load");
    assert_eq!(
        b.evaluate_to_string(
            id,
            "!!document.getElementById('fresh') && !document.getElementById('a')"
        ),
        "true"
    );
}

#[test]
fn post_message_between_a_window_and_its_iframe() {
    let origin = serve(vec![
        route(
            "/parent.html",
            "text/html; charset=utf-8",
            r#"<!doctype html><script>
var log = [];
window.addEventListener('message', function (e) {
  log.push('parent got ' + e.data + ' from-iframe:' + (e.source === frames[0]) +
           ' origin-ok:' + (e.origin === location.origin));
});
window.onload = function () {
  log.push('frames:' + frames.length + ':' + (document.querySelector('iframe').contentWindow === frames[0]));
  frames[0].postMessage({ text: 'hello', list: [1, 2] }, '*');
  var channel = new MessageChannel();
  channel.port1.onmessage = function (e) { log.push('port:' + e.data); };
  frames[0].postMessage('port', '*', [channel.port2]);
};
</script>
<iframe src="/child.html"></iframe>
"#,
        ),
        route(
            "/child.html",
            "text/html; charset=utf-8",
            r#"<!doctype html><script>
window.addEventListener('message', function (e) {
  if (e.data === 'port') {
    e.ports[0].postMessage('via-port');
    return;
  }
  parent.postMessage('echo:' + e.data.text + ':' + e.data.list.length + ':' + (e.source === parent), '*');
});
</script>
"#,
        ),
    ]);
    let mut b = browser();
    let id = b.open(&format!("{origin}/parent.html")).expect("open");
    assert!(
        wait_for(&mut b, id, "typeof log === 'object' && log.length >= 3"),
        "the messages never arrived"
    );
    assert_eq!(
        b.evaluate_to_string(id, "log.join('|')"),
        "frames:1:true|parent got echo:hello:2:true from-iframe:true origin-ok:true|port:via-port"
    );
}

#[test]
fn meta_elements_reflect_their_attributes() {
    // testharness.js reads `name` and `content` of every <meta> as it loads;
    // a throwing getter left every WPT test without results until timeout.
    let mut b = browser();
    let id = b.open("about:blank").expect("about:blank");
    b.evaluate(
        id,
        "var m = document.createElement('meta');
         m.setAttribute('name', 'timeout');
         m.setAttribute('content', 'long');
         m.httpEquiv = 'refresh';
         m.media = 'print';",
    )
    .expect("the script runs");
    assert_eq!(
        b.evaluate_to_string(
            id,
            "[m.name, m.content, m.getAttribute('http-equiv'), m.httpEquiv, m.media, m.scheme === ''].join(',')"
        ),
        "timeout,long,refresh,refresh,print,true"
    );
}
