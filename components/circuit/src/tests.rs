use crate::payload::{self, Direction};
use crate::phoenix::is_relevant_event;
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
fn filter_passes_swap_event() {
    let topic = vec![sym_topic("swap"), sym_topic("sender")];
    assert!(is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_passes_provide_liquidity_event() {
    let topic = vec![sym_topic("provide_liquidity"), sym_topic("token_a-amount")];
    assert!(is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_passes_withdraw_liquidity_event() {
    let topic = vec![sym_topic("withdraw_liquidity"), sym_topic("return_amount_a")];
    assert!(is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_passes_string_topic0() {
    // Phoenix mixes String and Symbol for topic[0] across entrypoints; the
    // filter must accept either encoding.
    let topic = vec![string_topic("swap"), string_topic("offer_amount")];
    assert!(is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_rejects_delegate_events() {
    // blend_pool/* events are emitted by the pool when the handler itself
    // moves USDC. Re-triggering off them would loop.
    for tag in ["blend_pool", "XYK Pool: ", "transfer", "burn", "mint"] {
        let topic = vec![sym_topic(tag), sym_topic("anything")];
        assert!(
            !is_relevant_event(&topic).unwrap(),
            "topic[0]={tag} must not trigger Rebalance"
        );
    }
}

#[test]
fn filter_rejects_empty_topic() {
    let topic: Vec<String> = Vec::new();
    assert!(!is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_rejects_non_string_symbol_topic() {
    // Address or i128 as topic[0] is not a Phoenix-emitted shape; reject it.
    let topic = vec![
        i128_value(42),
        sym_topic("anything"),
    ];
    assert!(!is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_with_real_phoenix_swap_event_shape() {
    // Matches the actual XDR-base64 wire shape produced by Phoenix.
    let topic = vec![string_topic("swap"), string_topic("sell_token")];
    assert!(is_relevant_event(&topic).unwrap());
}

#[test]
fn filter_with_real_phoenix_withdraw_event_shape() {
    let topic = vec![
        string_topic("withdraw_liquidity"),
        string_topic("return_amount_a"),
    ];
    assert!(is_relevant_event(&topic).unwrap());
}

/// String-encoded topic segment (Phoenix uses this for entrypoint emissions).
fn string_topic(s: &str) -> String {
    let inner: StringM = s.try_into().unwrap();
    ScVal::String(ScString(inner))
        .to_xdr_base64(Limits::none())
        .unwrap()
}

/// Symbol-encoded topic segment (also legal for Soroban events).
fn sym_topic(s: &str) -> String {
    let inner: StringM<32> = s.as_bytes().try_into().unwrap();
    ScVal::Symbol(ScSymbol(inner))
        .to_xdr_base64(Limits::none())
        .unwrap()
}

/// XDR-encoded i128 to verify the filter rejects non-string topics.
fn i128_value(n: i128) -> String {
    let hi = (n >> 64) as i64;
    let lo = n as u64;
    ScVal::I128(Int128Parts { hi, lo })
        .to_xdr_base64(Limits::none())
        .unwrap()
}

/// Address strkey to mirror a hostile-input scenario.
#[allow(dead_code)]
fn addr_value(strkey: &str) -> String {
    let addr = ScAddress::from_str(strkey).unwrap();
    ScVal::Address(addr).to_xdr_base64(Limits::none()).unwrap()
}
