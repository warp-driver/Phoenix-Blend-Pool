//! Project-specific contract events emitted by the automation handler.
//!
//! These are independent of `warpdrive_shared::handler::Verified` (which the
//! handler still emits after every successful `verify_xlm`); these events
//! carry domain-level information a monitoring dashboard or LP-facing
//! display needs.
//!
//! Convention: every event variant gets a `pub fn new(...) -> Self` so call
//! sites read as `RebalanceExecuted::new(...).publish(&env);`. `#[contractevent]`
//! emits a `publish(&self, env: &Env)` method that writes the event in the
//! contract's event stream.

use soroban_sdk::{contractevent, Env, Symbol};

/// `direction` value used by `RebalanceExecuted` when the handler supplies USDC
/// to Blend (pool over-liquid → park excess).
pub const DIRECTION_TO_BLEND: Symbol = soroban_sdk::symbol_short!("to_blend");

/// `direction` value used by `RebalanceExecuted` when the handler pulls USDC
/// back from Blend into the pool (pool under-liquid → top up).
pub const DIRECTION_FROM_BLEND: Symbol = soroban_sdk::symbol_short!("frm_blnd");

#[contractevent]
pub struct RebalanceExecuted {
    pub direction: Symbol,
    pub amount: i128,
    pub liquid_after: i128,
    pub delegated_after: i128,
    pub principal_after: i128,
}

impl RebalanceExecuted {
    pub fn new(
        direction: Symbol,
        amount: i128,
        liquid_after: i128,
        delegated_after: i128,
        principal_after: i128,
    ) -> Self {
        Self {
            direction,
            amount,
            liquid_after,
            delegated_after,
            principal_after,
        }
    }
}

#[contractevent]
pub struct HarvestCompleted {
    pub interest_donated: i128,
    pub blnd_routed: i128,
    pub principal_after: i128,
}

impl HarvestCompleted {
    pub fn new(interest_donated: i128, blnd_routed: i128, principal_after: i128) -> Self {
        Self {
            interest_donated,
            blnd_routed,
            principal_after,
        }
    }
}
/// Emitted after any admin-mediated configuration change. `field` is a
/// short Symbol identifying which knob changed (see comments at each call
/// site); `value` carries the new value coerced to i128 (bps fields fit,
/// u64 cooldowns fit, addresses use a separate event type if needed).
#[contractevent]
pub struct ConfigUpdated {
    pub field: Symbol,
    pub value: i128,
}

impl ConfigUpdated {
    pub fn new(field: Symbol, value: i128) -> Self {
        Self { field, value }
    }
}

/// Emitted whenever the admin toggles the pause state.
#[contractevent]
pub struct PauseToggled {
    pub paused: bool,
}

impl PauseToggled {
    pub fn new(paused: bool) -> Self {
        Self { paused }
    }
}

/// Emitted at the end of every `emergency_unwind` call. `redeemed` is the
/// total USDC pulled out of Blend; `principal_before` is the value
/// `principal_supplied` had at call entry. After the call,
/// `principal_supplied = 0`.
#[contractevent]
pub struct EmergencyUnwound {
    pub redeemed: i128,
    pub principal_before: i128,
}

impl EmergencyUnwound {
    pub fn new(redeemed: i128, principal_before: i128) -> Self {
        Self {
            redeemed,
            principal_before,
        }
    }
}

/// Emitted when the admin changes an address-typed config field (currently
/// `blnd_treasury`). Separate from `ConfigUpdated` so consumers don't need
/// to coerce between i128 and Address.
#[contractevent]
pub struct AddressConfigUpdated {
    pub field: Symbol,
    pub value: soroban_sdk::Address,
}

impl AddressConfigUpdated {
    pub fn new(field: Symbol, value: soroban_sdk::Address) -> Self {
        Self { field, value }
    }
}

/// Re-export `Env` for compatibility; not strictly needed but keeps the
/// publish-call sites readable when the caller already has `env` in scope.
#[allow(dead_code)]
pub(crate) fn touch(_env: &Env) {}
