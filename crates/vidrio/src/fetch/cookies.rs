//! A minimal cookie jar (RFC 6265 §5): `Set-Cookie` headers of HTTP(S)
//! responses are stored and sent back in `Cookie` headers of matching
//! requests. It understands `Domain`, `Path`, `Max-Age`, `Expires` (only
//! to delete a cookie: any date makes a cookie a session cookie unless it
//! is in the past by its year), `Secure` and `HttpOnly`; there is no
//! public suffix list and no `SameSite` (LIMITATIONS.md).

use std::time::{Duration, Instant};

use crate::url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

fn host_of(url: &Url) -> Option<String> {
    url.host
        .as_ref()
        .map(|h| h.to_string().to_ascii_lowercase())
}

/// The default path of a URL (RFC 6265 §5.1.4).
fn default_path(url: &Url) -> String {
    let path = url.path_string();
    if !path.starts_with('/') {
        return "/".to_string();
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    }
}

/// Domain matching (RFC 6265 §5.1.3).
fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.ends_with(domain)
            && host[..host.len() - domain.len()].ends_with('.')
            && host.parse::<std::net::IpAddr>().is_err())
}

/// Path matching (RFC 6265 §5.1.4).
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// The year of an `Expires` date, if it can be found.
fn expires_year(value: &str) -> Option<i32> {
    value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| t.len() == 4)
        .find_map(|t| t.parse::<i32>().ok())
}

impl CookieJar {
    /// The unexpired cookies that a request to `url` would send, longest
    /// paths first (WebDriver "Get All Cookies").
    pub fn matching(&mut self, url: &Url) -> Vec<Cookie> {
        let Some(host) = host_of(url) else {
            return Vec::new();
        };
        let now = Instant::now();
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        let path = url.path_string();
        let mut out: Vec<Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                (if c.host_only {
                    c.domain == host
                } else {
                    domain_matches(&host, &c.domain)
                }) && path_matches(&path, &c.path)
                    && (!c.secure || url.scheme == "https")
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        out
    }

    /// Deletes the cookies that [`CookieJar::matching`] returns for
    /// `url`; how many went.
    pub fn delete_matching(&mut self, url: &Url) -> usize {
        let gone = self.matching(url);
        self.cookies.retain(|c| !gone.contains(c));
        gone.len()
    }

    /// Stores the cookie of a `Set-Cookie` header received from `url`.
    pub fn set_from_header(&mut self, url: &Url, header: &str) {
        let Some(host) = host_of(url) else {
            return;
        };
        let mut parts = header.split(';');
        let pair = parts.next().unwrap_or("");
        let Some((name, value)) = pair.split_once('=') else {
            return;
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let mut cookie = Cookie {
            name,
            value: value.trim().to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            secure: false,
            http_only: false,
            expires: None,
        };
        let mut delete = false;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr.trim(), ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "domain" if !v.is_empty() => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    if !domain_matches(&host, &d) {
                        return;
                    }
                    cookie.domain = d;
                    cookie.host_only = false;
                }
                "path" if v.starts_with('/') => cookie.path = v.to_string(),
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                "max-age" => match v.parse::<i64>() {
                    Ok(s) if s <= 0 => delete = true,
                    Ok(s) => {
                        cookie.expires = Some(Instant::now() + Duration::from_secs(s as u64));
                    }
                    Err(_) => {}
                },
                "expires" if cookie.expires.is_none() => {
                    if expires_year(v).is_some_and(|y| y < 2000) {
                        delete = true;
                    }
                }
                _ => {}
            }
        }
        if cookie.secure && url.scheme != "https" {
            return;
        }
        self.cookies.retain(|c| {
            !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        });
        if !delete {
            self.cookies.push(cookie);
        }
    }

    /// The `Cookie` header value for a request to `url`, if any cookie
    /// applies.
    pub fn header_for(&mut self, url: &Url) -> Option<String> {
        let host = host_of(url)?;
        let now = Instant::now();
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        let path = url.path_string();
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                (if c.host_only {
                    c.domain == host
                } else {
                    domain_matches(&host, &c.domain)
                }) && path_matches(&path, &c.path)
                    && (!c.secure || url.scheme == "https")
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        // Longer paths first (RFC 6265 §5.4).
        matching.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        Some(
            matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s, None).unwrap()
    }

    #[test]
    fn cookies_round_trip_by_domain_and_path() {
        let mut jar = CookieJar::default();
        jar.set_from_header(&url("http://a.test/x/y"), "s=1");
        jar.set_from_header(&url("http://a.test/"), "p=2; Path=/x; HttpOnly");
        jar.set_from_header(&url("http://www.a.test/"), "d=3; Domain=a.test");
        jar.set_from_header(&url("http://a.test/"), "sec=4; Secure");
        assert_eq!(
            jar.header_for(&url("http://a.test/x/z")).as_deref(),
            Some("s=1; p=2; d=3")
        );
        assert_eq!(
            jar.header_for(&url("http://a.test/")).as_deref(),
            Some("d=3")
        );
        assert_eq!(
            jar.header_for(&url("http://b.a.test/")).as_deref(),
            Some("d=3")
        );
        assert_eq!(jar.header_for(&url("http://other.test/")), None);
        jar.set_from_header(&url("http://www.a.test/"), "d=; Domain=a.test; Max-Age=0");
        assert_eq!(jar.header_for(&url("http://a.test/")), None);
    }

    #[test]
    fn matching_cookies_can_be_listed_and_deleted() {
        let mut jar = CookieJar::default();
        jar.set_from_header(&url("http://a.test/"), "s=1");
        jar.set_from_header(&url("http://a.test/"), "p=2; Path=/x");
        jar.set_from_header(&url("http://other.test/"), "o=3");
        let names: Vec<String> = jar
            .matching(&url("http://a.test/x/y"))
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["p", "s"]);
        assert_eq!(jar.delete_matching(&url("http://a.test/")), 1);
        assert_eq!(
            jar.header_for(&url("http://a.test/x/y")).as_deref(),
            Some("p=2")
        );
        assert_eq!(
            jar.header_for(&url("http://other.test/")).as_deref(),
            Some("o=3")
        );
    }
}
