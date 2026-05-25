# phoenix-blend-pool

WarpDrive-driven rebalance automation for the Phoenix XLM-USDC "blended" pool variant (see `../phoenix-contracts/contracts/pool_blended/`). Mirrors the layout of `../hodlers-app/`.

## What it does (v1)

1. The **circuit** (`components/circuit/`) subscribes to `swap` events on the blended pool. When the pool gains USDC (a trader sold USDC into it), the circuit emits a `RebalanceToBlend { amount_usdc }` payload sized at 10% of the inbound USDC.
2. WarpDrive operators sign the envelope; the **aggregator** (`components/aggregator/`) submits it once quorum is reached.
3. The **automation-handler** (`contracts/automation-handler/`) on Stellar:
   - Verifies the quorum signature via the vendored `ed25519-verification` contract.
   - Reads the blended pool's logical reserves via `query_delegate_state()`.
   - Withdraws `amount_usdc` USDC + the proportional XLM amount from the blended pool (via `withdraw_to_delegate`), keeping the new pool's physical XLM:USDC ratio steady.
   - Swaps the XLM leg through the legacy Phoenix XLM-USDC pool to obtain more USDC.
   - Supplies the combined USDC to a Blend USDC lending pool via `submit(Supply)`.

The handler is the address that gets configured as the blended pool's delegate via `set_delegate(...)`.

## What it does NOT do (yet)

- The reverse direction (pull from Blend back into the pool when liquid USDC runs low).
- BLND emission harvesting + swap-and-donate of yield.
- A real drift-vs-target trigger (the current v1 logic is "emit 10% of every USDC-in swap"; production should compare current liquid ratio against a 50% target).
- Multi-operator deploy beyond a single dev node.

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
