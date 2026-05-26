use anyhow::{Context, Result};
use stellar_xdr::curr::{Limits, ScSymbol, ScVal, ScVec, StringM, VecM, WriteXdr};

/// Variant tag for the `RebalanceAction` enum on the handler. Both variants
/// are unit (no data); the off-chain circuit only fires triggers and the
/// handler decides direction + amount on-chain against pool state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    Rebalance,
    HarvestYield,
}

impl Direction {
    fn tag(self) -> &'static str {
        match self {
            Direction::Rebalance => "Rebalance",
            Direction::HarvestYield => "HarvestYield",
        }
    }
}

/// Encode a unit-variant `RebalanceAction` as XDR bytes the handler can decode
/// via `RebalanceAction::from_xdr`. Soroban encodes a contracttype enum as a
/// Vec whose first element is the variant Symbol; for a unit variant the
/// Symbol alone is the entire payload.
pub fn encode(direction: Direction) -> Result<Vec<u8>> {
    let tag = symbol_val(direction.tag())?;
    let elements: VecM<ScVal> = vec![tag].try_into().context("enum payload vec")?;
    ScVal::Vec(Some(ScVec(elements)))
        .to_xdr(Limits::none())
        .context("xdr-encode RebalanceAction")
}

fn symbol_val(s: &str) -> Result<ScVal> {
    let inner: StringM<32> = s.as_bytes().try_into().context("symbol too long")?;
    Ok(ScVal::Symbol(ScSymbol(inner)))
}
