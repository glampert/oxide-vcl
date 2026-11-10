//! The unpublished original-binary observations under `local/evidence`
//! (gitignored). Tests that need them are ignored when they are absent.

#![allow(dead_code)]

use std::path::PathBuf;

pub fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence")
}

pub fn path(relative: &str) -> PathBuf {
    root().join(relative)
}

pub fn text(relative: &str) -> String {
    let path = path(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
