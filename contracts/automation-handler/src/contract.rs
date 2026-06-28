use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractimpl, contracttype, vec, xdr::FromXdr, Address, Bytes, BytesN, Env,
    IntoVal, String, Symbol, Val, Vec,
};
use warpdrive_shared::interfaces::{
    handler::{Ed25519SignatureData, HandlerError, Verified, XlmEnvelope},
    verification::Ed25519VerificationClient,
    warpdrive::{ContractUpgraded, WarpDriveInterface},
};
use crate::events::{
    AddressConfigUpdated, BadDebtDetected, ConfigUpdated, EmergencyUnwound, HarvestCompleted,
    HarvestPartial, PauseToggled, RebalanceExecuted, DIRECTION_FROM_BLEND, DIRECTION_TO_BLEND,
};
use crate::externals::{
    BlendPoolClient, BlendRequest, BlendedPoolClient, BLEND_REQUEST_SUPPLY, BLEND_REQUEST_WITHDRAW,
};
use crate::storage;

/// Basis-point denominator. `bps_value / BPS_DEN == ratio`.
const BPS_DEN: i128 = 10_000;

/// Highest Blend pool `status` value at which the handler's standard
/// Supply / Withdraw calls are still expected to succeed. Statuses 0-3
/// (Active / Admin-Active / Admin-OnIce / OnIce) allow withdraw and
/// emissions claim; statuses 4-6 (Admin-Frozen / Frozen / Setup) put the
/// pool into a state the handler treats as "do not touch" — Rebalance
/// becomes a no-op and Harvest skips the withdraw/resupply leg. Admin can
/// always call `emergency_unwind` to drain a Frozen position explicitly.
const BLEND_HEALTHY_STATUS_MAX: u32 = 3;

/// Maximum age of a quorum envelope's `reference_block` accepted by
/// `verify_xlm`. The aggregator stamps the envelope with the current
/// ledger sequence at signature time; the on-chain verification
/// contract checks the registered signer set AS OF that ledger. A
/// stale `reference_block` lets a previously-quorate (but since
/// retired) signer set re-pass verification, so we bound how far back
/// it can point. 200 ledgers ≈ 17 minutes on mainnet (5-6 s/ledger);
/// well above any plausible aggregator → operator → submission latency,
/// well below the security contract's historical-weight retention.
const MAX_REFERENCE_BLOCK_AGE: u32 = 200;

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

/// Snapshot of the handler's full configuration + runtime state, returned
/// by `query_state()` as a single read. Dashboards and monitoring tools
/// consume this instead of paying one cross-contract call per field.
#[contracttype]
#[derive(Clone, Debug)]
pub struct HandlerState {
    pub admin: Address,
    pub pending_admin: Option<Address>,
    pub verification_contract: Address,
    pub blended_pool: Address,
    pub blend_pool: Address,
    pub usdc: Address,
    pub xlm: Address,
    pub blnd_treasury: Address,
    pub usdc_reserve_token_id: u32,
    pub target_ratio_bps: u32,
    pub rebalance_band_bps: u32,
    pub min_total_usdc: i128,
    pub max_rebalance_amount: i128,
    pub min_rebalance_amount: i128,
    pub rebalance_cooldown_secs: u64,
    pub principal_supplied: i128,
    pub last_rebalance_ts: u64,
    pub last_harvest_ts: u64,
    pub paused: bool,
    pub version: String,
}

#[contract]
pub struct AutomationHandler;

