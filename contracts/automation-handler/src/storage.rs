use soroban_sdk::{contracttype, Address, BytesN, Env, String};
use warpdrive_shared::ttl;

#[contracttype]
pub enum DataKey {
    VerificationContract,
    BlendedPool,
    LegacyPool,
    BlendPool,
    Usdc,
    Xlm,
    Blnd,
    BlndSwapPool,
    UsdcReserveTokenId,
    PrincipalSupplied,
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
    get_legacy_pool,
    set_legacy_pool,
    DataKey::LegacyPool,
    "legacy pool not set"
);
address_accessors!(
    get_blend_pool,
    set_blend_pool,
    DataKey::BlendPool,
    "blend pool not set"
);
address_accessors!(get_usdc, set_usdc, DataKey::Usdc, "usdc not set");
address_accessors!(get_xlm, set_xlm, DataKey::Xlm, "xlm not set");
address_accessors!(get_blnd, set_blnd, DataKey::Blnd, "blnd not set");
address_accessors!(
    get_blnd_swap_pool,
    set_blnd_swap_pool,
    DataKey::BlndSwapPool,
    "blnd swap pool not set"
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

/// Running sum of net USDC supplied to Blend. Incremented on ToBlend, decremented
/// on FromBlend. Used by HarvestYield to compute interest = redeemable - principal.
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
