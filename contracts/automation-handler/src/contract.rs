use soroban_sdk::{
    contract, contractimpl, contracttype, xdr::FromXdr, Address, Bytes, BytesN, Env, String, Vec,
};
use warpdrive_shared::interfaces::{
    handler::{Ed25519SignatureData, HandlerError, Verified, XlmEnvelope},
    verification::Ed25519VerificationClient,
};

use crate::externals::{
    BlendPoolClient, BlendRequest, BlendedPoolClient, LegacyPoolClient, BLEND_REQUEST_SUPPLY,
};
use crate::storage;

/// Payload encoded inside the XlmEnvelope by the off-chain circuit + quorum.
///
/// `amount_usdc` is the USDC amount to withdraw from the blended pool. The
/// handler computes the matching XLM withdrawal from the pool's current
/// logical reserves at execution time (preserving the pool's physical
/// XLM:USDC ratio), swaps that XLM through the legacy XLM-USDC Phoenix pool,
/// and supplies the combined USDC to Blend.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceToBlend {
    pub amount_usdc: i128,
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
    /// RebalanceToBlend action, and dispatches the cycle:
    ///   1. Pull `amount_usdc` USDC + proportional XLM from the blended pool
    ///   2. Swap the XLM on the legacy pool for USDC
    ///   3. Supply the combined USDC to Blend
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

        let payload = RebalanceToBlend::from_xdr(&env, &envelope.payload)
            .map_err(|_| HandlerError::InvalidEnvelope)?;

        if payload.amount_usdc <= 0 {
            return Err(HandlerError::InvalidEnvelope);
        }

        execute_rebalance(&env, payload.amount_usdc)?;

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

fn execute_rebalance(env: &Env, amount_usdc: i128) -> Result<(), HandlerError> {
    let blended_pool = storage::get_blended_pool(env);
    let legacy_pool = storage::get_legacy_pool(env);
    let blend_pool = storage::get_blend_pool(env);
    let usdc = storage::get_usdc(env);
    let xlm = storage::get_xlm(env);

    let pool_client = BlendedPoolClient::new(env, &blended_pool);
    let state = pool_client.query_delegate_state();

    // The blended pool stores reserves as (a, b) where token_a < token_b by
    // Address sort. Identify which slot is USDC vs XLM.
    let (total_usdc, total_xlm) = if usdc < xlm {
        (state.total_a, state.total_b)
    } else {
        (state.total_b, state.total_a)
    };
    if total_usdc <= 0 || total_xlm <= 0 {
        return Err(HandlerError::InvalidEnvelope);
    }

    // Proportional XLM pull to keep the new pool's physical ratio steady:
    //   amount_xlm / amount_usdc == total_xlm / total_usdc
    let amount_xlm = mul_div(amount_usdc, total_xlm, total_usdc)?;

    // Pull both legs from the blended pool. The handler is the registered
    // delegate, so the pool transfers tokens directly to this contract.
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

    // Supply combined USDC to Blend. Handler is `from` (position holder),
    // `spender` (token source), and `to` (any outbound tokens if Blend ever
    // routes some back, e.g. via dust accounting).
    let blend = BlendPoolClient::new(env, &blend_pool);
    let mut requests: Vec<BlendRequest> = Vec::new(env);
    requests.push_back(BlendRequest {
        request_type: BLEND_REQUEST_SUPPLY,
        address: usdc,
        amount: total_supply,
    });
    blend.submit(
        &env.current_contract_address(),
        &env.current_contract_address(),
        &env.current_contract_address(),
        &requests,
    );

    Ok(())
}

/// Saturating-checked `a * b / c`.
fn mul_div(a: i128, b: i128, c: i128) -> Result<i128, HandlerError> {
    a.checked_mul(b)
        .and_then(|v| v.checked_div(c))
        .ok_or(HandlerError::OtherInvocationError)
}
