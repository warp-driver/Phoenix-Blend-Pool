use soroban_sdk::{
    contract, contractimpl, contracttype, xdr::FromXdr, Address, Bytes, BytesN, Env, String, Vec,
};
use warpdrive_shared::interfaces::{
    handler::{Ed25519SignatureData, HandlerError, Verified, XlmEnvelope},
    verification::Ed25519VerificationClient,
};

use crate::externals::{
    BlendPoolClient, BlendRequest, BlendedPoolClient, LegacyPoolClient, BLEND_REQUEST_SUPPLY,
    BLEND_REQUEST_WITHDRAW,
};
use crate::storage;

/// Basis-point denominator. `bps_value / BPS_DEN == ratio`.
const BPS_DEN: i128 = 10_000;

/// Payload encoded inside the XlmEnvelope by the off-chain circuit + quorum.
///
/// Two variants:
///
/// - `Rebalance` — read the blended pool's `query_delegate_state`, compare
///   `liquid_usdc / total_usdc` against the configured 50% target (where
///   `total_usdc = liquid + delegated` — the delegated portion is the
///   principal sitting in Blend, accounted as "virtually in the pool").
///   If the drift exceeds `rebalance_band_bps`, move USDC between the pool's
///   liquid balance and Blend to restore the target. Skips if total USDC is
///   below `min_total_usdc`.
///
/// - `HarvestYield` — extract accrued yield (BLND emissions + USDC interest
///   from b-token appreciation), convert to USDC, donate to LP holders
///   pro-rata via `pool.donate(USDC, ...)`.
///
/// No amount/direction crosses the wire: the off-chain circuit only triggers
/// a tick. All sizing happens on-chain against authoritative pool state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebalanceAction {
    Rebalance,
    HarvestYield,
}

#[contract]
pub struct AutomationHandler;

#[contractimpl]
impl AutomationHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        verification_contract: Address,
        blended_pool: Address,
        blend_pool: Address,
        usdc: Address,
        xlm: Address,
        blnd: Address,
        blnd_swap_pool: Address,
        usdc_reserve_token_id: u32,
        target_ratio_bps: u32,
        rebalance_band_bps: u32,
        min_total_usdc: i128,
    ) {
        assert!(
            target_ratio_bps > 0 && target_ratio_bps < BPS_DEN as u32,
            "target_ratio_bps must be in (0, 10000)"
        );
        assert!(
            rebalance_band_bps < BPS_DEN as u32,
            "rebalance_band_bps must be < 10000"
        );
        assert!(min_total_usdc >= 0, "min_total_usdc must be non-negative");

        storage::set_verification_contract(&env, &verification_contract);
        storage::set_blended_pool(&env, &blended_pool);
        storage::set_blend_pool(&env, &blend_pool);
        storage::set_usdc(&env, &usdc);
        storage::set_xlm(&env, &xlm);
        storage::set_blnd(&env, &blnd);
        storage::set_blnd_swap_pool(&env, &blnd_swap_pool);
        storage::set_usdc_reserve_token_id(&env, usdc_reserve_token_id);
        storage::set_target_ratio_bps(&env, target_ratio_bps);
        storage::set_rebalance_band_bps(&env, rebalance_band_bps);
        storage::set_min_total_usdc(&env, min_total_usdc);
        storage::set_principal_supplied(&env, 0);
        storage::set_version(&env, &String::from_str(&env, env!("CARGO_PKG_VERSION")));
        storage::extend_instance_ttl(&env);
    }

    /// Quorum-signed entrypoint. Verifies the envelope, decodes a
    /// `RebalanceAction`, and dispatches forward or reverse.
    pub fn verify_xlm(
        env: Env,
        envelope_bytes: Bytes,
        sig_data: Ed25519SignatureData,
    ) -> Result<(), HandlerError> {
        let envelope = XlmEnvelope::from_xdr(&env, &envelope_bytes)
            .map_err(|_| HandlerError::InvalidEnvelope)?;
        let event_id = envelope.event_id.clone();

        if storage::is_event_seen(&env, &event_id) {
            return Err(HandlerError::EventAlreadySeen);
        }

        let verification_addr = storage::get_verification_contract(&env);
        match Ed25519VerificationClient::new(&env, &verification_addr).try_verify(
            &envelope_bytes,
            &sig_data.signatures,
            &sig_data.signers,
            &sig_data.reference_block,
        ) {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(HandlerError::UnknownVerificationError),
            Err(Ok(e)) => return Err(HandlerError::from(e)),
            Err(Err(_)) => return Err(HandlerError::OtherInvocationError),
        }

        let action = RebalanceAction::from_xdr(&env, &envelope.payload)
            .map_err(|_| HandlerError::InvalidEnvelope)?;

        match action {
            RebalanceAction::Rebalance => execute_rebalance(&env)?,
            RebalanceAction::HarvestYield => execute_harvest_yield(&env)?,
        }

        storage::mark_event_seen(&env, &event_id);
        storage::extend_instance_ttl(&env);
        Verified::new(event_id).publish(&env);
        Ok(())
    }

    pub fn verification_contract(env: Env) -> Address {
        storage::get_verification_contract(&env)
    }

    pub fn blended_pool(env: Env) -> Address {
        storage::get_blended_pool(&env)
    }

    pub fn blend_pool(env: Env) -> Address {
        storage::get_blend_pool(&env)
    }

    pub fn target_ratio_bps(env: Env) -> u32 {
        storage::get_target_ratio_bps(&env)
    }

    pub fn rebalance_band_bps(env: Env) -> u32 {
        storage::get_rebalance_band_bps(&env)
    }

    pub fn min_total_usdc(env: Env) -> i128 {
        storage::get_min_total_usdc(&env)
    }

    pub fn principal_supplied(env: Env) -> i128 {
        storage::get_principal_supplied(&env)
    }

    pub fn payload(_env: Env, _event_id: BytesN<20>) -> Option<Bytes> {
        None
    }
}

