use soroban_sdk::{contracttype, Address, BytesN, Env, String};
use warpdrive_shared::ttl;

#[contracttype]
pub enum DataKey {
    Admin,
    VerificationContract,
    BlendedPool,
    BlendPool,
    Usdc,
    Xlm,
    BlndTreasury,
    UsdcReserveTokenId,
    PrincipalSupplied,
    TargetRatioBps,
    RebalanceBandBps,
    MinTotalUsdc,
    RebalanceCooldownSecs,
    LastRebalanceTs,
    Version,
    EventSeen(BytesN<20>),
}

macro_rules! address_accessors {
    ($get:ident, $set:ident, $key:expr, $missing:expr) => {
        pub fn $set(env: &Env, addr: &Address) {
            env.storage().instance().set(&$key, addr);
        }

        pub fn $get(env: &Env) -> Address {
            env.storage().instance().get(&$key).expect($missing)
        }
    };
}

address_accessors!(get_admin, set_admin, DataKey::Admin, "admin not set");
address_accessors!(
    get_verification_contract,
    set_verification_contract,
    DataKey::VerificationContract,
    "verification contract not set"
);
address_accessors!(
    get_blended_pool,
    set_blended_pool,
    DataKey::BlendedPool,
    "blended pool not set"
);
address_accessors!(
    get_blend_pool,
    set_blend_pool,
    DataKey::BlendPool,
    "blend pool not set"
);
address_accessors!(get_usdc, set_usdc, DataKey::Usdc, "usdc not set");
address_accessors!(get_xlm, set_xlm, DataKey::Xlm, "xlm not set");
address_accessors!(
    get_blnd_treasury,
    set_blnd_treasury,
    DataKey::BlndTreasury,
    "blnd treasury not set"
);

pub fn set_usdc_reserve_token_id(env: &Env, id: u32) {
    env.storage()
        .instance()
        .set(&DataKey::UsdcReserveTokenId, &id);
}

pub fn get_usdc_reserve_token_id(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::UsdcReserveTokenId)
        .expect("usdc reserve token id not set")
}

/// Drift band (bps) around the 50% target above which the handler will move
/// USDC between Blend and the pool. Set in the constructor; read on each
/// Rebalance dispatch.
pub fn set_target_ratio_bps(env: &Env, bps: u32) {
    env.storage().instance().set(&DataKey::TargetRatioBps, &bps);
}

pub fn get_target_ratio_bps(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::TargetRatioBps)
        .expect("target ratio bps not set")
}

pub fn set_rebalance_band_bps(env: &Env, bps: u32) {
    env.storage()
        .instance()
        .set(&DataKey::RebalanceBandBps, &bps);
}

pub fn get_rebalance_band_bps(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::RebalanceBandBps)
        .expect("rebalance band bps not set")
}

/// Floor on the pool's *total* USDC (liquid + delegated) below which Rebalance
/// is a no-op. Avoids burning gas + Blend min-supply churn on dust pools.
pub fn set_min_total_usdc(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::MinTotalUsdc, &amount);
}

pub fn get_min_total_usdc(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::MinTotalUsdc)
        .expect("min total usdc not set")
}

/// Minimum seconds between two successive rebalance moves. Set in the
/// constructor; checked at the top of execute_rebalance. The cooldown gates
/// only ACTIONS, not no-op invocations: a within-band or below-min check
/// returns without touching this timestamp.
pub fn set_rebalance_cooldown_secs(env: &Env, secs: u64) {
    env.storage()
        .instance()
        .set(&DataKey::RebalanceCooldownSecs, &secs);
}

pub fn get_rebalance_cooldown_secs(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::RebalanceCooldownSecs)
        .expect("rebalance cooldown secs not set")
}

pub fn set_last_rebalance_ts(env: &Env, ts: u64) {
    env.storage()
        .instance()
        .set(&DataKey::LastRebalanceTs, &ts);
}

pub fn get_last_rebalance_ts(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::LastRebalanceTs)
        .unwrap_or(0)
}

/// Running sum of net USDC supplied to Blend. Incremented on the ToBlend leg
/// of Rebalance, decremented on the FromBlend leg. Used by HarvestYield to
/// compute interest = redeemable - principal.
pub fn set_principal_supplied(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::PrincipalSupplied, &amount);
}

pub fn get_principal_supplied(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::PrincipalSupplied)
        .unwrap_or(0)
}

pub fn set_version(env: &Env, v: &String) {
    env.storage().instance().set(&DataKey::Version, v);
}

pub fn get_version(env: &Env) -> String {
    env.storage()
        .instance()
        .get(&DataKey::Version)
        .expect("version not set")
}

pub fn is_event_seen(env: &Env, event_id: &BytesN<20>) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::EventSeen(event_id.clone()))
}

pub fn mark_event_seen(env: &Env, event_id: &BytesN<20>) {
    let key = DataKey::EventSeen(event_id.clone());
    env.storage().persistent().set(&key, &true);
    env.storage().persistent().extend_ttl(
        &key,
        ttl::PERSISTENT_RENEWAL_THRESHOLD,
        ttl::PERSISTENT_TARGET_TTL,
    );
}

pub fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(ttl::INSTANCE_RENEWAL_THRESHOLD, ttl::INSTANCE_TARGET_TTL);
}
