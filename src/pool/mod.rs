pub mod lease;
pub mod node;
pub mod selector;

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use arc_swap::ArcSwap;
use tokio::net::TcpStream;

use crate::{
    error::PoolError,
    outbound::OutboundConnector,
    protocol::TargetAddr,
};

pub use lease::{Lease, LeaseTable};
pub use node::{ConnectionPermit, NodeHealthStatus, ProxyNode};
pub use selector::{PowerOfTwoChoices, Random, RoundRobin, Selector};

pub struct PoolSnapshot {
    pub nodes: Arc<[Arc<ProxyNode>]>,
    pub nodes_by_id: std::collections::HashMap<usize, Arc<ProxyNode>>,
}

impl PoolSnapshot {
    pub fn new(nodes: Vec<Arc<ProxyNode>>) -> Self {
        let mut map = std::collections::HashMap::with_capacity(nodes.len());
        for n in &nodes {
            map.insert(n.id, n.clone());
        }
        Self {
            nodes: nodes.into(),
            nodes_by_id: map,
        }
    }
}

pub struct ConnectedProxy {
    pub stream: TcpStream,
    pub residual: bytes::Bytes,
    pub node: Arc<ProxyNode>,
    pub permit: ConnectionPermit,
}

impl ConnectedProxy {
    pub fn new(stream: TcpStream, residual: bytes::Bytes, node: Arc<ProxyNode>) -> Self {
        let permit = ConnectionPermit::new(node.clone());
        Self {
            stream,
            residual,
            node,
            permit,
        }
    }

    /// Record relay error result to passive circuit breaker
    pub fn record_relay_result(
        &self,
        res: &std::io::Result<(u64, u64)>,
        max_failures: usize,
        base_cooldown: Duration,
    ) {
        if let Err(e) = res {
            match e.kind() {
                std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::TimedOut => {
                    self.node.mark_failure(max_failures, base_cooldown);
                }
                _ => {}
            }
        }
    }
}

pub struct ProxyPool {
    snapshot: ArcSwap<PoolSnapshot>,
    selector: Box<dyn Selector>,
    leases: Arc<LeaseTable>,
    max_retries: usize,
    connect_timeout: Duration,
    max_consecutive_failures: usize,
    base_cooldown: Duration,
    reqwest_client: reqwest::Client,
}

impl ProxyPool {
    pub fn new(nodes: Vec<Arc<ProxyNode>>) -> Self {
        let pool = Self {
            snapshot: ArcSwap::from_pointee(PoolSnapshot::new(nodes)),
            selector: Box::new(PowerOfTwoChoices),
            leases: Arc::new(LeaseTable::new(Duration::from_secs(1800))), // 30 mins default TTL
            max_retries: 3,
            connect_timeout: Duration::from_secs(8),
            max_consecutive_failures: 3,
            base_cooldown: Duration::from_secs(10),
            reqwest_client: reqwest::Client::builder()
                .pool_idle_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        };

        pool
    }

    pub fn with_selector(mut self, selector: Box<dyn Selector>) -> Self {
        self.selector = selector;
        self
    }

    pub fn with_timeouts(mut self, connect_timeout: Duration, max_retries: usize) -> Self {
        self.connect_timeout = connect_timeout;
        self.max_retries = max_retries;
        self
    }

    pub fn with_sticky_ttl(self, ttl: Duration) -> Self {
        Self {
            leases: Arc::new(LeaseTable::new(ttl)),
            ..self
        }
    }

    pub fn leases(&self) -> &Arc<LeaseTable> {
        &self.leases
    }

    /// Atomically replace the proxy pool with a new snapshot of nodes (Zero downtime hot reload)
    /// Preserves existing runtime state (stats, EWMA, cooldown, verified egress IP)
    pub fn reload_nodes(&self, new_nodes: Vec<Arc<ProxyNode>>) {
        let current = self.snapshot.load();
        let mut merged = Vec::with_capacity(new_nodes.len());

        for new_node in new_nodes {
            if let Some(existing) = current
                .nodes
                .iter()
                .find(|n| n.tag == new_node.tag || n.id == new_node.id)
            {
                // Preserve runtime identity and stats
                if new_node.is_blocked.load(Ordering::Relaxed) {
                    existing.block();
                }
                merged.push(existing.clone());
            } else {
                merged.push(new_node);
            }
        }

        let new_snapshot = Arc::new(PoolSnapshot::new(merged));
        self.snapshot.store(new_snapshot);
    }

    /// Get current total number of nodes
    pub fn node_count(&self) -> usize {
        self.snapshot.load().nodes.len()
    }

    /// Get atomic snapshot of all nodes in pool
    pub fn all_nodes(&self) -> Arc<[Arc<ProxyNode>]> {
        self.snapshot.load().nodes.clone()
    }