/// Read the blended pool's delegate state and, if drift from the configured
/// target ratio exceeds the band, move USDC between the pool's liquid balance
/// and Blend. The pool's `total_a/total_b` already reflects `liquid +
/// delegated` (delegated USDC sits in Blend earning interest but is still
/// "virtually in the pool"), so we treat it as the authoritative denominator.
///
/// Below `min_total_usdc` this is a no-op — the event is still marked seen so
/// it doesn't replay, just no transfer fires.
fn execute_rebalance(env: &Env) -> Result<(), HandlerError> {
    let blended_pool = storage::get_blended_pool(env);
    let blend_pool = storage::get_blend_pool(env);
    let usdc = storage::get_usdc(env);
    let xlm = storage::get_xlm(env);

    let pool_client = BlendedPoolClient::new(env, &blended_pool);
    let state = pool_client.query_delegate_state();
    let (liquid_usdc, total_usdc) = if usdc < xlm {
        (state.liquid_a, state.total_a)
    } else {
        (state.liquid_b, state.total_b)
    };

    if total_usdc < storage::get_min_total_usdc(env) {
        return Ok(());
    }
    if liquid_usdc < 0 || total_usdc <= 0 {
        return Err(HandlerError::InvalidEnvelope);
    }

    let target_bps = storage::get_target_ratio_bps(env) as i128;
    let band_bps = storage::get_rebalance_band_bps(env) as i128;
    let target_liquid = mul_div(total_usdc, target_bps, BPS_DEN)?;
    let band = mul_div(total_usdc, band_bps, BPS_DEN)?;
    let upper = target_liquid
        .checked_add(band)
        .ok_or(HandlerError::OtherInvocationError)?;
    let lower = target_liquid.saturating_sub(band);

    if liquid_usdc > upper {
        // Pool is over-liquid in USDC — supply the excess to Blend.
        let amount = liquid_usdc
            .checked_sub(target_liquid)
            .ok_or(HandlerError::OtherInvocationError)?;
        pool_client.withdraw_to_delegate(&usdc, &amount);
        blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, amount);
        let prev = storage::get_principal_supplied(env);
        storage::set_principal_supplied(
            env,
            prev.checked_add(amount)
                .ok_or(HandlerError::OtherInvocationError)?,
        );
    } else if liquid_usdc < lower {
        // Pool is under-liquid in USDC — pull from Blend to top up.
        let mut amount = target_liquid
            .checked_sub(liquid_usdc)
            .ok_or(HandlerError::OtherInvocationError)?;
        // Cap at what we have parked. If the pool's `delegated_a/b` somehow
        // exceeds what's actually redeemable from Blend (e.g. bad-debt
        // write-down), the Blend call would revert and we'd be stuck. Use
        // the locally tracked principal as a sanity cap.
        let principal = storage::get_principal_supplied(env);
        if amount > principal {
            amount = principal;
        }
        if amount > 0 {
            blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, amount);
            pool_client.deposit_from_delegate(&usdc, &amount);
            storage::set_principal_supplied(env, (principal - amount).max(0));
        }
    }
    // Inside the band: no-op.
    Ok(())
}

