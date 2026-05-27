//! Integration tests for the automation handler.
//!
//! These tests bypass the envelope/signature path via the `#[cfg(test)]`
//! `test_rebalance` / `test_harvest` entrypoints and exercise the core
//! executor logic against mock implementations of the three external
//! contracts the handler talks to (the blended pool, the Blend lending pool,
//! and the BLND-USDC swap pool). All token movements use real
//! StellarAssetContract instances so balance assertions reflect actual
//! transfers.

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
const KEY_ASK: Symbol = symbol_short!("ASK");
const KEY_RATE: Symbol = symbol_short!("RATE");
const KEY_SWAP: Symbol = symbol_short!("SWAP");

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
        // Initial state is all zeros; tests override via set_state.
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

    // BlendedPool trait surface (matches the contractclient-generated signatures).

    pub fn withdraw_to_delegate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &delegate,
            &amount,
        );
        env.storage()
            .instance()
            .set(&KEY_WD, &(token, amount));
    }

    pub fn deposit_from_delegate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &delegate,
            &env.current_contract_address(),
            &amount,
        );
        env.storage()
            .instance()
            .set(&KEY_DP, &(token, amount));
    }

    pub fn donate(env: Env, token: Address, amount: i128) {
        let delegate: Address = env.storage().instance().get(&KEY_DELEG).expect("no delegate");
        token::Client::new(&env, &token).transfer(
            &delegate,
            &env.current_contract_address(),
            &amount,
        );
        env.storage()
            .instance()
            .set(&KEY_DONATE, &(token, amount));
    }

    pub fn query_delegate_state(env: Env) -> DelegateState {
        env.storage().instance().get(&KEY_STATE).unwrap()
    }
}

// --- MockBlendPool -----------------------------------------------------------
//
// Stand-in for the Blend lending pool. Implements `submit` (Supply / Withdraw)
// and `claim` (BLND emissions). Supply transfers from caller to mock; Withdraw
// transfers from mock back, capped at the supplied amount or the test-set
// `redeemable` override (used to simulate a bad-debt write-down). Claim pays
// out a test-set BLND amount.

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

    /// Override the redeemable amount on `Withdraw(i128::MAX)`. If unset
    /// (or set to a negative sentinel via this not being called), Withdraw
    /// returns the currently-supplied amount (happy path).
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
                    // "Withdraw all" sentinel: use the override if set
                    // (simulating bad-debt), else return the full supplied
                    // balance (happy path with interest accounted by the
                    // mock_blnd_pool externally via direct mint).
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

// --- MockLegacyPool (BLND-USDC swap) -----------------------------------------
//
// Stand-in for the Phoenix BLND-USDC pool used by HarvestYield to convert
// claimed BLND emissions to USDC. Implements only `swap` and pays out at a
// configurable rate (default 1:1).

#[contract]
pub struct MockLegacyPool;

#[contractimpl]
impl MockLegacyPool {
    pub fn __constructor(env: Env, ask_token: Address) {
        env.storage().instance().set(&KEY_ASK, &ask_token);
        env.storage().instance().set(&KEY_RATE, &1i128);
    }

    pub fn set_rate(env: Env, rate: i128) {
        env.storage().instance().set(&KEY_RATE, &rate);
    }

    pub fn last_swap(env: Env) -> Option<(Address, i128, i128)> {
        env.storage().instance().get(&KEY_SWAP)
    }

