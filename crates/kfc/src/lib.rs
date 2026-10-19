pub use kfc_base::*;

// ShroudForge consumes these additive APIs to build compatibility schemas and
// export snapshots. Existing KFC parsing remains implemented by the same types.
mod schema;
mod snapshot;

#[cfg(feature = "resource")]
pub use kfc_resource as resource;

#[cfg(feature = "content")]
pub use kfc_content as content;

/// Name-based helpers used by ShroudForge's resource bindings.
/// This re-export adds lookup functions without changing EML's parser path.
pub use container::resource_lookup as kfc_format;

// Public schema and snapshot API used by ShroudForge. EML does not call these exports.
pub use schema::*;
pub use snapshot::{SnapshotError, SnapshotResult, export_api_snapshot};
