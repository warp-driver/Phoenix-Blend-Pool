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
        TriggerData::StellarContractEvent(e) => handle_swap_event(e.event),
        TriggerData::Cron(_) => Ok(vec![harvest_yield_response()?]),
        _ => anyhow::bail!("unexpected trigger type"),
    }
}

fn handle_swap_event(
    event: warpdrive::types::chain::StellarEvent,
) -> anyhow::Result<Vec<WasmResponse>> {
    // tx_hash:op_index — same accumulator key Soroban uses for canonical
    // event_id derivation. We rely on it being unique per logical swap.
    let key = format!(
        "{}:{}",
        event.transaction_hash,
        event.operation_index.unwrap_or(0)
    );

    let update = phoenix::decode_field(&event.topic_segments, &event.value)?;

    // CAS-update the per-swap accumulator. Returns the rebalance emit only
    // if THIS invocation is the one that completed the SwapState and flipped
    // `finalized` true. wasi:keyvalue/atomics makes the read-modify-write
    // safe under the 8 concurrent events Phoenix fires per swap.
    let to_emit = state::update_with(&key, |s| {
        phoenix::apply(s, update.clone());
        if !s.finalized {
            if let Some(emit) = phoenix::try_finalize(s) {
                s.finalized = true;
                return Some(emit);
            }
        }
        None
    })?;

    if let Some(emit) = to_emit {
        let (direction, amount) = match emit {
            phoenix::RebalanceEmit::ToBlend(a) => (payload::Direction::ToBlend, a),
            phoenix::RebalanceEmit::FromBlend(a) => (payload::Direction::FromBlend, a),
        };
        let bytes = payload::encode(direction, amount)?;
        return Ok(vec![WasmResponse {
            payload: bytes,
            ordering: None,
            event_id_salt: None,
        }]);
    }

    Ok(vec![])
}

/// Cron-fired HarvestYield. No accumulator needed — every tick is a fresh
/// "harvest now" instruction, and the framework assigns a unique event_id
/// per cron firing so the handler dedupes naturally.
fn harvest_yield_response() -> anyhow::Result<WasmResponse> {
    let bytes = payload::encode(payload::Direction::HarvestYield, 0)?;
    Ok(WasmResponse {
        payload: bytes,
        ordering: None,
        event_id_salt: None,
    })
}

export!(Component);
