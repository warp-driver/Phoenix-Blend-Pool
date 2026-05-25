# Ed25519 Verification Contract

The Ed25519 Verification contract performs SEP-0053 ed25519 signature verification for Soroban-native WarpDrive attestations. It is the cryptographic core of the verification pipeline for processes that operate entirely within the Stellar ecosystem -- it takes an envelope and a set of signatures, verifies each signature against its expected signer using Soroban's native `ed25519_verify` precompile, confirms that each signer is registered in the Ed25519 Security contract, and checks that the cumulative weight of all valid signers meets the required threshold.

The contract is stateless beyond its configuration (admin address and linked Security contract). It does not store any attestation data; it purely validates and returns success or failure. This makes it reusable across different Handler implementations and composable with any contract that needs to verify Vectr attestations.

## When to use this contract

Use the Ed25519 Verification contract when your WarpDrive process operates natively on Soroban without needing cross-chain EVM compatibility. Vectrs sign attestations with ed25519 keys following SEP-0053 message formatting, and this contract uses Soroban's native `ed25519_verify` precompile and `sha256` precompile for verification. This is the right choice for Stellar-to-Stellar workflows, Soroban contract orchestration, and any process where operators already hold Stellar keypairs.

For cross-chain processes that bridge with Ethereum or other EVM chains, see the [Secp256k1 Verification contract](../secp256k1-verification/).

## Source Layout

| File | Purpose |
|------|---------|
| [`src/contract.rs`](./src/contract.rs) | Implements `Ed25519VerificationInterface` and `WarpDriveInterface` |
| [`src/utils.rs`](./src/utils.rs) | SEP-0053 message construction, SHA-256 hashing, and `ed25519_verify` glue |
| [`src/storage.rs`](./src/storage.rs) | Admin and linked Security contract address |
| [`src/lib.rs`](./src/lib.rs) | Crate root and module wiring |

## Contract Interactions

**[Ed25519 Security contract](../ed25519-security/)** -- Calls the Security contract via the [`Ed25519SecurityClient`](../../packages/shared/src/interfaces/security.rs) trait to look up signer weights and the required threshold. Uses historical weight queries (`get_signer_weights_at`, `required_weight_at`) when a `reference_block` is provided, ensuring verification uses the signer set that was active when the Vectrs signed.

**[Stellar Handler contract](../stellar-handler/)** -- The Handler calls `verify` after decoding the envelope and extracting signatures. The Verification contract validates the signatures and returns success or a typed error. The Handler does not need to understand the cryptographic details.

**Off-chain components** -- Aggregators do not interact with this contract directly. They submit envelopes to the Handler, which delegates to Verification. However, off-chain tooling can call `check_one` to pre-validate a single signature before aggregation, or query `required_weight` and `signer_weight` for monitoring. The [`warpdrive-client`](../../packages/client/) package provides a typed async client (`Ed25519Verification`) for these calls.

## Signature Verification Flow

1. Construct SEP-0053 payload: `"Stellar Signed Message:\n" || envelope`
2. Hash with Soroban's SHA-256 precompile: `message_hash = SHA256(payload)`
3. Verify each signature using Soroban's `ed25519_verify` precompile against `message_hash`
4. Signatures must be provided in strict ascending order of signer pubkeys (prevents duplicates)
5. Cumulative weight of all valid signers must meet or exceed `required_weight`

## Invalid Signature Behavior

Unlike the secp256k1 variant (which recovers a public key and compares it, yielding a clean error), Soroban's `ed25519_verify` host function **panics** when a signature is cryptographically invalid. This means:

- **All-zero signatures** are caught before the host call and return `Err(VerifyError::InvalidSignature)`.
- **Non-zero invalid signatures** (wrong key, corrupted bytes, etc.) cause `ed25519_verify` to trap. The transaction fails with a host error rather than a typed `VerifyError`.
- **Callers should use `try_check_one` / `try_verify`** to invoke these methods. The `try_` variants catch the host trap and return it as an `Err`, which the Handler contract can map to an appropriate error.

In practice this distinction is transparent to end users: invalid signatures are always rejected. The difference is only visible to contracts that pattern-match on the specific `VerifyError` variant -- they will see a generic host error instead of `VerifyError::InvalidSignature` for non-zero bad signatures.

## Interface

The full interface is defined in [`Ed25519VerificationInterface`](../../packages/shared/src/interfaces/verification.rs). Standard admin / upgrade / version methods come from [`WarpDriveInterface`](../../packages/shared/src/interfaces/warpdrive.rs).

### State-Changing Actions

| Function | Description |
|----------|-------------|
| `upgrade(new_wasm_hash, new_version)` | Upgrade the contract WASM. Admin-only. |
| `propose_admin(new_admin)` | Propose a new admin (two-step transfer). Current admin only. |
| `accept_admin()` | Accept a pending admin transfer. Pending admin only. |

### Queries

| Function | Description |
|----------|-------------|
| `verify(envelope, signatures, signer_pubkeys, reference_block)` | Verify that the given signatures over the envelope carry sufficient weight. Signatures and pubkeys must be parallel arrays in strict ascending pubkey order. Returns `Ok(())` on success. **Panics on invalid non-zero signatures** (see above). |
| `check_one(envelope, signature, signer_pubkey, reference_block)` | Verify a single signature and return the signer's weight. Useful for pre-validation before aggregation. If `reference_block` is `None`, uses current weights. **Panics on invalid non-zero signatures** (see above). |
| `security_contract() -> Address` | Return the address of the linked Security contract. |
| `required_weight() -> u64` | Return the current required weight from the Security contract. |
| `signer_weight(signer_pubkey) -> u64` | Return the current weight of a signer from the Security contract. |
| `admin() -> Address` | Return the current admin address. |
| `pending_admin() -> Option<Address>` | Return the pending admin, if a transfer is in progress. |
| `version() -> String` | Return the current contract version. |

### Types

Defined in [`packages/shared/src/interfaces/mod.rs`](../../packages/shared/src/interfaces/mod.rs):

- **`Ed25519PubKey`** -- `BytesN<32>` -- ed25519 public key (32 bytes).
- **`Ed25519Signature`** -- `BytesN<64>` -- ed25519 signature (64 bytes).

### Errors

Errors returned as typed `VerifyError` variants. Note that invalid non-zero signatures cause a host panic rather than returning `InvalidSignature` (see [Invalid Signature Behavior](#invalid-signature-behavior) above).

| Code | Name | Description |
|------|------|-------------|
| 301 | `InvalidSignature` | Signature is all-zero (explicitly rejected before host call) |
| 302 | `SignerNotRegistered` | Signer has zero weight in the Security contract |
| 303 | `InsufficientWeight` | Cumulative signer weight is below the required threshold |
| 304 | `EmptySignatures` | No signatures were provided |
| 305 | `LengthMismatch` | Signatures and signer_pubkeys arrays have different lengths |
| 306 | `SignersNotOrdered` | Signer public keys are not in strict ascending order |
| 307 | `ZeroRequiredWeight` | Required weight is zero (no signers registered or threshold misconfigured) |

## Events

The Verification contract is stateless beyond configuration. Only upgrades emit events.

| Event | Topic | Data Fields | Emitted By |
|-------|-------|-------------|------------|
| `ContractUpgraded` | -- | `version: String` | `upgrade` |