#[contractimpl]
impl AutomationHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        admin: Address,
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
        if usdc_reserve_token_id % 2 == 0 {
            soroban_sdk::panic_with_error!(
                &env,
                crate::error::LocalError::InvalidReserveTokenId
            );
        }

        storage::set_admin(&env, &admin);
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
        // max/min rebalance scope limits default to 0 ("unlimited" / "no
        // floor") and are tightened post-deploy by the admin via setters.
        // The defaults preserve handler behaviour for legacy deploys that
        // do not call the setters.
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
        if storage::get_paused(&env) {
            // Project-local panic with code 600. The Result<(), HandlerError>
            // signature is preserved; the panic short-circuits the call and
            // surfaces in the tx diagnostic with a precise reason.
            soroban_sdk::panic_with_error!(&env, crate::error::LocalError::Paused);
        }

        let envelope = XlmEnvelope::from_xdr(&env, &envelope_bytes)
            .map_err(|_| HandlerError::InvalidEnvelope)?;
        let event_id = envelope.event_id.clone();

        if storage::is_event_seen(&env, &event_id) {
            return Err(HandlerError::EventAlreadySeen);
        }

        let current_ledger = env.ledger().sequence();
        if sig_data.reference_block > current_ledger
            || current_ledger - sig_data.reference_block > MAX_REFERENCE_BLOCK_AGE
        {
            return Err(HandlerError::InvalidReferenceBlock);
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

    pub fn max_rebalance_amount(env: Env) -> i128 {
        storage::get_max_rebalance_amount(&env)
    }

    pub fn min_rebalance_amount(env: Env) -> i128 {
        storage::get_min_rebalance_amount(&env)
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

    pub fn last_harvest_ts(env: Env) -> u64 {
        storage::get_last_harvest_ts(&env)
    }


    /// Dashboard-friendly aggregate view. Returns every config + runtime
    /// field in one read, eliminating ~18 separate cross-contract calls
    /// the dashboard would otherwise need. The shape is stable across
    /// upgrades; new fields are appended, never reordered or removed.
    pub fn query_state(env: Env) -> HandlerState {
        HandlerState {
            admin: storage::get_admin(&env),
            pending_admin: warpdrive_shared::admin::pending(&env),
            verification_contract: storage::get_verification_contract(&env),
            blended_pool: storage::get_blended_pool(&env),
            blend_pool: storage::get_blend_pool(&env),
            usdc: storage::get_usdc(&env),
            xlm: storage::get_xlm(&env),
            blnd_treasury: storage::get_blnd_treasury(&env),
            usdc_reserve_token_id: storage::get_usdc_reserve_token_id(&env),
            target_ratio_bps: storage::get_target_ratio_bps(&env),
            rebalance_band_bps: storage::get_rebalance_band_bps(&env),
            min_total_usdc: storage::get_min_total_usdc(&env),
            max_rebalance_amount: storage::get_max_rebalance_amount(&env),
            min_rebalance_amount: storage::get_min_rebalance_amount(&env),
            rebalance_cooldown_secs: storage::get_rebalance_cooldown_secs(&env),
            principal_supplied: storage::get_principal_supplied(&env),
            last_rebalance_ts: storage::get_last_rebalance_ts(&env),
            last_harvest_ts: storage::get_last_harvest_ts(&env),
            paused: storage::get_paused(&env),
            version: storage::get_version(&env),
        }
    }
    pub fn payload(_env: Env, _event_id: BytesN<20>) -> Option<Bytes> {
        None
    }

    /// Admin-only: clamp the per-tx USDC amount moved between pool and Blend.
    /// `0` is the "unlimited" sentinel. Emits a `ConfigUpdated` event so
    /// dashboards can react.
    pub fn set_max_rebalance_amount(env: Env, amount: i128) {
        storage::get_admin(&env).require_auth();
        assert!(amount >= 0, "max_rebalance_amount must be non-negative");
        storage::set_max_rebalance_amount(&env, amount);
        ConfigUpdated::new(
            soroban_sdk::symbol_short!("max_reb"),
            amount,
        )
        .publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: dust floor below which a Rebalance is a silent no-op
    /// (does not consume the cooldown window). `0` means "no floor".
    pub fn set_min_rebalance_amount(env: Env, amount: i128) {
        storage::get_admin(&env).require_auth();
        assert!(amount >= 0, "min_rebalance_amount must be non-negative");
        storage::set_min_rebalance_amount(&env, amount);
        ConfigUpdated::new(
            soroban_sdk::symbol_short!("min_reb"),
            amount,
        )
        .publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// View: is the handler currently paused?
    pub fn paused(env: Env) -> bool {
        storage::get_paused(&env)
    }

    /// Admin-only: pause the handler. `verify_xlm` panics with `LocalError::Paused`
    /// (code 600) while paused; the envelope is NOT marked seen, so a
    /// re-submission after `unpause` will proceed normally.
    pub fn pause(env: Env) {
        storage::get_admin(&env).require_auth();
        storage::set_paused(&env, true);
        PauseToggled::new(true).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: lift a pause.
    pub fn unpause(env: Env) {
        storage::get_admin(&env).require_auth();
        storage::set_paused(&env, false);
        PauseToggled::new(false).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: drain the entire Blend USDC position back to the pool's
    /// liquid balance, bypassing cooldown / band / scope-limit gating.
    ///
    /// Operationally this is the off-ramp when Blend's status goes Frozen,
    /// when the operator network is paused but funds need to come home, or
    /// when migrating to a new handler.
    ///
    /// Mechanic:
    ///   1. `Blend.submit(Withdraw, USDC, i128::MAX)` → handler holds USDC.
    ///   2. Deposit `min(redeemed, principal_before)` back via
    ///      `deposit_from_delegate`; this matches the pool's
    ///      `delegated_out_*` counter so the call doesn't underflow.
    ///   3. Donate any excess (accrued interest) to the pool via `donate`.
    ///   4. Reset `principal_supplied = 0`.
    pub fn emergency_unwind(env: Env) {
        storage::get_admin(&env).require_auth();

        let blend_pool = storage::get_blend_pool(&env);
        let blended_pool = storage::get_blended_pool(&env);
        let usdc = storage::get_usdc(&env);
        let principal_before = storage::get_principal_supplied(&env);

        let mut redeemed: i128 = 0;
        if principal_before > 0 {
            let usdc_token = soroban_sdk::token::Client::new(&env, &usdc);
            let before = usdc_token.balance(&env.current_contract_address());
            blend_submit(&env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, i128::MAX);
            redeemed = usdc_token
                .balance(&env.current_contract_address())
                .saturating_sub(before);

            if redeemed > 0 {
                let deposit_amount = redeemed.min(principal_before);
                if deposit_amount > 0 {
                    deposit_from_delegate(&env, &blended_pool, &usdc, deposit_amount);
                }
                let donate_amount = redeemed.saturating_sub(principal_before);
                if donate_amount > 0 {
                    donate_to_pool(&env, &blended_pool, &usdc, donate_amount);
                }
            }
        }

        storage::set_principal_supplied(&env, 0);
        EmergencyUnwound::new(redeemed, principal_before).publish(&env);
        assert_no_usdc_residue(&env);
    }

    /// Admin-only: push `amount` USDC from the blended pool to Blend out
    /// of band. Useful for the initial seeding right after deploy (when
    /// no swap events have fired) and for manual top-ups during operator
    /// downtime.
    ///
    /// Bypasses cooldown, band, min-total, and scope-limit gates because
    /// the admin already exercised judgement. Still honours pause and the
    /// Blend-health gate (a Supply into a Frozen pool would revert anyway).
    pub fn manual_to_blend(env: Env, amount: i128) {
        storage::get_admin(&env).require_auth();
        assert!(amount > 0, "amount must be positive");
        if storage::get_paused(&env) {
            soroban_sdk::panic_with_error!(&env, crate::error::LocalError::Paused);
        }

        let blended_pool = storage::get_blended_pool(&env);
        let blend_pool = storage::get_blend_pool(&env);
        let usdc = storage::get_usdc(&env);
        let xlm = storage::get_xlm(&env);
        assert!(
            BlendPoolClient::new(&env, &blend_pool).get_config().status <= BLEND_HEALTHY_STATUS_MAX,
            "Blend pool is not healthy (status > 3); cannot Supply",
        );

        let pool_client = BlendedPoolClient::new(&env, &blended_pool);
        pool_client.withdraw_to_delegate(&usdc, &amount);
        blend_submit(&env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, amount);

        let prev = storage::get_principal_supplied(&env);
        let principal_after = prev.checked_add(amount).expect("principal overflow");
        storage::set_principal_supplied(&env, principal_after);

        let state = pool_client.query_delegate_state();
        let (liquid_after, delegated_after) = if usdc < xlm {
            (state.liquid_a, state.delegated_a)
        } else {
            (state.liquid_b, state.delegated_b)
        };
        RebalanceExecuted::new(
            DIRECTION_TO_BLEND,
            amount,
            liquid_after,
            delegated_after,
            principal_after,
        )
        .publish(&env);
        assert_no_usdc_residue(&env);
    }

    /// Admin-only: pull `amount` USDC from Blend back to the blended pool
    /// out of band. The dual of `manual_to_blend`. `emergency_unwind` is
    /// the right tool for a full drain; this exists for partial unwinds
    /// where the admin wants tactical control over the amount.
    pub fn manual_from_blend(env: Env, amount: i128) {
        storage::get_admin(&env).require_auth();
        assert!(amount > 0, "amount must be positive");
        if storage::get_paused(&env) {
            soroban_sdk::panic_with_error!(&env, crate::error::LocalError::Paused);
        }

        let principal_before = storage::get_principal_supplied(&env);
        assert!(
            amount <= principal_before,
            "amount exceeds principal_supplied; use emergency_unwind for full drain",
        );

        let blended_pool = storage::get_blended_pool(&env);
        let blend_pool = storage::get_blend_pool(&env);
        let usdc = storage::get_usdc(&env);
        let xlm = storage::get_xlm(&env);

        blend_submit(&env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, amount);
        deposit_from_delegate(&env, &blended_pool, &usdc, amount);

        let principal_after = (principal_before - amount).max(0);
        storage::set_principal_supplied(&env, principal_after);

        let pool_client = BlendedPoolClient::new(&env, &blended_pool);
        let state = pool_client.query_delegate_state();
        let (liquid_after, delegated_after) = if usdc < xlm {
            (state.liquid_a, state.delegated_a)
        } else {
            (state.liquid_b, state.delegated_b)
        };
        RebalanceExecuted::new(
            DIRECTION_FROM_BLEND,
            amount,
            liquid_after,
            delegated_after,
            principal_after,
        )
        .publish(&env);
        assert_no_usdc_residue(&env);
    }

    /// Admin-only: tighten or relax the target liquid-USDC share of total
    /// USDC. Same range validation as the constructor (strictly within
    /// (0, 10000) bps).
    pub fn set_target_ratio_bps(env: Env, bps: u32) {
        storage::get_admin(&env).require_auth();
        assert!(
            bps > 0 && bps < BPS_DEN as u32,
            "target_ratio_bps must be in (0, 10000)"
        );
        storage::set_target_ratio_bps(&env, bps);
        ConfigUpdated::new(soroban_sdk::symbol_short!("target"), bps as i128).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: widen or tighten the no-op band around the target. Same
    /// range as the constructor (`< 10000` bps).
    pub fn set_rebalance_band_bps(env: Env, bps: u32) {
        storage::get_admin(&env).require_auth();
        assert!(bps < BPS_DEN as u32, "rebalance_band_bps must be < 10000");
        storage::set_rebalance_band_bps(&env, bps);
        ConfigUpdated::new(soroban_sdk::symbol_short!("band"), bps as i128).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: floor on total pool USDC below which Rebalance is a
    /// no-op (does not consume cooldown).
    pub fn set_min_total_usdc(env: Env, amount: i128) {
        storage::get_admin(&env).require_auth();
        assert!(amount >= 0, "min_total_usdc must be non-negative");
        storage::set_min_total_usdc(&env, amount);
        ConfigUpdated::new(soroban_sdk::symbol_short!("min_tot"), amount).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: minimum seconds between successful Rebalance actions.
    /// `0` disables the cooldown gate entirely.
    pub fn set_rebalance_cooldown_secs(env: Env, secs: u64) {
        storage::get_admin(&env).require_auth();
        storage::set_rebalance_cooldown_secs(&env, secs);
        ConfigUpdated::new(soroban_sdk::symbol_short!("cooldown"), secs as i128).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: address that receives BLND emissions on every harvest.
    /// The handler never holds BLND, so this is a pure routing knob.
    pub fn set_blnd_treasury(env: Env, treasury: Address) {
        storage::get_admin(&env).require_auth();
        storage::set_blnd_treasury(&env, &treasury);
        AddressConfigUpdated::new(soroban_sdk::symbol_short!("treasury"), treasury).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: id of the (reserve, b-token) pair the handler claims
    /// BLND emissions against on Blend. Derived from `reserve_index * 2 +
    /// 1` for the USDC reserve. Must be updated if Blend reconfigures the
    /// reserve set or the handler is repointed at a new Blend pool.
    pub fn set_usdc_reserve_token_id(env: Env, id: u32) {
        storage::get_admin(&env).require_auth();
        if id % 2 == 0 {
            soroban_sdk::panic_with_error!(
                &env,
                crate::error::LocalError::InvalidReserveTokenId
            );
        }
        storage::set_usdc_reserve_token_id(&env, id);
        ConfigUpdated::new(soroban_sdk::symbol_short!("usdc_id"), id as i128).publish(&env);
        storage::extend_instance_ttl(&env);
    }

    /// Admin-only: view of the current usdc_reserve_token_id (kept here
    /// since the constructor takes it but there was no view accessor).
    pub fn usdc_reserve_token_id(env: Env) -> u32 {
        storage::get_usdc_reserve_token_id(&env)
    }
}

/// Standard WarpDrive admin + upgrade + version surface. Matches the
/// canonical pattern in `warpdrive-contracts/contracts/stellar-handler`
/// so dashboards and admin tooling can drive every WarpDrive handler
/// through one interface.
#[contractimpl]
impl WarpDriveInterface for AutomationHandler {
    fn upgrade(env: Env, new_wasm_hash: BytesN<32>, new_version: String) {
        let admin = storage::get_admin(&env);
        admin.require_auth();

        storage::set_version(&env, &new_version);
        storage::extend_instance_ttl(&env);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        ContractUpgraded::new(new_version).publish(&env);
    }

    fn admin(env: Env) -> Address {
        storage::get_admin(&env)
    }

    fn pending_admin(env: Env) -> Option<Address> {
        warpdrive_shared::admin::pending(&env)
    }

    fn propose_admin(env: Env, new_admin: Address) {
        warpdrive_shared::admin::propose(&env, &storage::get_admin(&env), new_admin);
    }

    fn accept_admin(env: Env) {
        let new_admin = warpdrive_shared::admin::accept(&env);
        storage::set_admin(&env, &new_admin);
    }

    fn version(env: Env) -> String {
        storage::get_version(&env)
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

    pub fn test_set_last_harvest_ts(env: Env, ts: u64) {
        storage::set_last_harvest_ts(&env, ts);
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

    // Blend health pre-check. If the Blend pool is Admin-Frozen / Frozen /
    // Setup (status > 3), Supply/Withdraw via `submit` would revert. Rather
    // than burn gas reverting, bail early as a silent no-op (no cooldown
    // consumed). The admin can call `emergency_unwind` explicitly to drain
    // a Frozen position.
    if BlendPoolClient::new(env, &blend_pool).get_config().status > BLEND_HEALTHY_STATUS_MAX {
        return Ok(());
    }

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

    let delegated_before = total_usdc - liquid_usdc;
    let max_cap = storage::get_max_rebalance_amount(env);
    let min_floor = storage::get_min_rebalance_amount(env);

    if liquid_usdc > upper {
        // Pool is over-liquid in USDC - supply the excess to Blend.
        let natural = liquid_usdc
            .checked_sub(target_liquid)
            .ok_or(HandlerError::OtherInvocationError)?;
        // Dust floor: skip without consuming cooldown.
        if natural < min_floor {
            return Ok(());
        }
        // Cap clamp: 0 means unlimited.
        let amount = if max_cap > 0 && natural > max_cap {
            max_cap
        } else {
            natural
        };
        pool_client.withdraw_to_delegate(&usdc, &amount);
        blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, amount);
        let prev = storage::get_principal_supplied(env);
        let principal_after = prev
            .checked_add(amount)
            .ok_or(HandlerError::OtherInvocationError)?;
        storage::set_principal_supplied(env, principal_after);
        storage::set_last_rebalance_ts(env, now);
        RebalanceExecuted::new(
            DIRECTION_TO_BLEND,
            amount,
            liquid_usdc - amount,
            delegated_before + amount,
            principal_after,
        )
        .publish(env);
    } else if liquid_usdc < lower {
        // Pool is under-liquid in USDC - pull from Blend to top up.
        let natural = target_liquid
            .checked_sub(liquid_usdc)
            .ok_or(HandlerError::OtherInvocationError)?;
        let principal = storage::get_principal_supplied(env);
        // Sanity cap at locally tracked principal (Blend would revert if a
        // bad-debt write-down made the position smaller than expected).
        let bounded = natural.min(principal);
        // Dust floor: skip without consuming cooldown.
        if bounded < min_floor {
            return Ok(());
        }
        // Cap clamp: 0 means unlimited.
        let amount = if max_cap > 0 && bounded > max_cap {
            max_cap
        } else {
            bounded
        };
        if amount > 0 {
            blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_WITHDRAW, amount);
            deposit_from_delegate(env, &blended_pool, &usdc, amount);
            let principal_after = (principal - amount).max(0);
            storage::set_principal_supplied(env, principal_after);
            storage::set_last_rebalance_ts(env, now);
            RebalanceExecuted::new(
                DIRECTION_FROM_BLEND,
                amount,
                liquid_usdc + amount,
                delegated_before - amount,
                principal_after,
            )
            .publish(env);
        }
    }
    // Inside the band: no-op.
    assert_no_usdc_residue(env);
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

    // 1. Route BLND emissions straight to the treasury. `claim` returns the
    //    amount transferred to `to`; capture it for the HarvestCompleted event.
    let blend = BlendPoolClient::new(env, &blend_pool);
    let mut ids: Vec<u32> = Vec::new(env);
    ids.push_back(usdc_reserve_token_id);
    let blnd_routed = blend.claim(&env.current_contract_address(), &ids, &blnd_treasury);

    // 2 + 3. Peel off USDC interest, then donate it. Only attempt the
    // withdraw/resupply cycle when Blend is healthy enough (`status <= 3`);
    // otherwise the Supply leg would revert. BLND emissions claim above
    // does not require pool health and runs unconditionally.
    let mut interest_donated: i128 = 0;
    let mut principal_after = principal;
    let blend_healthy = blend.get_config().status <= BLEND_HEALTHY_STATUS_MAX;
    if principal > 0 && blend_healthy {
        let usdc_token = soroban_sdk::token::Client::new(env, &usdc);
        let usdc_before_withdraw = usdc_token.balance(&env.current_contract_address());

        // Withdraw via try_submit so a Blend revert (e.g. utilization at
        // 100% rejecting the withdraw, status flipping to Frozen between
        // get_config and submit) does NOT roll back the BLND claim above.
        // Soroban contract panics roll back the WHOLE tx by default;
        // try_<method> is the auto-generated recoverable form on every
        // contractclient trait.
        let mut requests: Vec<BlendRequest> = Vec::new(env);
        requests.push_back(BlendRequest {
            request_type: BLEND_REQUEST_WITHDRAW,
            address: usdc.clone(),
            amount: i128::MAX,
        });
        let withdraw_ok = matches!(
            blend.try_submit(
                &env.current_contract_address(),
                &env.current_contract_address(),
                &env.current_contract_address(),
                &requests,
            ),
            Ok(Ok(_))
        );
        if !withdraw_ok {
            HarvestPartial::new(blnd_routed).publish(env);
        } else {
            let actual_redeemable = usdc_token
                .balance(&env.current_contract_address())
                .saturating_sub(usdc_before_withdraw);

            let supply_amount = principal.min(actual_redeemable);
            if supply_amount > 0 {
                blend_submit(env, &blend_pool, &usdc, BLEND_REQUEST_SUPPLY, supply_amount);
            }
            storage::set_principal_supplied(env, supply_amount);
            principal_after = supply_amount;

            // Bad-debt detection: if Blend redeemed less than the handler's
            // recorded principal, the b-token position has been written down.
            // Emit a high-signal event for monitoring; the handler proceeds
            // with the smaller principal (silent correction; not a panic).
            if actual_redeemable < principal {
                BadDebtDetected::new(
                    principal,
                    actual_redeemable,
                    principal - actual_redeemable,
                )
                .publish(env);
            }

            let interest = actual_redeemable.saturating_sub(supply_amount);
            if interest > 0 {
                donate_to_pool(env, &blended_pool, &usdc, interest);
                interest_donated = interest;
            }
        }
    }

    storage::set_last_harvest_ts(env, env.ledger().timestamp());
    HarvestCompleted::new(interest_donated, blnd_routed, principal_after).publish(env);

    assert_no_usdc_residue(env);
    Ok(())
}


/// Post-action invariant: the handler must NOT hold any USDC. Any
/// non-zero residue means an entrypoint forgot to push tokens out before
/// returning, which would silently leak funds across calls. Reverts the
/// transaction with `LocalError::UsdcLeak` (601) so the action is rolled
/// back. Called at the end of every money-moving entrypoint and executor.
fn assert_no_usdc_residue(env: &Env) {
    let usdc = storage::get_usdc(env);
    let bal = soroban_sdk::token::Client::new(env, &usdc)
        .balance(&env.current_contract_address());
    if bal != 0 {
        soroban_sdk::panic_with_error!(env, crate::error::LocalError::UsdcLeak);
    }
    // Renew the instance-storage TTL on every successful money-moving
    // action, defending against an extended dormant period (e.g. quorum
    // offline for a long time) that would let the contract's instance
    // storage archive.
    storage::extend_instance_ttl(env);
}

/// Pre-authorize a nested `token.transfer(from = handler, to = recipient, amount)`
/// sub-invocation. Required before any handler call that triggers a nested
/// pull-from-handler — Blend's `submit(SUPPLY)`, the blended pool's
/// `deposit_from_delegate`, the blended pool's `donate`. The SDK's
/// automatic direct-call auth covers only invocations the handler issues
/// itself; nested transfers initiated by Blend's or the pool's code need
/// this explicit handshake or the recording-auth pass fails with
/// `Error(Auth, InvalidAction)`.
fn authorize_handler_transfer(
    env: &Env,
    token: &soroban_sdk::Address,
    recipient: &soroban_sdk::Address,
    amount: i128,
) {
    let args: Vec<Val> = vec![
        env,
        env.current_contract_address().into_val(env),
        recipient.into_val(env),
        amount.into_val(env),
    ];
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: token.clone(),
                fn_name: Symbol::new(env, "transfer"),
                args,
            },
            sub_invocations: vec![env],
        }),
    ]);
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
    // SUPPLY triggers `token.transfer(handler → blend_pool, amount)` inside
    // Blend's submit. WITHDRAW pushes tokens FROM the pool, so no handler
    // pre-auth is needed.
    if request_type == BLEND_REQUEST_SUPPLY {
        authorize_handler_transfer(env, token, blend_pool, amount);
    }
    blend.submit(
        &env.current_contract_address(),
        &env.current_contract_address(),
        &env.current_contract_address(),
        &requests,
    );
}

/// Wrapper around `blended_pool.deposit_from_delegate(token, amount)` that
/// pre-authorizes the nested `token.transfer(handler → blended_pool)` the
/// pool issues internally.
fn deposit_from_delegate(
    env: &Env,
    blended_pool: &soroban_sdk::Address,
    token: &soroban_sdk::Address,
    amount: i128,
) {
    authorize_handler_transfer(env, token, blended_pool, amount);
    BlendedPoolClient::new(env, blended_pool).deposit_from_delegate(token, &amount);
}

/// Wrapper around `blended_pool.donate(token, amount)` that pre-authorizes
/// the nested `token.transfer(handler → blended_pool)` the pool issues
/// internally.
fn donate_to_pool(
    env: &Env,
    blended_pool: &soroban_sdk::Address,
    token: &soroban_sdk::Address,
    amount: i128,
) {
    authorize_handler_transfer(env, token, blended_pool, amount);
    BlendedPoolClient::new(env, blended_pool).donate(token, &amount);
}

/// Saturating-checked `a * b / c`.
fn mul_div(a: i128, b: i128, c: i128) -> Result<i128, HandlerError> {
    a.checked_mul(b)
        .and_then(|v| v.checked_div(c))
        .ok_or(HandlerError::OtherInvocationError)
}
