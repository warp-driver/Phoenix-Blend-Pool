extern crate std;

use warpdrive_shared::interfaces::Ed25519PubKey;

use super::setup::deploy_contract;
use soroban_sdk::{
    Address, Env, IntoVal, Vec,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
};
use warpdrive_shared::testutils::{ed25519_pubkey, make_ed25519_key};

fn make_signer(env: &Env, seed: u8) -> Ed25519PubKey {
    ed25519_pubkey(env, &make_ed25519_key(seed))
}

#[test]
fn test_add_signer_basic_queries() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    let key = make_signer(&env, 1);
    let key2 = make_signer(&env, 2);
    let weight = 45;

    // Nothing to start
    assert_eq!(client.get_total_weight(), 0);
    assert_eq!(client.get_signer_weight(&key), 0);
    assert_eq!(client.list_signers().len(), 0);

    // Add signer
    client.add_signer(&key, &weight);

    // Got values
    assert_eq!(client.get_total_weight(), weight);
    assert_eq!(client.get_signer_weight(&key), weight);
    assert_eq!(client.get_signer_weight(&key2), 0);
    assert_eq!(client.list_signers().len(), 1);
}

#[test]
fn test_add_and_remove_signer() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    let key1 = make_signer(&env, 1);
    let key2 = make_signer(&env, 2);

    client.add_signer(&key1, &30);
    client.add_signer(&key2, &70);

    assert_eq!(client.get_total_weight(), 100);
    assert_eq!(client.get_signer_weight(&key1), 30);
    assert_eq!(client.get_signer_weight(&key2), 70);
    assert_eq!(client.list_signers().len(), 2);

    client.remove_signer(&key1);

    assert_eq!(client.get_total_weight(), 70);
    assert_eq!(client.get_signer_weight(&key1), 0);
    assert_eq!(client.get_signer_weight(&key2), 70);
    assert_eq!(client.list_signers().len(), 1);
}

#[test]
fn test_update_signer_weight() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    let key = make_signer(&env, 1);

    client.add_signer(&key, &50);
    assert_eq!(client.get_signer_weight(&key), 50);
    assert_eq!(client.get_total_weight(), 50);

    // Update weight by setting same key again
    client.add_signer(&key, &80);
    assert_eq!(client.get_signer_weight(&key), 80);
    assert_eq!(client.get_total_weight(), 80);
    assert_eq!(client.list_signers().len(), 1);
}

#[test]
fn test_assert_admin_unauthorized() {
    let env = Env::default();

    let (client, _admin) = deploy_contract(&env);
    let non_admin = Address::generate(&env);
    let key = make_signer(&env, 4);
    let weight = 102;

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "add_signer",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);

    let result = client.try_add_signer(&key, &weight);
    assert!(result.is_err());
}

#[test]
fn test_assert_admin_auth() {
    let env = Env::default();

    let (client, admin) = deploy_contract(&env);
    let key = make_signer(&env, 4);
    let weight = 102;

    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "add_signer",
            args: (&key, weight).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    client.add_signer(&key, &weight);
    assert_eq!(client.get_total_weight(), weight);
}

// ── Removing a non-existent signer is a no-op ──────────────────────

#[test]
fn test_remove_nonexistent_signer_is_noop() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    let key1 = make_signer(&env, 1);
    let key2 = make_signer(&env, 2);

    client.add_signer(&key1, &50);
    assert_eq!(client.get_total_weight(), 50);

    // Remove a key that was never added — should be a silent no-op
    client.remove_signer(&key2);
    assert_eq!(client.get_total_weight(), 50);
    assert_eq!(client.get_signer_weight(&key1), 50);
    assert_eq!(client.list_signers().len(), 1);
}

// ── Admin auth on remove_signer ─────────────────────────────────────

#[test]
fn test_remove_signer_requires_admin() {
    let env = Env::default();

    let (client, _admin) = deploy_contract(&env);
    let non_admin = Address::generate(&env);
    let key = make_signer(&env, 1);

    // First add a signer so there's something to remove
    env.mock_all_auths();
    client.add_signer(&key, &50);

    // Now try to remove without admin auth
    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "remove_signer",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);

    let result = client.try_remove_signer(&key);
    assert!(result.is_err());
    // Signer should still be there
    assert_eq!(client.get_signer_weight(&key), 50);
}
