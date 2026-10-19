mod file;
mod header;
mod static_map;
mod error;
mod reader;
mod writer;

/// Identifier helpers for consumers that need to map reflected names to KFC resource IDs.
/// The module is additive and does not change container parsing or EML behavior.
pub mod resource_lookup;

pub use file::*;
pub use static_map::*;
pub use error::*;
pub use reader::*;
pub use writer::*;
