//! TLS 1.3 client (RFC 8446) for `https`: the I/O-free state machine in
//! [`client`], a blocking stream over any socket, and the connector the HTTP
//! client uses.

pub mod client;
pub mod codec;
pub mod key_schedule;

pub use client::{
    ClientConfig, ClientConnection, KeyShare, NamedGroup, ServerVerifier, TlsError, alert,
};
pub use key_schedule::CipherSuite;

use crate::transport::{TlsConnector, Transport};
use crate::x509::TrustStore;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn io_error(e: TlsError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// A TLS connection over a blocking socket.
pub struct TlsStream<S: Read + Write> {
    conn: ClientConnection,
    sock: S,
    eof: bool,
}

impl<S: Read + Write> TlsStream<S> {
    /// Run the handshake with `server_name` over `sock`.
    pub fn connect(
        config: Arc<ClientConfig>,
        server_name: &str,
        sock: S,
    ) -> io::Result<TlsStream<S>> {
        let conn = ClientConnection::new(config, server_name).map_err(io_error)?;
        let mut stream = TlsStream {
            conn,
            sock,
            eof: false,
        };
        stream.write_pending()?;
        while stream.conn.is_handshaking() {
            if stream.read_tls()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed during the TLS handshake",
                ));
            }
        }
        Ok(stream)
    }

    pub fn connection(&self) -> &ClientConnection {
        &self.conn
    }

    pub fn get_ref(&self) -> &S {
        &self.sock
    }

    fn write_pending(&mut self) -> io::Result<()> {
        let out = self.conn.take_outgoing();
        if !out.is_empty() {
            self.sock.write_all(&out)?;
            self.sock.flush()?;
        }
        Ok(())
    }

    /// One socket read fed to the connection; 0 at end of stream.
    fn read_tls(&mut self) -> io::Result<usize> {
        let mut buf = vec![0u8; client::MAX_FRAGMENT + 512];
        let n = loop {
            match self.sock.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };
        if n == 0 {
            self.eof = true;
            return Ok(0);
        }
        let fed = self.conn.feed(&buf[..n]);
        // Alerts and KeyUpdate replies go out even when feeding failed.
        let written = self.write_pending();
        fed.map_err(io_error)?;
        written?;
        Ok(n)
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.conn.read_plaintext(buf);
            if n > 0 {
                return Ok(n);
            }
            // A server that closes without close_notify ends the stream too;
            // HTTP framing detects truncated bodies.
            if self.conn.peer_closed() || self.eof {
                return Ok(0);
            }
            self.read_tls()?;
        }
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn.write_plaintext(buf).map_err(io_error)?;
        self.write_pending()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sock.flush()
    }
}

impl<S: Read + Write> Drop for TlsStream<S> {
    fn drop(&mut self) {
        if !self.eof && self.conn.is_connected() {
            self.conn.send_close_notify();
            let _ = self.write_pending();
        }
    }
}

impl Transport for TlsStream<TcpStream> {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_write_timeout(timeout)
    }
}

/// Opens TLS 1.3 connections for the HTTP client.
pub struct Connector {
    config: Arc<ClientConfig>,
}

impl Connector {
    pub fn new(config: Arc<ClientConfig>) -> Connector {
        Connector { config }
    }

    /// Trust the system bundle ([`crate::x509::SYSTEM_BUNDLE`]) plus the
    /// certificates in `ca_cert` (PEM or DER), as `--ca-cert` asks.
    pub fn with_roots(ca_cert: Option<&Path>) -> io::Result<Connector> {
        let mut store = TrustStore::system();
        if let Some(path) = ca_cert {
            store.add_file(path)?;
        }
        Ok(Connector::new(Arc::new(ClientConfig::new(Arc::new(store)))))
    }
}

impl TlsConnector for Connector {
    fn connect(&self, tcp: TcpStream, server_name: &str) -> io::Result<Box<dyn Transport>> {
        let stream = TlsStream::connect(Arc::clone(&self.config), server_name, tcp)?;
        Ok(Box::new(stream))
    }
}
