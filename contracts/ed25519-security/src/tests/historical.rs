extern crate std;

use super::setup::deploy_contract;
use soroban_sdk::{Env, Vec, testutils::Ledger as _};
use warpdrive_shared::testutils::{ed25519_pubkey, make_ed25519_key};

type PubKey = soroban_sdk::BytesN<32>;

#[test]
fn test_weight_at_before_any_checkpoint() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| li.sequence_number = 100);

    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);
    client.add_signer(&pk, &50);

    // Query before the signer was added
    assert_eq!(client.get_signer_weight_at(&pk, &99), 0);
    assert_eq!(client.get_total_weight_at(&99), 0);
}

#[test]
fn test_weight_at_exact_checkpoint() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| li.sequence_number = 100);

    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);
    client.add_signer(&pk, &50);

    assert_eq!(client.get_signer_weight_at(&pk, &100), 50);
    assert_eq!(client.get_total_weight_at(&100), 50);
}

#[test]
fn test_weight_at_between_checkpoints() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);
    client.add_signer(&pk, &50);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.add_signer(&pk, &100);

    // Between checkpoints: should return value at ledger 100
    assert_eq!(client.get_signer_weight_at(&pk, &150), 50);
    assert_eq!(client.get_total_weight_at(&150), 50);

    // At ledger 200
    assert_eq!(client.get_signer_weight_at(&pk, &200), 100);
    assert_eq!(client.get_total_weight_at(&200), 100);
}

#[test]
fn test_weight_at_after_removal() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let (client, _admin) = deploy_contract(&env);

    let key = make_ed25519_key(1);
    let pk = ed25519_pubkey(&env, &key);
    client.add_signer(&pk, &50);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.remove_signer(&pk);

    // Before removal: weight 50
    assert_eq!(client.get_signer_weight_at(&pk, &150), 50);
    // At removal: weight 0
    assert_eq!(client.get_signer_weight_at(&pk, &200), 0);
    assert_eq!(client.get_total_weight_at(&200), 0);
}

/// Batch weight queries at reference block
#[test]
fn test_batch_weights_at() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let (client, _admin) = deploy_contract(&env);

    let key1 = make_ed25519_key(1);
    let key2 = make_ed25519_key(2);
    let pk1 = ed25519_pubkey(&env, &key1);
    let pk2 = ed25519_pubkey(&env, &key2);

    client.add_signer(&pk1, &30);
    client.add_signer(&pk2, &70);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.add_signer(&pk1, &80);

    let mut keys: Vec<PubKey> = Vec::new(&env);
    keys.push_back(pk1.clone());
    keys.push_back(pk2.clone());

    // At ledger 100: pk1=30, pk2=70
    let weights = client.get_signer_weights_at(&keys, &100);
    assert_eq!(weights.get(0).unwrap(), 30);
    assert_eq!(weights.get(1).unwrap(), 70);

    // At ledger 200: pk1=80, pk2=70
    let weights = client.get_signer_weights_at(&keys, &200);
    assert_eq!(weights.get(0).unwrap(), 80);
    assert_eq!(weights.get(1).unwrap(), 70);
}

/// get_signer_weights (non-historical batch)
#[test]
fn test_batch_weights_current() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = deploy_contract(&env);

    let key1 = make_ed25519_key(1);
    let key2 = make_ed25519_key(2);
    let key3 = make_ed25519_key(3);
    let pk1 = ed25519_pubkey(&env, &key1);
    let pk2 = ed25519_pubkey(&env, &key2);
    let pk3 = ed25519_pubkey(&env, &key3);

    client.add_signer(&pk1, &30);
    client.add_signer(&pk2, &70);

    let mut keys: Vec<PubKey> = Vec::new(&env);
    keys.push_back(pk1);
    keys.push_back(pk2);
    keys.push_back(pk3); // not registered

    let weights = client.get_signer_weights(&keys);
    assert_eq!(weights.get(0).unwrap(), 30);
    assert_eq!(weights.get(1).unwrap(), 70);
    assert_eq!(weights.get(2).unwrap(), 0); // unregistered → 0
}
