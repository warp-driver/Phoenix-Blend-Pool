extern crate alloc;

use alloc::vec::Vec as StdVec;
use soroban_sdk::{Address, BytesN, Env, String, Vec, contracttype};
use warpdrive_shared::ttl;
use warpdrive_shared::vec_history::{self, Entry, VecHistoryStore};

pub use warpdrive_shared::interfaces::security::Ed25519SignerInfo;

use warpdrive_shared::interfaces::Ed25519PubKey;

const HISTORY_CUTOFF: u32 = 200;

#[contracttype]
#[derive(Clone)]
pub struct Threshold {
    pub numerator: u64,
    pub denominator: u64,
}

impl Threshold {
    pub fn new(numerator: u64, denominator: u64) -> Self {
        Threshold {
            numerator,
            denominator,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub struct StoredCheckpoint {
    pub ledger: u32,
    pub value: u64,
}

#[contracttype]
pub enum DataKey {
    // These are instance storage
    Admin,
    Version,
    Threshold,
    // These can grow quite large, all in persistent storage
    AllSigners,
    // Vec-based history (one key per timeline)
    SignerWeightHist(BytesN<32>),
    TotalWeightHist,
}

// ── Admin / Version / Threshold ─────────────────────────────────────

pub fn get_admin(env: &Env) -> Address {
    env.storage().instance().get(&DataKey::Admin).unwrap()
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&DataKey::Admin, admin);
}

pub fn get_version(env: &Env) -> String {
    env.storage().instance().get(&DataKey::Version).unwrap()
}

pub fn set_version(env: &Env, version: &String) {
    env.storage().instance().set(&DataKey::Version, version);
}

pub fn get_threshold(env: &Env) -> Threshold {
    env.storage().instance().get(&DataKey::Threshold).unwrap()
}

pub fn set_threshold(env: &Env, threshold: &Threshold) {
    env.storage().instance().set(&DataKey::Threshold, threshold);
}

pub fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(ttl::INSTANCE_RENEWAL_THRESHOLD, ttl::INSTANCE_TARGET_TTL);
}

// ── Total weight (current snapshot) ─────────────────────────────────

pub fn get_total_weight(env: &Env) -> u64 {
    vec_history::latest(&TotalWeightHistory::new(env))
}

// ── Signer management ───────────────────────────────────────────────

pub fn add_signer(env: &Env, key: Ed25519PubKey, weight: u64) {
    let mut total = get_total_weight(env);

    // If updating an existing signer, subtract the old weight first
    if let Some(old_weight) = get_signer_weight(env, key.clone()) {
        total -= old_weight;
    } else {
        insert_all_signers(env, key.clone());
    }

    total = total
        .checked_add(weight)
        .expect("total weight would overflow u64");

    vec_history::push(&SignerWeightHistory::new(env, key), weight);
    vec_history::push(&TotalWeightHistory::new(env), total);
}

pub fn remove_signer(env: &Env, key: Ed25519PubKey) {
    if let Some(old_weight) = get_signer_weight(env, key.clone()) {
        let total = get_total_weight(env);
        let new_total = total - old_weight;
        remove_all_signers(env, key.clone());

        vec_history::push(&SignerWeightHistory::new(env, key), 0);
        vec_history::push(&TotalWeightHistory::new(env), new_total);
    }
}

pub fn get_signer_weight(env: &Env, key: Ed25519PubKey) -> Option<u64> {
    let weight = vec_history::latest(&SignerWeightHistory::new(env, key));
    if weight == 0 { None } else { Some(weight) }
}

// ── Historical lookups ──────────────────────────────────────────────

pub fn get_signer_weight_at(env: &Env, key: Ed25519PubKey, reference_block: u32) -> u64 {
    vec_history::lookup_at(&SignerWeightHistory::new(env, key), reference_block)
}

pub fn get_signer_weights(env: &Env, keys: &Vec<Ed25519PubKey>) -> Vec<u64> {
    let mut result = Vec::new(env);
    for i in 0..keys.len() {
        let key = keys.get(i).unwrap();
        result.push_back(get_signer_weight(env, key).unwrap_or(0));
    }
    result
}

pub fn get_signer_weights_at(
    env: &Env,
    keys: &Vec<Ed25519PubKey>,
    reference_block: u32,
) -> Vec<u64> {
    let mut result = Vec::new(env);
    for i in 0..keys.len() {
        let key = keys.get(i).unwrap();
        result.push_back(get_signer_weight_at(env, key, reference_block));
    }
    result
}

pub fn get_total_weight_at(env: &Env, reference_block: u32) -> u64 {
    vec_history::lookup_at(&TotalWeightHistory::new(env), reference_block)
}

// ── Signer enumeration (for UI) ─────────────────────────────────────

pub fn list_signers(env: &Env) -> Vec<Ed25519SignerInfo> {
    let signers = all_signers(env);
    let mut result = Vec::new(env);
    for key in signers.into_iter() {
        if let Some(weight) = get_signer_weight(env, key.clone()) {
            result.push_back(Ed25519SignerInfo { key, weight });
        }
    }
    result
}

fn all_signers(env: &Env) -> Vec<Ed25519PubKey> {
    let key = DataKey::AllSigners;
    let result = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    if !result.is_empty() {
        env.storage().persistent().extend_ttl(
            &key,
            ttl::PERSISTENT_RENEWAL_THRESHOLD,
            ttl::PERSISTENT_TARGET_TTL,
        );
    }
    result
}

fn set_all_signers(env: &Env, signers: &Vec<Ed25519PubKey>) {
    let key = DataKey::AllSigners;
    env.storage().persistent().set(&key, signers);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_RENEWAL_THRESHOLD,
        ttl::PERSISTENT_TARGET_TTL,
    );
}

