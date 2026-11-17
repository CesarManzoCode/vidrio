//! Byte streams under HTTP and the extension point for `https`.
//!
//! The HTTP client only needs a bidirectional stream with read and write
//! timeouts. Plain `http` uses a `TcpStream`; `https` wraps the connected
//! socket with a [`TlsConnector`] registered on the client
//! (`crate::tls::Connector`, see `Client::enable_https`).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub trait Transport: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl Transport for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
}

/// Wraps a connected TCP socket in a secure channel to `server_name`.
pub trait TlsConnector: Send + Sync {
    fn connect(&self, tcp: TcpStream, server_name: &str) -> io::Result<Box<dyn Transport>>;
}
