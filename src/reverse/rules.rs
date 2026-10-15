use std::{collections::HashMap, net::IpAddr, sync::Arc};
use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRule {
    pub url_prefix: String,
    pub headers: Vec<String>,
}

#[derive(Default, Debug)]
struct TrieNode {
    children: HashMap<String, TrieNode>,
    headers: Option<Vec<String>>,
    prefix: Option<String>,
}

#[derive(Debug)]
pub struct AccountMatcher {
    root: TrieNode,
    wildcard: Option<Vec<String>>,
}

impl AccountMatcher {
    pub fn new(rules: &[AccountRule]) -> Self {
        let mut root = TrieNode::default();
        let mut wildcard = None;

        for rule in rules {
            let norm = normalize_rule_prefix(&rule.url_prefix);
            if norm == "*" {
                wildcard = Some(rule.headers.clone());
                continue;
            }

            let segments = split_prefix(&norm);
            let mut curr = &mut root;
            for seg in segments {
                curr = curr.children.entry(seg).or_default();
            }
            curr.headers = Some(rule.headers.clone());
            curr.prefix = Some(norm);
        }

        Self { root, wildcard }
    }

    /// Match URL against prefix tree, returning longest-matching header candidate list
    pub fn match_url(&self, host: &str, path: &str) -> Option<&[String]> {
        let norm_host = normalize_match_host(host);
        let path_segments = split_path(path);

        let mut curr = &self.root;
        let mut best_headers = None;

        // 1. Match host segment
        if let Some(child) = curr.children.get(&norm_host) {
            curr = child;
            if curr.headers.is_some() {
                best_headers = curr.headers.as_deref();
            }

            // 2. Walk path segments
            for seg in path_segments {
                if let Some(child) = curr.children.get(&seg) {
                    curr = child;
                    if curr.headers.is_some() {
                        best_headers = curr.headers.as_deref();
                    }
                } else {
                    break;
                }
            }
        }

        best_headers.or(self.wildcard.as_deref())
    }

    /// Resolve account from a list of header values based on matched rule
    pub fn resolve_account<'a, F>(&self, host: &str, path: &str, mut get_header: F) -> Option<String>
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        let header_names = self.match_url(host, path)?;
        for name in header_names {
            if let Some(val) = get_header(name) {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
}

pub struct AccountMatcherRuntime {
    current: ArcSwap<AccountMatcher>,
}

impl AccountMatcherRuntime {
    pub fn new(initial_rules: &[AccountRule]) -> Self {
        let matcher = AccountMatcher::new(initial_rules);
        Self {
            current: ArcSwap::from_pointee(matcher),
        }
    }

    pub fn load(&self) -> arc_swap::Guard<Arc<AccountMatcher>> {
        self.current.load()
    }

    /// Atomic hot-swap of entire rule tree without downtime or locks
    pub fn replace_rules(&self, new_rules: &[AccountRule]) {
        let new_matcher = AccountMatcher::new(new_rules);
        self.current.store(Arc::new(new_matcher));
    }
}

pub fn normalize_rule_prefix(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return "*".to_string();
    }

    let without_query = match trimmed.find('?') {
        Some(idx) => &trimmed[..idx],
        None => trimmed,
    };

    let (host, path) = match without_query.find('/') {
        Some(idx) => (&without_query[..idx], &without_query[idx..]),
        None => (without_query, ""),
    };

    let clean_host = normalize_match_host(host);
    format!("{clean_host}{path}")
}

pub fn normalize_match_host(raw_host: &str) -> String {
    let mut host = raw_host.trim().to_ascii_lowercase();

    // Strip [ipv6]:port or host:port
    if let Some(stripped) = host.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            let ip_part = &stripped[..end];
            if let Ok(ip) = ip_part.parse::<IpAddr>() {
                return ip.to_string();
            }
        }
    }

    if let Some(colon) = host.rfind(':') {
        if !host.contains(']') && host.find(':') == Some(colon) {
            host = host[..colon].to_string();
        }
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        ip.to_string()
    } else {
        host
    }
}

fn split_prefix(prefix: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let parts: Vec<&str> = prefix.splitn(2, '/').collect();
    segments.push(parts[0].to_ascii_lowercase());

    if parts.len() > 1 && !parts[1].is_empty() {
        for seg in parts[1].split('/') {
            if !seg.is_empty() {
                segments.push(seg.to_string());
            }
        }
    }

    segments
}

fn split_path(path: &str) -> Vec<String> {
    let without_query = match path.find('?') {
        Some(idx) => &path[..idx],
        None => path,
    };

    without_query
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}
