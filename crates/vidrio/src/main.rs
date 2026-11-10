use std::process::ExitCode;

const USAGE: &str = "usage: vidrio --version";

/// Maps the command-line arguments (without the program name) to the text to
/// print, whether it goes to stdout, and the exit code.
fn respond(args: &[String]) -> (String, bool, u8) {
    match args {
        [flag] if flag == "--version" => (format!("vidrio {}", env!("CARGO_PKG_VERSION")), true, 0),
        _ => (USAGE.to_string(), false, 2),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (text, to_stdout, code) = respond(&args);
    if to_stdout {
        println!("{text}");
    } else {
        eprintln!("{text}");
    }
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_prints_name_and_version() {
        assert_eq!(
            respond(&args(&["--version"])),
            ("vidrio 0.1.0".to_string(), true, 0)
        );
    }

    #[test]
    fn anything_else_prints_usage_and_exits_2() {
        for case in [&[][..], &["--help"], &["--version", "x"], &["page.html"]] {
            assert_eq!(respond(&args(case)), (USAGE.to_string(), false, 2));
        }
    }
}
