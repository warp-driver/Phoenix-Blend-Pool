wit_bindgen::generate!({
    world: "aggregator-world",
    path: "../../wit-definitions/wit",
    generate_all,
});

use anyhow::{anyhow, Context};

use warpdrive::aggregator::output::{StellarSubmitAction, SubmitAction};
use warpdrive::types::chain::StellarAddress;

struct Component;

impl Guest for Component {
    fn process_input(_input: AggregatorInput) -> Result<Vec<AggregatorAction>, String> {
        build().map_err(|e| format!("blend-rebalance-aggregator: {e:#}"))
    }

    fn handle_timer_callback(_input: AggregatorInput) -> Result<Vec<AggregatorAction>, String> {
        Ok(Vec::new())
    }

    fn handle_submit_callback(
        _input: AggregatorInput,
        _tx_result: Result<AnyTxHash, String>,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn build() -> anyhow::Result<Vec<AggregatorAction>> {
    let chain = host::config_var("chain").ok_or_else(|| anyhow!("missing config: chain"))?;
    let handler = host::config_var("service_handler")
        .ok_or_else(|| anyhow!("missing config: service_handler"))?;
    build_from_config(chain, &handler)
}

/// Pure (host-free) variant of `build()`. Takes the two config values
/// directly so unit tests can drive it without a WASI host. The split
/// keeps `build()` trivially a config-read + delegate.
fn build_from_config(chain: String, handler: &str) -> anyhow::Result<Vec<AggregatorAction>> {
    let contract = stellar_strkey::Contract::from_string(handler)
        .with_context(|| format!("invalid stellar contract id: {handler}"))?;
    Ok(vec![AggregatorAction::Submit(SubmitAction::Stellar(
        StellarSubmitAction {
            chain,
            address: StellarAddress {
                raw_bytes: contract.0.to_vec(),
            },
        },
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample testnet contract C-address (32 bytes of well-formed strkey).
    const TEST_HANDLER: &str = "CDVQVKOY2YSXS2IC7KN6MNASSHPAO7UN2UR2ON4OI2SKMFJNVAMDX6DP";

    #[test]
    fn build_from_config_emits_single_submit_action() {
        let actions = build_from_config("stellar:testnet".to_string(), TEST_HANDLER).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            AggregatorAction::Submit(SubmitAction::Stellar(s)) => {
                assert_eq!(s.chain, "stellar:testnet");
                assert_eq!(s.address.raw_bytes.len(), 32);
            }
            other => panic!("unexpected action variant: {:?}", other),
        }
    }

    #[test]
    fn build_from_config_rejects_invalid_strkey() {
        let err = build_from_config("stellar:pubnet".to_string(), "not-a-contract").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid stellar contract id"),
            "expected diagnostic about strkey; got: {msg}",
        );
    }

    #[test]
    fn build_from_config_passes_chain_through_verbatim() {
        let actions = build_from_config("stellar:pubnet".to_string(), TEST_HANDLER).unwrap();
        let AggregatorAction::Submit(SubmitAction::Stellar(s)) = &actions[0] else {
            panic!();
        };
        assert_eq!(s.chain, "stellar:pubnet");
    }

    #[test]
    fn address_bytes_match_strkey_decoding() {
        let actions = build_from_config("stellar:testnet".to_string(), TEST_HANDLER).unwrap();
        let AggregatorAction::Submit(SubmitAction::Stellar(s)) = &actions[0] else {
            panic!();
        };
        let expected = stellar_strkey::Contract::from_string(TEST_HANDLER).unwrap().0.to_vec();
        assert_eq!(s.address.raw_bytes, expected);
    }
}

export!(Component);
