use std::sync::Arc;
use crate::pool::node::ProxyNode;

pub trait Selector: Send + Sync {
    fn select<'a>(&self, nodes: &'a [Arc<ProxyNode>]) -> Option<&'a Arc<ProxyNode>>;
}

#[derive(Default)]
pub struct RoundRobin {
    counter: std::sync::atomic::AtomicUsize,
}

impl Selector for RoundRobin {
    fn select<'a>(&self, nodes: &'a [Arc<ProxyNode>]) -> Option<&'a Arc<ProxyNode>> {
        if nodes.is_empty() {
            return None;
        }
        let idx = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(&nodes[idx % nodes.len()])
    }
}

pub struct Random;

impl Selector for Random {
    fn select<'a>(&self, nodes: &'a [Arc<ProxyNode>]) -> Option<&'a Arc<ProxyNode>> {
        if nodes.is_empty() {
            return None;
        }
        let idx = fastrand::usize(..nodes.len());
        Some(&nodes[idx])
    }
}

/// Power of Two Choices (P2C): picks two random nodes and selects the one with fewer failures/active conns
pub struct PowerOfTwoChoices;

impl Selector for PowerOfTwoChoices {
    fn select<'a>(&self, nodes: &'a [Arc<ProxyNode>]) -> Option<&'a Arc<ProxyNode>> {
        if nodes.is_empty() {
            return None;
        }
        if nodes.len() == 1 {
            return Some(&nodes[0]);
        }

        let idx1 = fastrand::usize(..nodes.len());
        let mut idx2 = fastrand::usize(..nodes.len());
        if idx1 == idx2 {
            idx2 = (idx1 + 1) % nodes.len();
        }

        let n1 = &nodes[idx1];
        let n2 = &nodes[idx2];

        let score1 = n1.calculate_score();
        let score2 = n2.calculate_score();

        if score1 <= score2 {
            Some(n1)
        } else {
            Some(n2)
        }
    }
}
