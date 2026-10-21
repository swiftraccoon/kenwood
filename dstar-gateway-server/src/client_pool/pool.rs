//! Atomic primary-map and module-index transactions for one endpoint.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Instant;

use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::{Callsign, Module};
use tokio::sync::Mutex;

use crate::reflector::AccessPolicy;

use super::ClientHandle;

/// Consecutive send failures at which a client is scheduled for eviction.
pub(crate) const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum UnhealthyOutcome {
    StillHealthy { failure_count: u32 },
    ShouldEvict { failure_count: u32 },
}

/// Owned client observation copied under one registry lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientSnapshot {
    /// UDP address fixed for this protocol session.
    pub peer: SocketAddr,
    /// Latest accepted monotonic receive observation, including read-only drops.
    pub last_heard: Instant,
    /// Linked reflector module; `None` during an incomplete handshake.
    pub module: Option<Module>,
    /// Complete wire callsign reported by the client, when known.
    pub client_callsign: Option<Callsign>,
    /// Client-side module reported during LINK, when known.
    pub client_module: Option<Module>,
    /// Current accepted voice access policy.
    pub access: AccessPolicy,
    /// Consecutive unsuccessful sends since the latest successful UDP enqueue.
    pub send_failure_count: u32,
}

#[derive(Debug)]
struct PoolState<P> {
    clients: HashMap<SocketAddr, ClientHandle<P>>,
    by_module: HashMap<Module, HashSet<SocketAddr>>,
}

impl<P> PoolState<P> {
    /// Remove one known prior membership and empty index buckets.
    fn remove_membership(&mut self, peer: &SocketAddr, module: Option<Module>) {
        if let Some(module) = module
            && let Some(members) = self.by_module.get_mut(&module)
        {
            let _removed = members.remove(peer);
            if members.is_empty() {
                drop(self.by_module.remove(&module));
            }
        }
    }

    fn add_membership(&mut self, peer: SocketAddr, module: Option<Module>) {
        if let Some(module) = module {
            let _inserted = self.by_module.entry(module).or_default().insert(peer);
        }
    }
}

/// Endpoint-owned registry; every mutation has one await before its transaction.
///
/// Cancelling before lock acquisition changes nothing. Primary state and its
/// index are updated synchronously under that same lock, without an intervening
/// suspension point. Only the endpoint can mutate a stored client.
#[derive(Debug)]
pub(crate) struct ClientPool<P> {
    state: Mutex<PoolState<P>>,
}

