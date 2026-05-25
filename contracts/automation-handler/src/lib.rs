#![no_std]
extern crate alloc;

mod contract;
mod externals;
mod storage;

#[cfg(test)]
mod tests;

pub use contract::{AutomationHandler, AutomationHandlerClient, RebalanceToBlend};
pub use warpdrive_shared::interfaces::handler::{Ed25519SignatureData, HandlerError};
