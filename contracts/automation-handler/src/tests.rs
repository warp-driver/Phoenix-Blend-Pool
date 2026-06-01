//! Integration tests for the automation handler.
//!
//! These tests bypass the envelope/signature path via the `#[cfg(test)]`
//! `test_rebalance` / `test_harvest` entrypoints and exercise the core
//! executor logic against mock implementations of the two external
//! contracts the handler talks to: the blended pool and the Blend lending
//! pool. All token movements use real `StellarAssetContract` instances so
//! balance assertions reflect actual transfers.
//!
//! The handler does NOT swap BLND in this design. BLND emissions are routed
//! straight from `Blend.claim(..., to=blnd_treasury)`, so the handler never
//! holds BLND. The tests verify BLND lands in the treasury and the USDC
//! interest portion lands in the pool via `donate`.

extern crate alloc;

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{
    contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec,
};

use crate::contract::{AutomationHandler, AutomationHandlerClient};
use crate::externals::{
    BlendPositions, BlendRequest, DelegateState, BLEND_REQUEST_SUPPLY, BLEND_REQUEST_WITHDRAW,
};

const TARGET_BPS: u32 = 5_000; // 50%
const BAND_BPS: u32 = 500; // +/- 5%
const MIN_TOTAL_USDC: i128 = 1_000_000_000; // 100 USDC at 7 decimals
const COOLDOWN_SECS: u64 = 60;
const USDC_RESERVE_TOKEN_ID: u32 = 1;
const INITIAL_TS: u64 = 1_000_000;

// Storage keys for the mocks. Symbol_short for cheap reads.
const KEY_DELEG: Symbol = symbol_short!("DELEG");
const KEY_USDC: Symbol = symbol_short!("USDC");
const KEY_XLM: Symbol = symbol_short!("XLM");
const KEY_BLND: Symbol = symbol_short!("BLND");
const KEY_STATE: Symbol = symbol_short!("STATE");
const KEY_WD: Symbol = symbol_short!("WD");
const KEY_DP: Symbol = symbol_short!("DP");
const KEY_DONATE: Symbol = symbol_short!("DONATE");
const KEY_CLAIM_AMT: Symbol = symbol_short!("CLAIMAMT");
const KEY_REDEEM: Symbol = symbol_short!("REDEEM");
const KEY_SUPPLIED: Symbol = symbol_short!("SUPPLIED");
const KEY_SUP_CALL: Symbol = symbol_short!("SUPCALL");
const KEY_WD_CALL: Symbol = symbol_short!("WDCALL");

// --- MockBlendedPool ---------------------------------------------------------
//
// Stand-in for `phoenix-pool-blended`. Implements the four methods the handler
// calls via `BlendedPoolClient`. `query_delegate_state` returns whatever
// `set_state` was called with; the three mutating methods move real tokens
// and record the last call for assertions.

#[contract]
pub struct MockBlendedPool;

#[contractimpl]
impl MockBlendedPool {
    pub fn __constructor(env: Env, usdc: Address, xlm: Address) {
        env.storage().instance().set(&KEY_USDC, &usdc);
        env.storage().instance().set(&KEY_XLM, &xlm);
        let zero = DelegateState {
            delegate: None,
            liquid_a: 0,
            liquid_b: 0,
            delegated_a: 0,
            delegated_b: 0,
            total_a: 0,
            total_b: 0,
        };
        env.storage().instance().set(&KEY_STATE, &zero);
    }

    pub fn set_delegate(env: Env, delegate: Address) {
        env.storage().instance().set(&KEY_DELEG, &delegate);
        let mut state: DelegateState = env.storage().instance().get(&KEY_STATE).unwrap();
        state.delegate = Some(delegate);
        env.storage().instance().set(&KEY_STATE, &state);
    }

    pub fn set_state(env: Env, state: DelegateState) {
        env.storage().instance().set(&KEY_STATE, &state);
    }