    pub fn swap(
        env: Env,
        sender: Address,
        offer_asset: Address,
        offer_amount: i128,
        _ask_asset_min_amount: Option<i128>,
        _max_spread_bps: Option<i64>,
        _deadline: Option<u64>,
        _max_allowed_fee_bps: Option<i64>,
    ) -> i128 {
        let ask: Address = env.storage().instance().get(&KEY_ASK).unwrap();
        let rate: i128 = env.storage().instance().get(&KEY_RATE).unwrap_or(1);
        token::Client::new(&env, &offer_asset).transfer(
            &sender,
            &env.current_contract_address(),
            &offer_amount,
        );
        let ask_amount = offer_amount * rate;
        token::Client::new(&env, &ask).transfer(
            &env.current_contract_address(),
            &sender,
            &ask_amount,
        );
        env.storage()
            .instance()
            .set(&KEY_SWAP, &(offer_asset, offer_amount, ask_amount));
        ask_amount
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
    mock_swap: MockLegacyPoolClient<'a>,
    mock_swap_id: Address,
    usdc: Address,
    xlm: Address,
    #[allow(dead_code)]
    blnd: Address,
    usdc_admin: token::StellarAssetClient<'a>,
    blnd_admin: token::StellarAssetClient<'a>,
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

    let mock_pool_id = env.register(MockBlendedPool, (usdc.clone(), xlm.clone()));
    let mock_blend_id = env.register(MockBlendPool, (blnd.clone(),));
    // BLND-USDC swap pool: offer BLND, get USDC back.
    let mock_swap_id = env.register(MockLegacyPool, (usdc.clone(),));

    let verification = Address::generate(&env);

    let handler_id = env.register(
        AutomationHandler,
        (
            verification.clone(),
            mock_pool_id.clone(),
            mock_blend_id.clone(),
            usdc.clone(),
            xlm.clone(),
            blnd.clone(),
            mock_swap_id.clone(),
            USDC_RESERVE_TOKEN_ID,
            TARGET_BPS,
            BAND_BPS,
            MIN_TOTAL_USDC,
            COOLDOWN_SECS,
        ),
    );

    let mock_pool = MockBlendedPoolClient::new(&env, &mock_pool_id);
    let mock_blend = MockBlendPoolClient::new(&env, &mock_blend_id);
    let mock_swap = MockLegacyPoolClient::new(&env, &mock_swap_id);
    let handler = AutomationHandlerClient::new(&env, &handler_id);

    // Register the handler as the blended pool's delegate so transfers route.
    mock_pool.set_delegate(&handler_id);

    Harness {
        env,
        handler,
        handler_id,
        mock_pool,
        mock_pool_id,
        mock_blend,
        mock_blend_id,
        mock_swap,
        mock_swap_id,
        usdc,
        xlm,
        blnd,
        usdc_admin,
        blnd_admin,
    }
}

/// USDC sorts AFTER XLM by strkey, so in the pool's a/b layout XLM = a, USDC = b.
/// (Both are random SAC addresses in tests; we still derive which side USDC is
/// on the same way the handler does, by lexicographic compare.)
fn usdc_is_a(h: &Harness) -> bool {
    h.usdc < h.xlm
}

/// Configure the mock pool to report a `DelegateState` where USDC has the
/// given liquid/delegated/total breakdown. XLM amounts are kept fixed and
/// irrelevant to the handler's USDC-only ratio math.
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
    // Total USDC = 100k, liquid = 70k, delegated = 30k.
    // target_liquid = 50k. band = 5k. upper = 55k. liquid 70k > 55k.
    // Action: withdraw (70k - 50k) = 20k from pool, supply to Blend.
    let h = setup();
    let liquid: i128 = 700_000_000_000; // 70k USDC at 7 decimals
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
    // Total = 100k, liquid = 30k, delegated = 70k. target = 50k. lower = 45k.
    // liquid 30k < 45k. Action: withdraw (50k - 30k) = 20k from Blend,
    // deposit_from_delegate back into the pool.
    let h = setup();
    let liquid: i128 = 300_000_000_000;
    let delegated: i128 = 700_000_000_000;
    set_usdc_state(&h, liquid, delegated);
    // The pool needs to be configured with the principal already supplied to
    // Blend so the cap-at-principal sanity guard doesn't clamp the pull.
    h.handler.test_set_principal_supplied(&delegated);
    // Mock Blend needs the principal balance on hand to return on withdraw.
    h.usdc_admin.mint(&h.mock_blend_id, &delegated);
    // Mock Blend's bookkeeping needs to know the position size too. We
    // simulate that by recording it as if a prior ToBlend had run; the
    // simplest way is to call the mock's submit Supply path. But we cannot
    // call submit without first having a USDC source on the handler. The
    // cleaner shortcut is set_redeemable on the mock so Withdraw(i128::MAX)
    // would work, but the handler asks for a specific amount here, not MAX.
    // So we instead pre-supply the mock by minting + calling submit through
    // a separate driver. Easiest: bump the mock's `supplied` ledger via a
    // setter. We don't have one, so call submit through the mock_blend
    // client directly.
    // (We use a no-op driver: mint to handler, then call the mock through a
    // proxy that supplies it. But mock_blend.submit is the real entry. Just
    // use it directly with the harness deployer auth.)
    let mut requests: Vec<BlendRequest> = Vec::new(&h.env);
    requests.push_back(BlendRequest {
        request_type: BLEND_REQUEST_SUPPLY,
        address: h.usdc.clone(),
        amount: delegated,
    });
    // Supplier needs balance: mint a separate stash, then call submit.
    let stash = Address::generate(&h.env);
    h.usdc_admin.mint(&stash, &delegated);
    h.mock_blend.submit(&stash, &stash, &stash, &requests);
    // Now the mock's supplied ledger shows `delegated` for USDC. Reset the
    // last-call markers so the rebalance-driven Withdraw is what we observe.

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
    // liquid = 52k, target = 50k, band = 5k. drift inside band. No-op.
    let h = setup();
    set_usdc_state(&h, 520_000_000_000, 480_000_000_000);

