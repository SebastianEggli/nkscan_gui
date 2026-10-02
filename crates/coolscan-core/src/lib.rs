//! Everything the Coolscan app does that is not UI
//!
//! The scanner is reached through a [`Backend`]: [`NkscanBackend`] drives real
//! hardware through nkscan, [`FakeBackend`] synthesizes a strip so the rest can
//! be developed and tested without one. A [`Worker`] owns one backend on its
//! own thread, so a scan never blocks the caller.

pub mod backend;
pub mod fake;
pub mod nkscan_backend;
pub mod preview;
pub mod settings;
pub mod tiff_out;
pub mod types;
pub mod worker;

pub use backend::{Backend, Flow};
pub use fake::FakeBackend;
pub use nkscan_backend::NkscanBackend;
pub use settings::{FilmFormat, FilmType, ScanSettings};
pub use types::*;
pub use worker::{Command, Event, Worker};

/// A scan stopped on request rather than failing
#[derive(Debug, Clone, Copy)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scan cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `error` is a cancel, from either backend
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<Cancelled>()
            || matches!(
                cause.downcast_ref::<nkscan::error::Error>(),
                Some(nkscan::error::Error::Cancelled)
            )
    })
}
