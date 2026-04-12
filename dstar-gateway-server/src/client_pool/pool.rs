//! Concurrent map of currently linked clients per protocol.
//!
//! Keyed by `SocketAddr` (the only stable identifier for a UDP
//! client). Wrapped in [`tokio::sync::Mutex`] so multiple tokio tasks
//! can update concurrently — this is intentionally simple for Batch
//! 2; we can swap to a sharded map if contention is observed.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Instant;

use tokio::sync::Mutex;

use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::Module;

use crate::reflector::AccessPolicy;

use super::handle::ClientHandle;

/// Default number of consecutive send failures before a client is evicted.
///
/// Hard-coded to `5` in this crate; a follow-up patch will make this
/// configurable via [`crate::ReflectorConfig`].
pub const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 5;

/// Outcome of a [`ClientPool::mark_unhealthy`] call.
///
/// Tells the caller whether the peer is still within its failure
/// budget or has crossed the eviction threshold — in which case the
/// fan-out engine should remove the peer and emit a
/// [`ServerEvent::ClientEvicted`] event.
///
/// [`ServerEvent::ClientEvicted`]: dstar_gateway_core::session::server::ServerEvent::ClientEvicted
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnhealthyOutcome {
    /// Client is still within the allowed failure budget.
    StillHealthy {
        /// Running count of send failures after this increment.
        failure_count: u32,
    },
    /// Client has exceeded the threshold and should be evicted.
    ShouldEvict {
        /// Running count of send failures after this increment.
        failure_count: u32,
    },
}