    h.handler.test_rebalance();

    assert!(h.mock_pool.last_withdraw().is_none());
    assert!(h.mock_pool.last_deposit().is_none());
    assert!(h.mock_blend.last_submit_supply().is_none());
    assert!(h.mock_blend.last_submit_withdraw().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
    // last_rebalance_ts is NOT bumped on a no-op so a subsequent real drift
    // event isn't gated by the cooldown.
    assert_eq!(h.handler.last_rebalance_ts(), 0);
}

#[test]
fn rebalance_below_min_total_is_no_op() {
    // Total = 50 USDC, well below the 100 USDC min_total floor. No-op even
    // when drift would otherwise breach the band.
    let h = setup();
    set_usdc_state(&h, 100_000_000, 400_000_000); // 10 + 40 = 50 USDC

    h.handler.test_rebalance();

    assert!(h.mock_pool.last_withdraw().is_none());
    assert!(h.mock_blend.last_submit_supply().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
    assert_eq!(h.handler.last_rebalance_ts(), 0);
}

#[test]
fn rebalance_under_cooldown_is_no_op() {
    // Run one real rebalance, then immediately try another. The second
    // attempt is within the 60s cooldown so it should be a no-op.
    let h = setup();
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &700_000_000_000);

    h.handler.test_rebalance(); // populates last_rebalance_ts = INITIAL_TS
    let principal_after_first = h.handler.principal_supplied();
    assert!(principal_after_first > 0);

    // Drift the pool again immediately. Cooldown should suppress.
    set_usdc_state(&h, 800_000_000_000, 200_000_000_000);
    h.env.ledger().set_timestamp(INITIAL_TS + 30); // 30s < 60s cooldown

    h.handler.test_rebalance();

    // principal_supplied unchanged from first run.
    assert_eq!(h.handler.principal_supplied(), principal_after_first);
}

#[test]
fn rebalance_after_cooldown_acts_again() {
    let h = setup();
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &700_000_000_000);

    h.handler.test_rebalance();
    let principal_after_first = h.handler.principal_supplied();

    // Beyond cooldown. Drift again.
    h.env.ledger().set_timestamp(INITIAL_TS + COOLDOWN_SECS + 1);
    set_usdc_state(&h, 700_000_000_000, 300_000_000_000);
    h.usdc_admin.mint(&h.mock_pool_id, &200_000_000_000);

    h.handler.test_rebalance();

    assert!(h.handler.principal_supplied() > principal_after_first);
    assert_eq!(h.handler.last_rebalance_ts(), INITIAL_TS + COOLDOWN_SECS + 1);
}

