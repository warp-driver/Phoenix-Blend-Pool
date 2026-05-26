# phoenix-blend-pool

WarpDrive-driven rebalance automation for the Phoenix XLM-USDC "blended" pool variant (see `../phoenix-contracts/contracts/pool_blended/`). Mirrors the layout of `../hodlers-app/`.

## What it does

The circuit (`components/circuit/`) subscribes to all events from the blended pool (rest-wildcard trigger). The blended-pool fork emits exactly one event per logical action; the circuit filters topic[0] to the three families that move the pool's liquid USDC ratio - `swap`, `provide_liquidity`, `withdraw_liquidity` - and emits one `Rebalance` tick per match. The payload is a bare unit variant: no amount, no direction. No circuit-side KV state is kept; the handler is the source of truth and dedupes on-chain by `event_id`.

WarpDrive operators sign the envelope; the **aggregator** (`components/aggregator/`) submits it once quorum is reached. The **automation-handler** (`contracts/automation-handler/`) on Stellar verifies the quorum signature via the vendored `ed25519-verification` contract, dedupes by `event_id`, then runs one of two actions:

- **`Rebalance`** - reads the blended pool's `query_delegate_state` and computes:
    - `liquid_usdc` = pool's actual on-chain USDC balance (`state.liquid_a` or `state.liquid_b` depending on which side USDC sorts to)
    - `total_usdc` = `liquid + delegated_out_usdc` (the delegated portion is the principal currently parked in Blend earning interest - it is "virtually in the pool", and the Phoenix pool's reserve counter already reflects it because `withdraw_to_delegate` does not decrement reserves)
    - `target_liquid` = `total_usdc * target_ratio_bps / 10_000` (default 50%)
    - `band` = `total_usdc * rebalance_band_bps / 10_000` (default +/-5%)

    If `total_usdc < min_total_usdc` (default 10_000 USDC at 7 decimals), the action is a no-op (event still marked seen). Otherwise:
    - If `liquid_usdc > target_liquid + band`: `withdraw_to_delegate(USDC, liquid_usdc - target_liquid)` then Blend `submit(Supply, ...)`. `principal_supplied` increases by the same amount.
    - If `liquid_usdc < target_liquid - band`: Blend `submit(Withdraw, amount)` (capped at `principal_supplied` to guard against bad-debt write-downs) then `deposit_from_delegate(USDC, amount)`. `principal_supplied` decreases.
    - Inside the band: no-op.

    XLM never moves. Spec calls for 50% of *USDC* in Blend; the XLM side stays fully liquid in the Phoenix pool.

- **`HarvestYield`** (cron-triggered) - pulls accrued yield from both Blend sources and donates it pro-rata to LPs:
    1. `Blend.claim(...)` for BLND emissions on the USDC supply position.
    2. If BLND received, swap BLND -> USDC on the configured BLND-USDC pool.
    3. `Blend.submit(Withdraw, USDC, i128::MAX)` to pull everything (principal + interest), then re-supply `principal_supplied` to restore the position. The leftover USDC is exactly the accrued interest delta.
    4. `blended_pool.donate(USDC, total_yield)` distributes the combined (BLND-swap-proceeds + interest) to LP holders without minting LP tokens.

    `principal_supplied` is tracked on the handler across the Rebalance lifecycle so HarvestYield can isolate just the interest portion without ever reading the b-rate.

The handler is the address configured as the blended pool's delegate via `set_delegate(...)`.

The service deploys **two workflows** sharing the same circuit + aggregator wasms:

- **`rebalance`** - Stellar-event trigger on every event from the blended pool. Circuit filters and emits one `Rebalance` tick per relevant pool action.
- **`harvest`** - cron trigger (default `"0 0 0,4,8,12,16,20 * * *"` = top of every 4 hours). Emits `HarvestYield`. Override `HARVEST_SCHEDULE` to tune.

## What it does NOT do (yet)

- Cooldown between rebalances (currently fires on every relevant pool action whose drift breaches the band).
- Multi-operator deploy beyond a single dev node.
- Integration tests against mocked Blend / blended pool (placeholder stub in `contracts/automation-handler/src/tests.rs`).

## Layout

```
phoenix-blend-pool/
├── contracts/
│   ├── automation-handler/   ← quorum-verified dispatcher
│   ├── ed25519-security/     ← vendored from warpdrive-contracts
│   └── ed25519-verification/ ← vendored from warpdrive-contracts
├── components/
│   ├── circuit/              ← WASI 0.2: watches pool, emits Rebalance ticks
│   └── aggregator/           ← trivial Stellar SubmitAction emitter
├── wit-definitions/wit/      ← warpdrive WIT worlds
├── service/                  ← service.json (generated)
├── warpdrive.toml            ← node config
├── Taskfile.yml              ← deploy + build surface
└── rust-toolchain.toml       ← Rust 1.95 + wasm32-wasip1 + wasm32v1-none
```

## Build

```
task build-contracts     # security, verification, automation-handler
task build-circuit
task build-aggregator
```

See `Taskfile.yml` for the full surface (deploy-middleware, deploy-handler, upload-component, build-service, run-node, ...). The deploy flow mirrors `../hodlers-app/DEPLOY.md`.
