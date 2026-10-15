use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::RwLock,
};

use crate::protocol::{TargetAddr, TargetHost};

pub struct SecurityFilter {
    block_private_ips: bool,
    domain_blacklist: RwLock<HashSet<String>>,
}

impl Default for SecurityFilter {
    fn default() -> Self {
        Self::new(true)
    }
}

impl SecurityFilter {
    pub fn new(block_private_ips: bool) -> Self {
        Self {
            block_private_ips,
            domain_blacklist: RwLock::new(HashSet::new()),
        }
    }

    pub fn add_blocked_domain(&self, domain: impl Into<String>) {
        let mut set = self.domain_blacklist.write().unwrap();
        set.insert(domain.into().to_ascii_lowercase());
    }

    pub fn remove_blocked_domain(&self, domain: &str) -> bool {
        let mut set = self.domain_blacklist.write().unwrap();
        set.remove(&domain.to_ascii_lowercase())
    }

    pub fn list_blocked_domains(&self) -> Vec<String> {
        let set = self.domain_blacklist.read().unwrap();
        let mut list: Vec<String> = set.iter().cloned().collect();
        list.sort();
        list
    }

    /// Check if target address is allowed
    pub fn check_target(&self, target: &TargetAddr) -> Result<(), &'static str> {
        // 1. IP checks (SSRF private network guard)
        if let TargetHost::Ip(ip) = target.host {
            if self.block_private_ips && is_private_ip(ip) {
                return Err("Access to private/local IP address is blocked (SSRF guard)");
            }
        }

        // 2. Domain checks
        if let TargetHost::Domain(domain) = &target.host {
            let domain_lower = domain.to_ascii_lowercase();

            // Check if domain resolves literally to private IP
            if self.block_private_ips {
                if domain_lower == "localhost"
                    || domain_lower == "127.0.0.1"
                    || domain_lower == "::1"
                    || domain_lower.ends_with(".localhost")
                    || domain_lower.ends_with(".local")
                {
                    return Err("Access to localhost is blocked (SSRF guard)");
                }
            }

            // Check domain blacklist (exact match and suffix match)
            let set = self.domain_blacklist.read().unwrap();
            if set.contains(&domain_lower) {
                return Err("Target domain is in global blacklist");
            }

            for blocked in set.iter() {
                if let Some(suffix) = blocked.strip_prefix("*.") {
                    if domain_lower == suffix || domain_lower.ends_with(&format!(".{suffix}")) {
                        return Err("Target domain matches wildcard blacklist");
                    }
                }
            }
        }

        Ok(())
    }
}

pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(v4),
        IpAddr::V6(v6) => is_private_ipv6(v6),
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    // Loopback 127.0.0.0/8
    if ip.is_loopback() {
        return true;
    }
    // Private ranges: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
    let octets = ip.octets();
    if octets[0] == 10 {
        return true;
    }
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return true;
    }
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    // Link-local / Cloud metadata: 169.254.0.0/16 (e.g. AWS/GCP 169.254.169.254)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }
    // Broadcast / Unspecified
    if ip.is_broadcast() || ip.is_unspecified() {
        return true;
    }
    false
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // IPv4-mapped IPv6
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_ipv4(v4);
    }
    // Unique local address fc00::/7
    let segments = ip.segments();
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    false
}
