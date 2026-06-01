//! Hand-typed Soroban client traits for the three contracts the handler talks to.
//!
//! We don't path-dep on phoenix-pool-blended / phoenix-pool because they pin
//! soroban-sdk 22 while this handler targets 26. Re-declaring just the
//! signatures we call keeps the build clean and lets the actual contracts
//! evolve independently. The `#[contractclient]` macro consumes each trait
//! at expansion time to emit the *Client struct, so the traits themselves
//! show up as "unused" to rustc.
#![allow(dead_code)]
use soroban_sdk::{contractclient, contracttype, Address, Env, Vec};

// --- Phoenix blended-pool (the fork we built in phoenix-contracts/pool_blended) ---

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegateState {
    pub delegate: Option<Address>,
    pub liquid_a: i128,
    pub liquid_b: i128,
    pub delegated_a: i128,
    pub delegated_b: i128,
    pub total_a: i128,
    pub total_b: i128,
}

#[contractclient(name = "BlendedPoolClient")]
pub trait BlendedPool {
    fn withdraw_to_delegate(env: Env, token: Address, amount: i128);
    fn deposit_from_delegate(env: Env, token: Address, amount: i128);
    fn donate(env: Env, token: Address, amount: i128);
    fn query_delegate_state(env: Env) -> DelegateState;
}



// --- Blend lending pool ---
//
// Mirrored from blend-contracts-v2/pool/src/{contract,pool/actions}.rs.
// We only need Supply (request_type = 0) for the v1 slice.

#[contracttype]
#[derive(Clone)]
pub struct BlendRequest {
    pub request_type: u32,
    pub address: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone)]
pub struct BlendPositions {
    pub liabilities: soroban_sdk::Map<u32, i128>,
    pub collateral: soroban_sdk::Map<u32, i128>,
    pub supply: soroban_sdk::Map<u32, i128>,
}

pub const BLEND_REQUEST_SUPPLY: u32 = 0;
pub const BLEND_REQUEST_WITHDRAW: u32 = 1;

#[contractclient(name = "BlendPoolClient")]
pub trait BlendPool {
    fn submit(
        env: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<BlendRequest>,
    ) -> BlendPositions;

    /// Claim BLND emissions. `reserve_token_ids` identifies which (reserve, token-side)
    /// pairs to claim for; each id = reserve_index * 2 + (0 for d-tokens, 1 for b-tokens).
    /// Returns the total BLND transferred to `to`.
    fn claim(env: Env, from: Address, reserve_token_ids: Vec<u32>, to: Address) -> i128;
}
