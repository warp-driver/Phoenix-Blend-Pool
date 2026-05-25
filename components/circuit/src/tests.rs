use crate::payload::{self, Direction};
use crate::phoenix::{
    apply, decode_field, try_finalize, FieldUpdate, RebalanceEmit, SwapState, USDC_SAC_CONTRACT_ID,
};
use std::str::FromStr;
use stellar_xdr::curr::{
    Int128Parts, Limits, ReadXdr, ScAddress, ScString, ScSymbol, ScVal, StringM, WriteXdr,
};

fn decode_envelope_payload(bytes: &[u8]) -> (String, Option<i128>) {
    let decoded = ScVal::from_xdr(bytes, Limits::none()).unwrap();
    let elements = match decoded {
        ScVal::Vec(Some(v)) => v.0.to_vec(),
        other => panic!("expected ScVec, got {other:?}"),
    };
    assert!(
        elements.len() == 1 || elements.len() == 2,
        "enum payload is [tag] or [tag, amount]"
    );

    let tag = match &elements[0] {
        ScVal::Symbol(ScSymbol(s)) => s.to_string(),
        other => panic!("expected tag Symbol, got {other:?}"),
    };
    let amount = if elements.len() == 2 {
        match &elements[1] {
            ScVal::I128(Int128Parts { hi, lo }) => {
                Some(((*hi as i128) << 64) | (*lo as u128 as i128))
            }
            other => panic!("expected I128, got {other:?}"),
        }
    } else {
        None
    };
    (tag, amount)
}

#[test]
fn payload_encodes_to_blend_variant() {
    let amount: i128 = 1_000_0000000;
    let bytes = payload::encode(Direction::ToBlend, amount).unwrap();
    let (tag, decoded_amount) = decode_envelope_payload(&bytes);
    assert_eq!(tag, "ToBlend");
    assert_eq!(decoded_amount, Some(amount));
}

#[test]
fn payload_encodes_from_blend_variant() {
    let amount: i128 = 250_0000000;
    let bytes = payload::encode(Direction::FromBlend, amount).unwrap();
    let (tag, decoded_amount) = decode_envelope_payload(&bytes);
    assert_eq!(tag, "FromBlend");
    assert_eq!(decoded_amount, Some(amount));
}

#[test]
fn payload_encodes_harvest_yield_unit_variant() {
    // HarvestYield carries no data; encoding should emit just [Symbol].
    let bytes = payload::encode(Direction::HarvestYield, 0).unwrap();
    let (tag, decoded_amount) = decode_envelope_payload(&bytes);
    assert_eq!(tag, "HarvestYield");
    assert_eq!(decoded_amount, None);
}

#[test]
fn payload_roundtrips_negative_amounts() {
    // Production reject negatives at the handler; encoder still produces
    // well-formed XDR (we don't want silent corruption).
    let bytes = payload::encode(Direction::ToBlend, -1).unwrap();
    let (tag, decoded) = decode_envelope_payload(&bytes);
    assert_eq!(tag, "ToBlend");
    assert_eq!(decoded, Some(-1));
}

#[test]
fn finalize_emits_to_blend_on_usdc_sold_into_pool() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::BuyToken("CXLM_PLACEHOLDER".into()));
    apply(&mut s, FieldUpdate::OfferAmount(1_000_0000000));
    assert!(try_finalize(&s).is_none(), "not finalized until both amounts arrive");
    apply(&mut s, FieldUpdate::ReturnAmount(2_500_0000000));
    assert_eq!(try_finalize(&s), Some(RebalanceEmit::ToBlend(100_0000000)));
}

#[test]
fn finalize_emits_from_blend_on_usdc_bought_out_of_pool() {
    let mut s = SwapState::default();
    apply(&mut s, FieldUpdate::SellToken("CXLM_PLACEHOLDER".into()));
    apply(&mut s, FieldUpdate::BuyToken(USDC_SAC_CONTRACT_ID.into()));
    apply(&mut s, FieldUpdate::OfferAmount(2_500_0000000));
    apply(&mut s, FieldUpdate::ReturnAmount(1_000_0000000));
    assert_eq!(try_finalize(&s), Some(RebalanceEmit::FromBlend(100_0000000)));
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

    assert_eq!(try_finalize(&state), Some(RebalanceEmit::ToBlend(100_0000000)));
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

    assert_eq!(try_finalize(&state), Some(RebalanceEmit::FromBlend(100_0000000)));
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
