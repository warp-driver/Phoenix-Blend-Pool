# Deploying phoenix-blend-pool

Two shapes, both terminating in the same on-chain state:

- **Single-operator dev deploy.** Everything on one host. Quorum 1-of-1. The
  fastest way to see the rebalance + harvest pipeline end-to-end against
  testnet (or a small mainnet pool with a tiny TVL cap).
- **Multi-operator production deploy.** 5 to 7 hosts, each running their own
  operator with their own BIP39 mnemonic. Quorum 4-of-5 or 5-of-7 on-chain.
  This is the target for real-value Phoenix x Blend automation.

Read **Prerequisites** first; both paths depend on the same setup.

---

## Prerequisites

### Host packages

Linux with a recent glibc. The following tools must be on `PATH`:

| Tool                    | Why                                                                 |
| ----------------------- | ------------------------------------------------------------------- |
| `curl`, `jq`, `python3` | Shell / build / IPC                                                 |
| `docker`                | Runs the `warpdrive-stellar-middleware` container                   |
| Rust 1.95 (via rustup)  | Pinned by `rust-toolchain.toml`                                     |
| `task` (go-task)        | Runs `Taskfile.yml` targets                                         |
| `wkg`                   | Fetches WIT deps for the WASI components                            |
| `cargo-component`       | Builds WASI 0.2 components (`circuit`, `aggregator`)                |
| `stellar` CLI           | Soroban contract deploys, key management, RPC simulations           |
| `warpdrive`             | The operator runtime (from `warp-driver/warpdrive/packages/warpdrive`) |
| `warpdrive-cli`         | Service registration, signer queries, component uploads             |
| Pinata account          | IPFS-pin `service.json` (multi-operator only); `https://app.pinata.cloud` |

Rust target setup once:

```bash
rustup target add wasm32-wasip1 wasm32v1-none
```

After `wkg` is installed, point it at the registry so it resolves the
`warpdrive` namespace WIT packages:

```bash
mkdir -p ~/.config/wasm-pkg
cat > ~/.config/wasm-pkg/config.toml <<'EOF'
default_registry = "wa.dev"

[namespace_registries]
warpdrive = "warg.wa.dev"
EOF
```

### Warpdrive fork patches

Each operator MUST run the local-forked `warpdrive` binary (this workspace's
`../warpdrive`), not stock upstream. The fork carries three patches without
which the deploy mis-behaves:

