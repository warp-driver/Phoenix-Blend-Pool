extern crate std;

use crate::SecurityError;

use super::setup::deploy_contract;
use soroban_sdk::{
    Address, Env, Vec,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
};
use warpdrive_shared::testutils::{ed25519_pubkey, make_ed25519_key};

#[test]
fn test_set_threshold_success() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    // Initial threshold from deploy_contract is 2/3
    assert_eq!(client.threshold_numerator(), 2);
    assert_eq!(client.threshold_denominator(), 3);

    // Change to 7/10
    client.set_threshold(&7, &10);
    assert_eq!(client.threshold_numerator(), 7);
    assert_eq!(client.threshold_denominator(), 10);
}

#[test]
fn test_set_threshold_zero_denominator() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let result = client.try_set_threshold(&1, &0);
    assert_eq!(result, Err(Ok(SecurityError::ZeroDenominator)));
}

#[test]
fn test_set_threshold_zero_numerator() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let result = client.try_set_threshold(&0, &10);
    assert_eq!(result, Err(Ok(SecurityError::ZeroNumerator)));
}

#[test]
fn test_set_threshold_numerator_exceeds_denominator() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let result = client.try_set_threshold(&11, &10);
    assert_eq!(result, Err(Ok(SecurityError::NumeratorExceedsDenominator)));
}

#[test]
fn test_set_threshold_affects_required_weight() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);

    client.add_signer(&pk, &300);
    // threshold is 2/3 → required = 300 * 2 / 3 = 200
    assert_eq!(client.required_weight(), 200);

    // Change threshold to 1/2 → required = 300 * 1 / 2 = 150
    client.set_threshold(&1, &2);
    assert_eq!(client.required_weight(), 150);
}

#[test]
fn test_set_threshold_requires_admin() {
    let env = Env::default();

    let (client, _admin) = deploy_contract(&env);
    let non_admin = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "set_threshold",
            args: Vec::new(&env),
            sub_invokes: &[],
        },
    }]);

    let result = client.try_set_threshold(&1, &2);
    assert!(result.is_err());
}

#[test]
fn test_threshold_getters_match_constructor() {
    let env = Env::default();
    let (client, _admin) = deploy_contract(&env);

    // deploy_contract creates with (2, 3)
    assert_eq!(client.threshold_numerator(), 2);
    assert_eq!(client.threshold_denominator(), 3);
}

#[test]
fn test_add_signer_zero_weight_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);

    let result = client.try_add_signer(&pk, &0);
    assert_eq!(result, Err(Ok(SecurityError::ZeroWeight)));
    assert_eq!(client.get_total_weight(), 0);
    assert_eq!(client.list_signers().len(), 0);
}
