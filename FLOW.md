# Event flow: Phoenix ↔ WarpDrive ↔ Blend

Two independent paths share the same circuit + aggregator + handler.
Rebalance fires on every relevant pool event. Harvest fires on a cron tick.

Legend
- ` ─►` cross-system call / submission
- ` ━►` Soroban contract → contract call (same chain)
- `( )` filter / decision point
- `[ ]` event emitted (visible to dashboards)

---

## A. Rebalance — driven by Phoenix pool activity

```
┌──────────────────────────────┐
│  Phoenix BLENDED_POOL        │     trader / LP action
│  (phoenix-pool-blended)      │ ◄────────────────────────  swap | provide_liquidity | withdraw_liquidity
└──────────────┬───────────────┘
               │ emits exactly ONE event per logical action
               │ topic[0] ∈ { "swap", "provide_liquidity", "withdraw_liquidity" }
               ▼
┌──────────────────────────────┐
│  WarpDrive trigger           │   StellarContractEvent (rest-wildcard on BLENDED_POOL)
│  on each operator node       │
└──────────────┬───────────────┘
               ▼
┌──────────────────────────────┐
│  circuit (WASI 0.2)          │   ( topic[0] in {swap, provide, withdraw} ? )
│  components/circuit          │   yes ─► emit RebalanceAction::Rebalance  (unit variant)
└──────────────┬───────────────┘   no  ─► drop (no payload)
               ▼
┌──────────────────────────────┐
│  ed25519/SEP-53 quorum       │   each operator signs the envelope locally
│  libp2p gossip               │   signatures gathered until threshold (4-of-5)
└──────────────┬───────────────┘
               ▼
┌──────────────────────────────┐
│  aggregator (WASI 0.2)       │   emits AggregatorAction::Submit(Stellar { chain, handler })
│  components/aggregator       │
└──────────────┬───────────────┘
               ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  AutomationHandler.verify_xlm(envelope, sig_data)            (testnet)    │
│  ────────────────────────────────────────────────────────────────────────│
│   1. paused?            ─► panic LocalError::Paused (600)                 │
│   2. ed25519-verification ━► verify_xlm() ─► Ok(Verified)                 │
│   3. dedupe by event_id (first-time? mark seen)                           │
│   4. decode RebalanceAction → Rebalance                                   │
│   5. read pool snapshot:                                                  │
│         BLENDED_POOL ━► query_delegate_state()                            │
│         ─► DelegateState { liquid_usdc, delegated_usdc, total_usdc, … }   │
│   6. read Blend health:                                                   │
│         BLEND_POOL ━► get_config().status                                 │
│         status > 3  ─► no-op return (don't consume cooldown)              │
│   7. apply policy gates                                                   │
│         total < min_total_usdc           ─► no-op                         │
│         within target ± band             ─► no-op                         │
│         now < last_rebalance_ts + cool   ─► no-op                         │
│         natural amount < min_floor       ─► no-op                         │
│   8. dispatch:                                                            │
│      ┌─ ToBlend (liquid > upper) ──────────────────────────────────────┐  │
│      │  amount = min(excess, max_cap)                                  │  │
│      │  BLENDED_POOL ━► withdraw_to_delegate(USDC, amount)             │  │
│      │  BLEND_POOL   ━► submit(Supply USDC amount)                     │  │
│      │  principal_supplied += amount                                   │  │
│      │  [RebalanceExecuted { direction="to_blend", amount, ... }]      │  │
│      └────────────────────────────────────────────────────────────────┘   │
│      ┌─ FromBlend (liquid < lower) ────────────────────────────────────┐  │
│      │  amount = min(target - liquid, principal_supplied, max_cap)    │  │
│      │  BLEND_POOL   ━► submit(Withdraw USDC amount)                  │  │
│      │  BLENDED_POOL ━► deposit_from_delegate(USDC, amount)           │  │
│      │  principal_supplied -= amount                                  │  │
│      │  [RebalanceExecuted { direction="frm_blnd", amount, ... }]      │  │
│      └────────────────────────────────────────────────────────────────┘   │
│   9. last_rebalance_ts = now                                              │
│  10. assert_no_usdc_residue() — handler must hold 0 USDC                  │
│  11. [Verified { event_id }]                                              │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## B. Harvest — driven by cron

```
┌──────────────────────────────┐
│  Cron tick                   │   `0 0 0,4,8,12,16,20 * * *`  (every 4h)
│  WarpDrive trigger           │
└──────────────┬───────────────┘
               ▼
┌──────────────────────────────┐
│  circuit                     │   on TriggerData::Cron ─►
│                              │   emit RebalanceAction::HarvestYield  (unit variant)
└──────────────┬───────────────┘
               ▼
        (quorum + aggregator same as A)
               ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  AutomationHandler.verify_xlm(envelope, sig_data)                         │
