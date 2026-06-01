use soroban_sdk::{
    contract, contractimpl, contracttype, xdr::FromXdr, Address, Bytes, BytesN, Env, String, Vec,
};
use warpdrive_shared::interfaces::{
    handler::{Ed25519SignatureData, HandlerError, Verified, XlmEnvelope},
    verification::Ed25519VerificationClient,
};

use crate::externals::{
    BlendPoolClient, BlendRequest, BlendedPoolClient, BLEND_REQUEST_SUPPLY, BLEND_REQUEST_WITHDRAW,
};
use crate::storage;

/// Basis-point denominator. `bps_value / BPS_DEN == ratio`.
const BPS_DEN: i128 = 10_000;

/// Payload encoded inside the XlmEnvelope by the off-chain circuit + quorum.
///
/// Two variants:
///
/// - `Rebalance` - read the blended pool's `query_delegate_state`, compare
///   `liquid_usdc / total_usdc` against the configured 50% target (where
///   `total_usdc = liquid + delegated` - the delegated portion is the
///   principal sitting in Blend, accounted as "virtually in the pool").
///   If the drift exceeds `rebalance_band_bps`, move USDC between the pool's
///   liquid balance and Blend to restore the target. Skips if total USDC is
///   below `min_total_usdc`.
///
/// - `HarvestYield` - extract accrued yield (BLND emissions + USDC interest
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
        blnd_treasury: Address,
        usdc_reserve_token_id: u32,
        target_ratio_bps: u32,
        rebalance_band_bps: u32,
        min_total_usdc: i128,
        rebalance_cooldown_secs: u64,
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
        storage::set_blnd_treasury(&env, &blnd_treasury);
        storage::set_usdc_reserve_token_id(&env, usdc_reserve_token_id);
        storage::set_target_ratio_bps(&env, target_ratio_bps);
        storage::set_rebalance_band_bps(&env, rebalance_band_bps);
        storage::set_min_total_usdc(&env, min_total_usdc);
        storage::set_rebalance_cooldown_secs(&env, rebalance_cooldown_secs);
        storage::set_last_rebalance_ts(&env, 0);
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

    pub fn blnd_treasury(env: Env) -> Address {
        storage::get_blnd_treasury(&env)
    }

    pub fn rebalance_cooldown_secs(env: Env) -> u64 {
        storage::get_rebalance_cooldown_secs(&env)
    }

    pub fn last_rebalance_ts(env: Env) -> u64 {
        storage::get_last_rebalance_ts(&env)
    }

    pub fn payload(_env: Env, _event_id: BytesN<20>) -> Option<Bytes> {
        None
    }
}

/// Test-only entrypoints that bypass envelope + signature verification and
/// invoke the executors directly. Useful for exercising the rebalance and
/// harvest dispatch logic without constructing a real quorum envelope.
/// Excluded from production builds.
#[cfg(test)]
#[contractimpl]
impl AutomationHandler {
    pub fn test_rebalance(env: Env) -> Result<(), HandlerError> {
        execute_rebalance(&env)
    }

    pub fn test_harvest(env: Env) -> Result<(), HandlerError> {
        execute_harvest_yield(&env)
    }

    pub fn test_set_principal_supplied(env: Env, amount: i128) {
        storage::set_principal_supplied(&env, amount);
    }

    pub fn test_set_last_rebalance_ts(env: Env, ts: u64) {
        storage::set_last_rebalance_ts(&env, ts);
    }
}

/// Read the blended pool's delegate state and, if drift from the configured
/// target ratio exceeds the band, move USDC between the pool's liquid balance
/// and Blend. The pool's `total_a/total_b` already reflects `liquid +
/// delegated` (delegated USDC sits in Blend earning interest but is still
/// "virtually in the pool"), so we treat it as the authoritative denominator.
///
/// Below `min_total_usdc` this is a no-op - the event is still marked seen so
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

    // Cooldown gate: only ACTIONS (actual fund moves) are rate-limited.
    // No-op branches (below min, within band, dust) return without touching
    // last_rebalance_ts, so the next event after a gap can still react.
    let now = env.ledger().timestamp();
    let last_ts = storage::get_last_rebalance_ts(env);
    let cooldown = storage::get_rebalance_cooldown_secs(env);
    if now < last_ts.saturating_add(cooldown) {
        return Ok(());
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
        // Pool is over-liquid in USDC - supply the excess to Blend.
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
        storage::set_last_rebalance_ts(env, now);
    } else if liquid_usdc < lower {
        // Pool is under-liquid in USDC - pull from Blend to top up.
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
            storage::set_last_rebalance_ts(env, now);
        }
    }
    // Inside the band: no-op.
    Ok(())
}

/// Yield-harvest: route BLND emissions to the treasury, peel off USDC
/// interest from the b-token position, donate the interest to LPs.
///
/// Mechanic:
///   1. `Blend.claim(..., to=blnd_treasury)` routes BLND straight to the
///      configured treasury. The handler never holds BLND, which removes any
///      dependency on an external BLND-USDC swap venue and keeps the harvest
///      path pure-USDC.
///   2. Withdraw everything from Blend (`i128::MAX` is Blend's "all
///      redeemable" sentinel), then re-supply the smaller of `principal` and
///      the actual redeemable amount. In the happy case `actual_redeemable
///      == principal + interest`, so we re-supply `principal` and the
///      `interest` delta stays as USDC on the handler. In a bad-debt
///      write-down case `actual_redeemable < principal`, we re-supply
///      everything we got back and shrink `principal_supplied` to match the
///      new on-chain position.
///   3. Donate the interest delta to the blended pool, distributing pro-rata
///      to LP holders without minting LP tokens.
fn execute_harvest_yield(env: &Env) -> Result<(), HandlerError> {
    let blend_pool = storage::get_blend_pool(env);
    let blnd_treasury = storage::get_blnd_treasury(env);
    let blended_pool = storage::get_blended_pool(env);
    let usdc = storage::get_usdc(env);
    let usdc_reserve_token_id = storage::get_usdc_reserve_token_id(env);
    let principal = storage::get_principal_supplied(env);

    // 1. Route BLND emissions straight to the treasury.
    let blend = BlendPoolClient::new(env, &blend_pool);
    let mut ids: Vec<u32> = Vec::new(env);
    ids.push_back(usdc_reserve_token_id);
    blend.claim(&env.current_contract_address(), &ids, &blnd_treasury);

    // 2 + 3. Peel off USDC interest, then donate it.
    if principal > 0 {
        let usdc_token = soroban_sdk::token::Client::new(env, &usdc);
        let usdc_before_withdraw = usdc_token.balance(&env.current_contract_address());
        blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, i128::MAX);
        let actual_redeemable = usdc_token
            .balance(&env.current_contract_address())
            .saturating_sub(usdc_before_withdraw);

        let supply_amount = principal.min(actual_redeemable);
        if supply_amount > 0 {
            blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, supply_amount);
        }
        storage::set_principal_supplied(env, supply_amount);

        let interest = actual_redeemable.saturating_sub(supply_amount);
        if interest > 0 {
            let pool_client = BlendedPoolClient::new(env, &blended_pool);
            pool_client.donate(&usdc, &interest);
        }
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
