use crate::payload::{self, Direction};
use crate::phoenix::{apply, decode_field, try_finalize, FieldUpdate, SwapState, USDC_SAC_CONTRACT_ID};
use std::str::FromStr;
use stellar_xdr::curr::{
    Int128Parts, Limits, ReadXdr, ScAddress, ScString, ScSymbol, ScVal, StringM, WriteXdr,
};

/// Decode the encoded enum payload back into its tag Symbol. Both production
/// variants are unit, so we only ever expect a single-element ScVec.
fn decode_envelope_tag(bytes: &[u8]) -> String {
    let decoded = ScVal::from_xdr(bytes, Limits::none()).unwrap();
    let elements = match decoded {
        ScVal::Vec(Some(v)) => v.0.to_vec(),
        other => panic!("expected ScVec, got {other:?}"),
    };
    assert_eq!(
        elements.len(),
        1,
        "unit-variant payload must be exactly [tag]"
    );
    match &elements[0] {
        ScVal::Symbol(ScSymbol(s)) => s.to_string(),
        other => panic!("expected tag Symbol, got {other:?}"),
    }
}

#[test]
fn payload_encodes_rebalance_unit_variant() {
    let bytes = payload::encode(Direction::Rebalance).unwrap();
    assert_eq!(decode_envelope_tag(&bytes), "Rebalance");
}

#[test]
fn payload_encodes_harvest_yield_unit_variant() {
    let bytes = payload::encode(Direction::HarvestYield).unwrap();
    assert_eq!(decode_envelope_tag(&bytes), "HarvestYield");
}

#[test]
fn finalize_emits_when_usdc_sold_into_pool() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::BuyToken("CXLM_PLACEHOLDER".into()));
    apply(&mut s, FieldUpdate::OfferAmount(1_000_0000000));
    assert!(!try_finalize(&s), "not finalized until both amounts arrive");
    apply(&mut s, FieldUpdate::ReturnAmount(2_500_0000000));
    assert!(try_finalize(&s));
}

#[test]
fn finalize_emits_when_usdc_bought_out_of_pool() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken("CXLM_PLACEHOLDER".into()));
    apply(&mut s, FieldUpdate::BuyToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::OfferAmount(2_500_0000000));
    apply(&mut s, FieldUpdate::ReturnAmount(1_000_0000000));
    assert!(try_finalize(&s));
}

#[test]
fn finalize_skips_non_usdc_swap() {
    // Defensive: if the trigger ever gets pointed at a non-USDC pool,
    // we shouldn't emit a Rebalance for it.
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken("CFOO".into()));
    apply(&mut s, FieldUpdate::BuyToken("CBAR".into()));
    apply(&mut s, FieldUpdate::OfferAmount(100));
    apply(&mut s, FieldUpdate::ReturnAmount(99));
    assert!(!try_finalize(&s));
}

#[test]
fn finalize_skips_until_all_fields_present() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    assert!(!try_finalize(&s));
    apply(&mut s, FieldUpdate::BuyToken("CXLM".into()));
    assert!(!try_finalize(&s));
    apply(&mut s, FieldUpdate::OfferAmount(1));
    assert!(!try_finalize(&s));
    apply(&mut s, FieldUpdate::ReturnAmount(1));
    assert!(try_finalize(&s));
}

#[test]
fn finalize_does_not_depend_on_amount_magnitude() {
    // Dust swaps still tick — handler decides whether the drift is large
    // enough to actually act, and applies the min_total_usdc floor too.
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::BuyToken("CXLM".into()));
    apply(&mut s, FieldUpdate::OfferAmount(1));
    apply(&mut s, FieldUpdate::ReturnAmount(1));
    assert!(try_finalize(&s));
}

#[test]
fn decode_real_phoenix_event_shape_forward() {
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

    assert!(try_finalize(&state));
}

#[test]
fn decode_real_phoenix_event_shape_reverse() {
    let xlm = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
    let usdc = USDC_SAC_CONTRACT_ID;

    let events = [
        (string_topic("swap"), string_topic("sell_token"), addr_value(xlm)),
        (string_topic("swap"), string_topic("offer_amount"), i128_value(2_500_0000000)),
        (string_topic("swap"), string_topic("buy_token"), addr_value(usdc)),
        (string_topic("swap"), string_topic("return_amount"), i128_value(1_000_0000000)),
    ];

    let mut state = SwapState::default();
    for (t0, t1, v) in &events {
        let update = decode_field(&[t0.clone(), t1.clone()], v).unwrap();
        apply(&mut state, update);
    }

    assert!(try_finalize(&state));
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