    pub fn last_withdraw(env: Env) -> Option<(Address, i128)> {
        env.storage().instance().get(&KEY_WD)
    }

    pub fn last_deposit(env: Env) -> Option<(Address, i128)> {
        env.storage().instance().get(&KEY_DP)
    }

    pub fn last_donate(env: Env) -> Option<(Address, i128)> {
        env.storage().instance().get(&KEY_DONATE)
    }

    pub fn withdraw_to_delegate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &delegate,
            &amount,
        );
        env.storage().instance().set(&KEY_WD, &(token, amount));
    }

    pub fn deposit_from_delegate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &delegate,
            &env.current_contract_address(),
            &amount,
        );
        env.storage().instance().set(&KEY_DP, &(token, amount));
    }

    pub fn donate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &delegate,
            &env.current_contract_address(),
            &amount,
        );
        env.storage().instance().set(&KEY_DONATE, &(token, amount));
    }

    pub fn query_delegate_state(env: Env) -> DelegateState {
        env.storage().instance().get(&KEY_STATE).unwrap()
    }
}

// --- MockBlendPool -----------------------------------------------------------
//
// Stand-in for the Blend lending pool. Implements `submit` (Supply / Withdraw)
// and `claim` (BLND emissions transferred to the configured `to` address;
// the handler passes blnd_treasury). Supply transfers from caller to mock;
// Withdraw transfers from mock back, capped at the supplied amount or the
// test-set `redeemable` override (used to simulate a bad-debt write-down).

#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    pub fn __constructor(env: Env, blnd: Address) {
        env.storage().instance().set(&KEY_BLND, &blnd);
    }

    /// Set the amount BLND.claim should pay out next. Auto-resets to 0 after a
    /// successful claim so subsequent claims don't double-pay.
    pub fn set_claim_amount(env: Env, amount: i128) {
        env.storage().instance().set(&KEY_CLAIM_AMT, &amount);
    }

    /// Override the redeemable amount on `Withdraw(i128::MAX)`. If unset,
    /// Withdraw returns the currently-supplied amount (happy path).
    /// Used to simulate bad-debt: redeemable < supplied principal.
    pub fn set_redeemable(env: Env, amount: i128) {
        env.storage().instance().set(&KEY_REDEEM, &amount);
    }

    pub fn supplied_amount(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&(KEY_SUPPLIED, token))
            .unwrap_or(0i128)
    }

    pub fn last_submit_supply(env: Env) -> Option<(Address, i128)> {
        env.storage().instance().get(&KEY_SUP_CALL)
    }

    pub fn last_submit_withdraw(env: Env) -> Option<(Address, i128)> {
        env.storage().instance().get(&KEY_WD_CALL)
    }

    pub fn submit(
        env: Env,
        from: Address,
        _spender: Address,
        to: Address,
        requests: Vec<BlendRequest>,
    ) -> BlendPositions {
        for req in requests.iter() {
            if req.request_type == BLEND_REQUEST_SUPPLY {
                token::Client::new(&env, &req.address).transfer(
                    &from,
                    &env.current_contract_address(),
                    &req.amount,
                );
                let key = (KEY_SUPPLIED, req.address.clone());
                let prev: i128 = env.storage().instance().get(&key).unwrap_or(0);
                env.storage().instance().set(&key, &(prev + req.amount));
                env.storage()
                    .instance()
                    .set(&KEY_SUP_CALL, &(req.address.clone(), req.amount));
            } else if req.request_type == BLEND_REQUEST_WITHDRAW {
                let key = (KEY_SUPPLIED, req.address.clone());
                let supplied: i128 = env.storage().instance().get(&key).unwrap_or(0);
                let redeem_override: Option<i128> =
                    env.storage().instance().get(&KEY_REDEEM);
                let to_withdraw = if req.amount == i128::MAX {
                    redeem_override.unwrap_or(supplied)
                } else {
                    req.amount.min(supplied)
                };
                token::Client::new(&env, &req.address).transfer(
                    &env.current_contract_address(),
                    &to,
                    &to_withdraw,
                );
                env.storage()
                    .instance()
                    .set(&key, &(supplied - to_withdraw.min(supplied)));
                env.storage()
                    .instance()
                    .set(&KEY_WD_CALL, &(req.address.clone(), to_withdraw));
            }
        }
        BlendPositions {
            liabilities: Map::new(&env),
            collateral: Map::new(&env),
            supply: Map::new(&env),
        }
    }

    pub fn claim(env: Env, _from: Address, _ids: Vec<u32>, to: Address) -> i128 {
        let amount: i128 = env
            .storage()
            .instance()
            .get(&KEY_CLAIM_AMT)
            .unwrap_or(0i128);
        if amount > 0 {
            let blnd: Address = env.storage().instance().get(&KEY_BLND).unwrap();
            token::Client::new(&env, &blnd).transfer(
                &env.current_contract_address(),
                &to,
                &amount,
            );
            env.storage().instance().set(&KEY_CLAIM_AMT, &0i128);
        }
        amount
    }
}

