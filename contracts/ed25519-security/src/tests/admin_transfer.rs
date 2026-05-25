extern crate std;

use super::setup::deploy_contract;
use soroban_sdk::{
    Address, Env, Vec,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
};

#[test]
fn test_propose_and_accept_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, old_admin) = deploy_contract(&env);
    let new_admin = Address::generate(&env);

    assert_eq!(client.admin(), old_admin);
    assert_eq!(client.pending_admin(), None);

    // Propose
    client.propose_admin(&new_admin);
    assert_eq!(client.pending_admin(), Some(new_admin.clone()));
    assert_eq!(client.admin(), old_admin); // not changed yet

    // Accept
    client.accept_admin();
    assert_eq!(client.admin(), new_admin);
    assert_eq!(client.pending_admin(), None); // cleared
}

#[test]
fn test_propose_admin_requires_current_admin() {
    let env = Env::default();

    let (client, _admin) = deploy_contract(&env);
    let attacker = Address::generate(&env);
    let new_admin = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "propose_admin",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);

    let result = client.try_propose_admin(&new_admin);
    assert!(result.is_err());
    assert_eq!(client.pending_admin(), None);
}

#[test]
fn test_accept_admin_requires_pending_admin() {
    let env = Env::default();

    let (client, admin) = deploy_contract(&env);
    let new_admin = Address::generate(&env);
    let attacker = Address::generate(&env);

    // Propose first (as admin)
    env.mock_all_auths();
    client.propose_admin(&new_admin);

    // Try to accept as attacker (not the pending admin)
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "accept_admin",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);

    let result = client.try_accept_admin();
    assert!(result.is_err());
    assert_eq!(client.admin(), admin); // unchanged
}

#[test]
#[should_panic(expected = "no pending admin")]
fn test_accept_without_propose_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    client.accept_admin();
}

#[test]
fn test_propose_overwrites_previous_pending() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    client.propose_admin(&first);
    assert_eq!(client.pending_admin(), Some(first));

    client.propose_admin(&second);
    assert_eq!(client.pending_admin(), Some(second.clone()));

    client.accept_admin();
    assert_eq!(client.admin(), second);
}
