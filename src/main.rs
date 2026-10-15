use std::{path::Path, sync::Arc, time::Duration};

use proxy::{
    error::InboundError,
    forward::ForwardHandler,
    inbound::{InboundSession, sniff_inbound},
    outbound::{OutboundNodeConfig, OutboundProtocol, SocksDnsMode},
    pool::{PowerOfTwoChoices, ProxyNode, ProxyPool},
    protocol::Credentials,
    reverse::ReverseProxyHandler,
};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
};

async fn parse_node_list(path: &Path) -> anyhow::Result<Vec<Arc<ProxyNode>>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut nodes = Vec::new();
    let mut id = 0;

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(node) = parse_proxy_line(id, line).await {
            nodes.push(Arc::new(node));
            id += 1;
        }
    }

    Ok(nodes)
}

async fn parse_proxy_line(id: usize, line: &str) -> Option<ProxyNode> {
    let url = line.trim();
    let (proto, rest) = if let Some(stripped) = url.strip_prefix("http://") {
        ("http", stripped)
    } else if let Some(stripped) = url.strip_prefix("socks5://") {
        ("socks5", stripped)
    } else if let Some(stripped) = url.strip_prefix("socks5h://") {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = std::env::var("PROXY_PORT")
        .or_else(|_| std::env::var("proxy_port"))
        .unwrap_or_else(|_| "1099".to_string())
        .parse::<u16>()?;

    // Inbound authentication configuration (optional)
    let auth_user = std::env::var("PROXY_USER").ok();
    let auth_pass = std::env::var("PROXY_PASS").ok();
    let inbound_auth = match (auth_user, auth_pass) {
        (Some(u), Some(p)) => Some(Credentials::new(u, p)),
        _ => None,
    };

    // Load proxy pool
    let mut nodes = Vec::new();
    if let Ok(proxy_file) = std::env::var("PROXY_LIST_FILE") {
        let p = Path::new(&proxy_file);
        if p.exists() {
            match parse_node_list(p).await {
                Ok(loaded) => {
                    println!("[*] Loaded {} proxies from {}", loaded.len(), proxy_file);
                    nodes = loaded;
                }
                Err(e) => eprintln!("[!] Failed to read proxy list: {e}"),
            }
        }
    }

    // Fallback: environment-configured single proxy or Direct
    if nodes.is_empty() {
        let proxy_type = std::env::var("PROXY_TYPE")
            .or_else(|_| std::env::var("proxy_type"))
            .unwrap_or_else(|_| "direct".to_string());

        let node = match proxy_type.as_str() {
            "http" => parse_proxy_line(
                0,
                "http://lbhts51pgjj7:tq2e5w2wrqx3rz3@104.207.53.122:3129",
            )
            .await,
            "socks5" => parse_proxy_line(
                0,
                "socks5h://lbhts51pgjj7:tq2e5w2wrqx3rz3@104.207.53.122:1081",
            )
            .await,
            _ => {
                // Direct connection node
                Some(ProxyNode::new(
                    0,
                    "direct",
                    OutboundNodeConfig {
                        dial_addrs: Arc::from(vec![].into_boxed_slice()),
                        protocol: OutboundProtocol::Direct,
                    },
                ))
            }
        };

        if let Some(n) = node {
            nodes.push(Arc::new(n));
        }
    }

    println!("[*] Active pool node count: {}", nodes.len());

    let pool = Arc::new(
        ProxyPool::new(nodes)
            .with_selector(Box::new(PowerOfTwoChoices))
            .with_timeouts(Duration::from_secs(8), 3),
    );

    let forward_handler = Arc::new(ForwardHandler::new(pool.clone(), inbound_auth.clone()));
    let reverse_handler = Arc::new(ReverseProxyHandler::new(pool.clone(), inbound_auth));

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    println!("[+] Proxy gateway listening on 0.0.0.0:{}", port);

    loop {
        let (socket, addr) = listener.accept().await?;

        let fwd = forward_handler.clone();
        let rev = reverse_handler.clone();

        tokio::spawn(async move {
            match sniff_inbound(socket).await {
                Ok(InboundSession::Socks5(handshake)) => {
                    if let Err(e) = fwd.handle_socks5(handshake).await {
                        if !matches!(e, InboundError::PrematureEof) {
                            eprintln!("[SOCKS5] Error from {}: {}", addr, e);
                        }
                    }
                }
                Ok(InboundSession::Http(handshake)) => {
                    match handshake.parse().await {
                        Ok(proxy::inbound::http::HttpInboundSession::Connect(session)) => {
                            if let Err(e) = fwd.handle_http_connect(session).await {
                                if !matches!(e, InboundError::PrematureEof) {
                                    eprintln!("[HTTP CONNECT] Error from {}: {}", addr, e);
                                }
                            }
                        }
                        Ok(proxy::inbound::http::HttpInboundSession::Reverse(rev_req)) => {
                            if let Err(e) = rev.handle(rev_req).await {
                                if !matches!(e, InboundError::PrematureEof) {
                                    eprintln!("[HTTP REVERSE] Error from {}: {}", addr, e);
                                }
                            }
                        }
                        Err(e) => {
                            if !matches!(e, InboundError::PrematureEof) {
                                eprintln!("[HTTP parse] Error from {}: {}", addr, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    if !matches!(e, InboundError::PrematureEof) {
                        eprintln!("[Sniff] Error from {}: {}", addr, e);
                    }
                }
            }
        });
    }
}
