mod payload;
mod phoenix;
mod state;

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
    // Cheap topic filter first. The node wakes us on every event from the
    // pool (rest-wildcard on the trigger), so most invocations are not
    // ones we care about (delegate-emitted events from our own actions,
    // admin events, etc.).
    if !phoenix::is_relevant_event(&event.topic_segments)? {
        return Ok(vec![]);
    }

    // tx_hash:op_index is the canonical dedup unit. Phoenix fires many
    // events per logical pool action (swap, provide_liquidity,
    // withdraw_liquidity), all sharing this id. The CAS tombstone gates
    // emission to exactly one Rebalance tick per logical action.
    let key = format!(
        "{}:{}",
        event.transaction_hash,
        event.operation_index.unwrap_or(0)
    );

    if !state::mark_if_unseen(&key)? {
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
