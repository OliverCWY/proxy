use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub trait Strategy: Send + Sync {
    fn choose<'a>(&self, from: &'a [Arc<str>]) -> Option<&'a Arc<str>>;
}

#[derive(Default)]
pub struct RoundRobin {
    i: AtomicUsize,
}

impl Strategy for RoundRobin {
    fn choose<'a>(&self, from: &'a [Arc<str>]) -> Option<&'a Arc<str>> {
        if from.is_empty() {
            return None;
        }
        let i = self.i.fetch_add(1, Ordering::Relaxed);
        Some(&from[i % from.len()])
    }
}