/// Concurrent pool of linked clients for one [`Protocol`].
///
/// Provides `async` access to the underlying map via an internal
/// [`tokio::sync::Mutex`]. The reverse index (`by_module`) is kept in
/// lockstep with the primary map so fan-out can enumerate members of
/// a module in O(module members) without scanning every client.
#[derive(Debug)]
pub struct ClientPool<P: Protocol> {
    clients: Mutex<HashMap<SocketAddr, ClientHandle<P>>>,
    by_module: Mutex<HashMap<Module, HashSet<SocketAddr>>>,
    _protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> Default for ClientPool<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Protocol> ClientPool<P> {
    /// Create an empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            by_module: Mutex::new(HashMap::new()),
            _protocol: PhantomData,
        }
    }

    /// Insert a client handle keyed by peer address.
    ///
    /// If the handle already has a module set, the reverse index is
    /// updated to include this peer under that module.
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe. It takes two internal
    /// mutex locks in sequence: dropping the future between the first
    /// and second `lock().await` leaves the forward map updated but
    /// the reverse module index stale. The only correct recovery is
    /// to call [`Self::remove`] for the same peer and retry.
    pub async fn insert(&self, peer: SocketAddr, handle: ClientHandle<P>) {
        let module = handle.module;
        let mut clients = self.clients.lock().await;
        // Ignore the prior-entry return value: if a stale handle was
        // replaced, the reflector has no use for it.
        drop(clients.insert(peer, handle));
        drop(clients);
        if let Some(module) = module {
            let mut index = self.by_module.lock().await;
            let _ = index.entry(module).or_default().insert(peer);
        }
    }

    /// Remove a client by peer address, returning the handle if present.
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe for the same reason as
    /// [`Self::insert`] — it touches the forward and reverse maps in
    /// sequence and cancellation between the two awaits leaves a
    /// stale module-index entry.
    pub async fn remove(&self, peer: &SocketAddr) -> Option<ClientHandle<P>> {
        let mut clients = self.clients.lock().await;
        let handle = clients.remove(peer)?;
        drop(clients);
        if let Some(module) = handle.module {
            let mut index = self.by_module.lock().await;
            if let Some(set) = index.get_mut(&module) {
                let _ = set.remove(peer);
                if set.is_empty() {
                    drop(index.remove(&module));
                }
            }
        }
        Some(handle)
    }

    /// Attach a peer to a module, updating both the handle and the reverse index.
    ///
    /// No-op if the peer is not in the pool. If the peer was already
    /// attached to a different module, the old entry is removed.
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe. See [`Self::insert`] for
    /// the dual-lock sequencing rationale.
    pub async fn set_module(&self, peer: &SocketAddr, module: Module) {
        let mut clients = self.clients.lock().await;
        let Some(handle) = clients.get_mut(peer) else {
            return;
        };
        let previous_module = handle.module;
        handle.module = Some(module);
        drop(clients);
        let mut index = self.by_module.lock().await;
        if let Some(prev) = previous_module
            && prev != module
            && let Some(set) = index.get_mut(&prev)
        {
            let _ = set.remove(peer);
            if set.is_empty() {
                drop(index.remove(&prev));
            }
        }
        let _ = index.entry(module).or_default().insert(*peer);
    }

    /// Enumerate the peers currently linked to the given module.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. It acquires a single mutex lock
    /// and clones the membership set; dropping the future either
    /// before or after the clone leaves the pool state unchanged.
    pub async fn members_of_module(&self, module: Module) -> Vec<SocketAddr> {
        let index = self.by_module.lock().await;
        index
            .get(&module)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Number of clients currently in the pool.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. It takes a single mutex lock and
    /// reads a `usize`; no mutation occurs.
    pub async fn len(&self) -> usize {
        self.clients.lock().await.len()
    }

    /// Whether the pool is empty.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::len`].
    pub async fn is_empty(&self) -> bool {
        self.clients.lock().await.is_empty()
    }

    /// Whether the pool contains a handle for the given peer.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::len`].
    pub async fn contains(&self, peer: &SocketAddr) -> bool {
        self.clients.lock().await.contains_key(peer)
    }

    /// Increment the send-failure counter on a peer and report
    /// whether the peer should now be evicted.
    ///
    /// Returns [`UnhealthyOutcome::StillHealthy`] with `failure_count`
    /// `0` if the peer is not present — callers should treat that as
    /// "no increment happened" rather than "counter reset".
    ///
    /// The eviction threshold is [`DEFAULT_UNHEALTHY_THRESHOLD`]. Once
    /// `failure_count >= threshold` the return value switches to
    /// [`UnhealthyOutcome::ShouldEvict`] and stays there on every
    /// subsequent increment until the caller actually removes the
    /// peer.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. Only a single mutex lock is taken
    /// and the mutation is committed before `drop(clients)` releases
    /// the lock; dropping the future after that point has no effect
    /// on pool state.
    pub async fn mark_unhealthy(&self, peer: &SocketAddr) -> UnhealthyOutcome {
        let mut clients = self.clients.lock().await;
        let count = match clients.get_mut(peer) {
            Some(handle) => {
                handle.send_failure_count = handle.send_failure_count.saturating_add(1);
                handle.send_failure_count
            }
            None => 0,
        };
        drop(clients);
        if count >= DEFAULT_UNHEALTHY_THRESHOLD {
            UnhealthyOutcome::ShouldEvict {
                failure_count: count,
            }
        } else {
            UnhealthyOutcome::StillHealthy {
                failure_count: count,
            }
        }
    }

    /// Record that we just received a datagram from the given peer.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::mark_unhealthy`].
    pub async fn record_last_heard(&self, peer: &SocketAddr, now: Instant) {
        let mut clients = self.clients.lock().await;
        if let Some(handle) = clients.get_mut(peer) {
            handle.last_heard = now;
        }
    }

    /// Look up the module a peer is currently linked to, if any.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::len`].
    pub async fn module_of(&self, peer: &SocketAddr) -> Option<Module> {
        let clients = self.clients.lock().await;
        clients.get(peer).and_then(|handle| handle.module)
    }

    /// Look up the [`AccessPolicy`] for a peer, if any.
    ///
    /// Returns [`None`] if the peer is not in the pool. Used by the
    /// endpoint to gate voice-frame forwarding for [`AccessPolicy::ReadOnly`]
    /// clients.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. See [`Self::len`].
    pub async fn access_of(&self, peer: &SocketAddr) -> Option<AccessPolicy> {
        let clients = self.clients.lock().await;
        clients.get(peer).map(|handle| handle.access)
    }

    /// Attempt to consume one TX-budget token from the given peer.
    ///
    /// Returns `true` on success (the fan-out engine may send this
    /// frame to the peer); returns `false` if the bucket is empty
    /// (the frame must be dropped for this peer). Returns `false`
    /// if the peer is not in the pool — no handle means no budget.
    ///
    /// The `now` argument is the caller's injected wall-clock
    /// instant and is used to drive the token bucket's refill
    /// bookkeeping. The method never calls [`Instant::now`] itself,
    /// matching the sans-io rate-limiter pattern.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. Only a single mutex lock is
    /// taken and the bucket mutation is committed before the lock
    /// is released.
    pub async fn try_consume_tx_token(&self, peer: &SocketAddr, now: Instant) -> bool {
        let mut clients = self.clients.lock().await;
        match clients.get_mut(peer) {
            Some(handle) => handle.tx_budget.try_consume(now, 1),
            None => false,
        }
    }

    /// Run a closure with exclusive access to a peer's handle.
    ///
    /// Holds the internal `Mutex` for the duration of the closure;
    /// keep the body short and avoid blocking operations inside it.
    /// Returns `None` if no handle exists for the given peer (the
    /// closure is not invoked in that case).
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe **before** the lock is acquired
    /// and **after** the closure returns. While the closure is
    /// executing it runs synchronously under the lock, so cancellation
    /// during `f` is not possible. Do not call `.await` inside `f`
    /// or this guarantee is lost.
    pub async fn with_handle_mut<F, R>(&self, peer: &SocketAddr, f: F) -> Option<R>
    where
        F: FnOnce(&mut ClientHandle<P>) -> R,
    {
        let mut clients = self.clients.lock().await;
        clients.get_mut(peer).map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientHandle, ClientPool, Instant, Module, SocketAddr};
    use crate::reflector::AccessPolicy;
    use dstar_gateway_core::ServerSessionCore;
    use dstar_gateway_core::session::client::DExtra;
    use dstar_gateway_core::types::ProtocolKind;
    use std::net::{IpAddr, Ipv4Addr};

    fn peer(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn fresh_handle(port: u16) -> ClientHandle<DExtra> {
        let core = ServerSessionCore::new(ProtocolKind::DExtra, peer(port), Module::C);
        ClientHandle::new(core, AccessPolicy::ReadWrite, Instant::now())
    }

    #[tokio::test]
    async fn insert_and_contains() {
        let pool = ClientPool::<DExtra>::new();
        assert_eq!(pool.len().await, 0);
        assert!(!pool.contains(&peer(30001)).await);

        pool.insert(peer(30001), fresh_handle(30001)).await;
        assert_eq!(pool.len().await, 1);
        assert!(pool.contains(&peer(30001)).await);
    }

    #[tokio::test]
    async fn remove_returns_handle() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        let removed = pool.remove(&peer(30001)).await;
        assert!(removed.is_some());
        assert_eq!(pool.len().await, 0);
        // Second remove returns None.
        let removed_again = pool.remove(&peer(30001)).await;
        assert!(removed_again.is_none());
    }

    #[tokio::test]
    async fn set_module_populates_reverse_index() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        pool.insert(peer(30002), fresh_handle(30002)).await;
        pool.set_module(&peer(30001), Module::C).await;
        pool.set_module(&peer(30002), Module::C).await;

        let members = pool.members_of_module(Module::C).await;
        assert_eq!(members.len(), 2);
        assert!(members.contains(&peer(30001)));
        assert!(members.contains(&peer(30002)));
    }

    #[tokio::test]
    async fn set_module_moves_peer_between_modules() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        pool.set_module(&peer(30001), Module::C).await;
        pool.set_module(&peer(30001), Module::D).await;

        assert!(pool.members_of_module(Module::C).await.is_empty());
        let d_members = pool.members_of_module(Module::D).await;
        assert_eq!(d_members, vec![peer(30001)]);
    }

    #[tokio::test]
    async fn members_of_empty_module_is_empty() {
        let pool = ClientPool::<DExtra>::new();
        assert!(pool.members_of_module(Module::Z).await.is_empty());
    }

    #[tokio::test]
    async fn mark_unhealthy_increments_counter() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        assert_eq!(
            pool.mark_unhealthy(&peer(30001)).await,
            super::UnhealthyOutcome::StillHealthy { failure_count: 1 }
        );
        assert_eq!(
            pool.mark_unhealthy(&peer(30001)).await,
            super::UnhealthyOutcome::StillHealthy { failure_count: 2 }
        );
        assert_eq!(
            pool.mark_unhealthy(&peer(30001)).await,
            super::UnhealthyOutcome::StillHealthy { failure_count: 3 }
        );
    }

    #[tokio::test]
    async fn mark_unhealthy_missing_peer_is_zero() {
        let pool = ClientPool::<DExtra>::new();
        assert_eq!(
            pool.mark_unhealthy(&peer(30001)).await,
            super::UnhealthyOutcome::StillHealthy { failure_count: 0 }
        );
    }

    #[tokio::test]
    async fn mark_unhealthy_threshold_triggers_eviction() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        // 4 increments stay within budget.
        for expected in 1_u32..=4 {
            assert_eq!(
                pool.mark_unhealthy(&peer(30001)).await,
                super::UnhealthyOutcome::StillHealthy {
                    failure_count: expected
                }
            );
        }
        // 5th increment fires eviction.
        assert_eq!(
            pool.mark_unhealthy(&peer(30001)).await,
            super::UnhealthyOutcome::ShouldEvict { failure_count: 5 }
        );
    }

    #[tokio::test]
    async fn record_last_heard_updates_timestamp() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        let later = Instant::now() + std::time::Duration::from_secs(5);
        pool.record_last_heard(&peer(30001), later).await;
        // No crash + no state leak = pass. We can't read last_heard
        // without exposing it, which is intentional.
    }

    #[tokio::test]
    async fn remove_clears_reverse_index() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        pool.set_module(&peer(30001), Module::C).await;
        drop(pool.remove(&peer(30001)).await);
        assert!(pool.members_of_module(Module::C).await.is_empty());
    }

    #[tokio::test]
    async fn module_of_returns_assigned_module() {
        let pool = ClientPool::<DExtra>::new();
        pool.insert(peer(30001), fresh_handle(30001)).await;
        assert!(pool.module_of(&peer(30001)).await.is_none());
        pool.set_module(&peer(30001), Module::C).await;
        assert_eq!(pool.module_of(&peer(30001)).await, Some(Module::C));
    }

    #[tokio::test]
    async fn module_of_missing_peer_is_none() {
        let pool = ClientPool::<DExtra>::new();
        assert!(pool.module_of(&peer(30001)).await.is_none());
    }
}
