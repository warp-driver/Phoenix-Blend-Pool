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

| Port | Protocol | Source             | Purpose                                                                                  |
| ---- | -------- | ------------------ | ---------------------------------------------------------------------------------------- |
| 22   | TCP      | any                | SSH                                                                                      |
| 9000 | TCP      | peer-operator IPs  | libp2p P2P. **Closed for solo 1-of-1.** Open only to the other operators in multi-op.    |
| 8000 | TCP      | **none**           | warpdrive HTTP API. **Never expose.** The API is unauthenticated; reach it via the SSH tunnel (`task vps:tunnel`). |
| -    | ICMP     | any                | Ping diagnostics                                                                         |

`warpdrive.toml` ships with `host = "127.0.0.1"` so the HTTP API is
loopback-only. The `task vps:tunnel` SSH local-forward is how your
laptop reaches it during wire-up. Only flip `host` to `0.0.0.0` if you
genuinely need external HTTP access — and put an authenticating reverse
proxy in front when you do.

### Network targets

`warpdrive.toml` ships with two Stellar chains pre-wired:

- `stellar:pubnet`: source-of-truth events (the blended pool's swap /
  provide_liquidity / withdraw_liquidity events).
- `stellar:testnet`: where the automation-handler lives during the testnet
  shadow phase. For mainnet launch this changes to `pubnet` for both.

Override via `TRIGGER_CHAIN` and `MANAGER_CHAIN` when running tasks.

---

## Stellar testnet bring-up (turnkey)

The fastest path: a single `task testnet:full` deploys every contract,
wires the delegate, and verifies state. Targets the Blend "TestnetV2"
deploy and the Blend-issued test USDC SAC on Stellar testnet. Use this
for shadow runs in Phase A of the phased rollout, or as a smoke test
after refactoring.

### Pre-supplied testnet artefacts

Default `vars` in `taskfiles/testnet.yml`. Override via env if Blend
rotates an address.

| Var          | Value                                                        |
| ------------ | ------------------------------------------------------------ |
| `BLEND_POOL` | `CCEBVDYM32YNYCVNRXQKDFFPISJJCV557CDZEIRBEE4NCV4KHPQ44HGF`   |
| `USDC`       | `CAQCFVLOBK5GIULPNZRGATJJMIZL5BSP7X5YJVMGCPTUEPFM4AVSRCJU`   |
| `XLM`        | `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC`   |
| `BLND`       | `CB22KRA3YZVCNCQI64JQ5WE7UY2VAV7WFLK6A2JN3HEX56T2EDAFO7QF`   |

### What gets deployed

`task testnet:full` walks through:

1. `preflight` — checks `DEPLOYER_SECRET`/`DEPLOYER_ADDRESS`, `stellar`,
   `jq`, `cargo-component`, sibling `../phoenix-contracts` checkout.