// --- Setup -------------------------------------------------------------------

struct Harness<'a> {
    env: Env,
    handler: AutomationHandlerClient<'a>,
    handler_id: Address,
    mock_pool: MockBlendedPoolClient<'a>,
    mock_pool_id: Address,
    mock_blend: MockBlendPoolClient<'a>,
    mock_blend_id: Address,
    usdc: Address,
    xlm: Address,
    #[allow(dead_code)]
    blnd: Address,
    blnd_treasury: Address,
    usdc_admin: token::StellarAssetClient<'a>,
    blnd_admin: token::StellarAssetClient<'a>,
    blnd_token: token::Client<'a>,
}

fn setup() -> Harness<'static> {
    let env: Env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().set_timestamp(INITIAL_TS);

    let admin = Address::generate(&env);
    let usdc_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let xlm_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let blnd_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc = usdc_sac.address();
    let xlm = xlm_sac.address();
    let blnd = blnd_sac.address();
    let usdc_admin = token::StellarAssetClient::new(&env, &usdc);
    let blnd_admin = token::StellarAssetClient::new(&env, &blnd);
    let blnd_token = token::Client::new(&env, &blnd);

    let mock_pool_id = env.register(MockBlendedPool, (usdc.clone(), xlm.clone()));
    let mock_blend_id = env.register(MockBlendPool, (blnd.clone(),));

    // Treasury that receives BLND emissions. A plain generated address; tests
    // read its BLND balance directly.
    let blnd_treasury = Address::generate(&env);

    let verification = Address::generate(&env);

    let handler_id = env.register(
        AutomationHandler,
        (
            verification.clone(),
            mock_pool_id.clone(),
            mock_blend_id.clone(),
            usdc.clone(),
            xlm.clone(),
            blnd_treasury.clone(),
            USDC_RESERVE_TOKEN_ID,
            TARGET_BPS,
            BAND_BPS,
            MIN_TOTAL_USDC,
            COOLDOWN_SECS,
        ),
    );

    let mock_pool = MockBlendedPoolClient::new(&env, &mock_pool_id);
    let mock_blend = MockBlendPoolClient::new(&env, &mock_blend_id);
    let handler = AutomationHandlerClient::new(&env, &handler_id);

    mock_pool.set_delegate(&handler_id);

    Harness {
        env,
        handler,
        handler_id,
        mock_pool,
        mock_pool_id,
        mock_blend,
        mock_blend_id,
        usdc,
        xlm,
        blnd,
        blnd_treasury,
        usdc_admin,
        blnd_admin,
        blnd_token,
    }
}

fn usdc_is_a(h: &Harness) -> bool {
    h.usdc < h.xlm
}

