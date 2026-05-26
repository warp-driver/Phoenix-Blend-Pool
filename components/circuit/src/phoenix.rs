//! Topic filter for the blended-pool event stream.
//!
//! WarpDrive triggers wake the circuit on EVERY event the blended pool emits
//! (we filter with rest-wildcard on the node side so we don't miss a topic).
//! This module decides whether a given event should result in a Rebalance
//! tick. Three event families move the pool's liquid USDC ratio:
//!
//!   - `swap` ........... trader exchanges XLM<>USDC, both sides change.
//!   - `provide_liquidity` LP deposits raise both liquid USDC and total USDC,
//!                        but the ratio can drift up (toward 100% liquid).
//!   - `withdraw_liquidity` LP redeem pays out from physical balance using a
//!                        share_ratio applied to LOGICAL reserves; this can
//!                        drain liquid USDC even at fair share when some of
//!                        the USDC is parked in Blend. Must rebalance.
//!
//! Everything else (delegate-side events, admin events) is our own action or
//! out of scope. We ignore it to avoid spurious wake-ups.

use anyhow::{Context, Result};
use stellar_xdr::curr::{Limits, ReadXdr, ScSymbol, ScVal};

const TOPIC_SWAP: &str = "swap";
const TOPIC_PROVIDE_LIQUIDITY: &str = "provide_liquidity";
const TOPIC_WITHDRAW_LIQUIDITY: &str = "withdraw_liquidity";

/// True if topic[0] is one of the three event families that move the pool's
/// liquid USDC ratio. Returns false on any unrecognised or empty topic.
///
/// Decoding errors propagate so the host log shows what broke; the node will
/// not call back for the offending event, which is the safe failure mode.
pub fn is_relevant_event(topic_segments: &[String]) -> Result<bool> {
    let Some(first) = topic_segments.first() else {
        return Ok(false);
    };
    let scval = ScVal::from_xdr_base64(first, Limits::none()).context("decode topic[0]")?;
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
