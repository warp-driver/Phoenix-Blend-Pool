use crate::payload;
use crate::phoenix::{
    apply, decode_field, try_finalize, FieldUpdate, SwapState, USDC_SAC_CONTRACT_ID,
};
use std::str::FromStr;
use stellar_xdr::curr::{
    Int128Parts, Limits, ReadXdr, ScAddress, ScString, ScSymbol, ScVal, StringM, WriteXdr,
};

#[test]
fn payload_encodes_as_scmap_with_single_amount_entry() {
    let amount: i128 = 1_000_0000000; // 1000 USDC, 7 decimals
    let bytes = payload::encode(amount).unwrap();

    let decoded = ScVal::from_xdr(&bytes, Limits::none()).unwrap();
    let entries = match decoded {
        ScVal::Map(Some(m)) => m.0.to_vec(),
        other => panic!("expected ScMap, got {other:?}"),
    };
    assert_eq!(entries.len(), 1);

    let amount_sym = ScSymbol("amount_usdc".try_into().unwrap());
    assert_eq!(entries[0].key, ScVal::Symbol(amount_sym));

    match &entries[0].val {
        ScVal::I128(Int128Parts { hi, lo }) => {
            let recombined = ((*hi as i128) << 64) | (*lo as u128 as i128);
            assert_eq!(recombined, amount);
        }
        other => panic!("expected I128, got {other:?}"),
    }
}

#[test]
fn payload_encodes_negative_amounts_too() {
    // We never expect negatives in production (the handler rejects amount_usdc
    // <= 0) but the encoder should still produce well-formed XDR.
    let amount: i128 = -42_i128;
    let bytes = payload::encode(amount).unwrap();
    let decoded = ScVal::from_xdr(&bytes, Limits::none()).unwrap();
    match decoded {
        ScVal::Map(Some(m)) => {
            let parts = match &m.0.to_vec()[0].val {
                ScVal::I128(p) => p.clone(),
                _ => panic!(),
            };
            let recombined = ((parts.hi as i128) << 64) | (parts.lo as u128 as i128);
            assert_eq!(recombined, amount);
        }
        _ => panic!(),
    }
}

#[test]
fn finalize_emits_on_usdc_sale_into_pool() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::BuyToken("CXLMPLACEHOLDER".into()));
    apply(&mut s, FieldUpdate::OfferAmount(1_000_0000000)); // 1000 USDC
    assert!(try_finalize(&s).is_none(), "not finalized until return arrives");
    apply(&mut s, FieldUpdate::ReturnAmount(2_500_0000000));
    // 10% of inbound USDC
    assert_eq!(try_finalize(&s), Some(100_0000000));
}

#[test]
fn finalize_returns_none_when_pool_loses_usdc() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken("CXLM".into()));
    apply(&mut s, FieldUpdate::BuyToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::OfferAmount(2_500_0000000));
    apply(&mut s, FieldUpdate::ReturnAmount(1_000_0000000));
    assert_eq!(try_finalize(&s), None);
}

#[test]
fn finalize_returns_none_for_non_usdc_swap() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken("CFOO".into()));
    apply(&mut s, FieldUpdate::BuyToken("CBAR".into()));
    apply(&mut s, FieldUpdate::OfferAmount(100));
    apply(&mut s, FieldUpdate::ReturnAmount(99));
    assert_eq!(try_finalize(&s), None);
}

#[test]
fn finalize_skips_dust_swaps() {
    // 5 USDC base units => 0.5 after integer division => skip.
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::BuyToken("CXLM".into()));
    apply(&mut s, FieldUpdate::OfferAmount(5));
    apply(&mut s, FieldUpdate::ReturnAmount(1));
    assert_eq!(try_finalize(&s), None);
}

#[test]
fn decode_real_phoenix_event_shape() {
    // Synthetic data mirroring the on-chain event format: 5 events for one
    // logical swap. Trader sells USDC into the pool; circuit should emit a
    // RebalanceToBlend payload.
    let xlm = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
    let usdc = USDC_SAC_CONTRACT_ID;

    let events = [
        (string_topic("swap"), string_topic("sell_token"), addr_value(usdc)),
        (string_topic("swap"), string_topic("offer_amount"), i128_value(1_000_0000000)),
        (string_topic("swap"), string_topic("buy_token"), addr_value(xlm)),
        (string_topic("swap"), string_topic("return_amount"), i128_value(2_500_0000000)),
    ];

    let mut state = SwapState::default();
    for (t0, t1, v) in &events {
        let update = decode_field(&[t0.clone(), t1.clone()], v).unwrap();
        apply(&mut state, update);
    }

    assert_eq!(try_finalize(&state), Some(100_0000000));
}

fn string_topic(s: &str) -> String {
    let inner: StringM = s.try_into().unwrap();
    ScVal::String(ScString(inner))
        .to_xdr_base64(Limits::none())
        .unwrap()
}

fn addr_value(strkey: &str) -> String {
    let addr = ScAddress::from_str(strkey).unwrap();
    ScVal::Address(addr).to_xdr_base64(Limits::none()).unwrap()
}

fn i128_value(n: i128) -> String {
    let hi = (n >> 64) as i64;
    let lo = n as u64;
    ScVal::I128(Int128Parts { hi, lo })
        .to_xdr_base64(Limits::none())
        .unwrap()
}