fn set_usdc_state(h: &Harness, liquid: i128, delegated: i128) {
    let total = liquid + delegated;
    let dummy_xlm: i128 = 999_999_999_999;
    let state = if usdc_is_a(h) {
        DelegateState {
            delegate: Some(h.handler_id.clone()),
            liquid_a: liquid,
            liquid_b: dummy_xlm,
            delegated_a: delegated,
            delegated_b: 0,
            total_a: total,
            total_b: dummy_xlm,
        }
    } else {
        DelegateState {
            delegate: Some(h.handler_id.clone()),
            liquid_a: dummy_xlm,
            liquid_b: liquid,
            delegated_a: 0,
            delegated_b: delegated,
            total_a: dummy_xlm,
            total_b: total,
        }
    };
    h.mock_pool.set_state(&state);
}

// --- Rebalance tests ---------------------------------------------------------

#[test]
fn rebalance_to_blend_above_band_moves_excess() {
    let h = setup();
    let liquid: i128 = 700_000_000_000;
    let delegated: i128 = 300_000_000_000;
    set_usdc_state(&h, liquid, delegated);
    h.usdc_admin.mint(&h.mock_pool_id, &liquid);

    h.handler.test_rebalance();

    let expected_amount: i128 = 200_000_000_000;
    let wd = h.mock_pool.last_withdraw().expect("withdraw_to_delegate not called");
    assert_eq!(wd, (h.usdc.clone(), expected_amount));
    let sup = h.mock_blend.last_submit_supply().expect("Blend Supply not called");
    assert_eq!(sup, (h.usdc.clone(), expected_amount));
    assert_eq!(h.handler.principal_supplied(), expected_amount);
    assert_eq!(h.handler.last_rebalance_ts(), INITIAL_TS);
}

#[test]
fn rebalance_from_blend_below_band_pulls_topup() {
    let h = setup();
    let liquid: i128 = 300_000_000_000;
    let delegated: i128 = 700_000_000_000;
    set_usdc_state(&h, liquid, delegated);
    h.handler.test_set_principal_supplied(&delegated);
    h.usdc_admin.mint(&h.mock_blend_id, &delegated);
    let stash = Address::generate(&h.env);
    h.usdc_admin.mint(&stash, &delegated);
    let mut requests: Vec<BlendRequest> = Vec::new(&h.env);
    requests.push_back(BlendRequest {
        request_type: BLEND_REQUEST_SUPPLY,
        address: h.usdc.clone(),
        amount: delegated,
    });
    h.mock_blend.submit(&stash, &stash, &stash, &requests);

    h.handler.test_rebalance();

    let expected_amount: i128 = 200_000_000_000;
    let wd = h.mock_blend.last_submit_withdraw().expect("Blend Withdraw not called");
    assert_eq!(wd, (h.usdc.clone(), expected_amount));
    let dp = h.mock_pool.last_deposit().expect("deposit_from_delegate not called");
    assert_eq!(dp, (h.usdc.clone(), expected_amount));
    assert_eq!(h.handler.principal_supplied(), delegated - expected_amount);
    assert_eq!(h.handler.last_rebalance_ts(), INITIAL_TS);
}

#[test]
fn rebalance_within_band_is_no_op() {
    let h = setup();
    set_usdc_state(&h, 520_000_000_000, 480_000_000_000);

    h.handler.test_rebalance();

    assert!(h.mock_pool.last_withdraw().is_none());
    assert!(h.mock_pool.last_deposit().is_none());
    assert!(h.mock_blend.last_submit_supply().is_none());
    assert!(h.mock_blend.last_submit_withdraw().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
    assert_eq!(h.handler.last_rebalance_ts(), 0);
}

#[test]
fn rebalance_below_min_total_is_no_op() {
    let h = setup();
    set_usdc_state(&h, 100_000_000, 400_000_000);

    h.handler.test_rebalance();

    assert!(h.mock_pool.last_withdraw().is_none());
    assert!(h.mock_blend.last_submit_supply().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
    assert_eq!(h.handler.last_rebalance_ts(), 0);
}

#[test]
fn rebalance_under_cooldown_is_no_op() {
    let h = setup();
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &700_000_000_000);

    h.handler.test_rebalance();
    let principal_after_first = h.handler.principal_supplied();
    assert!(principal_after_first > 0);

    set_usdc_state(&h, 800_000_000_000, 200_000_000_000);
    h.env.ledger().set_timestamp(INITIAL_TS + 30);

    h.handler.test_rebalance();

    assert_eq!(h.handler.principal_supplied(), principal_after_first);
}

#[test]
fn rebalance_after_cooldown_acts_again() {
    let h = setup();
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &700_000_000_000);

    h.handler.test_rebalance();
    let principal_after_first = h.handler.principal_supplied();

    h.env.ledger().set_timestamp(INITIAL_TS + COOLDOWN_SECS + 1);
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &200_000_000_000);

    h.handler.test_rebalance();

    assert!(h.handler.principal_supplied() > principal_after_first);
    assert_eq!(h.handler.last_rebalance_ts(), INITIAL_TS + COOLDOWN_SECS + 1);
}

