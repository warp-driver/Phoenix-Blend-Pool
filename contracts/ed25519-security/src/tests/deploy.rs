extern crate std;

use super::setup::{deploy_contract, install_contract_wasm};
use soroban_sdk::{Env, String};

#[test]
fn test_deploy() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = deploy_contract(&env);

    assert_eq!(client.admin(), admin);
    assert_eq!(
        client.version(),
        String::from_str(&env, env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn test_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let (client, admin) = deploy_contract(&env);
    assert_eq!(
        client.version(),
        String::from_str(&env, env!("CARGO_PKG_VERSION"))
    );

    let new_wasm_hash = install_contract_wasm(&env);
    client.upgrade(&new_wasm_hash, &String::from_str(&env, "0.0.2"));

    assert_eq!(client.version(), String::from_str(&env, "0.0.2"));
    assert_eq!(client.admin(), admin);
}
