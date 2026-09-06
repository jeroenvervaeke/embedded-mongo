mod client;
mod error;
mod ffi;
mod options;

pub use client::Client;
pub use error::{Error, Result};
pub use options::{
    CacheSize, CommandStrands, EngineOptions, JournalFileSize, OutOfRange, Preallocation,
    check_range,
};
