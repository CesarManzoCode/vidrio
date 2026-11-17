//! Name resolution: first the file named by `HOST_FILE` (`/etc/hosts`
//! format), then the system resolver.

use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;

/// Parsed hosts file.
#[derive(Clone, Debug, Default)]
pub struct HostsFile {
    entries: Vec<(String, IpAddr)>,
}

impl HostsFile {
    /// Parse `/etc/hosts` syntax: an address followed by names; `#` starts a comment.
    pub fn parse(text: &str) -> HostsFile {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut fields = line.split_ascii_whitespace();
            let Some(addr) = fields.next() else {
                continue;
            };
            let addr = addr.split('%').next().unwrap_or(addr);
            let Ok(ip) = addr.parse::<IpAddr>() else {
                continue;
            };
            for name in fields {
                entries.push((normalize(name), ip));
            }
        }
        HostsFile { entries }
    }

    pub fn load(path: &Path) -> io::Result<HostsFile> {
        Ok(HostsFile::parse(&std::fs::read_to_string(path)?))
    }

    /// The file named by the `HOST_FILE` environment variable, if set and readable.
    pub fn from_env() -> Option<HostsFile> {
        let path = std::env::var_os("HOST_FILE")?;
        if path.is_empty() {
            return None;
        }
        HostsFile::load(Path::new(&path)).ok()
    }

    /// Addresses for `name`, in file order, without duplicates.
    pub fn lookup(&self, name: &str) -> Vec<IpAddr> {
        let name = normalize(name);
        let mut out: Vec<IpAddr> = Vec::new();
        for (n, ip) in &self.entries {
            if *n == name && !out.contains(ip) {
                out.push(*ip);
            }
        }
        out
    }
}

fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Resolver used by the HTTP client.
#[derive(Clone, Debug, Default)]
pub struct Resolver {
    hosts: Option<HostsFile>,
}

impl Resolver {
    pub fn new(hosts: Option<HostsFile>) -> Resolver {
        Resolver { hosts }
    }

    /// Resolver reading `HOST_FILE` once, at construction.
    pub fn from_env() -> Resolver {
        Resolver::new(HostsFile::from_env())
    }

    /// Socket addresses for `host:port`: IP literal, hosts file, then the system resolver.
    pub fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        if let Some(hosts) = &self.hosts {
            let ips = hosts.lookup(bare);
            if !ips.is_empty() {
                return Ok(ips
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect());
            }
        }
        let addrs: Vec<SocketAddr> = (bare, port).to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no address for {bare}"),
            ));
        }
        Ok(addrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_syntax() {
        let h = HostsFile::parse(
            "# comment\n127.0.0.1\tweb-platform.test www.web-platform.test # trailing\n\
             ::1 web-platform.test\nbogus line\n\n127.0.0.1 WWW1.Web-Platform.Test.\n",
        );
        assert_eq!(
            h.lookup("web-platform.test"),
            vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "::1".parse().unwrap()
            ]
        );
        assert_eq!(h.lookup("www1.web-platform.test").len(), 1);
        assert!(h.lookup("missing.test").is_empty());
    }

    #[test]
    fn hosts_file_wins_over_system() {
        let r = Resolver::new(Some(HostsFile::parse("10.1.2.3 localhost\n")));
        let a = r.resolve("localhost", 80).unwrap();
        assert_eq!(a, vec!["10.1.2.3:80".parse().unwrap()]);
        let lit = r.resolve("[::1]", 8000).unwrap();
        assert_eq!(lit, vec!["[::1]:8000".parse().unwrap()]);
    }
}