#[test]
fn rebalance_from_blend_caps_at_principal() {
    let h = setup();
    let liquid: i128 = 300_000_000_000;
    let delegated_in_pool: i128 = 700_000_000_000;
    set_usdc_state(&h, liquid, delegated_in_pool);

    let small_principal: i128 = 100_000_000_000;
    h.handler.test_set_principal_supplied(&small_principal);
    h.usdc_admin.mint(&h.mock_blend_id, &small_principal);
    let stash = Address::generate(&h.env);
    h.usdc_admin.mint(&stash, &small_principal);
    let mut req: Vec<BlendRequest> = Vec::new(&h.env);
    req.push_back(BlendRequest {
        request_type: BLEND_REQUEST_SUPPLY,
        address: h.usdc.clone(),
        amount: small_principal,
    });
    h.mock_blend.submit(&stash, &stash, &stash, &req);

    h.handler.test_rebalance();

    let wd = h.mock_blend.last_submit_withdraw().expect("Blend Withdraw not called");
    assert_eq!(wd, (h.usdc.clone(), small_principal));
    let dp = h.mock_pool.last_deposit().expect("deposit_from_delegate not called");
    assert_eq!(dp, (h.usdc.clone(), small_principal));
    assert_eq!(h.handler.principal_supplied(), 0);
}

// --- Harvest tests -----------------------------------------------------------

/// Seed the Blend mock with a USDC supply position. Sets the handler's
/// `principal_supplied` accounting to match, and drives a real Supply call on
/// the mock so its internal `supplied_amount` ledger is consistent.
fn seed_blend_supply(h: &Harness, amount: i128) {
    h.handler.test_set_principal_supplied(&amount);
    let stash = Address::generate(&h.env);
    h.usdc_admin.mint(&stash, &amount);
    let mut req: Vec<BlendRequest> = Vec::new(&h.env);
    req.push_back(BlendRequest {
        request_type: BLEND_REQUEST_SUPPLY,
        address: h.usdc.clone(),
        amount,
    });
    h.mock_blend.submit(&stash, &stash, &stash, &req);
}

#[test]
fn harvest_happy_path_donates_interest_only() {
    // Position: 100 USDC supplied. Blend has 102 USDC redeemable (2% interest).
    // No BLND emissions. Expected: re-supply 100, donate 2 USDC, principal
    // unchanged.
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let interest: i128 = 2_000_000_000;

    seed_blend_supply(&h, principal);
    h.usdc_admin.mint(&h.mock_blend_id, &interest);
    h.mock_blend.set_redeemable(&(principal + interest));

    h.env.ledger().set_timestamp(INITIAL_TS + 3600);

    h.handler.test_harvest();

    let donate = h.mock_pool.last_donate().expect("donate not called");
    assert_eq!(donate, (h.usdc.clone(), interest));
    assert_eq!(h.handler.principal_supplied(), principal);

    // BLND treasury balance is zero because no emissions were configured.
    assert_eq!(h.blnd_token.balance(&h.blnd_treasury), 0);
}

