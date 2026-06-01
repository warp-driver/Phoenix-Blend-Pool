//! Topic filter for the blended-pool event stream.
//!
//! WarpDrive triggers wake the circuit on every event the blended pool
//! emits (rest-wildcard on the trigger side). The fork emits exactly ONE
//! event per logical action; this module decides whether that action is
//! one that moves the pool's liquid USDC ratio:
//!
//!   - `swap`               trader exchanges XLM/USDC; both sides change.
//!   - `provide_liquidity`  LP deposit raises liquid + total USDC; ratio
//!                          drifts toward 100% liquid.
//!   - `withdraw_liquidity` LP redeem pays out from physical balance using
//!                          a share_ratio applied to LOGICAL reserves; this
//!                          can drain liquid USDC even at fair share when
//!                          some of the USDC is parked in Blend. Must
//!                          rebalance to top up.
//!
//! Everything else (delegate-side events from our own actions, admin
//! events, ERC-20-style transfer/mint/burn) is ignored.

use anyhow::Result;
use stellar_xdr::curr::{Limits, ReadXdr, ScSymbol, ScVal};

const TOPIC_SWAP: &str = "swap";
const TOPIC_PROVIDE_LIQUIDITY: &str = "provide_liquidity";
const TOPIC_WITHDRAW_LIQUIDITY: &str = "withdraw_liquidity";

/// True if topic[0] is one of the three event families that move the pool's
/// liquid USDC ratio. Returns false on any unrecognised, empty, or
/// undecodable topic.
///
/// Hostile or malformed topic segments (e.g. invalid base64, ScVal types
/// other than String/Symbol) are silently skipped rather than propagated
/// as Err: the only callers we have process one event at a time, and we
/// do not want a single noisy event-emitter to dominate the host log.
/// Legitimate Phoenix event-shape changes still surface via the empty
/// return values - the dashboard's "no rebalance fired in N hours" alert
/// catches that case.
pub fn is_relevant_event(topic_segments: &[String]) -> Result<bool> {
    let Some(first) = topic_segments.first() else {
        return Ok(false);
    };
    let scval = match ScVal::from_xdr_base64(first, Limits::none()) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let topic = match scval {
        ScVal::Symbol(ScSymbol(s)) => s.to_string(),
        ScVal::String(s) => s.to_string(),
        // Phoenix emits topic[0] as String/Symbol; anything else isn't ours.
        _ => return Ok(false),
    };
    Ok(matches!(
        topic.as_str(),
        TOPIC_SWAP | TOPIC_PROVIDE_LIQUIDITY | TOPIC_WITHDRAW_LIQUIDITY
    ))
}
