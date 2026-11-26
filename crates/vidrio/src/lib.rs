//! vidrio: the browser engine (HTML, XML, DOM, fetch, CSS, layout, painting, X11 window,
//! WebDriver).

pub mod bindings;
pub mod browser;
pub mod css;
pub mod dom;
pub mod encoding;
pub mod fetch;
pub mod generated;
pub mod html;
pub mod layout;
pub mod log;
pub mod mime;
pub mod paint;
pub mod url;
pub mod view;
pub mod webdriver;
pub mod window;
pub mod xml;

/// The Web IDL generator of `build.rs`, compiled here too so that its
/// unit tests run with the library's.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../bindgen/mod.rs"]
mod bindgen;
