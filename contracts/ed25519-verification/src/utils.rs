use soroban_sdk::{Bytes, BytesN, Env};
use warpdrive_shared::interfaces::{Ed25519PubKey, Ed25519Signature, verification::VerifyError};

/// SEP-0053 message hash: `SHA256("Stellar Signed Message:\n" || envelope)`
pub fn sep053_hash(env: &Env, envelope: &Bytes) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    payload.extend_from_slice(b"Stellar Signed Message:\n");
    payload.append(envelope);
    env.crypto().sha256(&payload).into()
}

/// Verify an ed25519 signature using SEP-0053 message formatting.
/// Hashes the envelope per SEP-0053 then verifies with Soroban's `ed25519_verify`.
///
/// Returns `Err(VerifyError::InvalidSignature)` for all-zero signatures.
///
/// # Panics
///
/// Panics via the Soroban `ed25519_verify` host function if the signature
/// is non-zero but cryptographically invalid. Callers that need a
/// `Result` should invoke the enclosing contract method through
/// `try_check_one` / `try_verify`.
pub fn verify_ed25519(
    env: &Env,
    envelope: &Bytes,
    signature: &Ed25519Signature,
    signer_pubkey: &Ed25519PubKey,
) -> Result<(), VerifyError> {
    // Reject all-zero signatures explicitly
    if signature.to_array().iter().all(|&b| b == 0) {
        return Err(VerifyError::InvalidSignature);
    }

    let message_hash = sep053_hash(env, envelope);
    verify_ed25519_prehashed(env, &message_hash, signature, signer_pubkey)
}

/// Verify an ed25519 signature against a pre-computed SEP-0053 message hash.
///
/// Returns `Err(VerifyError::InvalidSignature)` for all-zero signatures.
///
/// # Panics
///
/// Panics via the Soroban `ed25519_verify` host function if the signature
/// is non-zero but cryptographically invalid.
pub fn verify_ed25519_prehashed(
    env: &Env,
    message_hash: &BytesN<32>,
    signature: &Ed25519Signature,
    signer_pubkey: &Ed25519PubKey,
) -> Result<(), VerifyError> {
    // Reject all-zero signatures explicitly
    if signature.to_array().iter().all(|&b| b == 0) {
        return Err(VerifyError::InvalidSignature);
    }

    let message = Bytes::from_slice(env, &message_hash.to_array());
    env.crypto()
        .ed25519_verify(signer_pubkey, &message, signature);

    Ok(())
}
