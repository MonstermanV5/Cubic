//! Platform-independent, high-level client state and shared abstractions.

/// Returns the message emitted by the Phase 1 application scaffold.
#[must_use]
pub const fn startup_message() -> &'static str {
    "Cubic starting...\nPhase 1 repository scaffold initialized."
}