#[test]
fn harvest_routes_blnd_emissions_to_treasury() {
    // Position: 100 USDC + 50 BLND emissions accruable + 2 USDC interest.
    // Expected: BLND lands in the treasury, USDC interest lands as donate.
    // Handler holds NEITHER BLND nor USDC after the harvest.
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let interest: i128 = 2_000_000_000;
    let blnd_emissions: i128 = 50_000_000_000;

    seed_blend_supply(&h, principal);
    h.usdc_admin.mint(&h.mock_blend_id, &interest);
    h.mock_blend.set_redeemable(&(principal + interest));
    h.blnd_admin.mint(&h.mock_blend_id, &blnd_emissions);
    h.mock_blend.set_claim_amount(&blnd_emissions);

    h.handler.test_harvest();

    // BLND was routed directly to the treasury, NOT to the handler.
    assert_eq!(h.blnd_token.balance(&h.blnd_treasury), blnd_emissions);
    assert_eq!(h.blnd_token.balance(&h.handler_id), 0);

    // USDC interest lands as a donate.
    let donate = h.mock_pool.last_donate().expect("donate not called");
    assert_eq!(donate, (h.usdc.clone(), interest));
    assert_eq!(h.handler.principal_supplied(), principal);
}

#[test]
fn harvest_bad_debt_shrinks_principal_no_donate() {
    // Position: 100 USDC principal. Blend wrote down: only 70 redeemable.
    // Expected: re-supply 70, principal_supplied = 70, no donate (interest
    // delta is 0 since we re-supplied everything that came back).
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let redeemable: i128 = 70_000_000_000;

    seed_blend_supply(&h, principal);
    h.mock_blend.set_redeemable(&redeemable);

    h.handler.test_harvest();

    let sup = h.mock_blend.last_submit_supply().expect("Supply not called");
    assert_eq!(sup, (h.usdc.clone(), redeemable));
    assert_eq!(h.handler.principal_supplied(), redeemable);
    assert!(h.mock_pool.last_donate().is_none());
}

#[test]
fn harvest_bad_debt_still_routes_blnd_to_treasury() {
    // Bad-debt write-down zeros out the interest donate but BLND emissions
    // still route to the treasury. Verifies the two paths are independent.
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let redeemable: i128 = 70_000_000_000;
    let blnd_emissions: i128 = 10_000_000_000;

    seed_blend_supply(&h, principal);
    h.mock_blend.set_redeemable(&redeemable);
    h.blnd_admin.mint(&h.mock_blend_id, &blnd_emissions);
    h.mock_blend.set_claim_amount(&blnd_emissions);

    h.handler.test_harvest();

    assert_eq!(h.blnd_token.balance(&h.blnd_treasury), blnd_emissions);
    assert!(h.mock_pool.last_donate().is_none());
    assert_eq!(h.handler.principal_supplied(), redeemable);
}

#[test]
fn harvest_zero_principal_skips_withdraw_resupply() {
    // No position in Blend. Harvest claims emissions (here 0) and that's it.
    let h = setup();

    h.handler.test_harvest();

    assert!(h.mock_blend.last_submit_supply().is_none());
    assert!(h.mock_blend.last_submit_withdraw().is_none());
    assert!(h.mock_pool.last_donate().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
    assert_eq!(h.blnd_token.balance(&h.blnd_treasury), 0);
}

#[test]
fn harvest_zero_principal_still_routes_blnd_to_treasury() {
    // No USDC position but BLND emissions claimable from a prior cycle.
    // The treasury still receives BLND; no donate happens.
    let h = setup();
    let blnd_emissions: i128 = 5_000_000_000;
    h.blnd_admin.mint(&h.mock_blend_id, &blnd_emissions);
    h.mock_blend.set_claim_amount(&blnd_emissions);

    h.handler.test_harvest();

    assert_eq!(h.blnd_token.balance(&h.blnd_treasury), blnd_emissions);
    assert!(h.mock_pool.last_donate().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
}