    /// Find node by ID in O(1) lock-free time
    pub fn find_node(&self, id: usize) -> Option<Arc<ProxyNode>> {
        let snapshot = self.snapshot.load();
        snapshot.nodes_by_id.get(&id).cloned()
    }

    /// Connect to target with sticky session support.
    /// 1. If valid lease exists and node is healthy -> use bound node.
    /// 2. If bound node fails/unhealthy -> O(1) failover to candidate sharing same verified Egress IP!
    /// 3. If no same-IP node available -> select new node via P2C and update lease.
    pub async fn connect_sticky(
        &self,
        account: &str,
        target: &TargetAddr,
    ) -> Result<ConnectedProxy, PoolError> {
        // Step 1: Check existing lease
        if let Some(lease) = self.leases.get_lease(account) {
            if let Some(node) = self.find_node(lease.node_id) {
                if node.try_admit() {
                    let start = std::time::Instant::now();
                    let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;

                    if let Ok((stream, residual)) = res {
                        node.mark_success_with_rtt(start.elapsed());
                        return Ok(ConnectedProxy::new(stream, residual, node));
                    }
                    node.mark_failure(self.max_consecutive_failures, self.base_cooldown);
                }
            }

            // Step 2: Bound node failed, try verified same-IP failover candidates in O(1)
            if let Some(egress_ip) = lease.egress_ip {
                let candidates = self.leases.get_same_ip_candidates(egress_ip);
                for cand_id in candidates {
                    if cand_id == lease.node_id {
                        continue;
                    }
                    if let Some(node) = self.find_node(cand_id) {
                        if node.try_admit() {
                            let start = std::time::Instant::now();
                            let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;

                            if let Ok((stream, residual)) = res {
                                node.mark_success_with_rtt(start.elapsed());
                                self.leases.rotate_lease_node(account, cand_id);
                                return Ok(ConnectedProxy::new(stream, residual, node));
                            }
                            node.mark_failure(self.max_consecutive_failures, self.base_cooldown);
                        }
                    }
                }
            }
        }

        // Step 3: Fallback - pick new node with retry and record lease
        let conn = self.connect_with_retry(target).await?;
        let verified_egress_ip = conn.node.probed_egress_ip();
        self.leases.create_lease(account, conn.node.id, verified_egress_ip);

        Ok(conn)
    }

    /// Connect to target with automatic retry over candidate proxy nodes.
    /// Returns ConnectedProxy containing the connected TCP stream, residual bytes, ProxyNode, and RAII ConnectionPermit.
    pub async fn connect_with_retry(
        &self,
        target: &TargetAddr,
    ) -> Result<ConnectedProxy, PoolError> {
        let snapshot = self.snapshot.load();
        let total_nodes = snapshot.nodes.len();
        if total_nodes == 0 {
            return Err(PoolError::NoHealthyProxy);
        }

        let attempts = self.max_retries.min(total_nodes).max(1);
        let mut tried_node_ids = HashSet::with_capacity(attempts);
        let mut errors = Vec::new();

        for _ in 0..attempts {
            // Find an admitted node not yet tried
            let candidate = {
                let untried: Vec<&Arc<ProxyNode>> = snapshot
                    .nodes
                    .iter()
                    .filter(|n| !tried_node_ids.contains(&n.id))
                    .collect();

                if untried.is_empty() {
                    break;
                }

                // Prefer nodes that pass try_admit()
                let admitted: Vec<Arc<ProxyNode>> = untried
                    .iter()
                    .filter(|n| n.try_admit())
                    .map(|n| (*n).clone())
                    .collect();

                if !admitted.is_empty() {
                    self.selector.select(&admitted).cloned()
                } else {
                    // Degraded mode: all nodes are cooling down, select best among untried
                    let pool: Vec<Arc<ProxyNode>> = untried.into_iter().cloned().collect();
                    self.selector.select(&pool).cloned()
                }
            };

            let node = match candidate {
                Some(n) => n,
                None => break,
            };

            tried_node_ids.insert(node.id);

            let start = std::time::Instant::now();
            let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;

            match res {
                Ok((stream, residual)) => {
                    node.mark_success_with_rtt(start.elapsed());
                    return Ok(ConnectedProxy::new(stream, residual, node));
                }
                Err(err) => {
                    let is_retryable = err.is_retryable();
                    node.mark_failure(self.max_consecutive_failures, self.base_cooldown);
                    errors.push(err);

                    if !is_retryable {
                        // Non-retryable error (e.g. invalid target domain/unresolvable target)
                        break;
                    }
                }
            }
        }

        Err(PoolError::AllRetriesFailed(errors))
    }

    pub fn get_reqwest_client(&self) -> &reqwest::Client {
        &self.reqwest_client
    }
}
