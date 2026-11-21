use crate::wire::{Error, Result};
use std::path::Path;

pub use reader_core::Store;

pub fn open(path: &str) -> Result<Store> {
    Store::open(Path::new(path)).map_err(domain)
}

pub fn domain(message: String) -> Error {
    Error::new("core_error", message)
}
