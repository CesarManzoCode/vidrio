use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use vidrio::browser::{Browser, BrowserOptions};
use vidrio::log;

const USAGE: &str =
    "usage: vidrio [--webdriver=PORT] [--headless] [--ca-cert=PATH] [URL]\n       vidrio --version";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
struct Config {
    webdriver: Option<u16>,
    headless: bool,
    ca_cert: Option<PathBuf>,
    url: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Parsed {
    Version,
    Run(Config),
    /// A usage error, with its reason.
    Usage(String),
}

/// Parses the command-line arguments (without the program name). A flag
/// with a value takes it after `=` or as the next argument.
fn parse_args(args: &[String]) -> Parsed {
    if args.len() == 1 && args[0] == "--version" {
        return Parsed::Version;
    }
    let mut c = Config {
        webdriver: None,
        headless: false,
        ca_cert: None,
        url: None,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let value = |i: &mut usize| -> Option<String> {
            if inline.is_some() {
                return inline.clone();
            }
            *i += 1;
            args.get(*i).cloned()
        };
        match flag {
            "--webdriver" => match value(&mut i).and_then(|v| v.parse::<u16>().ok()) {
                Some(p) => c.webdriver = Some(p),
                None => return Parsed::Usage("--webdriver needs a port number".to_string()),
            },
            "--ca-cert" => match value(&mut i).filter(|v| !v.is_empty()) {
                Some(p) => c.ca_cert = Some(PathBuf::from(p)),
                None => return Parsed::Usage("--ca-cert needs a path".to_string()),
            },
            "--headless" if inline.is_none() => c.headless = true,
            f if f.starts_with('-') && f.len() > 1 => {
                return Parsed::Usage(format!("unknown option {a}"));
            }
            _ => {
                if c.url.is_some() {
                    return Parsed::Usage("only one URL may be given".to_string());
                }
                c.url = Some(a.clone());
            }
        }
        i += 1;
    }
    Parsed::Run(c)
}

/// The URL to open for the argument `arg`: an absolute URL as it is, and
/// anything else as a path of a local file.
fn url_for(arg: &str) -> String {
    if vidrio::url::Url::parse(arg, None).is_ok() {
        return arg.to_string();
    }
    let path = std::path::Path::new(arg);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    format!("file://{}", abs.display())
}

fn run(c: Config) -> ExitCode {
    log::init();
    log::info(&format!("vidrio {} starting", env!("CARGO_PKG_VERSION")));
    match std::env::var_os("HOST_FILE") {
        Some(p) => log::info(&format!(
            "resolving hosts with HOST_FILE={}",
            p.to_string_lossy()
        )),
        None => log::info("HOST_FILE is not set: hosts resolve through the system"),
    }
    if let Some(p) = &c.ca_cert {
        log::info(&format!("extra CA certificates from {}", p.display()));
    }
    let options = BrowserOptions {
        ca_cert: c.ca_cert.clone(),
        ..BrowserOptions::default()
    };
    let mut browser = Browser::new(options);
    let mut top = None;
    if let Some(arg) = &c.url {
        let url = url_for(arg);
        match browser.open(&url) {
            Ok(id) => {
                log::info(&format!("opening {url}"));
                top = Some(id);
            }
            Err(e) => {
                log::info(&format!("cannot open {arg}: {e}"));
                return ExitCode::from(1);
            }
        }
    }
    match c.webdriver {
        Some(port) => {
            if !c.headless {
                // WebDriver sessions paint into their own framebuffer.
                log::info("WebDriver runs without a window");
            }
            let (mut server, commands) =
                match vidrio::webdriver::http::start(&format!("127.0.0.1:{port}")) {
                    Ok(s) => s,
                    Err(e) => {
                        log::info(&format!("cannot listen on 127.0.0.1:{port}: {e}"));
                        return ExitCode::from(1);
                    }
                };
            log::info(&format!("WebDriver listening on {}", server.addr));
            let painter = Rc::new(vidrio::paint::DocumentPainter::new());
            vidrio::webdriver::run(&mut browser, commands, painter, c.headless);
            server.shutdown();
        }
        None if !c.headless => {
            let ctx = match top {
                Some(id) => id,
                None => match browser.open("about:blank") {
                    Ok(id) => id,
                    Err(e) => {
                        log::info(&format!("cannot open about:blank: {e}"));
                        return ExitCode::from(1);
                    }
                },
            };
            if let Err(e) = vidrio::window::run(&mut browser, ctx) {
                log::info(&format!("window: {e}"));
                return ExitCode::from(1);
            }
            log::info("the window closed");
        }
        None => {
            browser.run_until_idle(Duration::from_secs(u64::from(u32::MAX)));
            log::info("nothing left to do");
        }
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Parsed::Version => {
            println!("vidrio {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Parsed::Usage(reason) => {
            eprintln!("vidrio: {reason}\n{USAGE}");
            ExitCode::from(2)
        }
        Parsed::Run(c) => run(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn config(list: &[&str]) -> Config {
        match parse_args(&args(list)) {
            Parsed::Run(c) => c,
            other => panic!("{list:?}: {other:?}"),
        }
    }

    #[test]
    fn version_alone_prints_the_version() {
        assert_eq!(parse_args(&args(&["--version"])), Parsed::Version);
    }

    #[test]
    fn wptrunner_command_line() {
        let c = config(&["--webdriver=4444", "--headless", "--ca-cert=/tmp/ca.pem"]);
        assert_eq!(
            c,
            Config {
                webdriver: Some(4444),
                headless: true,
                ca_cert: Some(PathBuf::from("/tmp/ca.pem")),
                url: None,
            }
        );
        let c = config(&["--webdriver", "5555", "http://web-platform.test:8000/"]);
        assert_eq!(c.webdriver, Some(5555));
        assert!(!c.headless);
        assert_eq!(c.url.as_deref(), Some("http://web-platform.test:8000/"));
        assert_eq!(config(&[]).url, None);
    }

    #[test]
    fn bad_arguments_are_usage_errors() {
        for case in [
            &["--webdriver=x"][..],
            &["--webdriver"],
            &["--ca-cert="],
            &["--help"],
            &["--version", "x"],
            &["a.html", "b.html"],
            &["--headless=1"],
        ] {
            assert!(
                matches!(parse_args(&args(case)), Parsed::Usage(_)),
                "{case:?}"
            );
        }
    }

    #[test]
    fn paths_become_file_urls() {
        assert_eq!(url_for("http://a.test/x"), "http://a.test/x");
        assert_eq!(url_for("/tmp/page.html"), "file:///tmp/page.html");
        assert!(url_for("page.html").starts_with("file:///"));
    }
}