2. `fund-deployer` — friendbot drip (idempotent; tolerates "already
   exists" if the account is funded).
3. `check-balances` — prints XLM / USDC / BLND balances, warns if below
   the floors the deploy needs.
4. `build-phoenix-wasms` — builds `phoenix_pool_blended`, `phoenix_stake`,
   `soroban_token_contract` in the sibling repo.
5. `upload-phoenix-wasms` — installs stake + token wasms on-chain;
   captures the 32-byte hashes into `out/testnet/phoenix-wasms.json`.
6. `deploy-phoenix-pool` — deploys `phoenix_pool_blended` with USDC as
   token_a (USDC sorts alphabetically below XLM) and XLM as token_b.
   `factory_addr` = deployer; admin = deployer. Pool address lands in
   `out/testnet/pool.json`.
7. `lookup-usdc-reserve` — queries `Blend.get_reserve_list()`, finds
   USDC's reserve index, derives `usdc_reserve_token_id = idx*2+1`.
   Writes `out/testnet/blend-reserves.json`.
8. `deploy-handler-stack` — invokes the root `task deploy` with the
   testnet env (real `BLENDED_POOL` + `BLEND_POOL` + USDC/XLM addresses
   + the discovered `USDC_RESERVE_TOKEN_ID` + relaxed policy).
9. `set-delegate` — calls `pool_blended.set_delegate(handler)`.
10. `verify-deploy` — `query_state` cross-check + pool delegate match.

### Defaults sized for a 1k USDC deployer balance

The Blend testnet faucet drips a 1000 USDC + 5000 BLND + 0.5 wETH +
0.05 wBTC bundle per ping (Discord ask). A single drip is enough for
the testnet sequence. Defaults in `taskfiles/testnet.yml`:

| Var                       | Default          | Meaning                                 |
| ------------------------- | ---------------- | --------------------------------------- |
| `MIN_TOTAL_USDC`          | 1000 USDC (1e10) | Floor below which Rebalance no-ops      |
| `USDC_SEED`               | 1000 USDC (1e10) | Initial pool USDC (full deployer bal)   |
| `XLM_SEED`                | 2000 XLM (2e10)  | Initial pool XLM                        |
| `TARGET_RATIO_BPS`        | 5000             | 50% target liquid USDC                  |
| `REBALANCE_BAND_BPS`      | 500              | ±5% no-op band around target            |
| `REBALANCE_COOLDOWN_SECS` | 60               | Min seconds between successful actions  |

The pool starts 100% liquid (1000 USDC + 2000 XLM). Warpdrive's first
Rebalance tick — triggered by any subsequent swap / provide / withdraw on
the pool — moves the position to the 50% target (500 USDC liquid + 500 USDC
delegated). At a 1000 USDC total the pool sits exactly at the
`MIN_TOTAL_USDC` floor; the `< floor` check is strict-less-than, so the
rebalance still proceeds at the boundary.

If the deployer holds more USDC, override at run time:

```bash
USDC_SEED=45000000000 XLM_SEED=90000000000 MIN_TOTAL_USDC=10000000000 \
task testnet:full
```

(That's the 4500-USDC pool with a 1000-USDC floor.)

### Run sequence

Bootstrap the deployer once:

```bash
stellar keys generate phoenix-blend-deployer --fund --network testnet
export DEPLOYER_SECRET=$(stellar keys show phoenix-blend-deployer)
export DEPLOYER_ADDRESS=$(stellar keys address phoenix-blend-deployer)
```

Then in `phoenix-blend-pool/`:

```bash
# 0. Sanity check (no state change)
task testnet:check-balances

# 1. Deploy everything (contracts + delegate wiring + verification)
task testnet:full

# 2. Seed the pool with the full 1000 USDC + 2000 XLM
task testnet:seed-pool

# 3. Confirm steady state
task testnet:pool-state    # liquid_a=1000 USDC, delegated_a=0 USDC
task testnet:status        # full artefact dump
```

### Bringing up the operator node

After the contracts are deployed, the off-chain pipeline is identical
to the [Single-operator dev deploy](#single-operator-dev-deploy) flow
from step 3 onwards. Skip steps 1-2 of that section since
`testnet:full` already did them.

In a second terminal:

```bash
export WARPDRIVE_SIGNING_MNEMONIC="$(stellar keys show <operator-key> --phrase)"
task run-node
```

Back in the first terminal:

```bash
task wire-service          # upload-component + upload-aggregator + build-service + register-service
task register-manager      # node watches project_root
task fetch-signer          # captures operator's ed25519 pubkey
task register-signer       # registers pubkey on ed25519-security
# Threshold defaults to 1/1 — fine for solo testnet
```

The node now consumes pool events and dispatches Rebalance /
HarvestYield envelopes through `verify_xlm` on the deployed handler.

### Artefacts under `out/`

| File                                | Contents                                                   |
| ----------------------------------- | ---------------------------------------------------------- |
| `out/testnet/phoenix-wasms.json`    | stake + token wasm hashes                                  |
| `out/testnet/pool.json`             | `blended_pool` C-address + token_a/token_b                 |
| `out/testnet/blend-reserves.json`   | `usdc_index` + `usdc_reserve_token_id`                     |
| `out/deploy.json`                   | `ed25519_security`, `ed25519_verification`, `project_root` |
| `out/handler.json`                  | `automation_handler` C-address                             |
| `out/circuit.digest`                | content-address digest of the uploaded circuit             |
| `out/aggregator.digest`             | content-address digest of the uploaded aggregator          |
| `out/service.hash`                  | hash returned by `POST /dev/services`                      |
| `out/signer.{json,pubkey}`          | this operator's ed25519 pubkey                             |

### Inspection helpers

```bash
task testnet:status        # dump every JSON artefact under out/
task testnet:pool-state    # query_pool_info + query_delegate_state
task testnet:reset         # rm out/testnet/ (keeps handler.json + deploy.json)
```

### Retuning policy without redeploying

Every policy knob is admin-settable post-deploy. The `configure-handler`
task wraps every setter and reads the same env vars as the deploy:

```bash
# Tighten the floor to 5000 USDC after acquiring more drips
MIN_TOTAL_USDC=50000000000 task configure-handler

# Cap per-tx rebalance moves at 100 USDC
MAX_REBALANCE_AMOUNT=1000000000 task configure-handler

# Disable the dust floor
MIN_REBALANCE_AMOUNT=0 task configure-handler

# Move BLND emissions to a different treasury
BLND_TREASURY_OVERRIDE=GABC... task configure-handler
```

### Admin-only operations

Convenience tasks for incident response:

```bash
task pause                       # halt verify_xlm (LocalError::Paused 600)
task unpause                     # lift the pause
AMOUNT=2000000000 task manual-to-blend     # supply 200 USDC out of band
AMOUNT=1000000000 task manual-from-blend   # pull 100 USDC out of Blend
task emergency-unwind            # drain all of Blend, reset principal=0
```

### Troubleshooting

| Symptom                                          | Fix                                                                                  |
| ------------------------------------------------ | ------------------------------------------------------------------------------------ |
| `PHOENIX_REPO not found at ../phoenix-contracts` | export `PHOENIX_REPO=<absolute path>` or `cd` somewhere with the sibling checkout    |
| `stellar keys show` returns blank                | re-create the key: `stellar keys generate <name> --fund --network testnet`           |
| `out/handler.json` missing after `testnet:full`  | run `task testnet:deploy-handler-stack` solo with `--verbose` to see the failing call |
| pool deploy reverts                              | `task testnet:reset` then re-run `task testnet:full`                                 |
| `seed-pool` slippage error                       | override min amounts to absorb dust: `MIN_A=999000000 task testnet:seed-pool`        |
| `lookup-usdc-reserve`: USDC not in reserve list  | Blend may have rotated the reserve set — query `get_reserve_list` manually, derive `idx*2+1`, export `USDC_RESERVE_TOKEN_ID` and skip the lookup step |
| `friendbot HTTP 400`                             | account already funded; safe to ignore. Override `XLM_BAL` floor in `check-balances` if you need to bypass |
| `verify_xlm` returns `OtherInvocationError 505`  | dig into the diagnostic event log; most often a pool-side revert (delegate cleared, Blend frozen) |
| `LocalError::Paused 600` on every call           | the admin paused the handler; run `task unpause`                                     |
| `LocalError::UsdcLeak 601`                       | code bug: handler held USDC across an entrypoint return. Open an issue; the guard rolled the tx back |

---

## VPS bring-up for the operator node

The contract layer (handler, security, verification, blended pool) is
small and stateless — deploy it from your laptop with `task testnet:full`
and forget it. The WarpDrive operator node is the long-lived piece: it
watches Stellar pubnet for pool events, signs envelopes with the operator's
BIP39 mnemonic, and submits to the handler on testnet. That belongs on a
VPS where it can run 24/7 under systemd.

`taskfiles/vps.yml` automates this. Topology:

```
┌────────────── laptop ─────────────┐    ┌────────────────── VPS ──────────────────┐
│ task testnet:full     ─► Stellar  │    │ /usr/local/bin/warpdrive (systemd)      │
│ task wire-service ─► localhost ─SSH-L─► 127.0.0.1:8000 (warpdrive HTTP API)     │
│ task fetch-signer    │  tunnel    │    │ /etc/default/warpdrive-node (secrets)   │
│ task register-signer │            │    │ /var/lib/warpdrive (KV state + wasms)   │
│ frontend dashboard   │            │    │ /opt/phoenix-blend-pool/warpdrive.toml  │
└───────────────────────────────────┘    └─────────────────────────────────────────┘
```

No SSH tunnel except when wiring the service or fetching the signer — the
running node only needs **outbound** access to Stellar RPC / Horizon and
(optionally) inbound libp2p in multi-op deploys. For 1-of-1 you can keep
every port closed except SSH.

### Prerequisites on the VPS

- Linux x86_64 with glibc ≥ the one you compiled against on your
  laptop, OR a musl-static binary (`BUILD_TARGET=musl task vps:build-warpdrive`).
  Debian 12 / Ubuntu 22.04+ both work with the default glibc build from
  most current laptops.
- **512 MB RAM minimum** at runtime. Compilation happens on your laptop;
  the VPS only runs the resulting binary, so memory pressure during
  `task vps:provision` is negligible.
- SSH key auth from your laptop (no password prompts — `vps:preflight`
  asserts this).
- A user with passwordless `sudo` (the apt + systemd unit install
  steps need it).
- Outbound HTTPS to `*.stellar.org` and `*.sorobanrpc.com`.

No build toolchain on the VPS. Only `ca-certificates`, `jq`, `rsync`,
and `python3` are apt-installed during `provision`.

No inbound firewall rules required for solo (1-of-1) deploys.

### Prerequisites on your laptop

The patched warpdrive fork is compiled locally, so you need:

- Rust toolchain matching `../warpdrive/rust-toolchain.toml`
  (`rustup show` from inside that dir bootstraps it on first build).
- ~3 GB free disk for the warpdrive workspace's `target/` dir.
- For maximum binary portability across VPS distros: install
  `musl-tools` (`apt install musl-tools` / `pacman -S musl`) and
  add the musl target: `rustup target add x86_64-unknown-linux-musl`.
  Then pass `BUILD_TARGET=musl` to `task vps:provision` and
  `task vps:ship-warpdrive` — the resulting binary is fully static and
  runs on any x86_64 Linux regardless of its glibc version.

### Required env vars

On your laptop:

| Var | Meaning | Required |
|-----|---------|----------|
| `VPS_HOST` | `user@host` or an `~/.ssh/config` alias | yes |
| `VPS_DIR` | deploy dir on the VPS (default `/opt/phoenix-blend-pool`) | no |
| `VPS_NODE_PORT` | warpdrive HTTP API port (default `8000`) | no |
| `WARPDRIVE_REPO` | local path to the patched warpdrive fork (default `../warpdrive`) | no |

On the VPS (in `/etc/default/warpdrive-node` after `vps:provision`
installs the template — fill these in manually with `sudo nano`):

| Var | Meaning |
|-----|---------|
| `WARPDRIVE_SIGNING_MNEMONIC` | BIP39 mnemonic from `stellar keys show <operator> --phrase` |
| `DEPLOYER_SECRET` | Stellar S-key the aggregator uses to sign + submit verify_xlm txs (~20 XLM is plenty) |
| `WARPDRIVE_AGGREGATOR_STELLAR_CREDENTIAL` | optional override; defaults to `DEPLOYER_SECRET` |

The signing mnemonic is the master secret for this operator — anyone who
holds it can sign quorum envelopes as that operator. Keep `/etc/default/
warpdrive-node` at mode 0600 (the template installer sets this) and back
the mnemonic up to a secrets manager out of band.

### Run sequence

```bash
# Contracts already deployed via `task testnet:full` from your laptop.

export VPS_HOST="warpdrive-op-1.example.org"

# 1. One-shot VPS setup. Installs OS deps, rustup, builds the patched
#    warpdrive fork via cargo install, creates the warpdrive system user,
#    installs the systemd unit + env template. Idempotent; safe to re-run.
task vps:provision

# 2. Fill in secrets on the VPS (mnemonic + S-key). This is the only
#    step that requires sudo on the VPS interactively.
task vps:edit-env                  # nano /etc/default/warpdrive-node over a real tty

# 3. Ship warpdrive.toml + start the node under systemd.
task vps:deploy
task vps:status

# 4. In a side terminal, open the SSH tunnel so localhost:8000 hits the
#    VPS node:
task vps:tunnel

# 5. In your main terminal, run the existing wire-up tasks — they hit
#    127.0.0.1:8000 unchanged, which the tunnel routes to the VPS.
task wire-service          # uploads circuit + aggregator + registers service
task register-manager      # node starts watching project_root on chain
task fetch-signer          # captures the operator's ed25519 pubkey
task register-signer       # registers the pubkey on ed25519-security

# 6. Once envelopes start flowing, drop the tunnel.
#    (^C in the side terminal running task vps:tunnel)
```

### Day-2 operations

```bash
task vps:logs              # journalctl -f -u warpdrive-node
task vps:status            # systemctl status + last 30 log lines
task vps:restart           # restart after a warpdrive.toml change
task vps:stop              # halt the node (envelopes stop being signed)
task vps:ssh               # interactive shell on the VPS
```

If you change `warpdrive.toml` locally, run `task vps:deploy` again —
it rsyncs the file and restarts the service.

If you rebuild the patched warpdrive fork (e.g. after pulling new
patches), run `task vps:ship-warpdrive` to recompile on the VPS and
swap the binary in place. The systemd unit picks up the new binary on
next restart.

### Why systemd + SSH tunnel, not exposing port 8000

The warpdrive HTTP API at `127.0.0.1:8000` is **trusted** — it accepts
component uploads, service registrations, and signer queries without
authentication. Exposing it to the public internet would let anyone
swap the running service spec or replace the active components. Keep
it bound to localhost on the VPS; reach it only via the SSH tunnel
during wire-up; close the tunnel between uses.

For a public dashboard, run the frontend (or a separate Caddy
terminator) on a different port and have it talk to the on-chain RPCs
directly — the dashboard never needs the warpdrive HTTP API to render
state.

### Dashboard on the same VPS

Same loopback-then-tunnel pattern: the static dashboard runs under
systemd bound to `127.0.0.1:8081`; you reach it from your browser via
an SSH local-forward.

First-time install:

```bash
task vps:install-frontend          # creates /srv/phoenix-blend-display, installs systemd unit
```

After every contract redeploy (so `out/handler.json` etc. change):

```bash
task vps:dashboard                 # = testnet:make-frontend-config + vps:deploy-frontend + vps:tunnel-dashboard
```

If you'd rather drive the steps individually:

```bash
task testnet:make-frontend-config  # generate frontend/config.json locally
task vps:deploy-frontend           # rsync to /srv/phoenix-blend-display, restart systemd unit
task vps:tunnel-dashboard          # side terminal: SSH local-forward 8081
# open http://localhost:8081
```

Why a separate tunnel from `vps:tunnel` (the warpdrive API):

- `vps:tunnel` forwards port **8000** for `wire-service` / `fetch-signer`
  / `register-signer`. You only need it open during wire-up.
- `vps:tunnel-dashboard` forwards port **8081** for the dashboard. You
  want it open whenever you're watching state.

Run both in parallel terminals if you're doing wire-up and monitoring at
the same time.

The dashboard never touches the warpdrive HTTP API — it pulls every
value from Stellar Soroban RPC + Horizon directly, both of which the
VPS reaches over HTTPS. So once `vps:deploy-frontend` runs, the
dashboard works without any other operator-side glue.

Want a publicly accessible dashboard (real domain, HTTPS, no SSH
tunnel)? Front the loopback file server with Caddy. See
`../../hodlers-display/Caddyfile` for the template — substitute the
frontend path and your domain, install Caddy with `sudo apt install
caddy`, point port 443 at it, and open port 443 at the firewall.
`config.json` contains only public C-strkeys / G-addresses, no secrets,
so it's safe to expose.

### Migrating from local to VPS

If you already brought the node up locally with `task run-node` and want
to move it to a VPS without losing state:

1. `Ctrl-C` the local `task run-node`.
2. `rsync -a out/node-data/ $VPS_HOST:/var/lib/warpdrive/` — copy the
   KV state + uploaded components.
3. `task vps:provision && task vps:deploy` — bring up the VPS node.
4. The signer keypair derived from `WARPDRIVE_SIGNING_MNEMONIC` is
   deterministic; as long as the env file holds the same mnemonic, the
   on-chain `ed25519-security` registration stays valid. No
   re-registration needed.

If the mnemonic changes, you'll need to `Ed25519Security.remove_signer(old)`
+ `task register-signer` for the new pubkey.

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

Keep `host = "127.0.0.1"` under `[warpdrive]` even in multi-op — the
HTTP API is unauthenticated and should never be reached over the
internet. The libp2p listener (port 9000) is what peers connect to,
and it doesn't read `host`; it always binds to all interfaces. Open
9000 to the other operators' IPs at the firewall.

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

## Phased rollout

Per `PHOENIX_BLEND_AUTOMATION.md`. Each phase has explicit gating
criteria; do NOT advance until the current phase's success conditions
are met for at least the listed minimum window. Roll back to the
previous phase on any unresolved failure of a gating criterion.

### Phase A: Testnet shadow (1-2 weeks)

**Goal.** Prove the full stack works against a real (or mocked) Blend
with synthetic traffic before any mainnet value is at risk.

Setup:

- Deploy `phoenix-pool-blended` on Stellar testnet with admin = Phoenix
  deploy key. Seed liquidity with the deploy account.
- Deploy or mock a Blend USDC pool on testnet. If using a real testnet
  Blend, confirm it accepts Supply / Withdraw via `submit`.
- Deploy `automation-handler` with conservative defaults:
  `target=50%`, `band=10%`, `min_total=10_000 USDC`, `cooldown=15min`,
  `max_rebalance_amount=` "1-2% of pool TVL", `min_rebalance_amount=` a
  meaningful dust floor.
- Wire delegate (`task set-delegate`).
- 3 operators minimum, threshold 2-of-3.
- Seed via `manual_to_blend(target_share_of_pool_usdc)`.

Drive traffic:

- Synthetic swap script generating 50-200 swaps/day on the blended pool
  with realistic size distribution.
- Synthetic LP add/remove churn 5-10x/day.
- Manually trigger Blend bad-debt scenarios via the testnet Blend admin
  tools (or by adjusting the mock's redeemable value).

Gating criteria — ALL must hold for 7 consecutive days:

| Criterion | How to check |
| --------- | ------------ |
| Rebalance fires inside cooldown + band | `last_rebalance_ts` matches event-trigger time within seconds; `liquid / total` returns inside band after every action. |
| Harvest fires on cron schedule | Count `HarvestCompleted` events; should be exactly `(86400 / harvest_interval_secs)` per day. |
| Interest is actually donated | `interest_donated > 0` on every harvest with positive yield. Total LP claim on USDC grows monotonically. |
| BLND routes straight to treasury | Treasury BLND balance grows; handler BLND balance stays 0. |
| Bad-debt path doesn't brick the handler | After a forced write-down, `principal_supplied` shrinks; subsequent harvests + rebalances continue. |
| Pause works | Admin pauses; verify next quorum-signed envelope panics with code 600. Unpause resumes. |
| Emergency unwind works | Admin calls `emergency_unwind`; `principal_supplied` returns to 0; pool reabsorbs USDC. |
| Blend Frozen handled | Force Blend status > 3 (testnet admin); confirm Rebalance no-ops, Harvest claims BLND only. |

On failure: fix the bug in this repo, re-deploy, restart the 7-day clock.

### Phase B: Mainnet dry-run (48-72 hours)

**Goal.** Validate against real production contracts and real Blend
state, with no actual automation movement.

Setup:

- All production addresses populated per the
  "Resolving production addresses" section above.
- Deploy `automation-handler` on mainnet against the real `BLENDED_POOL`
  and `BLEND_POOL`.
- Quorum is the production size (4-of-5 or 5-of-7 — see
  `ARCHITECTURE.md` Q7 for the chosen threshold).
- `min_total_usdc` set ABOVE the current actual pool USDC TVL so every
  rebalance attempt is a no-op. The handler is "armed but dormant".
- Wire delegate (`task set-delegate`).
- DO NOT seed (`manual_to_blend` deliberately skipped — principal stays
  zero, no money in Blend yet).

Drive traffic:

- Real swap traffic from Phoenix's normal user base. Opt-in beta LPs may
  be encouraged but not required.

Gating criteria — ALL must hold for 48 hours:

| Criterion | How to check |
| --------- | ------------ |
| Quorum-signed envelopes arrive on every swap | Aggregator logs show `signatures_collected=N/N` where N is threshold. |
| `verify_xlm` returns `Ok` consistently | No `OtherInvocationError` / `Paused` errors on the handler tx logs. |
| Rebalance no-op behaviour holds | `last_rebalance_ts` stays 0; no `RebalanceExecuted` events. |
| Harvest cron fires on schedule | `HarvestCompleted` event per scheduled tick, all with `principal_after == 0`, `interest_donated == 0`. |
| Operator network is healthy | All operators reachable on libp2p; `peers >= threshold - 1`. |

On failure: pause via `pause()`, debug, unpause when fixed. If a
contract-level bug is found, return to Phase A.

### Phase C: Mainnet launch (gradual ramp)

**Goal.** Switch automation on, ramped slowly enough that a problem can
be paused before doing damage.

Cutover sequence, executed by the admin multisig in one session:

1. Confirm Phase B gating criteria still hold.
2. `set_min_total_usdc(<real-floor>)`. From this moment on, Rebalance is
   armed.
3. `manual_to_blend(seed_amount)` where `seed_amount` is the smaller of:
   - the real target liquid share of pool TVL (e.g. 5% on day one);
   - `max_rebalance_amount` (already configured).
   This sets the principal counter without going through quorum.
4. Phoenix migrates LP incentives to the blended pool. LPs migrate.
5. Monitor the dashboard for 72 hours minimum:
   - `principal_supplied` and `liquid / total` track the target ±band.
   - `interest_donated` accumulates per harvest.
   - No `Paused` events.
   - No `EmergencyUnwound` events.
6. Once stable, increase `max_rebalance_amount` to a value that allows
   converging to target in one or two rebalances after a large swap, and
   call `manual_to_blend` to bring principal up to the steady-state
   target.

Roll-back:

- Soft: `pause()` halts off-chain dispatch immediately. The pool stays
  in its current liquid/delegated split.
- Medium: `emergency_unwind()` drains Blend back to the pool. Position
  is closed; LPs are not affected.
- Hard: pool admin calls `pool_blended.set_delegate(None)`. The handler
  can no longer touch the pool even if unpaused.

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