#[test]
fn rebalance_from_blend_caps_at_principal() {
    // Pool says delegated_b = 70k but principal_supplied = 10k. Mocked
    // bad-state guard: only pull 10k (the actually-recoverable principal).
    let h = setup();
    let liquid: i128 = 300_000_000_000;
    let delegated_in_pool: i128 = 700_000_000_000;
    set_usdc_state(&h, liquid, delegated_in_pool);

    let small_principal: i128 = 100_000_000_000; // 10k
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

/// Seed the Blend mock with a USDC supply position. Returns the supplied
/// amount for convenience.
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
fn harvest_happy_path_donates_interest_plus_blnd_swap_proceeds() {
    // Position: 100 USDC principal supplied. Blend has 102 USDC redeemable
    // (2% interest). BLND emissions = 5 BLND, swap pool returns USDC at 2:1
    // (so 5 BLND -> 10 USDC). Total donation = 2 (interest) + 10 (BLND swap)
    // = 12 USDC.
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let interest: i128 = 2_000_000_000;
    let blnd_emissions: i128 = 50_000_000_000;
    let swap_rate: i128 = 2;

    seed_blend_supply(&h, principal);
    // Top up the mock blend's USDC reserve so it can return principal+interest.
    h.usdc_admin.mint(&h.mock_blend_id, &interest);
    h.mock_blend.set_redeemable(&(principal + interest));

    // Mint BLND into mock_blend to back the claim; configure claim amount.
    h.blnd_admin.mint(&h.mock_blend_id, &blnd_emissions);
    h.mock_blend.set_claim_amount(&blnd_emissions);

    // Mint USDC into mock_swap so it can pay out the swap proceeds.
    h.usdc_admin
        .mint(&h.mock_swap_id, &(blnd_emissions * swap_rate));
    h.mock_swap.set_rate(&swap_rate);

    // Move ledger time forward so cooldown isn't relevant (harvest is not
    // gated by it anyway, but keeps the test honest).
    h.env.ledger().set_timestamp(INITIAL_TS + 3600);

    h.handler.test_harvest();

    let donate = h.mock_pool.last_donate().expect("donate not called");
    let expected = interest + blnd_emissions * swap_rate;
    assert_eq!(donate, (h.usdc.clone(), expected));

    // principal_supplied is reset to the same `principal` in the happy case
    // (interest peeled off, principal re-supplied).
    assert_eq!(h.handler.principal_supplied(), principal);
}

#[test]
fn harvest_bad_debt_shrinks_principal_and_donates_only_blnd() {
    // Position: 100 USDC principal. Blend has been written down: only 70
    // USDC redeemable. No BLND emissions. Expected: re-supply 70 (not 100),
    // principal_supplied = 70, donate = 0 (no positive yield).
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let redeemable: i128 = 70_000_000_000;

    seed_blend_supply(&h, principal);
    h.mock_blend.set_redeemable(&redeemable);

    h.handler.test_harvest();

    // Re-supply was for the smaller actual_redeemable, not the stale principal.
    let sup = h.mock_blend.last_submit_supply().expect("Supply not called");
    assert_eq!(sup, (h.usdc.clone(), redeemable));
    // principal_supplied shrunk to the new position size.
    assert_eq!(h.handler.principal_supplied(), redeemable);
    // No donate because the USDC delta is 0 (we re-supplied everything that
    // came back; no BLND emissions either).
    assert!(h.mock_pool.last_donate().is_none());
}

#[test]
fn harvest_bad_debt_with_blnd_still_donates_blnd_proceeds() {
    // Bad-debt write-down (70/100 redeemable) but BLND emissions are non-zero,
    // so the donate amount equals the BLND swap proceeds even though no
    // interest is extractable.
    let h = setup();
    let principal: i128 = 100_000_000_000;
    let redeemable: i128 = 70_000_000_000;
    let blnd_emissions: i128 = 10_000_000_000;
    let swap_rate: i128 = 3;

    seed_blend_supply(&h, principal);
    h.mock_blend.set_redeemable(&redeemable);
    h.blnd_admin.mint(&h.mock_blend_id, &blnd_emissions);
    h.mock_blend.set_claim_amount(&blnd_emissions);
    h.usdc_admin
        .mint(&h.mock_swap_id, &(blnd_emissions * swap_rate));
    h.mock_swap.set_rate(&swap_rate);

    h.handler.test_harvest();

    let donate = h.mock_pool.last_donate().expect("donate not called");
    assert_eq!(donate, (h.usdc.clone(), blnd_emissions * swap_rate));
    assert_eq!(h.handler.principal_supplied(), redeemable);
}

#[test]
fn harvest_zero_principal_skips_withdraw_resupply() {
    // No position in Blend. Harvest should still claim emissions (here 0) and
    // donate any USDC sitting on the handler (here 0). Effectively a no-op.
    let h = setup();
    // principal_supplied starts at 0 from the constructor.
    h.handler.test_harvest();

    assert!(h.mock_blend.last_submit_supply().is_none());
    assert!(h.mock_blend.last_submit_withdraw().is_none());
    assert!(h.mock_pool.last_donate().is_none());
    assert_eq!(h.handler.principal_supplied(), 0);
}

#[test]
fn harvest_zero_blnd_emissions_still_donates_pure_interest() {
    // No BLND emitted this cycle, but interest is real.
    let h = setup();
    let principal: i128 = 50_000_000_000;
    let interest: i128 = 1_000_000_000;

    seed_blend_supply(&h, principal);
    h.usdc_admin.mint(&h.mock_blend_id, &interest);
    h.mock_blend.set_redeemable(&(principal + interest));
    // No claim amount set -> claim returns 0, no swap happens.

    h.handler.test_harvest();

    // mock_swap.last_swap should be unset since no BLND swap fired.
    assert!(h.mock_swap.last_swap().is_none());
    let donate = h.mock_pool.last_donate().expect("donate not called");
    assert_eq!(donate, (h.usdc.clone(), interest));
    assert_eq!(h.handler.principal_supplied(), principal);
}
