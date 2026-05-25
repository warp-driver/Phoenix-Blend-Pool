use anyhow::{Context, Result};
use stellar_xdr::curr::{
    Int128Parts, Limits, ScSymbol, ScVal, ScVec, StringM, VecM, WriteXdr,
};

/// Variant tag for the discriminated RebalanceAction enum on the handler.
/// Soroban encodes a contracttype enum as a Vec whose first element is the
/// variant Symbol; tuple variants follow with their data values, unit
/// variants are just the Symbol alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    ToBlend,
    FromBlend,
    HarvestYield,
}

impl Direction {
    fn tag(self) -> &'static str {
        match self {
            Direction::ToBlend => "ToBlend",
            Direction::FromBlend => "FromBlend",
            Direction::HarvestYield => "HarvestYield",
        }
    }
}

/// Encode a `RebalanceAction` variant as XDR bytes that the handler can decode
/// via `RebalanceAction::from_xdr`. `amount_usdc` is required for ToBlend and
/// FromBlend; HarvestYield is a unit variant and ignores it.
pub fn encode(direction: Direction, amount_usdc: i128) -> Result<Vec<u8>> {
    let tag = symbol_val(direction.tag())?;
    let elements: VecM<ScVal> = match direction {
        Direction::HarvestYield => vec![tag].try_into().context("enum payload vec")?,
        Direction::ToBlend | Direction::FromBlend => {
            let hi = (amount_usdc >> 64) as i64;
            let lo = (amount_usdc as u128 & u64::MAX as u128) as u64;
            let amount = ScVal::I128(Int128Parts { hi, lo });
            vec![tag, amount].try_into().context("enum payload vec")?
        }
    };

    ScVal::Vec(Some(ScVec(elements)))
        .to_xdr(Limits::none())
        .context("xdr-encode RebalanceAction")
}

fn symbol_val(s: &str) -> Result<ScVal> {
    let inner: StringM<32> = s.as_bytes().try_into().context("symbol too long")?;
    Ok(ScVal::Symbol(ScSymbol(inner)))
}
