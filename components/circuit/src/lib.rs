mod payload;
mod phoenix;

#[cfg(test)]
mod tests;

wit_bindgen::generate!({
    world: "circuit-world",
    path: "../../wit-definitions/wit",
    generate_all,
});

use warpdrive::vectr::input::TriggerData;

struct Component;

impl Guest for Component {
    fn run(trigger_action: TriggerAction) -> Result<Vec<WasmResponse>, String> {
        run_inner(trigger_action).map_err(|e| format!("{e:?}"))
    }
}

fn run_inner(trigger_action: TriggerAction) -> anyhow::Result<Vec<WasmResponse>> {
    match trigger_action.data {
        TriggerData::StellarContractEvent(e) => handle_pool_event(e.event),
        TriggerData::Cron(_) => Ok(vec![harvest_yield_response()?]),
        _ => anyhow::bail!("unexpected trigger type"),
    }
}

fn handle_pool_event(
    event: warpdrive::types::chain::StellarEvent,
) -> anyhow::Result<Vec<WasmResponse>> {
    // Filter topic[0]. The blended pool fork emits one event per logical
    // action (swap / provide_liquidity / withdraw_liquidity), each with a
    // unique tx_hash:op_index, so no per-tx dedup is needed at this layer.
    // The handler on-chain still dedupes by event_id as defence in depth.
    if !phoenix::is_relevant_event(&event.topic_segments)? {
        return Ok(vec![]);
    }

    let bytes = payload::encode(payload::Direction::Rebalance)?;
    Ok(vec![WasmResponse {
        payload: bytes,
        ordering: None,
        event_id_salt: None,
    }])
}

/// Cron-fired HarvestYield. No accumulator needed: every tick is a fresh
/// "harvest now" instruction, and the framework assigns a unique event_id
/// per cron firing so the handler dedupes naturally.
fn harvest_yield_response() -> anyhow::Result<WasmResponse> {
    let bytes = payload::encode(payload::Direction::HarvestYield)?;
    Ok(WasmResponse {
        payload: bytes,
        ordering: None,
        event_id_salt: None,
    })
}

export!(Component);
