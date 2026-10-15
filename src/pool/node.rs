use std::{
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::outbound::OutboundNodeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeHealthStatus {
    Healthy,
    Cooldown,
    HalfOpen,
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
    cooldown_until: std::sync::Mutex<Option<Instant>>,
}

impl ProxyNode {
    pub fn new(id: usize, tag: impl Into<String>, config: OutboundNodeConfig) -> Self {
        Self {
            id,
            tag: tag.into(),
            config,
            stats: NodeStats::default(),
            cooldown_until: std::sync::Mutex::new(None),
        }
    }

    pub fn status(&self) -> NodeHealthStatus {
        let lock = self.cooldown_until.lock().unwrap();
        if let Some(until) = *lock {
            if Instant::now() < until {
                NodeHealthStatus::Cooldown
            } else {
                NodeHealthStatus::HalfOpen
            }
        } else {
            NodeHealthStatus::Healthy
        }
    }

    pub fn mark_success(&self) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.successful_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.consecutive_failures.store(0, Ordering::Relaxed);

        let mut lock = self.cooldown_until.lock().unwrap();
        *lock = None;
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

            let mut lock = self.cooldown_until.lock().unwrap();
            *lock = Some(Instant::now() + cooldown);
        }
    }

    pub fn is_available(&self) -> bool {
        match self.status() {
            NodeHealthStatus::Healthy => true,
            NodeHealthStatus::HalfOpen => true,
            NodeHealthStatus::Cooldown => false,
        }
    }
}
