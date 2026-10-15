use std::sync::Arc;
use base64::prelude::*;
use crate::{
    outbound::{OutboundNodeConfig, OutboundProtocol, SocksDnsMode},
    pool::ProxyNode,
    protocol::Credentials,
};

pub struct SubscriptionParser;

impl SubscriptionParser {
    /// Parse subscription text (plain lines or base64 encoded lines) into ProxyNode list
    pub async fn parse_subscription_text(text: &str) -> Vec<Arc<ProxyNode>> {
        let trimmed = text.trim();
        let decoded_str = if let Ok(bytes) = BASE64_STANDARD.decode(trimmed.replace("\n", "").replace("\r", "")) {
            String::from_utf8(bytes).unwrap_or_else(|_| trimmed.to_string())
        } else {
            trimmed.to_string()
        };

        let mut nodes = Vec::new();
        let mut id = 1000;

        for line in decoded_str.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(node) = parse_line(id, line).await {
                nodes.push(Arc::new(node));
                id += 1;
            }
        }

        nodes
    }

    /// Fetch remote subscription URL and parse into ProxyNodes
    pub async fn fetch_and_parse(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<Arc<ProxyNode>>> {
        let resp = client.get(url).send().await?.text().await?;
        Ok(Self::parse_subscription_text(&resp).await)
    }
}

async fn parse_line(id: usize, line: &str) -> Option<ProxyNode> {
    let (proto, rest) = if let Some(stripped) = line.strip_prefix("http://") {
        ("http", stripped)
    } else if let Some(stripped) = line.strip_prefix("socks5://") {
        ("socks5", stripped)
    } else if let Some(stripped) = line.strip_prefix("socks5h://") {
        ("socks5h", stripped)
    } else {
        return None;
    };

    let (auth, host_port) = match rest.find('@') {
        Some(idx) => (Some(&rest[..idx]), &rest[idx + 1..]),
        None => (None, rest),
    };

    let colon = host_port.rfind(':')?;
    let host = &host_port[..colon];
    let port = host_port[colon + 1..].parse::<u16>().ok()?;

    let addrs = tokio::net::lookup_host((host, port)).await.ok()?.collect::<Vec<_>>();
    if addrs.is_empty() {
        return None;
    }

    let protocol = match proto {
        "http" => {
            let auth_header = auth.map(|a| {
                let creds = match a.find(':') {
                    Some(i) => Credentials::new(&a[..i], &a[i + 1..]),
                    None => Credentials::new(a, ""),
                };
                creds.to_http_basic_header()
            });
            OutboundProtocol::Http { auth_header }
        }
        "socks5" | "socks5h" => {
            let auth_payload = auth.map(|a| {
                let creds = match a.find(':') {
                    Some(i) => Credentials::new(&a[..i], &a[i + 1..]),
                    None => Credentials::new(a, ""),
                };
                creds.to_socks5_auth_payload()
            });
            let dns_mode = if proto == "socks5h" {
                SocksDnsMode::Remote
            } else {
                SocksDnsMode::Local
            };
            OutboundProtocol::Socks5 {
                auth_payload,
                dns_mode,
            }
        }
        _ => return None,
    };

    let config = OutboundNodeConfig {
        dial_addrs: Arc::from(addrs.into_boxed_slice()),
        protocol,
    };

    Some(ProxyNode::new(id, line, config))
}
