//! Per-tx dedup tombstone via wasi:keyvalue/atomics CAS.
//!
//! Phoenix emits many events per logical pool action (8 per swap, 5 per
//! provide_liquidity, 4-5 per withdraw_liquidity). All of them share the
//! same tx_hash:op_index. We want exactly one Rebalance tick per logical
//! action, so the first event whose CAS swap succeeds emits; the rest see
//! the tombstone and skip.

use anyhow::{anyhow, Result};

use crate::wasi::keyvalue::atomics;
use crate::wasi::keyvalue::store;

const BUCKET: &str = "blend-rebalance-circuit-tombstones";
const MARK: &[u8] = b"1";

/// Returns true iff THIS invocation was the one that first marked `key`.
/// Subsequent calls for the same key return false. CAS-safe under the
/// concurrent event fan-out the node performs per logical pool action.
pub fn mark_if_unseen(key: &str) -> Result<bool> {
    let bucket = open_bucket()?;
    loop {
        let cas = atomics::Cas::new(&bucket, &key.to_string())
            .map_err(|e| anyhow!("cas open: {e:?}"))?;
        let current = cas.current().map_err(|e| anyhow!("cas current: {e:?}"))?;
        if current.is_some() {
            // Already tombstoned by a prior invocation.
            return Ok(false);
        }
        match atomics::swap(cas, MARK) {
            Ok(()) => return Ok(true),
            // Lost the race; another invocation tombstoned first. Re-read to
            // confirm and return false next iteration.
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


