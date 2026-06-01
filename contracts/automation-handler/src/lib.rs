#![no_std]
extern crate alloc;

mod contract;
mod error;
mod events;
mod externals;
mod storage;

#[cfg(test)]
mod tests;

pub use contract::{AutomationHandler, AutomationHandlerClient, RebalanceAction};
pub use warpdrive_shared::interfaces::handler::{Ed25519SignatureData, HandlerError};
