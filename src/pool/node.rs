use std::{
    net::IpAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::outbound::OutboundNodeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeHealthStatus {
    Healthy,
    Cooldown,
    HalfOpen,
    Blocked,
}

#[derive(Debug)]
pub struct NodeStats {
    pub total_requests: AtomicU64,
    pub successful_requests: AtomicU64,
    pub consecutive_failures: AtomicUsize,
    pub active_connections: AtomicUsize,
    pub latency_ewma_ms: AtomicU64,
    pub last_update_secs: AtomicU64,
}

impl NodeStats {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            consecutive_failures: AtomicUsize::new(0),
            active_connections: AtomicUsize::new(0),
            latency_ewma_ms: AtomicU64::new(50),
            last_update_secs: AtomicU64::new(0),
        }
    }

    /// Record a sample latency and apply Time-Decay EWMA smoothing
    pub fn record_latency(&self, rtt: Duration) {
        let rtt_ms = (rtt.as_millis().max(1) as f64).min(10_000.0);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let old_secs = self.last_update_secs.swap(now_secs, Ordering::Relaxed);
        let dt = if old_secs > 0 && now_secs > old_secs {
            (now_secs - old_secs).min(60) as f64
        } else {
            0.0
        };

        // Time-decay half life tau = 10s: decay = exp(-dt / 10.0)
        let decay = (-dt / 10.0).exp();
        let old_ewma = self.latency_ewma_ms.load(Ordering::Relaxed) as f64;

        // Weight alpha = 0.2 for latest measurement, resting baseline = 50ms
        let alpha = 0.2;
        let decayed_ewma = old_ewma * decay + 50.0 * (1.0 - decay);
        let new_ewma = (alpha * rtt_ms + (1.0 - alpha) * decayed_ewma).round() as u64;

        self.latency_ewma_ms.store(new_ewma.max(1), Ordering::Relaxed);
    }
}

impl Default for NodeStats {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ProxyNode {
    pub id: usize,
    pub tag: String,
    pub config: OutboundNodeConfig,
    pub stats: NodeStats,
    pub is_blocked: AtomicBool,
    /// Unix timestamp millis until which node is cooling down (0 = healthy)
    cooldown_until_ms: AtomicU64,
    /// Concurrency token for half-open probe admission (only 1 probe at a time)
    half_open_probe_active: AtomicBool,
    /// Verified public egress IP detected via external probe
    probed_egress_ip: RwLock<Option<IpAddr>>,
}

/// RAII Permit that tracks active connection lifetime accurately
#[derive(Debug)]
pub struct ConnectionPermit {
    node: Arc<ProxyNode>,
}

impl ConnectionPermit {
    pub fn new(node: Arc<ProxyNode>) -> Self {
        node.stats.active_connections.fetch_add(1, Ordering::Relaxed);
        Self { node }
    }

    pub fn node(&self) -> &Arc<ProxyNode> {
        &self.node
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.node.stats.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ProxyNode {
    pub fn new(id: usize, tag: impl Into<String>, config: OutboundNodeConfig) -> Self {
        Self {
            id,
            tag: tag.into(),
            config,
            stats: NodeStats::default(),
            is_blocked: AtomicBool::new(false),
            cooldown_until_ms: AtomicU64::new(0),
            half_open_probe_active: AtomicBool::new(false),
            probed_egress_ip: RwLock::new(None),
        }
    }

    pub fn block(&self) {
        self.is_blocked.store(true, Ordering::Relaxed);
    }

    pub fn unblock(&self) {
        self.is_blocked.store(false, Ordering::Relaxed);
    }

    pub fn probed_egress_ip(&self) -> Option<IpAddr> {
        *self.probed_egress_ip.read().unwrap()
    }

    pub fn set_probed_egress_ip(&self, ip: IpAddr) {
        *self.probed_egress_ip.write().unwrap() = Some(ip);
    }

    pub fn status(&self) -> NodeHealthStatus {
        if self.is_blocked.load(Ordering::Relaxed) {
            return NodeHealthStatus::Blocked;
        }

        let until = self.cooldown_until_ms.load(Ordering::Relaxed);
        if until == 0 {
            NodeHealthStatus::Healthy
        } else {
            let now_ms = now_unix_ms();
            if now_ms < until {
                NodeHealthStatus::Cooldown
            } else {
                NodeHealthStatus::HalfOpen
            }
        }
    }

    /// High performance, lock-free admission control with true HalfOpen single-probe throttling
    pub fn try_admit(&self) -> bool {
        if self.is_blocked.load(Ordering::Relaxed) {
            return false;
        }

        let until = self.cooldown_until_ms.load(Ordering::Relaxed);
        if until == 0 {
            // Healthy node: always admitted
            true
        } else {
            let now_ms = now_unix_ms();
            if now_ms < until {
                // Still in cooldown
                false
            } else {
                // Cooldown expired -> HalfOpen state!
                // Atomically allow exactly ONE probe request through.
                self.half_open_probe_active
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            }
        }
    }

    pub fn mark_success(&self) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.successful_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.consecutive_failures.store(0, Ordering::Relaxed);

        // Reset cooldown and half-open probe lock-free
        self.cooldown_until_ms.store(0, Ordering::Relaxed);
        self.half_open_probe_active.store(false, Ordering::Relaxed);
    }

    pub fn mark_success_with_rtt(&self, rtt: Duration) {
        self.mark_success();
        self.stats.record_latency(rtt);
    }

    pub fn calculate_score(&self) -> f64 {
        let failures = self.stats.consecutive_failures.load(Ordering::Relaxed).min(8) as u32;
        let active = self.stats.active_connections.load(Ordering::Relaxed) as f64;
        let ewma = self.stats.latency_ewma_ms.load(Ordering::Relaxed) as f64;

        let penalty = (1 << failures) as f64;
        (ewma + 1.0) * (active + 1.0) * penalty
    }

    pub fn mark_failure(&self, max_consecutive_failures: usize, base_cooldown: Duration) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        let failures = self.stats.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        if failures >= max_consecutive_failures {
            // Exponential backoff: base_cooldown * 2^(failures - max) capped at 5 minutes
            let exponent = (failures - max_consecutive_failures).min(6) as u32;
            let cooldown = base_cooldown.saturating_mul(1 << exponent).min(Duration::from_secs(300));

            let deadline_ms = now_unix_ms() + cooldown.as_millis() as u64;
            self.cooldown_until_ms.store(deadline_ms, Ordering::Relaxed);
        }

        // Release half-open probe active lock so subsequent retries/probes can try after cooldown
        self.half_open_probe_active.store(false, Ordering::Relaxed);
    }

    pub fn is_available(&self) -> bool {
        match self.status() {
            NodeHealthStatus::Healthy | NodeHealthStatus::HalfOpen => true,
            NodeHealthStatus::Cooldown | NodeHealthStatus::Blocked => false,
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
