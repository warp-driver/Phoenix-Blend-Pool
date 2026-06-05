// phoenix-blend-display — minimal dashboard for the testnet bring-up.
// All state on-chain; we just poll Soroban RPC and render. No backend.
//
// Panels prove each step of the WarpDrive flow:
//   1. Pool exists + delegate wired   → "Phoenix pool state" + "Pool events"
//   2. Triggers fire                  → "Pool events" rows for swap / provide / withdraw
//   3. Off-chain pipeline closes loop → "Handler events" rows for RebalanceExecuted / HarvestCompleted
//   4. State converges to target      → "Drift indicator" marker tracks the band
//   5. Operator funded                → "Deployer wallet" balances

(() => {
  "use strict";

  // ---- Bootstrap ---------------------------------------------------------

  let cfg = null;
  let sdk = null;          // window.StellarSdk after the CDN <script> loads
  let rpc = null;          // sdk.rpc (v13+) or sdk.SorobanRpc (v12)
  let server = null;       // rpc.Server
  let pollTimer = null;

  // Cached latest state across panels so the drift indicator can compute
  // its predicted action from the same snapshot.
  let latestPoolState = null;
  let latestHandlerState = null;
  let latestBlendStatus = null;

  const $ = (sel) => document.querySelector(sel);
  const setText = (sel, val) => { const n = $(sel); if (n) n.textContent = val; };
  const setErr  = (msg) => { const n = $("#err-line"); if (n) n.textContent = msg || ""; };

  async function loadConfig() {
    const res = await fetch("config.json", { cache: "no-store" });
    if (!res.ok) throw new Error(`config.json fetch failed: ${res.status}`);
    return res.json();
  }

  async function waitForSdk(timeoutMs = 8000) {
    const start = Date.now();
    while (!window.StellarSdk && Date.now() - start < timeoutMs) {
      await new Promise((r) => setTimeout(r, 50));
    }
    if (!window.StellarSdk) throw new Error("StellarSdk failed to load from CDN");
    sdk = window.StellarSdk;
    // v13 renamed SorobanRpc → rpc; keep working on both.
    rpc = sdk.rpc || sdk.SorobanRpc;
    if (!rpc) throw new Error("StellarSdk missing Soroban RPC namespace (rpc / SorobanRpc)");
  }

  // ---- Contract error decoding -----------------------------------------

  /**
   * Phoenix XYK blended-pool contract error map (300..336).
   * Source: phoenix-contracts/contracts/pool_blended/src/error.rs.
   */
  const PHOENIX_POOL_ERRORS = {
    300: ["SpreadExceedsLimit",                                 "swap moves the price by more than max_spread_bps — reduce offer amount or raise max-spread"],
    301: ["ProvideLiquiditySlippageToleranceTooHigh",           "slippage > pool max_allowed_slippage_bps"],
    302: ["ProvideLiquidityAtLeastOneTokenMustBeBiggerThanZero","both desired_a and desired_b are zero"],
    303: ["WithdrawLiquidityMinimumAmountOfAOrBIsNotSatisfied", "min_a or min_b higher than what the burn yields"],
    304: ["SplitDepositBothPoolsAndDepositMustBePositive",      "internal split-deposit math hit a non-positive input"],
    305: ["ValidateFeeBpsTotalFeesCantBeGreaterThan100",        "fee config sums to > 10_000 bps"],
    306: ["GetDepositAmountsMinABiggerThanDesiredA",            "min_a > desired_a"],
    307: ["GetDepositAmountsMinBBiggerThanDesiredB",            "min_b > desired_b"],
    308: ["GetDepositAmountsAmountABiggerThanDesiredA",         "computed deposit_a exceeds desired_a"],
    309: ["GetDepositAmountsAmountALessThanMinA",               "computed deposit_a < min_a (slippage)"],
    310: ["GetDepositAmountsAmountBBiggerThanDesiredB",         "computed deposit_b exceeds desired_b"],
    311: ["GetDepositAmountsAmountBLessThanMinB",               "computed deposit_b < min_b (slippage)"],
    312: ["TotalSharesEqualZero",                               "pool has no LP shares yet — first depositor must seed both sides"],
    313: ["DesiredAmountsBelowOrEqualZero",                     "desired_a/desired_b must be > 0"],
    314: ["MinAmountsBelowZero",                                "min_a or min_b is negative"],
    315: ["AssetNotInPool",                                     "offer_asset is neither token_a nor token_b of this pool"],
    316: ["AlreadyInitialized",                                 "pool already initialized"],
    317: ["TokenABiggerThanTokenB",                             "init: token_a address must sort below token_b"],
    318: ["InvalidBps",                                         "value out of 0..=max_allowed_spread_bps — check max_spread_bps and pool config"],
    319: ["SlippageInvalid",                                    "slippage value out of 0..=10_000"],
    320: ["SwapMinReceivedBiggerThanReturn",                    "ask_asset_min_amount > what the swap would return — lower min or raise offer"],
    321: ["TransactionAfterTimestampDeadline",                  "deadline elapsed before execution — retry with a larger deadline"],
    322: ["CannotConvertU256ToI128",                            "internal: u256 → i128 overflow"],
    323: ["UserDeclinesPoolFee",                                "max_allowed_fee_bps < pool.total_fee_bps"],
    324: ["SwapFeeBpsOverLimit",                                "configured fee bps exceeds protocol ceiling"],
    325: ["NotEnoughSharesToBeMinted",                          "deposit too small to mint at least 1 share"],
    326: ["NotEnoughLiquidityProvided",                         "post-deposit reserves below the minimum-liquidity floor"],
    327: ["AdminNotSet",                                        "admin storage entry missing"],
    328: ["ContractMathError",                                  "internal arithmetic overflow / divide-by-zero"],
    329: ["NegativeInputProvided",                              "an i128 input is < 0"],
    330: ["SameAdmin",                                          "proposed admin equals current admin"],
    331: ["NoAdminChangeInPlace",                               "accept_admin_change called with no pending change"],
    332: ["AdminChangeExpired",                                 "pending admin change passed its expiry ledger"],
    333: ["DelegateNotSet",                                     "delegate storage entry missing"],
    334: ["DelegateUnauthorizedToken",                          "delegate called with the wrong token"],
    335: ["DelegatedOutUnderflow",                              "delegated balance would go negative"],
    336: ["DelegateInvalidAmount",                              "delegate call passed an invalid (≤0) amount"],
  };

  /**
   * warpdrive-shared HandlerError codes (300-series mirror VerifyError, 500-series handler-local).
   * Source: warpdrive-contracts/packages/shared/src/interfaces/handler.rs + project-local error.rs.
   */
  const HANDLER_ERRORS = {
    301: ["InvalidSignature",         "signature did not verify against the claimed signer"],
    302: ["SignerNotRegistered",      "signer pubkey is not in the security contract's signer set"],
    303: ["InsufficientWeight",       "signatures collected total less than the required threshold"],
    304: ["EmptySignatures",          "the verification call was given zero signatures"],
    305: ["LengthMismatch",           "len(signers) != len(signatures)"],
    306: ["SignersNotOrdered",        "signers list is not strictly ascending"],
    307: ["ZeroRequiredWeight",       "the security contract reports a required weight of 0"],
    501: ["EventAlreadySeen",         "this event_id was already processed (replay)"],
    502: ["InvalidReferenceBlock",    "reference_block too old or in the future"],
    503: ["InvalidEnvelope",          "envelope failed to XDR-decode, or post-condition violated"],
    504: ["UnknownVerificationError", "verification contract returned an unexpected non-VerifyError"],
    505: ["OtherInvocationError",     "host-side invocation failure when calling the verification contract"],
    600: ["Paused",                   "handler is paused — wait for unpause"],
    601: ["UsdcLeak",                 "post-condition: handler still holds USDC after action"],
  };

  /**
   * Stellar Asset Contract (SAC) host-side error codes (1..15). These come
   * from soroban-env-host/src/builtin_contracts/contract_error.rs and are
   * what every token contract panics with — including pool deposits failing
   * mid-transfer.
   */
  const SAC_ERRORS = {
    1:  ["_Reserved1",                "reserved (legacy InternalError; host internal error)"],
    2:  ["OperationNotSupportedError","operation invalid for this asset (e.g. burn on native XLM)"],
    3:  ["AlreadyInitializedError",   "asset contract already initialized"],
    4:  ["UnauthorizedError",         "the caller is not authorized for this token"],
    5:  ["AuthenticationError",       "require_auth failed (Freighter declined or wrong signer)"],
    6:  ["AccountMissingError",       "the classic account does not exist on the Stellar network"],
    7:  ["AccountIsNotClassic",       "expected a classic account but got a contract address"],
    8:  ["NegativeAmountError",       "negative amount is not allowed"],
    9:  ["AllowanceError",            "approve / spend allowance failure"],
    10: ["BalanceError",              "insufficient balance — wallet doesn't hold enough of the token for this transfer"],
    11: ["BalanceDeauthorizedError", "balance is deauthorized; the issuer must reauthorize"],
    12: ["OverflowError",             "i128 overflow inside the token contract"],
    13: ["TrustlineMissingError",     "destination has no trustline for this asset"],
    14: ["InsufficientAccountReserve","classic account would go below the base reserve after this op"],
    15: ["TooManyAccountSubentries",  "classic account is at the subentry limit (max trustlines / offers)"],
  };

  /**
   * Extract `Error(Contract, #N)` plus the failing contract id from a simulate
   * error string and produce a one-line friendly description. Falls back to
   * the raw text when nothing matches.
   */
  function decodeContractError(raw) {
    if (!raw) return "(no error message)";
    const text = String(raw);
    const codeMatch = text.match(/Error\(Contract,\s*#(\d+)\)/);
    if (!codeMatch) {
      // Not a contract error — look for the next-most-common case: a Rust
      // panic that surfaces as a WasmVm trap. The diagnostic line usually
      // names the entrypoint, e.g. `data:["VM call trapped: ...", swap]`.
      if (/UnreachableCodeReached|WasmVm[^"]*InvalidAction/.test(text)) {
        const fnMatch = text.match(/VM call trapped:[^,]+,\s*([a-z_][a-z0-9_]*)/i);
        const fn = fnMatch ? fnMatch[1] : "contract call";
        return `${fn}: Rust panic (WasmVm trap). Most common cause: passing 0 (or "0") to an `
             + `Option<i128> min/desired argument that the pool requires to be > 0 — leave it `
             + `blank for "no minimum".`;
      }
      return text;
    }
    const code = Number(codeMatch[1]);
    const ctrMatch = text.match(/contract:(C[A-Z2-7]{55})/);
    const ctr = ctrMatch ? ctrMatch[1] : null;

    // 300-series codes overlap between Phoenix pool and HandlerError, so
    // dispatch by contract id whenever we can identify it.
    let entry = null, ns = "unknown";
    if (ctr && cfg && ctr === cfg.blended_pool_id) {
      entry = PHOENIX_POOL_ERRORS[code]; ns = "phoenix-pool";
    } else if (ctr && cfg && ctr === cfg.handler_id) {
      entry = HANDLER_ERRORS[code]; ns = "handler";
    } else {
      entry = PHOENIX_POOL_ERRORS[code] || HANDLER_ERRORS[code] || SAC_ERRORS[code];
      ns = PHOENIX_POOL_ERRORS[code] ? "phoenix-pool"
         : (HANDLER_ERRORS[code] ? "handler"
         : (SAC_ERRORS[code] ? "stellar-asset-contract" : "unknown"));
    }
    // Pool / handler calls cascade into token contracts; if neither map
    // has the code but it's in 1..15, it's almost certainly an SAC error
    // from an inner token call (e.g. balance / auth check).
    if (!entry && code >= 1 && code <= 15) {
      entry = SAC_ERRORS[code]; ns = "stellar-asset-contract (inner call)";
    }
    if (!entry) {
      return `Error #${code} (${ctr ? shortAddr(ctr) : "?"}, unknown code)`;
    }
    const [name, hint] = entry;
    const where = ctr ? `${ns} ${shortAddr(ctr)}` : ns;
    return `Error #${code} ${name} (${where}) — ${hint}`;
  }

  // ---- Soroban read helpers ---------------------------------------------

  /** Run a read-only contract call via simulateTransaction; return native JS. */
  async function readContract(contractId, method, args = []) {
    let account;
    try {
      account = await server.getAccount(cfg.source_account);
    } catch (e) {
      throw new Error(`source_account ${cfg.source_account} not found on chain: ${e.message || e}`);
    }
    const contract = new sdk.Contract(contractId);
    const tx = new sdk.TransactionBuilder(account, {
      fee: "100",
      networkPassphrase: cfg.network_passphrase,
    })
      .addOperation(contract.call(method, ...args))
      .setTimeout(30)
      .build();

    const sim = await server.simulateTransaction(tx);
    const isErr = rpc.Api && rpc.Api.isSimulationError
      ? rpc.Api.isSimulationError(sim)
      : Boolean(sim.error);
    if (isErr) {
      throw new Error(`simulate ${shortAddr(contractId)}::${method} failed: ${decodeContractError(sim.error)}`);
    }
    if (!sim.result || !sim.result.retval) {
      throw new Error(`simulate ${shortAddr(contractId)}::${method} returned no retval`);
    }
    return sdk.scValToNative(sim.result.retval);
  }

  async function getLatestLedger() {
    const info = await server.getLatestLedger();
    return info.sequence;
  }

  // ---- Formatting -------------------------------------------------------

  const TOKEN_DECIMALS = 7;

  function fmtAmt(raw, decimals = TOKEN_DECIMALS) {
    if (raw === null || raw === undefined) return "--";
    let big;
    try { big = BigInt(raw); }
    catch (_) { return String(raw); }
    const neg = big < 0n;
    const abs = neg ? -big : big;
    const scale = BigInt(10) ** BigInt(decimals);
    const whole = abs / scale;
    const frac = abs % scale;
    const fracStr = frac.toString().padStart(decimals, "0").replace(/0+$/, "");
    const out = fracStr ? `${whole}.${fracStr}` : `${whole}`;
    return neg ? `-${out}` : out;
  }

  function shortAddr(addr, head = 6, tail = 4) {
    if (!addr) return "--";
    const s = String(addr);
    if (s.length <= head + tail + 3) return s;
    return `${s.slice(0, head)}…${s.slice(-tail)}`;
  }

  function fmtRelative(ts) {
    if (!ts) return "--";
    const tsNum = Number(ts);
    if (!tsNum) return "--";
    const now = Math.floor(Date.now() / 1000);
    const delta = now - tsNum;
    if (delta < 0)     return "future?";
    if (delta < 60)    return `${delta}s ago`;
    if (delta < 3600)  return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  function bpsPct(bps) {
    if (bps === null || bps === undefined) return "--";
    return `${(Number(bps) / 100).toFixed(2)}%`;
  }

  /** Blend's PoolStatus discriminator → label. */
  const BLEND_STATUS = {
    0: "Admin-Active",
    1: "Active",
    2: "Admin-OnIce",
    3: "OnIce",
    4: "Admin-Frozen",
    5: "Frozen",
    6: "Setup",
  };

  function addressToScVal(strkey) {
    return new sdk.Address(strkey).toScVal();
  }

  // ---- Render: Phoenix pool ---------------------------------------------

  async function renderPoolPanel() {
    const [poolInfo, delegateState] = await Promise.all([
      readContract(cfg.blended_pool_id, "query_pool_info"),
      readContract(cfg.blended_pool_id, "query_delegate_state"),
    ]);

    const aAddr = poolInfo.asset_a?.address;
    const bAddr = poolInfo.asset_b?.address;

    const labelFor = (addr) => {
      if (!addr) return "--";
      if (addr === cfg.usdc_id) return "USDC";
      if (addr === cfg.xlm_id)  return "XLM";
      if (addr === cfg.blnd_id) return "BLND";
      return shortAddr(addr);
    };
    const aLabel = labelFor(aAddr);
    const bLabel = labelFor(bAddr);

    const liqA = BigInt(delegateState.liquid_a    ?? 0);
    const liqB = BigInt(delegateState.liquid_b    ?? 0);
    const delA = BigInt(delegateState.delegated_a ?? 0);
    const delB = BigInt(delegateState.delegated_b ?? 0);
    const totA = BigInt(delegateState.total_a     ?? poolInfo.asset_a?.amount ?? 0);
    const totB = BigInt(delegateState.total_b     ?? poolInfo.asset_b?.amount ?? 0);

    // Cache so the drift panel can mirror the same snapshot.
    latestPoolState = {
      aAddr, bAddr, aLabel, bLabel,
      liqA, liqB, delA, delB, totA, totB,
      aIsUsdc: aAddr === cfg.usdc_id,
      delegate: delegateState.delegate || null,
    };

    const pct = (liq, tot) =>
      tot === 0n ? "--" : `${(Number((liq * 10000n) / tot) / 100).toFixed(2)}%`;

    $("#tbl-pool tbody").innerHTML = `
      <tr>
        <td>${aLabel}</td>
        <td class="num">${fmtAmt(liqA)}</td>
        <td class="num">${fmtAmt(delA)}</td>
        <td class="num">${fmtAmt(totA)}</td>
        <td class="num">${pct(liqA, totA)}</td>
      </tr>
      <tr>
        <td>${bLabel}</td>
        <td class="num">${fmtAmt(liqB)}</td>
        <td class="num">${fmtAmt(delB)}</td>
        <td class="num">${fmtAmt(totB)}</td>
        <td class="num">${pct(liqB, totB)}</td>
      </tr>
    `;

    $("#pool-meta").innerHTML = `
      <dt>delegate</dt><dd>${delegateState.delegate ? shortAddr(delegateState.delegate) : "<em>not set</em>"}</dd>
      <dt>token_a</dt><dd>${shortAddr(aAddr)} <span class="dim">${aLabel}</span></dd>
      <dt>token_b</dt><dd>${shortAddr(bAddr)} <span class="dim">${bLabel}</span></dd>
      <dt>LP shares</dt><dd class="num">${fmtAmt(poolInfo.asset_lp_share?.amount)}</dd>
      <dt>stake</dt><dd>${shortAddr(poolInfo.stake_address)}</dd>
    `;
    refreshDepositHint();
  }

  // ---- Render: Deployer wallet ------------------------------------------

  async function renderWalletPanel() {
    const acct = cfg.deployer_address || cfg.source_account;
    if (!acct) {
      setText("#wallet-addr", "(no deployer_address in config)");
      return;
    }
    setText("#wallet-addr", acct);

    const balOf = (id) =>
      readContract(id, "balance", [addressToScVal(acct)]).catch(() => null);

    const [xlm, usdc, blnd] = await Promise.all([
      balOf(cfg.xlm_id), balOf(cfg.usdc_id), balOf(cfg.blnd_id),
    ]);
    setText("#wallet-xlm",  fmtAmt(xlm));
    setText("#wallet-usdc", fmtAmt(usdc));
    setText("#wallet-blnd", fmtAmt(blnd));
  }

  // ---- Render: Drift indicator + predicted action -----------------------

  function predictAction(pool, h, blendStatus) {
    if (h.paused) {
      return { cls: "paused", label: "blocked (paused)",
        reason: "handler is paused — verify_xlm reverts with LocalError::Paused (600)" };
    }
    if (blendStatus !== null && Number(blendStatus) > 3) {
      return { cls: "unhealthy", label: "blocked (Blend unhealthy)",
        reason: `Blend pool status ${blendStatus} > 3 (Frozen/Setup) — handler refuses Supply/Withdraw` };
    }

    const liq = pool.aIsUsdc ? pool.liqA : pool.liqB;
    const del = pool.aIsUsdc ? pool.delA : pool.delB;
    const tot = liq + del;

    const floor = BigInt(h.min_total_usdc || 0);
    if (tot < floor) {
      return { cls: "below-floor", label: "no-op (below floor)",
        reason: `total ${fmtAmt(tot)} USDC < floor ${fmtAmt(floor)} USDC` };
    }

    const BPS = 10000n;
    const tgtBps = BigInt(h.target_ratio_bps || 0);
    const bandBps = BigInt(h.rebalance_band_bps || 0);
    const targetLiq = (tot * tgtBps) / BPS;
    const upper = (tot * (tgtBps + bandBps)) / BPS;
    const lower = tgtBps > bandBps ? (tot * (tgtBps - bandBps)) / BPS : 0n;

    const maxCap = BigInt(h.max_rebalance_amount || 0);
    const minFloor = BigInt(h.min_rebalance_amount || 0);
    const clamp = (amount) => (maxCap > 0n && amount > maxCap) ? maxCap : amount;

    if (liq > upper) {
      const natural = liq - targetLiq;
      if (natural < minFloor) {
        return { cls: "noop", label: "no-op (dust)",
          reason: `natural ToBlend ${fmtAmt(natural)} < min_rebalance ${fmtAmt(minFloor)}` };
      }
      const amount = clamp(natural);
      return { cls: "to-blend", label: `ToBlend ${fmtAmt(amount)} USDC`,
        reason: `liquid ${fmtAmt(liq)} > upper band ${fmtAmt(upper)}; clamp=${maxCap === 0n ? "—" : fmtAmt(maxCap)}` };
    }

    if (liq < lower) {
      const natural = targetLiq - liq;
      const principal = BigInt(h.principal_supplied || 0);
      const bounded = natural > principal ? principal : natural;
      if (bounded === 0n) {
        return { cls: "noop", label: "no-op (no principal)",
          reason: `would top up by ${fmtAmt(natural)} but principal_supplied = 0` };
      }
      if (bounded < minFloor) {
        return { cls: "noop", label: "no-op (dust)",
          reason: `bounded FromBlend ${fmtAmt(bounded)} < min_rebalance ${fmtAmt(minFloor)}` };
      }
      const amount = clamp(bounded);
      return { cls: "from-blend", label: `FromBlend ${fmtAmt(amount)} USDC`,
        reason: `liquid ${fmtAmt(liq)} < lower band ${fmtAmt(lower)}; principal cap=${fmtAmt(principal)}` };
    }

    return { cls: "noop", label: "no-op (within band)",
      reason: `liquid ${fmtAmt(liq)} ∈ [${fmtAmt(lower)}, ${fmtAmt(upper)}]` };
  }

  function renderDriftPanel() {
    if (!latestPoolState || !latestHandlerState) return;
    const pool = latestPoolState;
    const h = latestHandlerState;

    const liq = pool.aIsUsdc ? pool.liqA : pool.liqB;
    const del = pool.aIsUsdc ? pool.delA : pool.delB;
    const tot = liq + del;
    const pctNum = tot === 0n ? 0 : Number((liq * 10000n) / tot) / 100;

    setText("#drift-liquid",    `${fmtAmt(liq)} USDC`);
    setText("#drift-delegated", `${fmtAmt(del)} USDC`);
    setText("#drift-total",     `${fmtAmt(tot)} USDC`);
    setText("#drift-pct",       `${pctNum.toFixed(2)}%`);
    setText("#drift-target",    `${bpsPct(h.target_ratio_bps)} ± ${bpsPct(h.rebalance_band_bps)}`);

    // Position elements as percentages along the 0–100% bar.
    const tgtBps = Number(h.target_ratio_bps || 0);
    const bandBps = Number(h.rebalance_band_bps || 0);
    const tgtPct = tgtBps / 100;
    const upperPct = Math.min(100, (tgtBps + bandBps) / 100);
    const lowerPct = Math.max(0, (tgtBps - bandBps) / 100);

    const tline = $("#drift-target-line");
    if (tline) tline.style.left = `${tgtPct}%`;

    const band = $("#drift-band");
    if (band) {
      band.style.left  = `${lowerPct}%`;
      band.style.width = `${upperPct - lowerPct}%`;
    }

    const marker = $("#drift-marker");
    if (marker) {
      marker.style.left = `${Math.min(100, Math.max(0, pctNum))}%`;
      marker.classList.remove("out-of-band", "below-floor");
      const floor = BigInt(h.min_total_usdc || 0);
      if (tot < floor) marker.classList.add("below-floor");
      else if (pctNum < lowerPct || pctNum > upperPct) marker.classList.add("out-of-band");
    }

    const action = predictAction(pool, h, latestBlendStatus);
    const actionEl = $("#drift-action");
    if (actionEl) {
      actionEl.className = action.cls;
      actionEl.textContent = action.label;
    }
    setText("#drift-action-reason", action.reason);
  }

  // ---- Render: Blend position -------------------------------------------

  async function renderBlendPanel() {
    const hState = await readContract(cfg.handler_id, "query_state");
    latestHandlerState = hState;

    // Run dependent reads in parallel — both tolerate failure (no row turns "--").
    const treasury = hState.blnd_treasury;
    const [blendCfg, blndBal] = await Promise.all([
      readContract(cfg.blend_pool_id, "get_config")
        .catch((e) => ({ status: null, _err: e.message || String(e) })),
      treasury
        ? readContract(cfg.blnd_id, "balance", [addressToScVal(treasury)])
            .catch(() => null)
        : Promise.resolve(null),
    ]);

    const status = blendCfg?.status ?? null;
    latestBlendStatus = status;
    const statusLabel = status === null ? "—" : (BLEND_STATUS[Number(status)] || `unknown(${status})`);
    const statusClass = status !== null && Number(status) > 3 ? "err" : "";

    const maxReb = BigInt(hState.max_rebalance_amount || 0);
    const minReb = BigInt(hState.min_rebalance_amount || 0);

    $("#tbl-blend tbody").innerHTML = `
      <tr><td>principal_supplied (USDC)</td><td class="num">${fmtAmt(hState.principal_supplied)}</td></tr>
      <tr><td>BLND in treasury</td><td class="num">${fmtAmt(blndBal)}</td></tr>
      <tr><td>blend status</td><td class="${statusClass}">${statusLabel}</td></tr>
      <tr><td>last rebalance</td><td>${fmtRelative(hState.last_rebalance_ts)}</td></tr>
      <tr><td>last harvest</td><td>${fmtRelative(hState.last_harvest_ts)}</td></tr>
      <tr><td>paused</td><td class="${hState.paused ? "err" : ""}">${hState.paused ? "YES" : "no"}</td></tr>
      <tr><td>target / band</td><td>${bpsPct(hState.target_ratio_bps)} &plusmn; ${bpsPct(hState.rebalance_band_bps)}</td></tr>
      <tr><td>floor (min_total_usdc)</td><td class="num">${fmtAmt(hState.min_total_usdc)}</td></tr>
      <tr><td>max rebalance / tx</td><td class="num">${
        maxReb === 0n ? "<em>unlimited</em>" : fmtAmt(maxReb)
      }</td></tr>
      <tr><td>min rebalance / tx</td><td class="num">${
        minReb === 0n ? "<em>no floor</em>" : fmtAmt(minReb)
      }</td></tr>
      <tr><td>cooldown</td><td>${hState.rebalance_cooldown_secs}s</td></tr>
      <tr><td>admin</td><td>${shortAddr(hState.admin)}</td></tr>
      <tr><td>pending admin</td><td>${hState.pending_admin ? shortAddr(hState.pending_admin) : "<em>none</em>"}</td></tr>
      <tr><td>blnd treasury</td><td>${shortAddr(treasury)}</td></tr>
      <tr><td>version</td><td>${hState.version}</td></tr>
    `;
  }

  // ---- Render: Event log ------------------------------------------------

  // Project-specific event names that get a class hook for accent colours.
  const KNOWN_EVENT_TOPICS = new Set([
    "RebalanceExecuted",
    "HarvestCompleted",
    "BadDebtDetected",
    "EmergencyUnwound",
    "PauseToggled",
    "ConfigUpdated",
    "AddressConfigUpdated",
    "Verified",
    "ContractUpgraded",
  ]);

  // ---- Render: Pool events ----------------------------------------------

  // Topics emitted by phoenix-pool-blended that we want to surface as
  // "trigger sources" — every one of these makes the WarpDrive circuit
  // emit a Rebalance payload. Maps event topic → CSS class hook.
  const POOL_TOPIC_HOOKS = new Map([
    ["swap",                  "pool-swap"],
    ["provide_liquidity",     "pool-provide_liquidity"],
    ["withdraw_liquidity",    "pool-withdraw_liquidity"],
    ["donate",                "pool-donate"],
    ["withdraw_to_delegate",  "pool-withdraw_to_delegate"],
    ["deposit_from_delegate", "pool-deposit_from_delegate"],
    ["set_delegate",          "pool-set_delegate"],
  ]);

  /**
   * Soroban-RPC's `getEvents` scans at most ~10_000 ledgers per call from
   * `startLedger`; events past that window are reached only via the
   * returned `cursor`. If the lookback exceeds the scan cap, the first
   * page can come back empty even when fresh events exist — we keep
   * paging until the cursor stops advancing (`MAX_PAGES` is a runaway
   * guard since cursor format is opaque).
   */
  const EVENTS_PAGE_LIMIT = 200;
  const EVENTS_MAX_PAGES  = 16;

  async function fetchEventsPaginated(contractId) {
    const latest = await getLatestLedger();
    const lookback = Number(cfg.event_lookback_ledgers ?? 17280);
    const startLedger = Math.max(1, latest - lookback);
    const baseFilters = [{ type: "contract", contractIds: [contractId] }];
    const all = [];
    let cursor = null;
    let prevCursor = "";
    for (let page = 0; page < EVENTS_MAX_PAGES; page++) {
      const req = cursor
        ? { filters: baseFilters, cursor,      limit: EVENTS_PAGE_LIMIT }
        : { filters: baseFilters, startLedger, limit: EVENTS_PAGE_LIMIT };
      const res = await server.getEvents(req);
      const events = res.events || [];
      all.push(...events);
      cursor = res.cursor;
      if (!cursor || cursor === prevCursor) break;
      prevCursor = cursor;
    }
    return all;
  }

  async function fetchPoolEvents() {
    return fetchEventsPaginated(cfg.blended_pool_id);
  }

  async function renderPoolEventsPanel() {
    const events = await fetchPoolEvents();
    const tbody = $("#tbl-pool-events tbody");
    if (!events.length) {
      tbody.innerHTML = `<tr><td colspan="4" class="no-data">no events in lookback window — pool has not been touched yet</td></tr>`;
      return;
    }
    const rows = events
      .slice()
      .reverse()
      .map((ev) => {
        const { topicName } = decodeEvent(ev);
        const clsHook = POOL_TOPIC_HOOKS.get(topicName) || "pool-other";
        const when = ev.ledgerClosedAt
          ? fmtRelative(Math.floor(new Date(ev.ledgerClosedAt).getTime() / 1000))
          : "--";
        return `<tr class="${clsHook}">
          <td>${ev.ledger}</td>
          <td>${when}</td>
          <td>${topicName}</td>
          <td>${txLink(ev)}</td>
        </tr>`;
      });
    tbody.innerHTML = rows.join("");
  }

  async function fetchHandlerEvents() {
    return fetchEventsPaginated(cfg.handler_id);
  }

  function decodeEvent(ev) {
    let topicName = "(unnamed)";
    try {
      if (ev.topic && ev.topic.length > 0) {
        const first = sdk.scValToNative(ev.topic[0]);
        if (typeof first === "string") topicName = first;
      }
    } catch (_) { /* ignore decode errors */ }

    let data = null;
    try {
      data = ev.value ? sdk.scValToNative(ev.value) : null;
    } catch (_) { /* ignore */ }

    return { topicName, data };
  }

  function bigIntReplacer(_k, v) { return typeof v === "bigint" ? v.toString() : v; }

  function dataSummary(topicName, data) {
    if (!data) return `<span class="no-data">(no payload)</span>`;
    switch (topicName) {
      case "RebalanceExecuted":
        return `direction=<b>${data.direction}</b> ` +
               `amount=${fmtAmt(data.amount)} ` +
               `liquid_after=${fmtAmt(data.liquid_after)} ` +
               `delegated_after=${fmtAmt(data.delegated_after)} ` +
               `principal_after=${fmtAmt(data.principal_after)}`;
      case "HarvestCompleted":
        return `interest_donated=${fmtAmt(data.interest_donated)} ` +
               `blnd_routed=${fmtAmt(data.blnd_routed)} ` +
               `principal_after=${fmtAmt(data.principal_after)}`;
      case "BadDebtDetected":
        return `previous=${fmtAmt(data.previous_principal)} ` +
               `redeemable=${fmtAmt(data.redeemable)} ` +
               `shortfall=${fmtAmt(data.shortfall)}`;
      case "EmergencyUnwound":
        return `redeemed=${fmtAmt(data.redeemed)} ` +
               `principal_before=${fmtAmt(data.principal_before)}`;
      case "PauseToggled":
        return `paused=<b>${data.paused}</b>`;
      case "ConfigUpdated":
        return `field=<b>${data.field}</b> value=${data.value}`;
      case "AddressConfigUpdated":
        return `field=<b>${data.field}</b> value=${shortAddr(data.value)}`;
      case "Verified":
        return `event_id=${shortAddr(data.event_id ?? data, 8, 6)}`;
      case "ContractUpgraded":
        return `version=${data.version}`;
      default:
        try { return `<code>${JSON.stringify(data, bigIntReplacer)}</code>`; }
        catch (_) { return `<span class="no-data">(opaque)</span>`; }
    }
  }

  function txLink(ev) {
    const base = cfg.stellar_expert_base || "https://stellar.expert/explorer/testnet";
    const h = ev.txHash || ev.transactionHash;
    if (!h) return "--";
    return `<a href="${base}/tx/${h}" target="_blank" rel="noopener">${h.slice(0, 8)}…</a>`;
  }

  async function renderEventsPanel() {
    const events = await fetchHandlerEvents();
    const tbody = $("#tbl-events tbody");
    if (!events.length) {
      tbody.innerHTML = `<tr><td colspan="5" class="no-data">no events in lookback window</td></tr>`;
      return;
    }

    const rows = events
      .slice()
      .reverse() // newest first
      .map((ev) => {
        const { topicName, data } = decodeEvent(ev);
        const clsHook = KNOWN_EVENT_TOPICS.has(topicName) ? `evt-${topicName}` : "evt-other";
        const when = ev.ledgerClosedAt
          ? fmtRelative(Math.floor(new Date(ev.ledgerClosedAt).getTime() / 1000))
          : "--";
        return `<tr class="${clsHook}">
          <td>${ev.ledger}</td>
          <td>${when}</td>
          <td>${topicName}</td>
          <td>${dataSummary(topicName, data)}</td>
          <td>${txLink(ev)}</td>
        </tr>`;
      });

    tbody.innerHTML = rows.join("");
  }

  // ---- Tick -------------------------------------------------------------

  async function tick() {
    const t0 = performance.now();
    // Pool + Blend reads must finish before drift can compute, so run them
    // first; everything else races in parallel.
    const stateResults = await Promise.allSettled([
      renderPoolPanel(),
      renderBlendPanel(),
    ]);
    // Drift uses the cached snapshots from the two reads above.
    try { renderDriftPanel(); } catch (e) { console.error("drift render:", e); }

    const auxResults = await Promise.allSettled([
      renderWalletPanel(),
      renderPoolEventsPanel(),
      renderEventsPanel(),
      refreshUserLpBalance(),
    ]);
    const results = stateResults.concat(auxResults);
    const failures = results.filter((r) => r.status === "rejected");
    if (failures.length === 0) {
      markConnected(true);
      setErr("");
    } else {
      markConnected(false);
      const msgs = failures.map((f) => String(f.reason && f.reason.message || f.reason)).join(" | ");
      setErr(msgs);
      console.error("tick failures:", failures);
    }
    const dt = ((performance.now() - t0) / 1000).toFixed(2);
    setText("#last-tick", `${new Date().toISOString().split("T")[1].split(".")[0]}Z (${dt}s)`);
  }

  function markConnected(ok) {
    const dot = $("#conn-dot");
    const txt = $("#conn-text");
    if (!dot || !txt) return;
    dot.classList.toggle("live", ok);
    dot.classList.toggle("err", !ok);
    txt.textContent = ok ? "live" : "error";
  }

  // ---- Freighter wallet + signed transactions ---------------------------

  // Per-user state. `userAddress` is the Freighter-connected G-strkey.
  let userAddress = null;

  // Cached pool LP share-token address (lazy-fetched via query_share_token_address).
  let shareTokenAddress = null;

  // Returns the freighter-api object once the CDN bundle has finished loading.
  // The bundle is a UMD that publishes itself as `window.freighterApi`; the
  // extension itself does NOT inject a global, so the library is what we talk
  // to here, and `isConnected()` is what reports whether the extension is
  // installed.
  async function getFreighterApi({ timeoutMs = 3000 } = {}) {
    const start = Date.now();
    while (!window.freighterApi) {
      if (Date.now() - start > timeoutMs) return null;
      await new Promise((r) => setTimeout(r, 50));
    }
    return window.freighterApi;
  }

  /** Unwrap a `{...value, error?}` Freighter result; throw on `error`. */
  function unwrap(res) {
    if (res && typeof res === "object" && res.error) {
      const msg = res.error.message || res.error;
      throw new Error(`Freighter: ${msg}`);
    }
    return res;
  }

  async function connectFreighter() {
    try {
      const fr = await getFreighterApi();
      if (!fr) {
        alert("Freighter library failed to load. Check your network and reload.");
        return;
      }
      const probe = unwrap(await fr.isConnected());
      if (!probe.isConnected) {
        alert("Freighter not detected. Install the extension from https://freighter.app/ and reload.");
        return;
      }
      const access = unwrap(await fr.requestAccess());
      if (!access.address) throw new Error("Freighter returned no address");
      userAddress = access.address;
      renderWalletConnected();
      await tick();
    } catch (e) {
      setErr(String(e.message || e));
      console.error(e);
    }
  }

  function disconnectFreighter() {
    userAddress = null;
    renderWalletConnected();
  }

  function renderWalletConnected() {
    const btnConnect = $("#btn-connect");
    const display    = $("#wallet-display");
    const addrEl     = $("#wallet-addr-display");
    const actionsRow = $("#row-actions");
    if (userAddress) {
      btnConnect.hidden = true;
      display.hidden = false;
      addrEl.textContent = shortAddr(userAddress, 6, 4);
      actionsRow.hidden = false;
    } else {
      btnConnect.hidden = false;
      display.hidden = true;
      addrEl.textContent = "--";
      actionsRow.hidden = true;
    }
  }

  // Soroban-CLI-style arg builders.
  const Opt = {
    none: () => sdk.xdr.ScVal.scvVoid(),
    i128: (val) => (val === null || val === undefined)
      ? sdk.xdr.ScVal.scvVoid()
      : sdk.nativeToScVal(BigInt(val), { type: "i128" }),
    i64:  (val) => (val === null || val === undefined)
      ? sdk.xdr.ScVal.scvVoid()
      : sdk.nativeToScVal(BigInt(val), { type: "i64" }),
    u64:  (val) => (val === null || val === undefined)
      ? sdk.xdr.ScVal.scvVoid()
      : sdk.nativeToScVal(BigInt(val), { type: "u64" }),
  };
  const ArgAddress = (s) => new sdk.Address(s).toScVal();
  const ArgI128    = (v) => sdk.nativeToScVal(BigInt(v), { type: "i128" });
  const ArgBool    = (v) => sdk.xdr.ScVal.scvBool(Boolean(v));

  /** Convert a user-entered decimal string to a raw i128 BigInt (7 dp). */
  function parseAmount7(input) {
    if (input === null || input === undefined) return null;
    const s = String(input).trim();
    if (!s) return null;
    if (!/^\d+(\.\d{1,7})?$/.test(s)) {
      throw new Error(`invalid amount "${s}" — use up to 7 decimals`);
    }
    const [whole, frac = ""] = s.split(".");
    const fracPadded = (frac + "0000000").slice(0, 7);
    return BigInt(whole) * 10000000n + BigInt(fracPadded);
  }

  /** Parse 0..10_000 bps; empty string -> null. */
  function parseSlippageBps(input) {
    if (input === null || input === undefined) return null;
    const s = String(input).trim();
    if (!s) return null;
    if (!/^\d+$/.test(s)) throw new Error(`invalid bps "${s}" — use a whole number 0..10000`);
    const n = Number(s);
    if (n < 0 || n > 10000) throw new Error(`bps "${s}" out of range (0..10000)`);
    return n;
  }

  /**
   * Update the "Pool ratio: N XLM / USDC" hint on the deposit tab. Reads from
   * `latestPoolState` so it can run after every tick without re-querying.
   */
  function refreshDepositHint() {
    const el = $("#deposit-pool-ratio");
    if (!el) return;
    if (!latestPoolState) { el.textContent = "--"; return; }
    const { totA, totB, aIsUsdc } = latestPoolState;
    if (totA <= 0n || totB <= 0n) { el.textContent = "(empty pool)"; return; }
    // Both reserves share the same 7-dp scaling, so the ratio is unit-free.
    const usdcReserve = aIsUsdc ? totA : totB;
    const xlmReserve  = aIsUsdc ? totB : totA;
    // Render with 6 dp via integer division on a 1e6 multiplier.
    const scaled = (xlmReserve * 1000000n) / usdcReserve;
    const whole = scaled / 1000000n;
    const frac  = (scaled % 1000000n).toString().padStart(6, "0").replace(/0+$/, "");
    el.textContent = frac ? `${whole}.${frac}` : `${whole}`;
  }

  /**
   * Build a soroban transaction, simulate, have Freighter sign, submit, poll
   * until completion. Throws on any failure. Returns the tx hash on success.
   */
  async function buildAndSubmit(operationFn, { onPending } = {}) {
    if (!userAddress) throw new Error("connect Freighter first");
    const fr = await getFreighterApi();
    if (!fr) throw new Error("Freighter library not loaded");

    const userAccount = await server.getAccount(userAddress);

    let tx = new sdk.TransactionBuilder(userAccount, {
      fee: sdk.BASE_FEE,
      networkPassphrase: cfg.network_passphrase,
    })
      .addOperation(operationFn())
      .setTimeout(60)
      .build();

    const sim = await server.simulateTransaction(tx);
    if (rpc.Api && rpc.Api.isSimulationError && rpc.Api.isSimulationError(sim)) {
      throw new Error(`simulate failed: ${decodeContractError(sim.error)}`);
    }
    const prepared = (rpc.assembleTransaction
      ? rpc.assembleTransaction(tx, sim)
      : sdk.SorobanRpc.assembleTransaction(tx, sim)
    ).build();

    if (onPending) onPending("signing");
    const signedRes = unwrap(await fr.signTransaction(prepared.toXDR(), {
      networkPassphrase: cfg.network_passphrase,
      address: userAddress,
    }));
    const signedXdr = signedRes.signedTxXdr;
    if (!signedXdr) throw new Error("Freighter returned no signed XDR");
    const submittable = sdk.TransactionBuilder.fromXDR(signedXdr, cfg.network_passphrase);

    if (onPending) onPending("submitting");
    let send = await server.sendTransaction(submittable);
    if (send.errorResult) {
      throw new Error(`sendTransaction failed: ${JSON.stringify(send.errorResult)}`);
    }

    if (onPending) onPending("waiting for confirmation");
    const hash = send.hash;
    let result;
    for (let i = 0; i < 30; i++) {
      await new Promise((r) => setTimeout(r, 2000));
      result = await getTxStatus(hash);
      if (result.status !== "NOT_FOUND" && result.status !== "PENDING") break;
    }
    if (!result || result.status !== "SUCCESS") {
      throw new Error(`tx ${hash} ended status=${result?.status || "TIMEOUT"}${result?.errorMessage ? `: ${result.errorMessage}` : ""}`);
    }
    return hash;
  }

  /**
   * Poll the RPC server for a transaction's status, falling back to a raw
   * JSON-RPC request when the SDK's XDR decoder chokes on protocol fields
   * it doesn't recognize (classic symptom: "Bad union switch: N" thrown
   * AFTER the tx has already landed on chain). We only ever consume
   * `status` here, so we don't need any of the XDR-decoded fields.
   */
  async function getTxStatus(hash) {
    try {
      const r = await server.getTransaction(hash);
      return { status: r.status };
    } catch (e) {
      const msg = String(e.message || e);
      if (!/Bad union switch|XDR/i.test(msg)) throw e;
      // Fall back to raw JSON-RPC. soroban-rpc returns `status` as a plain
      // string, no XDR involved. We deliberately ignore `resultXdr` /
      // `resultMetaXdr` since those are what tripped the decoder.
      const res = await fetch(cfg.rpc_url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          jsonrpc: "2.0", id: 1, method: "getTransaction",
          params: { hash },
        }),
      });
      const json = await res.json();
      if (json.error) throw new Error(`raw getTransaction: ${json.error.message || JSON.stringify(json.error)}`);
      return {
        status: json.result?.status || "UNKNOWN",
        errorMessage: "SDK decode failed; result fetched via raw RPC",
      };
    }
  }

  function reportResult(panelId, classes, html) {
    const el = $(panelId);
    if (!el) return;
    el.className = "action-result " + (classes || "");
    el.innerHTML = html;
  }

  function txExplorerLink(hash) {
    const base = cfg.stellar_expert_base || "https://stellar.expert/explorer/testnet";
    return `<a href="${base}/tx/${hash}" target="_blank" rel="noopener">${hash.slice(0,8)}…</a>`;
  }

  // ---- Action: Deposit (provide_liquidity) ----------------------------

  async function submitDeposit(ev) {
    ev.preventDefault();
    const btn = ev.target.querySelector("button[type=submit]");
    const result = "#deposit-result";
    try {
      const usdcA = parseAmount7($("#deposit-usdc").value);
      const xlmA  = parseAmount7($("#deposit-xlm").value);
      if (!usdcA || !xlmA) throw new Error("USDC and XLM amounts are required");
      // Pool's `validate_int_parameters!` macro rejects Some(0); treat 0 as "no min".
      const minUsdcRaw = parseAmount7($("#deposit-min-usdc").value);
      const minXlmRaw  = parseAmount7($("#deposit-min-xlm").value);
      const minUsdc = minUsdcRaw && minUsdcRaw > 0n ? minUsdcRaw : null;
      const minXlm  = minXlmRaw  && minXlmRaw  > 0n ? minXlmRaw  : null;
      const slippageBps = parseSlippageBps($("#deposit-slippage").value);
      // Token order on the pool: USDC=token_a (sorts alphabetically below XLM).
      // pool.provide_liquidity(depositor, desired_a, min_a, desired_b, min_b, custom_slippage_bps, deadline, auto_stake)
      btn.classList.add("pending"); btn.disabled = true;
      reportResult(result, "", "preparing...");
      const hash = await buildAndSubmit(() => {
        const c = new sdk.Contract(cfg.blended_pool_id);
        return c.call(
          "provide_liquidity",
          ArgAddress(userAddress),
          Opt.i128(usdcA),
          Opt.i128(minUsdc),
          Opt.i128(xlmA),
          Opt.i128(minXlm),
          slippageBps == null ? Opt.none() : Opt.i64(slippageBps),
          Opt.none(),         // deadline
          ArgBool(false),     // auto_stake
        );
      }, { onPending: (s) => reportResult(result, "", `${s}...`) });
      reportResult(result, "ok", `provided liquidity. tx ${txExplorerLink(hash)}`);
      await tick();
    } catch (e) {
      reportResult(result, "error", String(e.message || e));
      console.error(e);
    } finally {
      btn.classList.remove("pending"); btn.disabled = false;
    }
  }

  // ---- Action: Withdraw (withdraw_liquidity) --------------------------

  async function submitWithdraw(ev) {
    ev.preventDefault();
    const btn = ev.target.querySelector("button[type=submit]");
    const result = "#withdraw-result";
    try {
      const shares = parseAmount7($("#withdraw-shares").value);
      if (!shares) throw new Error("LP share amount is required");
      const minUsdc = parseAmount7($("#withdraw-min-usdc").value) || 0n;
      const minXlm  = parseAmount7($("#withdraw-min-xlm").value)  || 0n;

      btn.classList.add("pending"); btn.disabled = true;
      reportResult(result, "", "preparing...");
      const hash = await buildAndSubmit(() => {
        const c = new sdk.Contract(cfg.blended_pool_id);
        // withdraw_liquidity(recipient, share_amount, min_a, min_b, deadline, auto_unstake)
        return c.call(
          "withdraw_liquidity",
          ArgAddress(userAddress),
          ArgI128(shares),
          ArgI128(minUsdc),
          ArgI128(minXlm),
          Opt.none(),         // deadline
          Opt.none(),         // auto_unstake
        );
      }, { onPending: (s) => reportResult(result, "", `${s}...`) });
      reportResult(result, "ok", `withdrew liquidity. tx ${txExplorerLink(hash)}`);
      await tick();
    } catch (e) {
      reportResult(result, "error", String(e.message || e));
      console.error(e);
    } finally {
      btn.classList.remove("pending"); btn.disabled = false;
    }
  }

  // ---- Action: Swap ----------------------------------------------------

  function updateSwapTokenLabels() {
    const dir = $("#swap-direction").value;
    const [offer, ask] = dir === "xlm-to-usdc" ? ["XLM", "USDC"] : ["USDC", "XLM"];
    setText("#swap-offer-token", offer);
    setText("#swap-ask-token",   ask);
  }

  async function submitSwap(ev) {
    ev.preventDefault();
    const btn = ev.target.querySelector("button[type=submit]");
    const result = "#swap-result";
    try {
      const offer = parseAmount7($("#swap-offer").value);
      if (!offer) throw new Error("offer amount is required");
      const minAskRaw = parseAmount7($("#swap-min-ask").value);
      const minAsk    = minAskRaw && minAskRaw > 0n ? minAskRaw : null;
      const maxSpread = $("#swap-max-spread").value.trim();
      const offerAsset = $("#swap-direction").value === "xlm-to-usdc"
        ? cfg.xlm_id
        : cfg.usdc_id;

      btn.classList.add("pending"); btn.disabled = true;
      reportResult(result, "", "preparing...");
      const hash = await buildAndSubmit(() => {
        const c = new sdk.Contract(cfg.blended_pool_id);
        // swap(sender, offer_asset, offer_amount, ask_asset_min_amount, max_spread_bps, deadline, max_allowed_fee_bps)
        return c.call(
          "swap",
          ArgAddress(userAddress),
          ArgAddress(offerAsset),
          ArgI128(offer),
          Opt.i128(minAsk),
          maxSpread ? Opt.i64(maxSpread) : Opt.none(),
          Opt.none(),         // deadline
          Opt.none(),         // max_allowed_fee_bps
        );
      }, { onPending: (s) => reportResult(result, "", `${s}...`) });
      reportResult(result, "ok", `swap complete. tx ${txExplorerLink(hash)}`);
      await tick();
    } catch (e) {
      reportResult(result, "error", String(e.message || e));
      console.error(e);
    } finally {
      btn.classList.remove("pending"); btn.disabled = false;
    }
  }

  // ---- Optional: read user's LP balance for the withdraw "max" hint ----

  async function refreshUserLpBalance() {
    if (!userAddress) return;
    if (!shareTokenAddress) {
      try {
        shareTokenAddress = await readContract(cfg.blended_pool_id, "query_share_token_address");
      } catch (e) { return; }
    }
    try {
      const bal = await readContract(shareTokenAddress, "balance", [addressToScVal(userAddress)]);
      setText("#withdraw-lp-bal", fmtAmt(bal));
      const maxBtn = $("#btn-withdraw-max");
      maxBtn._maxRaw = bal; // stash for the click handler
    } catch (_) { /* ignore */ }
  }

  // ---- Tab + form wiring ----------------------------------------------

  function wireTabs() {
    document.querySelectorAll(".tab").forEach((btn) => {
      btn.addEventListener("click", () => {
        const tab = btn.dataset.tab;
        document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b === btn));
        document.querySelectorAll(".tab-body").forEach((body) => {
          body.classList.toggle("active", body.dataset.tabBody === tab);
        });
      });
    });
  }

  function wireActionForms() {
    $("#btn-connect").addEventListener("click", connectFreighter);
    $("#btn-disconnect").addEventListener("click", disconnectFreighter);
    $("#form-deposit").addEventListener("submit", submitDeposit);
    $("#form-withdraw").addEventListener("submit", submitWithdraw);
    $("#form-swap").addEventListener("submit", submitSwap);
    $("#swap-direction").addEventListener("change", updateSwapTokenLabels);
    $("#btn-withdraw-max").addEventListener("click", () => {
      const raw = $("#btn-withdraw-max")._maxRaw;
      if (raw !== undefined && raw !== null) {
        $("#withdraw-shares").value = fmtAmt(raw);
      }
    });

    $("#btn-deposit-match").addEventListener("click", () => {
      if (!latestPoolState) return;
      const { totA, totB, aIsUsdc } = latestPoolState;
      if (totA <= 0n || totB <= 0n) return;
      // ratioFromUsdc = (XLM reserves) / (USDC reserves) in 7-dp units; we
      // multiply user input (raw i128 7-dp) by this ratio without losing
      // precision by staying in BigInt land.
      const usdcReserve = aIsUsdc ? totA : totB;
      const xlmReserve  = aIsUsdc ? totB : totA;
      const usdcStr = $("#deposit-usdc").value.trim();
      const xlmStr  = $("#deposit-xlm").value.trim();
      if (usdcStr && !xlmStr) {
        const usdcRaw = parseAmount7(usdcStr);
        const xlmRaw  = usdcRaw * xlmReserve / usdcReserve;
        $("#deposit-xlm").value = fmtAmt(xlmRaw);
      } else if (xlmStr && !usdcStr) {
        const xlmRaw  = parseAmount7(xlmStr);
        const usdcRaw = xlmRaw * usdcReserve / xlmReserve;
        $("#deposit-usdc").value = fmtAmt(usdcRaw);
      } else {
        // Both filled: rebalance XLM to match USDC.
        const usdcRaw = parseAmount7(usdcStr || "0");
        if (usdcRaw && usdcRaw > 0n) {
          $("#deposit-xlm").value = fmtAmt(usdcRaw * xlmReserve / usdcReserve);
        }
      }
    });
    updateSwapTokenLabels();
  }

  // ---- Init -------------------------------------------------------------

  async function init() {
    try { cfg = await loadConfig(); }
    catch (e) { setErr(String(e.message || e)); console.error(e); return; }

    try { await waitForSdk(); }
    catch (e) { setErr(String(e.message || e)); return; }

    try {
      server = new rpc.Server(cfg.rpc_url, { allowHttp: false });
    } catch (e) {
      setErr(`rpc server init: ${e.message || e}`);
      return;
    }

    setText("#net-badge", isMainnet(cfg.network_passphrase) ? "MAINNET" : "TESTNET");
    setText("#handler-addr", `handler: ${cfg.handler_id}`);
    setText("#pool-addr",    `pool: ${cfg.blended_pool_id}`);

    wireTabs();
    wireActionForms();
    renderWalletConnected();

    await tick();
    pollTimer = setInterval(tick, cfg.refresh_interval_ms || 10000);
  }

  function isMainnet(passphrase) {
    return passphrase === "Public Global Stellar Network ; September 2015";
  }

  window.addEventListener("DOMContentLoaded", init);
})();
