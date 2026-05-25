use crate::{Ed25519Verification, Ed25519VerificationClient};
use soroban_sdk::{Address, BytesN, Env, testutils::Address as _};
use ed25519_security::Ed25519Security;

mod contract_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/ed25519_verification.wasm"
    );
}

pub fn deploy_contract<'a>(env: &Env) -> (Ed25519VerificationClient<'a>, Address) {
    let admin = Address::generate(env);

    // Deploy security contract first
    let security_id = env.register(Ed25519Security, (&admin, 2u64, 3u64));

    let contract_id = env.register(Ed25519Verification, (&admin, &security_id));
    let client = Ed25519VerificationClient::new(env, &contract_id);
    (client, admin)
}

pub fn install_contract_wasm(env: &Env) -> BytesN<32> {
    env.deployer().upload_contract_wasm(contract_wasm::WASM)
}
