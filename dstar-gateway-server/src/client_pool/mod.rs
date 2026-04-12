//! Client pool — concurrent `SocketAddr` → `ClientHandle` map.

pub mod handle;
pub mod pool;

pub use handle::{
    ClientHandle, DEFAULT_TX_BUDGET_MAX_TOKENS, DEFAULT_TX_BUDGET_REFILL_PER_SEC, TokenBucket,
};
pub use pool::{ClientPool, DEFAULT_UNHEALTHY_THRESHOLD, UnhealthyOutcome};
