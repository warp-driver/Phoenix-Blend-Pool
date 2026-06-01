# Operators

Operator inventory for the phoenix-blend-pool WarpDrive deployment.
This file is canonical; updates require admin sign-off.

For deploy mechanics see `DEPLOY.md`. For the rollout schedule and gating
criteria see `DEPLOY.md` § "Phased rollout".

---

## Current threshold

- **Production target:** `_-of-_` (TBD; pending Phoenix sign-off, see
  `ARCHITECTURE.md` Q7).
- **Currently on-chain:** see `Ed25519Security.threshold()`.

To change: admin runs
`THRESHOLD_NUM=<num> THRESHOLD_DEN=<den> task set-threshold`. The change
takes effect on the next quorum check (i.e. the next inbound event).

---

## Operator roster

Fill one row per operator. Order is not significant; libp2p discovers
peers via the `bootstrap_nodes` list each operator carries.

| # | Operator label   | Host / region        | G-address (deploy / aggregator) | ed25519 pubkey (BIP39 derived) | Peer ID (libp2p) | Multiaddr                                     | Weight | Status |
|---|------------------|----------------------|---------------------------------|---------------------------------|------------------|-----------------------------------------------|--------|--------|
| 1 | `op-a`           | aws-eu-west-1        | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |
| 2 | `op-b`           | aws-us-east-1        | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |
| 3 | `op-c`           | hetzner-fsn1         | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |
| 4 | `op-d`           | hetzner-hel1         | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |
| 5 | `op-e`           | gcp-us-central1      | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |

Optional expansion to 7 operators for 5-of-7 quorum:

| 6 | `op-f`           | aws-ap-southeast-1   | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |
| 7 | `op-g`           | hetzner-nbg1         | G…                              | E…                              | 12D3Koo…         | `/ip4/x.x.x.x/tcp/9000/p2p/12D3Koo…`         | 1      | TBD    |

Status legend:

- `TBD` — slot reserved, operator not yet provisioned.
- `bootstrapping` — node running, signer not yet registered on
  `Ed25519Security`.
- `live` — signer registered, contributing to quorum.
- `paused` — node up but admin-removed via `remove_signer`; threshold
  may need a temporary drop.
- `retired` — fully removed; bootstrap list cleaned up; threshold
  re-balanced for the new operator count.

---

## Per-operator provisioning checklist

Every operator slot, before they go `live`:

1. **Box provisioned.** Linux host with the prerequisites from
   `DEPLOY.md` § "Host packages".
2. **Local warpdrive built.** `../warpdrive` cloned and
   `cargo install --path packages/warpdrive --locked` succeeded. The
   fork's three patches are in place (see
   `DEPLOY.md` § "Warpdrive fork patches").
3. **Deploy keys generated.** `stellar keys generate <op-label> --fund`
   for the network in use. Public address recorded above.
4. **BIP39 mnemonic generated and stored in operator's secrets manager.**
   `stellar keys generate <op-label>-signing --phrase` produces a 12 or
   24 word mnemonic. This is what becomes
   `WARPDRIVE_SIGNING_MNEMONIC`. **The mnemonic itself never leaves the
   operator's box.** The admin only ever sees the derived ed25519 pubkey.
5. **`warpdrive.toml` populated** with the bootstrap list of peer
   multiaddrs from rows in this table.
6. **Node up.** `task run-node` running under systemd (or equivalent),
   process restart on failure, logs shipped to the central log sink.
7. **Manager registered.** `task register-manager` succeeded against the
   on-chain project_root.
8. **Signer fetched.** `task fetch-signer` written
   `out/signer.{json,pubkey}`. The operator hands the pubkey to the
   admin.
9. **Signer registered.** Admin appends a row to the table above and
   runs `task register-signer SIGNER_PUBKEY=<their-pubkey>`. The
   security contract now accepts this operator's signature.
10. **Threshold set.** Admin runs `task set-threshold` with the new
    `THRESHOLD_NUM/DEN`.
