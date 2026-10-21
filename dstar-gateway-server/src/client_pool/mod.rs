//! Client pool: concurrent `SocketAddr` → `ClientHandle` map.

pub(crate) use handle::ClientHandle;
pub use handle::{DEFAULT_TX_BUDGET_MAX_TOKENS, DEFAULT_TX_BUDGET_REFILL_PER_SEC, TokenBucket};
pub use pool::ClientSnapshot;
pub(crate) use pool::{ClientPool, UnhealthyOutcome};
pub use rate::{FrameRate, FrameRateError};

mod handle;
mod pool;
mod rate;
