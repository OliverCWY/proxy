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
pub use node::{NodeHealthStatus, ProxyNode};
pub use selector::{PowerOfTwoChoices, Random, RoundRobin, Selector};

pub struct PoolSnapshot {
    pub nodes: Arc<[Arc<ProxyNode>]>,
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
            snapshot: ArcSwap::from_pointee(PoolSnapshot {
                nodes: nodes.clone().into(),
            }),
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

        // Populate initial egress IP index for nodes that have dial_addrs
        for node in &nodes {
            if let Some(first_addr) = node.config.dial_addrs.first() {
                pool.leases.update_node_egress_ip(node.id, None, Some(first_addr.ip()));
            }
        }

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
    pub fn reload_nodes(&self, new_nodes: Vec<Arc<ProxyNode>>) {
        let new_snapshot = Arc::new(PoolSnapshot {
            nodes: new_nodes.into(),
        });
        self.snapshot.store(new_snapshot);
    }

    /// Get current total number of nodes
    pub fn node_count(&self) -> usize {
        self.snapshot.load().nodes.len()
    }

    /// Find node by ID
    pub fn find_node(&self, id: usize) -> Option<Arc<ProxyNode>> {
        let snapshot = self.snapshot.load();
        snapshot.nodes.iter().find(|n| n.id == id).cloned()
    }

    /// Connect to target with sticky session support.
    /// 1. If valid lease exists and node is healthy -> use bound node.
    /// 2. If bound node fails/unhealthy -> O(1) failover to candidate sharing same Egress IP!
    /// 3. If no same-IP node available -> select new node via P2C and update lease.
    pub async fn connect_sticky(
        &self,
        account: &str,
        target: &TargetAddr,
    ) -> Result<(TcpStream, bytes::Bytes, Arc<ProxyNode>), PoolError> {
        // Step 1: Check existing lease
        if let Some(lease) = self.leases.get_lease(account) {
            if let Some(node) = self.find_node(lease.node_id) {
                if node.is_available() {
                    node.stats.active_connections.fetch_add(1, Ordering::Relaxed);
                    let start = std::time::Instant::now();
                    let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;
                    node.stats.active_connections.fetch_sub(1, Ordering::Relaxed);

                    if let Ok((stream, residual)) = res {
                        node.mark_success_with_rtt(start.elapsed());
                        return Ok((stream, residual, node));
                    }
                    node.mark_failure(self.max_consecutive_failures, self.base_cooldown);
                }
            }

            // Step 2: Bound node failed, try same-IP failover candidates in O(1)
            if let Some(egress_ip) = lease.egress_ip {
                let candidates = self.leases.get_same_ip_candidates(egress_ip);
                for cand_id in candidates {
                    if cand_id == lease.node_id {
                        continue;
                    }
                    if let Some(node) = self.find_node(cand_id) {
                        if node.is_available() {
                            node.stats.active_connections.fetch_add(1, Ordering::Relaxed);
                            let start = std::time::Instant::now();
                            let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;
                            node.stats.active_connections.fetch_sub(1, Ordering::Relaxed);

                            if let Ok((stream, residual)) = res {
                                node.mark_success_with_rtt(start.elapsed());
                                self.leases.rotate_lease_node(account, cand_id);
                                return Ok((stream, residual, node));
                            }
                            node.mark_failure(self.max_consecutive_failures, self.base_cooldown);
                        }
                    }
                }
            }
        }

        // Step 3: Fallback - pick new node with retry and record lease
        let (stream, residual, node) = self.connect_with_retry(target).await?;
        let egress_ip = node.config.dial_addrs.first().map(|a| a.ip());
        self.leases.create_lease(account, node.id, egress_ip);

        Ok((stream, residual, node))
    }

    /// Connect to target with automatic retry over candidate proxy nodes.
    /// Returns the connected TCP stream, residual bytes, and the chosen ProxyNode.
    pub async fn connect_with_retry(
        &self,
        target: &TargetAddr,
    ) -> Result<(TcpStream, bytes::Bytes, Arc<ProxyNode>), PoolError> {
        let snapshot = self.snapshot.load();
        let total_nodes = snapshot.nodes.len();
        if total_nodes == 0 {
            return Err(PoolError::NoHealthyProxy);
        }

        // Filter available nodes (Healthy or HalfOpen)
        let eligible: Vec<Arc<ProxyNode>> = snapshot
            .nodes
            .iter()
            .filter(|n| n.is_available())
            .cloned()
            .collect();

        let candidate_pool = if eligible.is_empty() {
            // Degraded mode: all nodes are cooling down, try all nodes
            &snapshot.nodes[..]
        } else {
            &eligible[..]
        };

        let attempts = self.max_retries.min(candidate_pool.len()).max(1);
        let mut tried_node_ids = HashSet::with_capacity(attempts);
        let mut errors = Vec::new();

        for _ in 0..attempts {
            // Pick a candidate not yet tried
            let remaining: Vec<Arc<ProxyNode>> = candidate_pool
                .iter()
                .filter(|n| !tried_node_ids.contains(&n.id))
                .cloned()
                .collect();

            let node = match self.selector.select(&remaining) {
                Some(n) => n.clone(),
                None => break,
            };

            tried_node_ids.insert(node.id);
            node.stats.active_connections.fetch_add(1, Ordering::Relaxed);

            let start = std::time::Instant::now();
            let res = OutboundConnector::connect(&node.config, target, self.connect_timeout).await;
            node.stats.active_connections.fetch_sub(1, Ordering::Relaxed);

            match res {
                Ok((stream, residual)) => {
                    node.mark_success_with_rtt(start.elapsed());
                    return Ok((stream, residual, node));
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