11. **End-to-end smoke.** Trigger a synthetic swap on the blended pool
    (testnet) or wait for the next real swap (mainnet). Confirm in
    the operator's logs:
    - the trigger fires,
    - the circuit emits a `Rebalance` payload,
    - the aggregator collects at least `THRESHOLD_NUM` signatures,
    - the resulting tx hits `verify_xlm` with `Ok(())`,
    - the dashboard records a `RebalanceExecuted` event.

---

## Per-operator hot-running checklist

Every live operator confirms weekly:

- Node uptime > 99% in the last 7 days.
- `peers >= threshold - 1` consistently.
- No `Paused` errors observed in the operator's verify_xlm log tail.
- Local Stellar RPC has not lagged > 30 seconds behind the network.
- Logs are flowing to the central sink without backpressure.
- BIP39 mnemonic backup still verifiable in the operator's secrets
  manager (no rotation event without admin notification).

---

## Coordinated change procedures

### Onboarding a new operator (`bootstrapping` → `live`)

1. Operator runs steps 1-8 from "Per-operator provisioning checklist".
2. Operator submits their pubkey + multiaddr to admin via the secure
   channel (this is a shared encrypted channel — Signal group, Keybase,
   etc.; agree on it once and stick to it).
3. Admin appends a row to the table above with `Status = bootstrapping`.
4. Existing operators add the new operator's multiaddr to their
   `warpdrive.toml` `bootstrap_nodes` and reload their nodes (rolling
   restart, no quorum disruption).
5. Admin runs `task register-signer` for the new pubkey.
6. Admin re-evaluates `THRESHOLD_NUM/THRESHOLD_DEN` and runs
   `task set-threshold` if the threshold should change.
7. Admin flips the new row's `Status` to `live`.

### Off-boarding an operator (`live` → `retired`)

1. Admin announces in the operator channel with the retirement window.
2. Admin runs `THRESHOLD_NUM=<new-num> THRESHOLD_DEN=<new-den>
   task set-threshold` to lower the threshold safely first (if
   required; with 5 operators dropping to 4, threshold drops from 4 to
   3 to maintain super-majority).
3. Admin runs `Ed25519Security.remove_signer(<pubkey>)`.
4. Operator stops their warpdrive node.
5. Existing operators remove the retired operator's multiaddr from
   `warpdrive.toml` `bootstrap_nodes` and reload.
6. Admin flips the table row's `Status` to `retired`. Row stays in the
   table for audit; do not delete.

### Rotating an operator's signer

If an operator suspects their mnemonic is compromised:

1. Operator generates a new mnemonic on a fresh secrets-manager entry.
2. Operator updates `WARPDRIVE_SIGNING_MNEMONIC` in their systemd env
   file and restarts their node.
3. Operator runs `task fetch-signer` to produce the new pubkey.
4. Admin runs `Ed25519Security.remove_signer(old_pubkey)` then
   `task register-signer SIGNER_PUBKEY=<new-pubkey>` in one session.
5. Old pubkey's row in the table above is replaced (not appended;
   replacement preserves the operator's identity in the inventory).

---

## Admin custody

The admin role is the one configured in the handler's `__constructor`
(`HANDLER_ADMIN` env at deploy time; defaults to `DEPLOYER_ADDRESS`).

In production this MUST be a multisig:

- 2-of-3 or 3-of-5 threshold over multiple Phoenix team principals.
- Cold storage signers preferred over hot keys.
- Admin transfer is two-step (`propose_admin` then `accept_admin` from
  the proposed key) so a mis-typed address cannot brick the contract.

Admin actions (all `require_auth()` on `admin`):

- `pause()` / `unpause()` — pause toggle.
- `emergency_unwind()` — drain Blend back to pool.
- `manual_to_blend(amount)` / `manual_from_blend(amount)` — out-of-band
  position adjustments.
- `set_target_ratio_bps` / `set_rebalance_band_bps` /
  `set_min_total_usdc` / `set_rebalance_cooldown_secs` /
  `set_max_rebalance_amount` / `set_min_rebalance_amount` — retune.
- `set_blnd_treasury` / `set_usdc_reserve_token_id` — re-pin externals.
- `upgrade(new_wasm_hash, new_version)` — code rollover.
- `propose_admin` / `accept_admin` — multisig rotation.

Quorum on `Ed25519Security` (the operator network) is independent of
the handler admin. Compromising one does not compromise the other.