impl<P: Protocol> Default for ClientPool<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Protocol> ClientPool<P> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PoolState {
                clients: HashMap::new(),
                by_module: HashMap::new(),
            }),
        }
    }

    /// Insert by the handle's actual peer, returning any replaced client.
    #[cfg(test)]
    pub(crate) async fn insert(&self, handle: ClientHandle<P>) -> Option<ClientHandle<P>> {
        let peer = handle.peer();
        let module = handle.module;
        let mut state = self.state.lock().await;
        let previous = state.clients.insert(peer, handle);
        state.remove_membership(&peer, previous.as_ref().and_then(|client| client.module));
        state.add_membership(peer, module);
        previous
    }

    pub(crate) async fn remove(&self, peer: &SocketAddr) -> Option<ClientHandle<P>> {
        let mut state = self.state.lock().await;
        let handle = state.clients.remove(peer)?;
        state.remove_membership(peer, handle.module);
        drop(state);
        Some(handle)
    }

    pub(crate) async fn members_of_module(&self, module: Module) -> Vec<SocketAddr> {
        self.state
            .lock()
            .await
            .by_module
            .get(&module)
            .map(|members| members.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) async fn len(&self) -> usize {
        self.state.lock().await.clients.len()
    }

    pub(crate) async fn contains(&self, peer: &SocketAddr) -> bool {
        self.state.lock().await.clients.contains_key(peer)
    }

    pub(crate) async fn mark_unhealthy(&self, peer: &SocketAddr) -> UnhealthyOutcome {
        let mut state = self.state.lock().await;
        let count = state.clients.get_mut(peer).map_or(0, |handle| {
            handle.send_failure_count = handle.send_failure_count.saturating_add(1);
            handle.send_failure_count
        });
        drop(state);
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

    /// A successful datagram ends the consecutive-failure streak.
    pub(crate) async fn record_send_success(&self, peer: &SocketAddr) {
        if let Some(handle) = self.state.lock().await.clients.get_mut(peer) {
            handle.send_failure_count = 0;
        }
    }

    pub(crate) async fn record_last_heard(&self, peer: &SocketAddr, now: Instant) {
        if let Some(handle) = self.state.lock().await.clients.get_mut(peer) {
            handle.last_heard = handle.last_heard.max(now);
        }
    }

    pub(crate) async fn module_of(&self, peer: &SocketAddr) -> Option<Module> {
        self.state
            .lock()
            .await
            .clients
            .get(peer)
            .and_then(|handle| handle.module)
    }

    pub(crate) async fn access_of(&self, peer: &SocketAddr) -> Option<AccessPolicy> {
        self.state
            .lock()
            .await
            .clients
            .get(peer)
            .map(|handle| handle.access)
    }

    pub(crate) async fn try_consume_tx_token(&self, peer: &SocketAddr, now: Instant) -> bool {
        self.state
            .lock()
            .await
            .clients
            .get_mut(peer)
            .is_some_and(|handle| handle.tx_budget.try_consume(now, 1))
    }

    pub(crate) async fn sweep_snapshot(&self) -> Vec<ClientSnapshot> {
        self.state
            .lock()
            .await
            .clients
            .iter()
            .map(|(peer, handle)| ClientSnapshot {
                peer: *peer,
                last_heard: handle.last_heard,
                module: handle.module,
                client_callsign: handle.session.client_callsign(),
                client_module: handle.session.client_module(),
                access: handle.access,
                send_failure_count: handle.send_failure_count,
            })
            .collect()
    }

    /// Creation, protocol progress, and index updates share one suspension-free transaction.
    pub(crate) async fn get_or_try_update<F, C, R, E>(
        &self,
        peer: SocketAddr,
        create: C,
        operation: F,
    ) -> Result<R, E>
    where
        C: FnOnce() -> ClientHandle<P>,
        F: FnOnce(&mut ClientHandle<P>) -> Result<R, E>,
    {
        let mut state = self.state.lock().await;
        let created = !state.clients.contains_key(&peer);
        let handle = state.clients.entry(peer).or_insert_with(create);
        let previous = handle.module;
        let result = operation(handle);
        let current = handle.module;
        let closed = handle.session.state_kind()
            == dstar_gateway_core::session::server::ServerStateKind::Closed;
        if closed || (created && result.is_err()) {
            let _removed = state.clients.remove(&peer);
            state.remove_membership(&peer, previous);
        } else if created || previous != current {
            state.remove_membership(&peer, previous);
            state.add_membership(peer, current);
        }
        drop(state);
        result
    }

    #[cfg(test)]
    pub(crate) async fn hold_registry_for_test(&self) -> impl Sized + '_ {
        self.state.lock().await
    }

    /// Synchronous endpoint work commits its membership/index together.
    #[cfg(test)]
    pub(crate) async fn with_handle_mut<F, R>(&self, peer: &SocketAddr, operation: F) -> Option<R>
    where
        F: FnOnce(&mut ClientHandle<P>) -> R,
    {
        let mut state = self.state.lock().await;
        let handle = state.clients.get_mut(peer)?;
        let previous = handle.module;
        let result = operation(handle);
        let current = handle.module;
        if handle.session.state_kind()
            == dstar_gateway_core::session::server::ServerStateKind::Closed
        {
            let _removed = state.clients.remove(peer);
            state.remove_membership(peer, previous);
        } else if previous != current {
            state.remove_membership(peer, previous);
            state.add_membership(*peer, current);
        }
        drop(state);
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::session::client::DExtra;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn linked_handle(module: Module) -> Result<ClientHandle<DExtra>, dstar_gateway_core::Error> {
        let now = Instant::now();
        let mut handle = ClientHandle::new(PEER, module, AccessPolicy::ReadWrite, now);
        let mut link = *b"W1AW    BC\0";
        link[9] = module.as_byte();
        handle.session.handle_input(now, &link)?;
        handle.module = Some(module);
        Ok(handle)
    }

    async fn assert_consistent(pool: &ClientPool<DExtra>) {
        let state = pool.state.lock().await;
        let mut expected: HashMap<Module, HashSet<SocketAddr>> = HashMap::new();
        for (peer, handle) in &state.clients {
            assert_eq!(*peer, handle.peer(), "map key must be the actual core peer");
            if let Some(module) = handle.module {
                let _inserted = expected.entry(module).or_default().insert(*peer);
            }
        }
        assert_eq!(
            state.by_module, expected,
            "index must describe this exact primary-map version"
        );
        drop(state);
    }

    #[tokio::test]
    async fn replacement_removes_prior_membership_and_remove_clears_the_index() -> TestResult {
        let pool = ClientPool::new();
        assert!(pool.insert(linked_handle(Module::C)?).await.is_none());
        assert!(pool.insert(linked_handle(Module::D)?).await.is_some());
        assert_eq!(pool.len().await, 1);
        assert!(pool.members_of_module(Module::C).await.is_empty());
        assert_eq!(pool.members_of_module(Module::D).await, [PEER]);
        assert_consistent(&pool).await;
        assert!(pool.remove(&PEER).await.is_some());
        assert!(pool.members_of_module(Module::D).await.is_empty());
        assert!(pool.remove(&PEER).await.is_none());
        assert_consistent(&pool).await;
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_waiting_insert_preserves_both_maps() -> TestResult {
        let pool = ClientPool::new();
        drop(pool.insert(linked_handle(Module::C)?).await);
        let guard = pool.state.lock().await;
        let result = tokio::time::timeout(
            Duration::from_millis(1),
            pool.insert(linked_handle(Module::D)?),
        )
        .await;
        assert!(
            result.is_err(),
            "the held transaction lock must block replacement"
        );
        drop(guard);
        assert_eq!(pool.module_of(&PEER).await, Some(Module::C));
        assert_eq!(pool.members_of_module(Module::C).await, [PEER]);
        assert!(pool.members_of_module(Module::D).await.is_empty());
        assert_consistent(&pool).await;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_replacements_never_publish_a_stale_index() -> TestResult {
        let pool = Arc::new(ClientPool::new());
        let mut writers = Vec::new();
        for index in 0..64 {
            let pool = Arc::clone(&pool);
            let module = if index % 2 == 0 { Module::C } else { Module::D };
            let handle = linked_handle(module)?;
            writers.push(tokio::spawn(async move {
                drop(pool.insert(handle).await);
            }));
        }
        for _ in 0..64 {
            assert_consistent(&pool).await;
            tokio::task::yield_now().await;
        }
        for writer in writers {
            writer.await?;
        }
        assert_eq!(pool.len().await, 1);
        assert_consistent(&pool).await;
        Ok(())
    }

    #[tokio::test]
    async fn successful_send_resets_the_consecutive_failure_budget() -> TestResult {
        let pool = ClientPool::new();
        drop(pool.insert(linked_handle(Module::C)?).await);
        for expected in 1..DEFAULT_UNHEALTHY_THRESHOLD {
            assert_eq!(
                pool.mark_unhealthy(&PEER).await,
                UnhealthyOutcome::StillHealthy {
                    failure_count: expected
                }
            );
        }
        pool.record_send_success(&PEER).await;
        assert_eq!(
            pool.mark_unhealthy(&PEER).await,
            UnhealthyOutcome::StillHealthy { failure_count: 1 }
        );
        assert_consistent(&pool).await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_initial_protocol_update_leaves_no_client_or_membership() -> TestResult {
        let pool = ClientPool::new();
        let candidate = linked_handle(Module::C)?;
        let result = pool
            .get_or_try_update(
                PEER,
                || candidate,
                |handle| {
                    handle.module = Some(Module::D);
                    Err::<(), _>("injected protocol failure")
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            pool.len().await,
            0,
            "a rejected first input cannot retain client capacity"
        );
        assert!(pool.members_of_module(Module::C).await.is_empty());
        assert!(pool.members_of_module(Module::D).await.is_empty());
        assert_consistent(&pool).await;
        Ok(())
    }
}
