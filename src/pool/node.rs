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
}

impl Default for NodeStats {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            consecutive_failures: AtomicUsize::new(0),
            active_connections: AtomicUsize::new(0),
        }
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
