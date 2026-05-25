#![no_std]

mod contract;
pub mod storage;

pub use contract::Ed25519Security;
pub use contract::Ed25519SecurityClient;
pub use warpdrive_shared::interfaces::security::SecurityError;

#[cfg(test)]
mod tests;
