use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use stellar_xdr::curr::{Limits, ReadXdr, ScSymbol, ScVal};

// USDC Stellar Asset Contract on mainnet. The blended pool we integrate
// with is XLM-USDC; this is the side we ultimately move into Blend.
//
// Source: blend-contracts-v2/test-suites/src/snapshot.rs USDC_ID.
pub const USDC_SAC_CONTRACT_ID: &str =
    "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75";

#[derive(Default, Serialize, Deserialize)]
pub struct SwapState {
    pub sell_token: Option<String>,
    pub buy_token: Option<String>,
    pub offer_amount: Option<i128>,
    pub return_amount: Option<i128>,
    /// Set true the first time a finalize emits a payload AND the calling
    /// event wins the CAS race to flip the flag. Subsequent events on the
    /// same accumulator key (same tx_hash:op_index) see this tombstone and
    /// skip emitting, so we get exactly-once delivery per Phoenix swap (which
    /// fires 8 events on the pool).
    #[serde(default)]
    pub finalized: bool,
}

#[derive(Clone)]
pub enum FieldUpdate {
    SellToken(String),
    BuyToken(String),
    OfferAmount(i128),
    ReturnAmount(i128),
    Other,
}

pub fn decode_field(topic_segments: &[String], value: &str) -> Result<FieldUpdate> {
    if topic_segments.len() < 2 {
        return Ok(FieldUpdate::Other);
    }
    let key_scval = ScVal::from_xdr_base64(&topic_segments[1], Limits::none())
        .context("decode topic[1]")?;
    let key = match key_scval {
        ScVal::Symbol(ScSymbol(s)) => s.to_string(),
        ScVal::String(s) => s.to_string(),
        _ => return Ok(FieldUpdate::Other),
    };
    let val = ScVal::from_xdr_base64(value, Limits::none()).context("decode value")?;

    Ok(match key.as_str() {
        "sell_token" => FieldUpdate::SellToken(decode_address_strkey(&val)?),
        "buy_token" => FieldUpdate::BuyToken(decode_address_strkey(&val)?),
        "offer_amount" => FieldUpdate::OfferAmount(decode_i128(&val)?),
        "return_amount" => FieldUpdate::ReturnAmount(decode_i128(&val)?),
        _ => FieldUpdate::Other,
    })
}

pub fn apply(state: &mut SwapState, update: FieldUpdate) {
    match update {
        FieldUpdate::SellToken(s) => state.sell_token = Some(s),
        FieldUpdate::BuyToken(s) => state.buy_token = Some(s),
        FieldUpdate::OfferAmount(n) => state.offer_amount = Some(n),
        FieldUpdate::ReturnAmount(n) => state.return_amount = Some(n),
        FieldUpdate::Other => {}
    }
}

/// Decide whether this swap should trigger a rebalance, and if so return
/// the direction + `amount_usdc` to emit.
///
/// v1 trigger: emit 10% of the swap's USDC volume in whichever direction
/// the pool experienced. Intentionally crude — production logic would
/// compare current reserves against a 50% target via RPC. For the first
/// slice we just need the pipeline to fire end-to-end in both directions.
pub fn try_finalize(state: &SwapState) -> Option<RebalanceEmit> {
    let sell_token = state.sell_token.as_ref()?;
    let buy_token = state.buy_token.as_ref()?;
    let offer_amount = state.offer_amount?;
    let return_amount = state.return_amount?;

    if sell_token == USDC_SAC_CONTRACT_ID {
        // Trader sold USDC into the pool. Pool's liquid USDC went UP. Push
        // some of the excess into Blend.
        let amount = offer_amount / 10;
        if amount > 0 {
            return Some(RebalanceEmit::ToBlend(amount));
        }
    } else if buy_token == USDC_SAC_CONTRACT_ID {
        // Trader bought USDC out of the pool. Pool's liquid USDC went DOWN.
        // Pull some USDC back from Blend to top it up.
        let amount = return_amount / 10;
        if amount > 0 {
            return Some(RebalanceEmit::FromBlend(amount));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebalanceEmit {
    ToBlend(i128),
    FromBlend(i128),
}

fn decode_address_strkey(val: &ScVal) -> Result<String> {
    match val {
        ScVal::Address(addr) => Ok(addr.to_string()),
        _ => Err(anyhow!("expected ScVal::Address, got {val:?}")),
    }
}

fn decode_i128(val: &ScVal) -> Result<i128> {
    match val {
        ScVal::I128(parts) => Ok(((parts.hi as i128) << 64) | (parts.lo as u128 as i128)),
        _ => Err(anyhow!("expected ScVal::I128, got {val:?}")),
    }
}