| File                                              | Without it                                                     |
| ------------------------------------------------- | -------------------------------------------------------------- |
| `packages/utils/src/health.rs` + `error.rs`       | Stellar health checks return `NotImplemented`; node spam-logs  |
| `packages/warpdrive/src/subsystems/aggregator/validate.rs` | Aggregator validate path doesn't log envelope/signature/pubkey on `UnreachableCodeReached`, making operator-network failures opaque |
| `packages/engine/src/backend/wasi_keyvalue/atomics.rs` | Stub CAS that always succeeded. (Phoenix-blend-pool's circuit does not use atomics, but other services in the same node would silently lose writes.) |

Build the fork once, on each operator host:

```bash
cd ../warpdrive
cargo install --path packages/warpdrive --locked
cargo install --path packages/cli --locked
```

### Environment variables

Each operator box exports the following before running any task:

| Variable                     | Set by                                | Notes                                                                                                                  |
| ---------------------------- | ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `DEPLOYER_SECRET`            | `stellar keys show <alias>`           | Funded S-secret on the target network. Used as `--source` for contract deploys AND as the aggregator Stellar credential. |
| `DEPLOYER_ADDRESS`           | `stellar keys address <alias>`        | G-address matching `DEPLOYER_SECRET`. Required by the warpdrive middleware Docker container.                           |
| `WARPDRIVE_SIGNING_MNEMONIC` | `stellar keys show <alias> --phrase`  | MUST be a BIP39 mnemonic, not a raw hex secret. The node HD-derives operator keys at non-zero indices.                 |
| `PINATA_JWT`                 | `https://app.pinata.cloud`            | Multi-op admin only. Used by `task publish-service` to pin `service/service.json` to IPFS.                             |
| `BLENDED_POOL`               | Phoenix team after pool deploy        | C-address of the new XLM-USDC blended pool. Placeholder by default.                                                    |
| `BLEND_POOL`                 | Blend deploy                          | C-address of the Blend USDC lending pool. Defaults to the V1 mainnet pool.                                             |
| `BLND_TREASURY`              | Coordinated with Phoenix/Blend        | C-address that receives BLND emissions claimed during HarvestYield. Placeholder by default. The handler never holds BLND.        |
| `USDC_RESERVE_TOKEN_ID`      | Blend pool config                     | b-token reserve id for USDC on the configured Blend pool. Default `1`; verify per deploy.                              |

Optional rebalance-policy overrides (defaults in `Taskfile.yml`):

| Variable                  | Default        | Meaning                                                              |
| ------------------------- | -------------- | -------------------------------------------------------------------- |
| `TARGET_RATIO_BPS`        | `5000`         | Target liquid-USDC share of total USDC, in bps. 5000 == 50%.         |
| `REBALANCE_BAND_BPS`      | `500`          | +/-band around target. 500 == 5%, so action band is [45%, 55%].      |
| `MIN_TOTAL_USDC`          | `100000000000` | 10_000 USDC at 7 decimals. Below this total, Rebalance is a no-op.   |
| `REBALANCE_COOLDOWN_SECS` | `300`          | Minimum seconds between two successive rebalance actions.            |
| `HARVEST_SCHEDULE`        | `0 0 0,4,8,12,16,20 * * *` | Cron expression. Top of every 4 hours.                   |

### Resolving production addresses

The four address-typed variables above are placeholders until Phoenix +
Blend confirm production deploys. This is the playbook for filling each
one in.

#### `BLENDED_POOL` — the new XLM-USDC blended pool

1. Phoenix deploys `phoenix-pool-blended` (the fork in
   `../phoenix-contracts/contracts/pool_blended/`) as a new XLM-USDC
   variant alongside the existing canonical pool.
2. Pool admin is initially Phoenix's deploy key; the long-term admin
   needs to be a multisig that controls `set_delegate` (and therefore the
   handler-rotation lifecycle).
3. The C-address Phoenix returns is `BLENDED_POOL`. Verify by querying:
   ```
   stellar contract invoke --id "$BLENDED_POOL" --network <net> \
       --source <any-funded-key> -- query_delegate_state
   ```
   The response should be a `DelegateState` ScMap with `delegate: None`
   (until `task set-delegate` runs) and `liquid_a/b == total_a/b`.

#### `BLEND_POOL` — the Blend USDC lending pool to target

1. Confirm with the Blend team which pool we point at:
   - V1 mainnet USDC pool: `CDVQVKOY2YSXS2IC7KN6MNASSHPAO7UN2UR2ON4OI2SKMFJNVAMDX6DP`
     (current default in `Taskfile.yml`).
   - A newer pool variant if Blend has rolled one out.
2. Verify the pool is healthy and lists USDC as a reserve:
   ```
   stellar contract invoke --id "$BLEND_POOL" --network mainnet \
       --source <any-funded-key> -- get_config
   ```
   `status` should be 0 or 1 (Active / Admin-Active). Any value > 3 is
   Frozen / Setup and the handler will treat it as do-not-touch — wait
   for Blend to flip status to Active before flipping `min_total_usdc`
   down.

#### `USDC_RESERVE_TOKEN_ID` — Blend's b-token id for the USDC reserve

1. Read the reserve list from the chosen Blend pool:
   ```
   stellar contract invoke --id "$BLEND_POOL" --network mainnet \
       --source <any-funded-key> -- get_reserve_list
   ```
   This returns `Vec<Address>` ordered by reserve index.
2. Find the index `i` whose address matches `USDC`.
3. `USDC_RESERVE_TOKEN_ID = i * 2 + 1` (b-token side; `i * 2` would be
   the d-token side, which we never claim against).
4. Sanity-check by inspecting the reserve:
   ```
   stellar contract invoke --id "$BLEND_POOL" --network mainnet \
       --source <any-funded-key> -- get_reserve --asset "$USDC"
   ```
   Confirm the response matches the USDC asset.

#### `BLND_TREASURY` — destination for claimed BLND emissions

1. Coordinate with Phoenix on the recipient. Typical choices:
   - Phoenix DAO multisig (BLND held for governance / treasury ops).
   - A Phoenix-controlled keeper that periodically swaps BLND→USDC and
     donates back to the pool. (This is out-of-scope for the handler.)
2. The address can be any Stellar C- or G-address. The handler does NOT
   call back into this address; it just hands BLND to Blend's `claim` as
   the `to` argument.
3. Verify the address is funded (G-addresses need an existing trustline
   for the BLND asset; SAC-typed C-addresses do not).

#### Confirming everything is wired

After `task deploy` + `task set-delegate`:

- `query_delegate_state` on the blended pool returns
  `delegate == automation_handler_address`.
- `automation_handler.admin()` returns the configured `HANDLER_ADMIN`
  (default `DEPLOYER_ADDRESS`).
- `automation_handler.blend_pool() == $BLEND_POOL`.
- `automation_handler.blnd_treasury() == $BLND_TREASURY`.
- `automation_handler.usdc_reserve_token_id() == $USDC_RESERVE_TOKEN_ID`.
- `automation_handler.principal_supplied() == 0` (no seeding yet).
- `automation_handler.paused() == false`.

Mismatches mean the deploy env was wrong; use the admin setters
(`set_blnd_treasury`, `set_usdc_reserve_token_id`, etc.) to correct
without re-deploying.

Persist them in a `.env` at the repo root and source before each session:

```bash
cd phoenix-blend-pool
stellar keys generate phoenix-blend-deployer --fund --network mainnet
stellar keys generate warpdrive-operator
cat > .env <<EOF
DEPLOYER_SECRET=$(stellar keys show phoenix-blend-deployer)
DEPLOYER_ADDRESS=$(stellar keys address phoenix-blend-deployer)
WARPDRIVE_SIGNING_MNEMONIC="$(stellar keys show warpdrive-operator --phrase)"
BLENDED_POOL=C...     # fill in from Phoenix
BLND_TREASURY=C...    # fill in (DAO multisig, treasury contract, ...)
EOF
set -a; source .env; set +a
```

### Firewall (multi-operator only)

For each operator box:

| Port | Protocol | Source                          | Purpose                                                |
| ---- | -------- | ------------------------------- | ------------------------------------------------------ |
| 22   | TCP      | any                             | SSH                                                    |
| 9000 | TCP      | any                             | libp2p P2P between operators                           |
| 8000 | TCP      | restricted (or kept internal)   | warpdrive node HTTP API; open only for a dashboard etc |
| -    | ICMP     | any                             | Ping diagnostics                                       |

If you expose `:8000` to any host other than `127.0.0.1`, edit
`warpdrive.toml` and set `host = "0.0.0.0"` under `[warpdrive]`.

### Network targets

`warpdrive.toml` ships with two Stellar chains pre-wired:

- `stellar:pubnet`: source-of-truth events (the blended pool's swap /
  provide_liquidity / withdraw_liquidity events).
- `stellar:testnet`: where the automation-handler lives during the testnet
  shadow phase. For mainnet launch this changes to `pubnet` for both.

Override via `TRIGGER_CHAIN` and `MANAGER_CHAIN` when running tasks.

---

## Single-operator dev deploy

All on one box. ~10 commands. Quorum 1-of-1; service registered via the
node's `/dev/services` HTTP endpoint (no IPFS).

### 1. Build + deploy on-chain contracts

```bash
cd phoenix-blend-pool
task fetch-wit                 # one-time
task deploy                    # builds + deploys middleware + handler
```

After this completes:
- `out/deploy.json` - middleware-produced manifest with `ed25519_security`,
  `ed25519_verification`, `project_root`.
- `out/handler.json` - automation-handler C-address.

### 2. Wire the handler as the blended pool's delegate

The Phoenix team OR the deployer (depending on pool admin) calls
`set_delegate(handler_c_address)` on the blended pool:

```bash
HANDLER=$(jq -r .automation_handler out/handler.json)
stellar contract invoke \
  --id "$BLENDED_POOL" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source "$POOL_ADMIN_SECRET" \
  -- set_delegate --delegate "$HANDLER"
```

If `$POOL_ADMIN_SECRET` is not the same as `$DEPLOYER_SECRET`, this step
runs from a different box. Coordinate with whoever holds the pool admin key.

### 3. Start the operator node

In a second terminal:

```bash
cd phoenix-blend-pool
set -a; source .env; set +a
task run-node
```

Wait for both:

```
INFO Stellar chain [stellar:pubnet]  is healthy
INFO Stellar chain [stellar:testnet] is healthy
```

Leave the node running.

### 4. Upload components + register the service

Back in the first terminal:

```bash
task wire-service
```

That runs `upload-component` + `upload-aggregator` against the local node
(produces `out/circuit.digest`, `out/aggregator.digest`), assembles
`service/service.json` with both workflows (`rebalance` + `harvest`), and
POSTs to `http://127.0.0.1:8000/dev/services`.

The node log should show `Adding service: phoenix-blend-pool`,
`services=1, workflows=2, components=2`, then `StartListeningChain` for
`stellar:pubnet` and `StartCronTrigger` for the harvest workflow.

### 5. Register the operator's signing key on-chain

```bash
task register-signer
```

Defaults to threshold 1/1, weight 100. Adequate for single-op.

### 6. Verify end-to-end

Trigger any swap, provide_liquidity, or withdraw_liquidity on the blended
pool. The node log should show:

1. Stellar event delivered with `topic[0]=swap` (or similar).
2. Circuit emit: `payload_size=4` (the unit-variant Rebalance tag).
3. Aggregator sign + submit to handler.
4. Handler verify + dispatch. Either a Blend `Supply` / `Withdraw` (if drift
   broke the band and cooldown was met) or no-op.

Cross-check by reading handler state:

```bash
HANDLER=$(jq -r .automation_handler out/handler.json)
stellar contract invoke --id "$HANDLER" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source "$DEPLOYER_SECRET" \
  -- principal_supplied
stellar contract invoke --id "$HANDLER" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source "$DEPLOYER_SECRET" \
  -- last_rebalance_ts
```

Cron-driven HarvestYield fires per `HARVEST_SCHEDULE`. Watching for it in
real time means sitting through up to 4 hours. To force one early:

```bash
# (Stop node, set HARVEST_SCHEDULE='*/2 * * * * *' for every 2 seconds,
#  restart node, observe, then restore.)
```

---

## Multi-operator production deploy

Same on-chain contracts as the single-op path, but now N operator hosts
cooperate over libp2p, signatures are aggregated to a quorum threshold
(4-of-5 or 5-of-7), and `service.json` is pinned to IPFS and surfaced via
`project_root.service_uri()`.

The walkthrough below assumes 5 operators. Generalising to 7 is the same
shape with `THRESHOLD_NUM=5 THRESHOLD_DEN=7`.

### 1. Admin box - deploy on-chain contracts once

```bash
cd phoenix-blend-pool
task fetch-wit
task deploy
```

Distribute the resulting JSON files to every operator so they all see the
same contract addresses:

```bash
for host in op-1 op-2 op-3 op-4 op-5; do
  scp out/{deploy,handler}.json $host:~/phoenix-blend-pool/out/
done
```

### 2. Each operator box - clone, bootstrap secrets

On every operator:

```bash
git clone <repo-url> phoenix-blend-pool
cd phoenix-blend-pool
task fetch-wit

stellar keys generate phoenix-blend-deployer --fund --network mainnet
stellar keys generate warpdrive-operator
cat > .env <<EOF
DEPLOYER_SECRET=$(stellar keys show phoenix-blend-deployer)
DEPLOYER_ADDRESS=$(stellar keys address phoenix-blend-deployer)
WARPDRIVE_SIGNING_MNEMONIC="$(stellar keys show warpdrive-operator --phrase)"
BLENDED_POOL=C...
BLND_TREASURY=C...
EOF
```

Edit `warpdrive.toml` and set `host = "0.0.0.0"` under `[warpdrive]`.

### 3. Bootstrap operator (op-1) - start node to learn its peer_id

On op-1, edit `warpdrive.toml` so the `[warpdrive.p2p.remote]` block has
`bootstrap_nodes = []`. Then:

```bash
set -a; source .env; set +a
task run-node
```

Find the log line:

```
INFO Using P2P identity derived from signing_mnemonic (peer_id: 12D3KooW...)
```

That value is op-1's libp2p PeerId. It is deterministic (derived from
`WARPDRIVE_SIGNING_MNEMONIC` at HD path `m/44'/60'/0'/0/0`).

