#![no_std]

mod contract;
pub mod storage;
pub mod utils;

pub use contract::Ed25519Verification;
pub use contract::Ed25519VerificationClient;
pub use warpdrive_shared::VerifyError;

#[cfg(test)]
mod tests;