│  ────────────────────────────────────────────────────────────────────────│
│   1. paused?                    ─► panic LocalError::Paused (600)         │
│   2. verify + dedupe (as A)                                               │
│   3. decode RebalanceAction → HarvestYield                                │
│   4. BLEND_POOL ━► claim([usdc_reserve_token_id], to=BLND_TREASURY)       │
│        ─► BLND_TREASURY  receives BLND emissions directly                 │
│        ─► blnd_routed = amount transferred                                │
│   5. if principal_supplied > 0 AND Blend healthy (status ≤ 3):            │
│        usdc_before = handler.usdc_balance                                 │
│        BLEND_POOL ━► submit(Withdraw USDC i128::MAX)                      │
│        redeemed = handler.usdc_balance - usdc_before                      │
│        supply_amount = min(principal_supplied, redeemed)                  │
│        if supply_amount > 0:                                              │
│            BLEND_POOL ━► submit(Supply USDC supply_amount)                │
│        principal_supplied = supply_amount                                 │
│        if redeemed < principal_supplied_old:                              │
│            [BadDebtDetected { previous, redeemable, shortfall }]          │
│        interest = redeemed - supply_amount                                │
│        if interest > 0:                                                   │
│            BLENDED_POOL ━► donate(USDC, interest)                         │
│                ─► pool's stored reserve grows by `interest`               │
│                ─► every existing LP's claim on USDC grows pro-rata        │
│                ─► no LP shares are minted                                 │
│      else (Blend frozen / setup): skip the USDC leg, keep BLND routed     │
│   6. last_harvest_ts = now                                                │
│   7. [HarvestCompleted { interest_donated, blnd_routed, principal_after }]│
│   8. assert_no_usdc_residue() — handler must hold 0 USDC                  │
│   9. [Verified { event_id }]                                              │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## C. Admin out-of-band paths (no off-chain envelope)

```
admin (Phoenix multisig)
   │
   ├─► pause()                  ─► [PauseToggled { paused: true }]
   ├─► unpause()                ─► [PauseToggled { paused: false }]
   ├─► manual_to_blend(amount)  ─► (same on-chain steps as Rebalance ToBlend)
   │                              [RebalanceExecuted { ... }]
   ├─► manual_from_blend(amount)─► (same on-chain steps as Rebalance FromBlend)
   │                              [RebalanceExecuted { ... }]
   ├─► emergency_unwind()       ─► blend.submit(Withdraw i128::MAX)
   │                              pool.deposit_from_delegate(min(redeemed, principal))
   │                              pool.donate(USDC, excess)              if any
   │                              principal_supplied = 0
   │                              [EmergencyUnwound { redeemed, principal_before }]
   ├─► set_target_ratio_bps(n)  ─► [ConfigUpdated { field="target",   value=n }]
   ├─► set_rebalance_band_bps(n)─► [ConfigUpdated { field="band",     value=n }]
   ├─► set_min_total_usdc(n)    ─► [ConfigUpdated { field="min_tot",  value=n }]
   ├─► set_max_rebalance_amount ─► [ConfigUpdated { field="max_reb",  value=n }]
   ├─► set_min_rebalance_amount ─► [ConfigUpdated { field="min_reb",  value=n }]
   ├─► set_rebalance_cooldown   ─► [ConfigUpdated { field="cooldown", value=n }]
   ├─► set_blnd_treasury(addr)  ─► [AddressConfigUpdated { field="treasury", value=addr }]
   ├─► set_usdc_reserve_token_id─► [ConfigUpdated { field="usdc_id",  value=id }]
   ├─► propose_admin(new)       ─►  pending_admin = Some(new)
   └─► accept_admin()           ─►  admin = pending_admin; [ContractUpgraded] on upgrade
```

Pool admin (Phoenix governance, not the handler admin):

```
pool admin
   │
   ├─► pool.set_delegate(handler_address)   — wires the handler as delegate
   └─► pool.set_delegate(null)              — emergency revocation
```

---

## D. Value flow (steady state)

```
┌──────────────────────┐                                  ┌──────────────────────┐
│  XLM-USDC LPs        │ ◄── pro-rata claim grows ──────  │  BLENDED_POOL        │
│  (token holders)     │      every time donate fires     │  reserves            │
└──────────────────────┘                                  │   XLM:   X           │
                                                          │   USDC:  Y_liquid +  │
                                                          │         Y_delegated  │
                                                          └──────┬───────────────┘
                                                                 │ withdraw_to_delegate
                                                                 │ deposit_from_delegate
                                                                 │ donate
                                                                 ▼
                                                          ┌──────────────────────┐
                                                          │  handler             │
                                                          │  (delegate)          │
                                                          │  USDC: 0 between     │
                                                          │  actions (residue    │
                                                          │  guard enforces it)  │
                                                          └──────┬───────────────┘
                                                                 │ submit(Supply / Withdraw)
                                                                 ▼
                                                          ┌──────────────────────┐
                                                          │  BLEND_POOL          │
                                                          │  USDC reserve        │
                                                          │  ─ handler's         │
                                                          │    principal earns   │
                                                          │    b-token interest  │
                                                          │  ─ + BLND emissions  │
                                                          └──────┬───────────────┘
                                                                 │ claim(to=treasury)
                                                                 ▼
                                                          ┌──────────────────────┐
                                                          │  BLND_TREASURY       │
                                                          │  (Phoenix DAO)       │
                                                          └──────────────────────┘
```

Two return paths for value:
- USDC interest → `donate` → all LPs share pro-rata (no LP minted).
- BLND emissions → straight to treasury, handler never holds BLND.
