use anyhow::{anyhow, Context, Result};

use crate::phoenix::SwapState;
use crate::wasi::keyvalue::atomics;
use crate::wasi::keyvalue::store;

const BUCKET: &str = "blend-rebalance-circuit-swap-state";

/// Atomically read the current SwapState for `key`, apply `mutate`, and commit
/// the result via wasi:keyvalue/atomics CAS. Retries on CAS conflict so that
/// concurrent invocations (8 events per Phoenix swap fire in parallel) compose
/// without losing field updates.
///
/// Returns the post-mutation state so the caller can decide whether to finalize.
pub fn update_with<F, R>(key: &str, mut mutate: F) -> Result<R>
where
    F: FnMut(&mut SwapState) -> R,
{
    let bucket = open_bucket()?;
    loop {
        let cas = atomics::Cas::new(&bucket, &key.to_string())
            .map_err(|e| anyhow!("cas open: {e:?}"))?;
        let mut state = match cas.current().map_err(|e| anyhow!("cas current: {e:?}"))? {
            Some(bytes) => serde_json::from_slice(&bytes).context("deserialize SwapState")?,
            None => SwapState::default(),
        };
        let result = mutate(&mut state);
        let bytes = serde_json::to_vec(&state).context("serialize SwapState")?;
        match atomics::swap(cas, &bytes) {
            Ok(()) => return Ok(result),
            Err(atomics::CasError::CasFailed(_)) => continue,
            Err(atomics::CasError::StoreError(e)) => {
                return Err(anyhow!("cas swap store error: {e:?}"))
            }
        }
    }
}

fn open_bucket() -> Result<store::Bucket> {
    store::open(&BUCKET.to_string()).map_err(|e| anyhow!("open kv bucket: {e:?}"))
}
