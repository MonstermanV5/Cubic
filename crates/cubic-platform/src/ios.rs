//! iOS-only handoff point for a future native Xcode application host.

use crate::PlatformError;

/// Runs Cubic after a future native iOS host transfers control to Rust.
///
/// Phase 2 intentionally does not export a C ABI symbol or supply the required
/// Xcode application wrapper. This Rust entry point is compile-checked for
/// `aarch64-apple-ios` and keeps that future boundary isolated.
pub fn run_from_native_host() -> Result<(), PlatformError> {
    crate::run()
}