fn insert_all_signers(env: &Env, key: Ed25519PubKey) {
    let mut signers = all_signers(env);
    let len = signers.len();
    let mut idx = len;
    for i in 0..len {
        let existing = signers.get(i).unwrap();
        match key.cmp(&existing) {
            core::cmp::Ordering::Less => {
                idx = i;
                break;
            }
            core::cmp::Ordering::Equal => return,
            core::cmp::Ordering::Greater => {}
        }
    }
    signers.insert(idx, key);
    set_all_signers(env, &signers);
}

fn remove_all_signers(env: &Env, key: Ed25519PubKey) {
    let mut signers = all_signers(env);
    for i in 0..signers.len() {
        let existing = signers.get(i).unwrap();
        if existing == key {
            signers.remove(i);
            set_all_signers(env, &signers);
            return;
        }
        if existing > key {
            break;
        }
    }
}

// ── VecHistoryStore implementations ─────────────────────────────────

fn load_history(env: &Env, key: &DataKey) -> StdVec<Entry<u64>> {
    let stored: Vec<StoredCheckpoint> = env
        .storage()
        .persistent()
        .get(key)
        .unwrap_or_else(|| Vec::new(env));
    if !stored.is_empty() {
        env.storage().persistent().extend_ttl(
            key,
            ttl::PERSISTENT_RENEWAL_THRESHOLD,
            ttl::PERSISTENT_TARGET_TTL,
        );
    }
    let mut result = StdVec::with_capacity(stored.len() as usize);
    for i in 0..stored.len() {
        let sc = stored.get(i).unwrap();
        result.push(Entry {
            ledger: sc.ledger,
            value: sc.value,
        });
    }
    result
}

fn save_history(env: &Env, key: &DataKey, entries: StdVec<Entry<u64>>) {
    let mut stored = Vec::new(env);
    for e in entries {
        stored.push_back(StoredCheckpoint {
            ledger: e.ledger,
            value: e.value,
        });
    }
    env.storage().persistent().set(key, &stored);
    env.storage().persistent().extend_ttl(
        key,
        ttl::PERSISTENT_RENEWAL_THRESHOLD,
        ttl::PERSISTENT_TARGET_TTL,
    );
}

pub struct SignerWeightHistory<'a> {
    env: &'a Env,
    key: Ed25519PubKey,
}

impl<'a> SignerWeightHistory<'a> {
    pub fn new(env: &'a Env, key: Ed25519PubKey) -> Self {
        Self { env, key }
    }
}

impl VecHistoryStore for SignerWeightHistory<'_> {
    type Value = u64;

    fn cutoff(&self) -> u32 {
        HISTORY_CUTOFF
    }

    fn load(&self) -> StdVec<Entry<u64>> {
        load_history(self.env, &DataKey::SignerWeightHist(self.key.clone()))
    }

    fn save(&self, entries: StdVec<Entry<u64>>) {
        save_history(
            self.env,
            &DataKey::SignerWeightHist(self.key.clone()),
            entries,
        );
    }

    fn current_ledger(&self) -> u32 {
        self.env.ledger().sequence()
    }
}

pub struct TotalWeightHistory<'a> {
    env: &'a Env,
}

impl<'a> TotalWeightHistory<'a> {
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }
}

impl VecHistoryStore for TotalWeightHistory<'_> {
    type Value = u64;

    fn cutoff(&self) -> u32 {
        HISTORY_CUTOFF
    }

    fn load(&self) -> StdVec<Entry<u64>> {
        load_history(self.env, &DataKey::TotalWeightHist)
    }

    fn save(&self, entries: StdVec<Entry<u64>>) {
        save_history(self.env, &DataKey::TotalWeightHist, entries);
    }

    fn current_ledger(&self) -> u32 {
        self.env.ledger().sequence()
    }
}
