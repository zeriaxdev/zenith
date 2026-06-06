//! Shared domain types and the global log/progress bus. No IO, no GPUI.

pub mod log;
pub mod types;

pub use types::{Account, Loader, Session, VersionEntry};
