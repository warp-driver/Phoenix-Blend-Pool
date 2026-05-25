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
    let contract = stellar_strkey::Contract::from_string(&handler)
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

export!(Component);
