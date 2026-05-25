use soroban_sdk::{Address, BytesN, Env, String, Vec, contract, contractimpl};

use warpdrive_shared::interfaces::{
    security::{
        Ed25519SecurityInterface, Ed25519SignerAdded, Ed25519SignerInfo, Ed25519SignerRemoved,
        SecurityError, ThresholdSet,
    },
    warpdrive::{ContractUpgraded, WarpDriveInterface},
};

use crate::storage::{self, Threshold};

#[contract]
pub struct Ed25519Security;

#[contractimpl]
impl Ed25519Security {
    pub fn __constructor(
        env: Env,
        admin: Address,
        threshold_numerator: u64,
        threshold_denominator: u64,
    ) -> Result<(), SecurityError> {
        if threshold_denominator == 0 {
            return Err(SecurityError::ZeroDenominator);
        }
        if threshold_numerator == 0 {
            return Err(SecurityError::ZeroNumerator);
        }
        if threshold_numerator > threshold_denominator {
            return Err(SecurityError::NumeratorExceedsDenominator);
        }
        storage::set_admin(&env, &admin);
        storage::set_version(&env, &String::from_str(&env, env!("CARGO_PKG_VERSION")));
        storage::set_threshold(
            &env,
            &Threshold::new(threshold_numerator, threshold_denominator),
        );
        storage::extend_instance_ttl(&env);
        Ok(())
    }
}

#[contractimpl]
impl WarpDriveInterface for Ed25519Security {
    fn upgrade(env: Env, new_wasm_hash: BytesN<32>, new_version: String) {
        storage::get_admin(&env).require_auth();

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
impl Ed25519SecurityInterface for Ed25519Security {
    fn add_signer(env: Env, key: BytesN<32>, weight: u64) -> Result<(), SecurityError> {
        storage::get_admin(&env).require_auth();
        if weight == 0 {
            return Err(SecurityError::ZeroWeight);
        }
        storage::extend_instance_ttl(&env);
        storage::add_signer(&env, key.clone(), weight);
        Ed25519SignerAdded::new(key, weight).publish(&env);
        Ok(())
    }

    fn remove_signer(env: Env, key: BytesN<32>) {
        storage::get_admin(&env).require_auth();
        storage::extend_instance_ttl(&env);
        storage::remove_signer(&env, key.clone());
        Ed25519SignerRemoved::new(key).publish(&env);
    }

    fn set_threshold(env: Env, numerator: u64, denominator: u64) -> Result<(), SecurityError> {
        storage::get_admin(&env).require_auth();
        if denominator == 0 {
            return Err(SecurityError::ZeroDenominator);
        }
        if numerator == 0 {
            return Err(SecurityError::ZeroNumerator);
        }
        if numerator > denominator {
            return Err(SecurityError::NumeratorExceedsDenominator);
        }
        storage::set_threshold(&env, &Threshold::new(numerator, denominator));
        ThresholdSet::new(numerator, denominator).publish(&env);
        Ok(())
    }

    fn get_total_weight(env: Env) -> u64 {
        storage::get_total_weight(&env)
    }

    fn get_signer_weight(env: Env, key: BytesN<32>) -> u64 {
        storage::get_signer_weight(&env, key).unwrap_or(0)
    }

    fn get_signer_weight_at(env: Env, key: BytesN<32>, reference_block: u32) -> u64 {
        storage::get_signer_weight_at(&env, key, reference_block)
    }

    fn get_signer_weights(env: Env, keys: Vec<BytesN<32>>) -> Vec<u64> {
        storage::get_signer_weights(&env, &keys)
    }

    fn get_signer_weights_at(env: Env, keys: Vec<BytesN<32>>, reference_block: u32) -> Vec<u64> {
        storage::get_signer_weights_at(&env, &keys, reference_block)
    }

    fn get_total_weight_at(env: Env, reference_block: u32) -> u64 {
        storage::get_total_weight_at(&env, reference_block)
    }

    fn required_weight_at(env: Env, reference_block: u32) -> u64 {
        let total = storage::get_total_weight_at(&env, reference_block);
        let threshold = storage::get_threshold(&env);
        ((total as u128) * (threshold.numerator as u128) / (threshold.denominator as u128)) as u64
    }

    fn list_signers(env: Env) -> Vec<Ed25519SignerInfo> {
        storage::list_signers(&env)
    }

    fn threshold_numerator(env: Env) -> u64 {
        storage::get_threshold(&env).numerator
    }

    fn threshold_denominator(env: Env) -> u64 {
        storage::get_threshold(&env).denominator
    }

    fn required_weight(env: Env) -> u64 {
        let total = storage::get_total_weight(&env);
        let threshold = storage::get_threshold(&env);
        ((total as u128) * (threshold.numerator as u128) / (threshold.denominator as u128)) as u64
    }
}
