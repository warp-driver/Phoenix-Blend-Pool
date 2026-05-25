use anyhow::{Context, Result};
use stellar_xdr::curr::{
    Int128Parts, Limits, ScMap, ScMapEntry, ScSymbol, ScVal, StringM, WriteXdr,
};

// Encode a Soroban `contracttype` RebalanceToBlend { amount_usdc: i128 }
// as an XDR-serialized ScVal::Map. Single field, so no key-ordering hazard,
// but we still emit the alphabetically-sorted shape Soroban's `contracttype`
// decoders expect.
pub fn encode(amount_usdc: i128) -> Result<Vec<u8>> {
    let hi = (amount_usdc >> 64) as i64;
    let lo = (amount_usdc as u128 & u64::MAX as u128) as u64;

    let entry = ScMapEntry {
        key: symbol_val("amount_usdc")?,
        val: ScVal::I128(Int128Parts { hi, lo }),
    };

    let map = ScMap(vec![entry].try_into().context("ScMap construction")?);

    ScVal::Map(Some(map))
        .to_xdr(Limits::none())
        .context("xdr-encode RebalanceToBlend")
}

fn symbol_val(s: &str) -> Result<ScVal> {
    let inner: StringM<32> = s.as_bytes().try_into().context("symbol too long")?;
    Ok(ScVal::Symbol(ScSymbol(inner)))
}