Keep the node running.

### 4. Other operators - point at op-1's multiaddr, start

On op-2 through op-5, edit `warpdrive.toml` so the `[warpdrive.p2p.remote]`
block contains:

```toml
[warpdrive.p2p.remote]
listen_port = 9000
bootstrap_nodes = [
  "/ip4/<op-1-public-ip>/tcp/9000/p2p/<op-1-peer-id>",
]
```

Then start each:

```bash
set -a; source .env; set +a
task run-node
```

All five nodes should now show each other in the libp2p logs.

### 5. Admin box - pin service.json + register on-chain

Once op-1 is running and reachable, the admin box assembles and pins the
service spec to IPFS, then registers the IPFS URI on `project_root`:

```bash
task wire-service       # builds + uploads components + assembles service.json
task publish-service    # pins service.json to IPFS, then set_service_spec
```

`out/service.cid` holds the IPFS CID. Every operator node polling
`project_root.service_uri()` auto-discovers the spec from the configured
`ipfs_gateway`.

### 6. Each operator - register manager + register signer

On every operator (including op-1):

```bash
task register-manager     # tells local node to watch project_root
task fetch-signer         # asks node for HD-derived ed25519 pubkey
task register-signer      # registers pubkey on the security contract
```

### 7. Admin box - set the production threshold

