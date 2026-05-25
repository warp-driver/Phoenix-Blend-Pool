use soroban_sdk::{Address, Bytes, BytesN, Env, String, Vec, contract, contractimpl};

use warpdrive_shared::interfaces::{
    security::Ed25519SecurityClient,
    verification::{Ed25519VerificationInterface, VerifyError},
    warpdrive::{ContractUpgraded, WarpDriveInterface},
};

use crate::storage;
use crate::utils;

#[contract]
pub struct Ed25519Verification;

#[contractimpl]
impl Ed25519Verification {
    pub fn __constructor(env: Env, admin: Address, security_contract: Address) {
        storage::set_admin(&env, &admin);
        storage::set_version(&env, &String::from_str(&env, env!("CARGO_PKG_VERSION")));
        storage::set_security_contract(&env, &security_contract);
        storage::extend_instance_ttl(&env);
    }
}

#[contractimpl]
impl WarpDriveInterface for Ed25519Verification {
    fn upgrade(env: Env, new_wasm_hash: BytesN<32>, new_version: String) {
        let admin = storage::get_admin(&env);
        admin.require_auth();

        storage::set_version(&env, &new_version);
        storage::extend_instance_ttl(&env);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        ContractUpgraded::new(new_version).publish(&env);
    }

    fn admin(env: Env) -> Address {
        storage::get_admin(&env)
    }

    fn pending_admin(env: Env) -> Option<Address> {
        warpdrive_shared::admin::pending(&env)
    }

    fn propose_admin(env: Env, new_admin: Address) {
        warpdrive_shared::admin::propose(&env, &storage::get_admin(&env), new_admin);
    }

    fn accept_admin(env: Env) {
        let new_admin = warpdrive_shared::admin::accept(&env);
        storage::set_admin(&env, &new_admin);
    }

    fn version(env: Env) -> String {
        storage::get_version(&env)
    }
}

#[contractimpl]
impl Ed25519VerificationInterface for Ed25519Verification {
    fn security_contract(env: Env) -> Address {
        storage::get_security_contract(&env)
    }

    fn required_weight(env: Env) -> u64 {
        let security_addr = storage::get_security_contract(&env);
        Ed25519SecurityClient::new(&env, &security_addr).required_weight()
    }

    fn signer_weight(env: Env, signer_pubkey: BytesN<32>) -> u64 {
        let security_addr = storage::get_security_contract(&env);
        Ed25519SecurityClient::new(&env, &security_addr).get_signer_weight(&signer_pubkey)
    }

    /// Verify a single ed25519 signature and return the signer's weight.
    ///
    /// # Panics
    ///
    /// Panics if the signature is cryptographically invalid (non-zero but
    /// does not match the public key and message). Soroban's `ed25519_verify`
    /// host function traps on verification failure rather than returning an
    /// error. Callers should use `try_check_one` to catch this as a host error.
    /// All-zero signatures are caught before the host call and return
    /// `Err(VerifyError::InvalidSignature)`.
    fn check_one(
        env: Env,
        envelope: Bytes,
        signature: BytesN<64>,
        signer_pubkey: BytesN<32>,
        reference_block: Option<u32>,
    ) -> Result<u64, VerifyError> {
        utils::verify_ed25519(&env, &envelope, &signature, &signer_pubkey)?;

        let security_addr = storage::get_security_contract(&env);
        let security = Ed25519SecurityClient::new(&env, &security_addr);

        let weight = match reference_block {
            Some(block) => security.get_signer_weight_at(&signer_pubkey, &block),
            None => security.get_signer_weight(&signer_pubkey),
        };
        if weight == 0 {
            return Err(VerifyError::SignerNotRegistered);
        }

        Ok(weight)
    }

    /// Verify multiple ed25519 signatures and check cumulative weight meets threshold.
    ///
    /// # Panics
    ///
    /// Panics if any signature is cryptographically invalid (non-zero but
    /// does not match its public key and the message). Soroban's `ed25519_verify`
    /// host function traps on verification failure. Callers should use
    /// `try_verify` to catch this as a host error. All-zero signatures are
    /// caught before the host call and return `Err(VerifyError::InvalidSignature)`.
    fn verify(
        env: Env,
        envelope: Bytes,
        signatures: Vec<BytesN<64>>,
        signer_pubkeys: Vec<BytesN<32>>,
        reference_block: u32,
    ) -> Result<(), VerifyError> {
        storage::extend_instance_ttl(&env);

        if signatures.is_empty() {
            return Err(VerifyError::EmptySignatures);
        }

        if signatures.len() != signer_pubkeys.len() {
            return Err(VerifyError::LengthMismatch);
        }

        let security_addr = storage::get_security_contract(&env);
        let security = Ed25519SecurityClient::new(&env, &security_addr);
        let weights = security.get_signer_weights_at(&signer_pubkeys, &reference_block);
        let required = security.required_weight_at(&reference_block);

        if required == 0 {
            return Err(VerifyError::ZeroRequiredWeight);
        }

        let message_hash = utils::sep053_hash(&env, &envelope);

        let mut total_weight: u64 = 0;
        let mut prev_pubkey: Option<BytesN<32>> = None;

        for i in 0..signatures.len() {
            let sig = signatures.get(i).unwrap();
            let pubkey = signer_pubkeys.get(i).unwrap();

            if let Some(ref prev) = prev_pubkey
                && pubkey.to_array() <= prev.to_array()
            {
                return Err(VerifyError::SignersNotOrdered);
            }
            prev_pubkey = Some(pubkey.clone());

            utils::verify_ed25519_prehashed(&env, &message_hash, &sig, &pubkey)?;

            let weight = weights.get(i).unwrap();
            if weight == 0 {
                return Err(VerifyError::SignerNotRegistered);
            }

            total_weight = total_weight.checked_add(weight).expect("weight overflow");
        }

        if total_weight < required {
            return Err(VerifyError::InsufficientWeight);
        }

        Ok(())
    }
}
