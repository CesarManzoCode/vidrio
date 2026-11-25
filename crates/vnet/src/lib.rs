//! vnet: sockets, HTTP/1.1, TLS 1.3, cryptography, X.509, inflate/deflate and brotli.

pub mod brotli;
pub mod checksum;
pub mod crypto;
pub mod deflate;
pub mod http;
pub mod inflate;
pub mod resolve;
pub mod tls;
pub mod transport;
pub mod x509;
