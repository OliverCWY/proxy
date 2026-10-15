use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use dashmap::DashMap;
use smallvec::SmallVec;

#[derive(Debug, Clone)]
pub struct Lease {
    pub node_id: usize,
    pub egress_ip: Option<IpAddr>,
    pub created_at_ns: i64,
    pub expiry_ns: i64,
    pub last_accessed_ns: i64,
}

impl Lease {
    pub fn is_expired(&self, now_ns: i64) -> bool {
        self.expiry_ns < now_ns
    }
}

pub struct LeaseTable {
    /// Account -> Lease mapping (in-memory lock-free concurrency)
    leases: DashMap<String, Lease>,
    /// Egress IP -> active node IDs (O(1) failover candidate index)
    egress_index: DashMap<IpAddr, SmallVec<[usize; 4]>>,
    /// Active lease count per Egress IP for load-balanced P2C routing
    ip_lease_counts: DashMap<IpAddr, Arc<AtomicI64>>,
    default_ttl: Duration,
}

impl LeaseTable {
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            leases: DashMap::new(),
            egress_index: DashMap::new(),
            ip_lease_counts: DashMap::new(),
            default_ttl,
        }
    }

    /// Update the index of live nodes associated with an Egress IP
    pub fn update_node_egress_ip(&self, node_id: usize, old_ip: Option<IpAddr>, new_ip: Option<IpAddr>) {
        if old_ip == new_ip {
            return;
        }

        if let Some(old) = old_ip {
            if let Some(mut entry) = self.egress_index.get_mut(&old) {
                entry.retain(|id| *id != node_id);
            }
        }

        if let Some(new) = new_ip {
            self.egress_index.entry(new).or_default().push(node_id);
        }
    }

    /// Remove a node from egress index when node is removed
    pub fn remove_node(&self, node_id: usize, egress_ip: Option<IpAddr>) {
        if let Some(ip) = egress_ip {
            if let Some(mut entry) = self.egress_index.get_mut(&ip) {
                entry.retain(|id| *id != node_id);
            }
        }
    }

    /// Return current number of active sticky leases
    pub fn active_count(&self) -> usize {
        self.leases.len()
    }

    /// Get all live candidate node IDs that share the same Egress IP (O(1) lookup!)
    pub fn get_same_ip_candidates(&self, egress_ip: IpAddr) -> SmallVec<[usize; 4]> {
        self.egress_index
            .get(&egress_ip)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Get active lease count for an Egress IP (used for P2C scoring)
    pub fn get_ip_lease_count(&self, ip: IpAddr) -> i64 {
        self.ip_lease_counts
            .get(&ip)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Try retrieving valid existing lease, refreshing last_accessed_ns
    pub fn get_lease(&self, account: &str) -> Option<Lease> {
        let now_ns = now_unix_ns();
        let mut entry = self.leases.get_mut(account)?;

        if entry.is_expired(now_ns) {
            return None;
        }

        entry.last_accessed_ns = now_ns;
        Some(entry.clone())
    }

    /// Create or update a lease for an account, maintaining per-IP counters
    pub fn create_lease(&self, account: &str, node_id: usize, egress_ip: Option<IpAddr>) -> Lease {
        let now_ns = now_unix_ns();
        let expiry_ns = now_ns + self.default_ttl.as_nanos() as i64;

        let lease = Lease {
            node_id,
            egress_ip,
            created_at_ns: now_ns,
            expiry_ns,
            last_accessed_ns: now_ns,
        };

        // If replacing an existing lease, decrement old IP counter
        if let Some(old) = self.leases.insert(account.to_string(), lease.clone()) {
            if let Some(old_ip) = old.egress_ip {
                self.dec_ip_count(old_ip);
            }
        }

        if let Some(ip) = egress_ip {
            self.inc_ip_count(ip);
        }

        lease
    }

    /// Migrate an existing lease to a new node sharing the same Egress IP
    pub fn rotate_lease_node(&self, account: &str, new_node_id: usize) -> Option<Lease> {
        let now_ns = now_unix_ns();
        let mut entry = self.leases.get_mut(account)?;
        entry.node_id = new_node_id;
        entry.last_accessed_ns = now_ns;
        Some(entry.clone())
    }

    fn inc_ip_count(&self, ip: IpAddr) {
        let counter = self.ip_lease_counts
            .entry(ip)
            .or_insert_with(|| Arc::new(AtomicI64::new(0)));
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn dec_ip_count(&self, ip: IpAddr) {
        if let Some(counter) = self.ip_lease_counts.get(&ip) {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Sweep expired leases. Completely in-memory, zero disk I/O.
    pub fn sweep_expired(&self) -> usize {
        let now_ns = now_unix_ns();
        let mut expired_keys = Vec::new();

        for entry in self.leases.iter() {
            if entry.value().is_expired(now_ns) {
                expired_keys.push((entry.key().clone(), entry.value().egress_ip));
            }
        }

        let count = expired_keys.len();
        for (key, egress_ip) in expired_keys {
            if self.leases.remove(&key).is_some() {
                if let Some(ip) = egress_ip {
                    self.dec_ip_count(ip);
                }
            }
        }

        count
    }
}

fn now_unix_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
