# phoenix-blend-display

Live monitor for the testnet deployment. Pure static HTML + CSS + JS, no
build step, no backend — drops anywhere a static file server can run.
Polls Soroban RPC every 10 seconds and renders the full state of the
end-to-end pipeline.

```
┌── header: net badge · live dot · last tick ─────────────────────────┐
├──────────────────────────────────┬──────────────────────────────────┤
│ Deployer wallet                  │ Drift indicator                  │
│   XLM / USDC / BLND balances     │   liquid/total vs target±band    │
│                                  │   predicted next Rebalance       │
├──────────────────────────────────┼──────────────────────────────────┤
│ Phoenix pool state               │ Blend position                   │
│   liquid / delegated / total     │   principal, BLND, status,       │
│   per token, % liquid            │   policy, last rebalance/harvest │
├──────────────────────────────────┴──────────────────────────────────┤
│ Pool events  (trigger sources — swap / provide / withdraw / donate) │
├─────────────────────────────────────────────────────────────────────┤
│ Handler events  (action outcomes — RebalanceExecuted, Harvest…)     │
└─────────────────────────────────────────────────────────────────────┘
```

Matrix theme: phosphor green on black, monospace, soft glow, CRT
scanline overlay. Per-event accent colours (red for BadDebt, amber for
Pause/Emergency, dim for ConfigUpdated/Verified).

## What each panel verifies in the testnet bring-up

| Panel | Confirms |
|-------|----------|
| **Deployer wallet** | testnet keys still hold XLM/USDC/BLND to drive the flow; if balances drift unexpectedly, something is moving funds |
| **Drift indicator** | the policy logic on the handler (target/band/floor/cap) is reading the right pool state; the predicted action panel mirrors what the on-chain handler would do if a Rebalance fired right now |
| **Phoenix pool state** | the pool exists, `set_delegate` ran, reserves are tracked correctly across liquid/delegated splits |
| **Blend position** | handler's `principal_supplied` matches what's parked in Blend; Blend pool status is healthy enough to keep moving |
| **Pool events** | step 1 of the WarpDrive pipeline — chain events from the blended pool that trigger the off-chain circuit |
| **Handler events** | step 5 of the pipeline — the handler actually dispatched (`RebalanceExecuted`) or harvested (`HarvestCompleted`) in response. Pair with pool-event timestamps to verify the off-chain operator network closes the loop |

If both pool events AND handler events appear in time-order
(`provide_liquidity` at T0 → `RebalanceExecuted` at T0+Δ), the whole
flow works end-to-end.

## Run

1. Copy the template and fill in your testnet addresses:
   ```bash
   cd phoenix-blend-pool/frontend
   cp config.example.json config.json
   ```

   Required edits (everything else has sensible Stellar-testnet defaults):

   | Key                 | From                                                |
   |---------------------|------------------------------------------------------|
   | `handler_id`        | `out/handler.json` → `.automation_handler`           |
   | `blended_pool_id`   | `out/testnet/pool.json` → `.blended_pool`            |
   | `source_account`    | any funded testnet G-address (your `DEPLOYER_ADDRESS`) |
   | `deployer_address`  | the wallet you want balances tracked for (usually `DEPLOYER_ADDRESS`) |

2. Serve the directory over HTTP (file:// blocks `fetch` for the
   config):
   ```bash
   python3 -m http.server 8080
   # or: npx serve -l 8080
   # or: caddy file-server --root . --listen :8080
   ```

3. Open `http://localhost:8080/`. First tick lands in a few seconds.

## Drift indicator semantics

- The **horizontal bar** spans 0%-100% liquid USDC share.
- The **bright green line** marks `target_ratio_bps` (default 50%).
- The **shaded green band** spans `target ± rebalance_band_bps`
  (default ±5% → 45%-55%).
- The **circle marker** is the current `liquid_usdc / total_usdc` ratio:
  green inside the band, amber outside, red if `total_usdc` is below
  `min_total_usdc` (handler treats this as below-floor).
- The **"predicted next Rebalance"** line below the bar shows exactly
  what the handler would do if a Rebalance fired right now, computed in
  JS from the same logic as `execute_rebalance` in
  `contracts/automation-handler/src/contract.rs`. Possible outcomes:

  | Verdict | When |
  |---------|------|
  | `ToBlend N USDC` | liquid > upper band — pull `N` from pool, supply to Blend |
  | `FromBlend N USDC` | liquid < lower band — withdraw `N` from Blend, return to pool |
  | `no-op (within band)` | liquid sits inside the band |
  | `no-op (below floor)` | total USDC < `min_total_usdc` |
  | `no-op (dust)` | natural amount < `min_rebalance_amount` |
  | `no-op (no principal)` | would top up but `principal_supplied` is 0 |
  | `blocked (paused)` | `paused == true` |
  | `blocked (Blend unhealthy)` | Blend pool status > 3 |

## Pool events vs handler events

- **Pool events** are emitted by `phoenix-pool-blended` itself. The
  WarpDrive circuit subscribes to `swap` / `provide_liquidity` /
  `withdraw_liquidity` and emits a `Rebalance` payload for each. Admin
  paths (`set_delegate`, `withdraw_to_delegate`, `deposit_from_delegate`,
  `donate`) also show up here for full visibility.
- **Handler events** are emitted by the automation handler after
  `verify_xlm` runs through the quorum-signed envelope:
  - `Verified { event_id }` — every successful envelope
  - `RebalanceExecuted { direction, amount, liquid_after, delegated_after, principal_after }`
  - `HarvestCompleted { interest_donated, blnd_routed, principal_after }`
  - `BadDebtDetected { previous_principal, redeemable, shortfall }`
  - `PauseToggled { paused }` / `EmergencyUnwound { redeemed, principal_before }`
  - `ConfigUpdated` / `AddressConfigUpdated` (admin retunes)
  - `ContractUpgraded { version }` (on `upgrade()` calls)

A working testnet looks like: pool event at ledger N, handler event at
ledger ~N+3 to N+10 (gap is the operator round-trip).

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `config.json fetch failed` | you opened `index.html` over `file://` — serve via HTTP |
| `source_account not found on chain` | the address in `config.json` doesn't exist; pick one that's been funded by friendbot |
| `simulate failed: Contract not found` | wrong C-address in `handler_id` or `blended_pool_id` |
| `BLND in treasury` empty | handler hasn't run harvest yet — wait for the next cron tick or trigger one |
| pool events panel empty | nothing has touched the pool yet (no `provide_liquidity` / swaps / etc.) — run `task testnet:seed-pool` |
| handler events panel empty after pool events arrive | off-chain operator network isn't dispatching. Check the warpdrive node logs |
| drift marker stuck below the band | `principal_supplied == 0`, so the FromBlend leg has nothing to pull. The first Rebalance tick after the pool is seeded will route ToBlend (excess liquid → Blend) and start building principal; trigger one by running `task testnet:seed-pool` or any swap |
| `getEvents` 400 with `startLedger out of range` | bump `event_lookback_ledgers` down or wait for testnet to advance |

## Mainnet deploy

Same flow, swap the config:

```json
{
  "rpc_url":            "https://mainnet.sorobanrpc.com",
  "network_passphrase": "Public Global Stellar Network ; September 2015",
  "horizon_url":        "https://horizon.stellar.org",
  "stellar_expert_base":"https://stellar.expert/explorer/public",
  ...
}
```

For a publicly visible dashboard, drop the directory behind Caddy with
HTTPS (see `../../hodlers-display/Caddyfile`) — but it's fully
client-side, so GitHub Pages / Cloudflare Pages / S3 work too.
