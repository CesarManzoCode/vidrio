//! The tables that `build.rs` generates from `crates/vidrio/idl` with the
//! generator of `crates/vidrio/bindgen` (described by [`super::meta`]).

#![allow(clippy::all, dead_code)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