After ALL operator signers are registered:

```bash
THRESHOLD_NUM=4 THRESHOLD_DEN=5 task set-threshold
```

This sets 4-of-5 quorum on the security contract. For 7 operators, use
`THRESHOLD_NUM=5 THRESHOLD_DEN=7`.

### 8. Wire the handler as the blended pool's delegate

Same as single-op step 2. The Phoenix pool admin calls
`set_delegate(handler_c_address)`.

### 9. Verify end-to-end

Trigger a real swap or LP event on the blended pool. The libp2p gossip
should converge inside ~5 seconds; the aggregator on whichever operator
wins the race submits the signed envelope to the handler.

Sanity reads (any operator's local node):

```bash
HANDLER=$(jq -r .automation_handler out/handler.json)
stellar contract invoke --id "$HANDLER" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source "$DEPLOYER_SECRET" \
  -- principal_supplied

stellar contract invoke --id "$BLENDED_POOL" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source "$DEPLOYER_SECRET" \
  -- query_delegate_state
```

The `liquid_*` vs `delegated_*` fields should reconcile within +/-band of
the configured target ratio after each rebalance fires.

---

## Phased rollout (recommended)

Per `PHOENIX_BLEND_AUTOMATION.md`:

1. **Testnet shadow.** Deploy full stack against testnet Phoenix + testnet
   Blend (or mocked Blend). Run 1-2 weeks with synthetic swap traffic.
   Verify cooldown gating, harvest yield correctness, drift convergence,
   and Blend bad-debt recovery (simulate by adjusting `set_redeemable`-
   equivalent on a test Blend deploy).
2. **Mainnet dry-run.** Deploy contracts to mainnet but with a small TVL
   cap (e.g. `min_total_usdc` set to a value above the actual pool TVL so
   automation is a no-op). Run for 48-72 hours with opt-in beta LPs.
3. **Mainnet launch.** Phoenix flips routing to the new pool. Lower
   `min_total_usdc` to its real-deployment value, migrate incentives.

---

## Operational guidance

### Rebalance never fires

Possible causes, in order of likelihood:

1. Drift is inside the +/-band. Read `query_delegate_state`; if
   `liquid / total` is between `target - band / 10000` and `target + band /
   10000` no action fires. Working as designed.
2. Total USDC is below `min_total_usdc`. Same check, but compare against
   the configured floor.
3. Cooldown hasn't elapsed since the last action. Read `last_rebalance_ts`
   and compare against current ledger time.
4. The handler isn't the configured delegate. Read
   `query_delegate_state().delegate` and check it matches the handler
   C-address.
5. Operator quorum isn't being reached. Check libp2p health on each node
   and look for `signatures_collected=` lines in aggregator logs.

### Harvest fails repeatedly

If `principal_supplied` shrinks unexpectedly after a harvest, Blend has
written down the position. Check the Blend pool's status and
utilisation. Once Blend recovers, principal will not auto-restore;
manual operator intervention (or a subsequent ToBlend rebalance) is
needed to re-fund the position.

### Removing an operator

1. Admin box: `THRESHOLD_NUM=<new-num> THRESHOLD_DEN=<new-den> task set-threshold`
   (e.g. 3-of-4 if dropping from 4-of-5).
2. Admin box: invoke `Ed25519Security.remove_signer(pubkey)` for the
   leaving operator.
3. Stop the operator's warpdrive node.
4. Update remaining operators' `warpdrive.toml` `bootstrap_nodes` to remove
   any stale entries pointing at the dropped operator.

### Adding an operator

Reverse: new operator boots, `register-manager`, `fetch-signer`,
`register-signer`. Admin bumps threshold via `set-threshold`. Remaining
operators add the new multiaddr to their bootstrap list.
