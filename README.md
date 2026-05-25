# phoenix-blend-pool

WarpDrive-driven rebalance automation for the Phoenix XLM-USDC "blended" pool variant (see `../phoenix-contracts/contracts/pool_blended/`). Mirrors the layout of `../hodlers-app/`.

## What it does (v1)

The circuit (`components/circuit/`) subscribes to `swap` events on the blended pool. On each finalized swap, it inspects the direction and emits a `RebalanceAction` payload sized at 10% of the swap's USDC volume:

- **Pool gained USDC** (trader sold USDC in) → emit `ToBlend(amount_usdc)`.
- **Pool lost USDC** (trader bought USDC out) → emit `FromBlend(amount_usdc)`.

WarpDrive operators sign the envelope; the **aggregator** (`components/aggregator/`) submits it once quorum is reached. The **automation-handler** (`contracts/automation-handler/`) on Stellar verifies the quorum signature via the vendored `ed25519-verification` contract, dedupes by `event_id`, then dispatches one of three actions:

- **`ToBlend(amount_usdc)`** — withdraws `amount_usdc` USDC + the proportional XLM from the blended pool (via `withdraw_to_delegate`), keeping the new pool's physical XLM:USDC ratio steady. Swaps the XLM leg through the legacy Phoenix XLM-USDC pool. Supplies the combined USDC to the Blend lending pool via `submit(Supply)`. Increments `principal_supplied`.

- **`FromBlend(amount_usdc)`** — withdraws `amount_usdc` USDC from Blend via `submit(Withdraw)`. Splits half/half: deposits one half directly as USDC, swaps the other half on the legacy pool for XLM, then deposits both legs back into the blended pool via `deposit_from_delegate`. The pool's DelegatedOutA and DelegatedOutB both decrement. Decrements `principal_supplied`.

- **`HarvestYield`** (unit variant, no data) — pulls accrued yield from both Blend sources and donates it pro-rata to LPs:
    1. `Blend.claim(...)` for BLND emissions on the USDC supply position.
    2. If BLND received, swap BLND → USDC on the configured BLND-USDC pool.
    3. `Blend.submit(Withdraw, USDC, i128::MAX)` to pull everything (principal + interest), then re-supply `principal_supplied` to restore the position. The leftover USDC is exactly the accrued interest delta.
    4. `blended_pool.donate(USDC, total_yield)` distributes the combined (BLND-swap-proceeds + interest) to LP holders without minting LP tokens.

   `principal_supplied` is tracked on the handler across the ToBlend/FromBlend lifecycle so HarvestYield can isolate just the interest portion without ever reading the b-rate.

The handler is the address configured as the blended pool's delegate via `set_delegate(...)`.

## What it does NOT do (yet)

- **HarvestYield trigger.** The handler entrypoint exists and is callable, but the circuit doesn't emit `HarvestYield` automatically. Production would use a heartbeat trigger (every N hours) or a threshold trigger (when accumulated yield > X). For now it can be invoked manually with a hand-built envelope.
- A real drift-vs-target trigger for the rebalance actions (current v1 logic is "emit 10% of every USDC-touching swap"; production should compare current liquid ratio against a 50% target).
- Cooldown between rebalances (currently fires on every relevant swap).
- Multi-operator deploy beyond a single dev node.
- Integration tests against mocked Blend / legacy pool / blended pool (placeholder stub in `tests.rs`).

## Layout

```
phoenix-blend-pool/
├── contracts/
│   ├── automation-handler/   ← quorum-verified dispatcher
│   ├── ed25519-security/     ← vendored from warpdrive-contracts
│   └── ed25519-verification/ ← vendored from warpdrive-contracts
├── components/
│   ├── circuit/              ← WASI 0.2: watches pool, emits RebalanceToBlend
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
