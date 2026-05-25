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

/// Payload encoded inside the XlmEnvelope by the off-chain circuit + quorum.
///
/// Two directions:
///
/// - `ToBlend(amount_usdc)` — forward rebalance. `amount_usdc` is the direct
///   USDC pull from the blended pool. The handler also pulls the
///   proportional XLM, swaps it on the legacy pool, and supplies the
///   combined ~2 * amount_usdc USDC into Blend.
///
/// - `FromBlend(amount_usdc)` — reverse rebalance. `amount_usdc` is the
///   total USDC withdrawn from Blend; the handler splits it half-and-half,
///   swaps one half on the legacy pool for XLM, and returns both legs to the
///   blended pool via `deposit_from_delegate`. Net effect: pool's
///   DelegatedOutA and DelegatedOutB both decrement.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebalanceAction {
    ToBlend(i128),
    FromBlend(i128),
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
        legacy_pool: Address,
        blend_pool: Address,
        usdc: Address,
        xlm: Address,
    ) {
        storage::set_verification_contract(&env, &verification_contract);
        storage::set_blended_pool(&env, &blended_pool);
        storage::set_legacy_pool(&env, &legacy_pool);
        storage::set_blend_pool(&env, &blend_pool);
        storage::set_usdc(&env, &usdc);
        storage::set_xlm(&env, &xlm);
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
            RebalanceAction::ToBlend(amount_usdc) => {
                if amount_usdc <= 0 {
                    return Err(HandlerError::InvalidEnvelope);
                }
                execute_to_blend(&env, amount_usdc)?;
            }
            RebalanceAction::FromBlend(amount_usdc) => {
                if amount_usdc <= 0 {
                    return Err(HandlerError::InvalidEnvelope);
                }
                execute_from_blend(&env, amount_usdc)?;
            }
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

    pub fn legacy_pool(env: Env) -> Address {
        storage::get_legacy_pool(&env)
    }

    pub fn blend_pool(env: Env) -> Address {
        storage::get_blend_pool(&env)
    }

    pub fn payload(_env: Env, _event_id: BytesN<20>) -> Option<Bytes> {
        None
    }
}

/// Forward direction: pool -> Blend.
///
/// Pulls `amount_usdc` directly + proportional XLM, swaps XLM to USDC on
/// the legacy pool, supplies combined USDC to Blend.
fn execute_to_blend(env: &Env, amount_usdc: i128) -> Result<(), HandlerError> {
    let blended_pool = storage::get_blended_pool(env);
    let legacy_pool = storage::get_legacy_pool(env);
    let blend_pool = storage::get_blend_pool(env);
    let usdc = storage::get_usdc(env);
    let xlm = storage::get_xlm(env);

    let pool_client = BlendedPoolClient::new(env, &blended_pool);
    let (total_usdc, total_xlm) = pool_reserves(&pool_client, &usdc, &xlm)?;

    // Proportional XLM pull to keep the new pool's physical ratio steady:
    //   amount_xlm / amount_usdc == total_xlm / total_usdc
    let amount_xlm = mul_div(amount_usdc, total_xlm, total_usdc)?;

    pool_client.withdraw_to_delegate(&xlm, &amount_xlm);
    pool_client.withdraw_to_delegate(&usdc, &amount_usdc);

    // Swap the XLM leg on the legacy XLM-USDC pool for USDC. Generous spread
    // cap; the quorum is the real safety against bad-faith dispatches.
    let legacy = LegacyPoolClient::new(env, &legacy_pool);
    let swapped_usdc = legacy.swap(
        &env.current_contract_address(),
        &xlm,
        &amount_xlm,
        &None::<i128>,
        &Some(10_000i64),
        &None::<u64>,
        &None::<i64>,
    );

    let total_supply = amount_usdc
        .checked_add(swapped_usdc)
        .ok_or(HandlerError::OtherInvocationError)?;

    blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, total_supply);
    Ok(())
}

/// Reverse direction: Blend -> pool.
///
/// `amount_usdc` is the total USDC to withdraw from Blend. Half lands as USDC
/// back in the blended pool; the other half gets swapped on the legacy pool
/// to XLM and lands as XLM. The pool's DelegatedOutA and DelegatedOutB both
/// decrease accordingly.
fn execute_from_blend(env: &Env, amount_usdc: i128) -> Result<(), HandlerError> {
    let blended_pool = storage::get_blended_pool(env);
    let legacy_pool = storage::get_legacy_pool(env);
    let blend_pool = storage::get_blend_pool(env);
    let usdc = storage::get_usdc(env);
    let xlm = storage::get_xlm(env);

    let pool_client = BlendedPoolClient::new(env, &blended_pool);
    // We don't need exact reserves for the split — half/half by USDC value
    // gives the pool back equal value on each side. But we DO read state to
    // bail early if the pool has no logical reserves yet (the math below
    // would panic in deposit_from_delegate when it tries to underflow
    // DelegatedOut counters that aren't positive).
    let (total_usdc, total_xlm) = pool_reserves(&pool_client, &usdc, &xlm)?;
    let _ = (total_usdc, total_xlm);

    let half = amount_usdc / 2;
    let other_half = amount_usdc
        .checked_sub(half)
        .ok_or(HandlerError::OtherInvocationError)?;
    if half <= 0 || other_half <= 0 {
        return Err(HandlerError::InvalidEnvelope);
    }

    // Pull combined USDC out of Blend in one call.
    blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, amount_usdc);

    // Swap `half` USDC for XLM on legacy. The returned amount is whatever
    // the legacy pool's spot price gives us (legacy pool may have its own
    // ratio drift after the swap).
    let legacy = LegacyPoolClient::new(env, &legacy_pool);
    let xlm_received = legacy.swap(
        &env.current_contract_address(),
        &usdc,
        &half,
        &None::<i128>,
        &Some(10_000i64),
        &None::<u64>,
        &None::<i64>,
    );

    // Push both legs back into the blended pool. The handler is the
    // registered delegate, so the pool decrements DelegatedOut accordingly.
    pool_client.deposit_from_delegate(&usdc, &other_half);
    pool_client.deposit_from_delegate(&xlm, &xlm_received);

    Ok(())
}

fn pool_reserves(
    pool_client: &BlendedPoolClient,
    usdc: &soroban_sdk::Address,
    xlm: &soroban_sdk::Address,
) -> Result<(i128, i128), HandlerError> {
    let state = pool_client.query_delegate_state();
    let (total_usdc, total_xlm) = if usdc < xlm {
        (state.total_a, state.total_b)
    } else {
        (state.total_b, state.total_a)
    };
    if total_usdc <= 0 || total_xlm <= 0 {
        return Err(HandlerError::InvalidEnvelope);
    }
    Ok((total_usdc, total_xlm))
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
