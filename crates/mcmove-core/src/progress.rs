//! Progress reporting that decouples core logic from any UI.
//!
//! Core operations take a `&dyn Reporter` and emit [`Progress`] events. The CLI renders
//! them with `indicatif`; a GPUI launcher renders them in-app. The core never calls
//! `println!`.

/// A progress event emitted by a long-running core operation.
#[derive(Debug, Clone)]
pub enum Progress {
    /// A new phase started (e.g. "Scanning jars", "Uploading mods").
    Phase { name: String },
    /// Total unit count for the current phase is now known (e.g. number of files).
    Total { units: u64 },
    /// `units` more units of the current phase completed.
    Advance { units: u64, label: String },
    /// A non-fatal informational line (e.g. "skipped client-only mod X").
    Info { message: String },
    /// A non-fatal warning (e.g. "Modrinth lookup failed for 3 files").
    Warn { message: String },
    /// The current phase finished.
    PhaseDone { name: String },
}

/// Sink for [`Progress`] events. Implemented by each front-end.
pub trait Reporter: Send + Sync {
    fn report(&self, event: Progress);
}

/// A `Reporter` that drops everything — useful for tests and non-interactive callers.
pub struct NoopReporter;

impl Reporter for NoopReporter {
    fn report(&self, _event: Progress) {}
}