/// Yield-harvest: pull accrued BLND emissions + USDC interest, donate to LPs.
///
/// Mechanic:
///   1. Claim BLND emissions for our USDC supply position.
///   2. Swap any BLND received → USDC on the configured BLND-USDC pool.
///   3. Withdraw everything from Blend (Blend treats i128::MAX as "all
///      available"), then re-supply `principal_supplied`. The leftover USDC
///      is exactly the accrued interest delta.
///   4. Donate the combined (BLND-swap-proceeds + interest) USDC to the
///      blended pool, distributing pro-rata to LP holders without minting.
fn execute_harvest_yield(env: &Env) -> Result<(), HandlerError> {
    let blend_pool = storage::get_blend_pool(env);
    let blnd_swap_pool = storage::get_blnd_swap_pool(env);
    let blended_pool = storage::get_blended_pool(env);
    let usdc = storage::get_usdc(env);
    let blnd = storage::get_blnd(env);
    let usdc_reserve_token_id = storage::get_usdc_reserve_token_id(env);
    let principal = storage::get_principal_supplied(env);

    let usdc_token = soroban_sdk::token::Client::new(env, &usdc);
    let usdc_start = usdc_token.balance(&env.current_contract_address());

    // 1. Claim BLND emissions for our USDC supply position. May return 0.
    let blend = BlendPoolClient::new(env, &blend_pool);
    let mut ids: Vec<u32> = Vec::new(env);
    ids.push_back(usdc_reserve_token_id);
    blend.claim(
        &env.current_contract_address(),
        &ids,
        &env.current_contract_address(),
    );

    // 2. Swap any BLND we just received → USDC. Skip if zero.
    let blnd_token = soroban_sdk::token::Client::new(env, &blnd);
    let blnd_balance = blnd_token.balance(&env.current_contract_address());
    if blnd_balance > 0 {
        let blnd_pool = LegacyPoolClient::new(env, &blnd_swap_pool);
        let _ = blnd_pool.swap(
            &env.current_contract_address(),
            &blnd,
            &blnd_balance,
            &None::<i128>,
            &Some(10_000i64),
            &None::<u64>,
            &None::<i64>,
        );
    }

    // 3. Withdraw-all + re-supply principal to extract just the interest.
    //    i128::MAX is the canonical "withdraw everything redeemable" sentinel;
    //    Blend caps it at the position's actual b-token value. The re-supply
    //    re-establishes the same principal, so principal_supplied stays
    //    consistent across harvest cycles.
    if principal > 0 {
        blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, i128::MAX);
        blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, principal);
    }

    // 4. Total yield = USDC balance delta over the whole flow = BLND swap
    //    proceeds + interest. Donate to LPs (no-op if zero or somehow negative
    //    — the latter would indicate a Blend bad-debt write-down).
    let usdc_end = usdc_token.balance(&env.current_contract_address());
    let total_yield = usdc_end.saturating_sub(usdc_start);

    if total_yield > 0 {
        let pool_client = BlendedPoolClient::new(env, &blended_pool);
        pool_client.donate(&usdc, &total_yield);
    }

    Ok(())
}


fn blend_submit(
    env: &Env,
    blend_pool: &soroban_sdk::Address,
    token: &soroban_sdk::Address,
    request_type: u32,
    amount: i128,
) {
    let blend = BlendPoolClient::new(env, blend_pool);
    let mut requests: Vec<BlendRequest> = Vec::new(env);
    requests.push_back(BlendRequest {
        request_type,
        address: token.clone(),
        amount,
    });
    blend.submit(
        &env.current_contract_address(),
        &env.current_contract_address(),
        &env.current_contract_address(),
        &requests,
    );
}

/// Saturating-checked `a * b / c`.
fn mul_div(a: i128, b: i128, c: i128) -> Result<i128, HandlerError> {
    a.checked_mul(b)
        .and_then(|v| v.checked_div(c))
        .ok_or(HandlerError::OtherInvocationError)
}
