//! Project-local handler errors.
//!
//! `verify_xlm` still returns `Result<(), warpdrive_shared::HandlerError>` to
//! stay interop-compatible with WarpDrive dashboards and operator-side
//! decoders. Project-specific failure modes that don't map cleanly to one of
//! the shared variants panic with one of the codes defined here so the error
//! shows up in the tx diagnostic with a precise reason. Off-chain consumers
//! can decode the code from the contract event log.
//!
//! Code-space convention: 600+ to leave 500-599 to `warpdrive_shared`.

use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum LocalError {
    /// `verify_xlm` invoked while the handler is paused. Operators should
    /// not retry until the dashboard shows `paused = false`.
    Paused = 600,
}
