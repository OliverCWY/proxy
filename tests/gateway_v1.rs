use std::{sync::Arc, time::Duration};
use proxy::{
    forward::ForwardHandler,
    inbound::{InboundSession, sniff_inbound},
    outbound::{OutboundNodeConfig, OutboundProtocol},
    pool::{ProxyNode, ProxyPool, RoundRobin},
    protocol::{Credentials, TargetAddr},
    reverse::ReverseProxyHandler,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Helper: run an echo server that responds to requests
async fn run_mock_echo_server() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let dummy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    (dummy_listener, port)
}

#[tokio::test]
async fn test_pool_retry_on_first_node_failure() {
    let (_echo_l, echo_port) = run_mock_echo_server().await;

    // Node 1: points to closed port (will fail TCP connect)
    let bad_node = Arc::new(ProxyNode::new(
        1,
        "bad-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec!["127.0.0.1:1".parse().unwrap()].into_boxed_slice()),
            protocol: OutboundProtocol::Http { auth_header: None },
        },
    ));

    // Node 2: direct connection (will succeed)
    let good_node = Arc::new(ProxyNode::new(
        2,
        "good-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));

    let pool = ProxyPool::new(vec![bad_node, good_node])
        .with_selector(Box::new(RoundRobin::default()))
        .with_timeouts(Duration::from_millis(500), 2);

    let target = TargetAddr::from_ip("127.0.0.1".parse().unwrap(), echo_port);

    // Connecting with retry should transparently succeed using good_node
    let res = pool.connect_with_retry(&target).await;
    assert!(res.is_ok(), "Expected retry to succeed on good node");
    let mut conn = res.unwrap();
    assert_eq!(conn.node.id, 2);

    // Verify echo
    conn.stream.write_all(b"ping").await.unwrap();
    let mut resp = [0u8; 4];
    conn.stream.read_exact(&mut resp).await.unwrap();
    assert_eq!(&resp, b"ping");
}

#[tokio::test]
async fn test_socks5_delayed_commit_and_auth() {
    let (_echo_l, echo_port) = run_mock_echo_server().await;

    let pool = Arc::new(
        ProxyPool::new(vec![Arc::new(ProxyNode::new(
            1,
            "direct",
            OutboundNodeConfig {
                dial_addrs: Arc::from(vec![].into_boxed_slice()),
                protocol: OutboundProtocol::Direct,
            },
        ))]),
    );

    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = gateway.local_addr().unwrap().port();

    let auth = Some(Credentials::new("alice", "secret123"));
    let fwd = Arc::new(ForwardHandler::new(pool, auth));

    tokio::spawn(async move {
        while let Ok((socket, _)) = gateway.accept().await {
            let f = fwd.clone();
            tokio::spawn(async move {
                if let Ok(InboundSession::Socks5(handshake)) = sniff_inbound(socket).await {
                    let _ = f.handle_socks5(handshake).await;
                }
            });
        }
    });

    // 1. Client connects with bad password -> must be rejected
    {
        let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
        // Client greeting: supports USER/PASS (0x02)
        client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x02]);

        // Subnegotiation auth with wrong password
        // [0x01, ulen, "alice", plen, "wrong"]
        client.write_all(&[0x01, 5, b'a', b'l', b'i', b'c', b'e', 5, b'w', b'r', b'o', b'n', b'g']).await.unwrap();
        let mut auth_resp = [0u8; 2];
        client.read_exact(&mut auth_resp).await.unwrap();
        assert_eq!(auth_resp, [0x01, 0x01]); // 0x01 = failure
    }

    // 2. Client connects with correct password -> succeeds and echoes
    {
        let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
        client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x02]);

        // Auth with correct password
        client.write_all(&[0x01, 5, b'a', b'l', b'i', b'c', b'e', 9, b's', b'e', b'c', b'r', b'e', b't', b'1', b'2', b'3']).await.unwrap();
        let mut auth_resp = [0u8; 2];
        client.read_exact(&mut auth_resp).await.unwrap();
        assert_eq!(auth_resp, [0x01, 0x00]); // 0x00 = success

        // SOCKS5 CONNECT to echo server
        let mut req = vec![0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1];
        req.extend_from_slice(&echo_port.to_be_bytes());
        client.write_all(&req).await.unwrap();

        // Gateway only commits success AFTER connecting upstream!
        let mut rep = [0u8; 10];
        client.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep[0..3], [0x05, 0x00, 0x00]); // 0x00 = succeeded!

        // Test bidirectional echo
        client.write_all(b"hello socks5").await.unwrap();
        let mut echo_buf = [0u8; 12];
        client.read_exact(&mut echo_buf).await.unwrap();
        assert_eq!(&echo_buf, b"hello socks5");
    }
}

#[tokio::test]
async fn test_http_connect_delayed_commit_and_auth() {
    let (_echo_l, echo_port) = run_mock_echo_server().await;

    let pool = Arc::new(
        ProxyPool::new(vec![Arc::new(ProxyNode::new(
            1,
            "direct",
            OutboundNodeConfig {
                dial_addrs: Arc::from(vec![].into_boxed_slice()),
                protocol: OutboundProtocol::Direct,
            },
        ))]),
    );

    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = gateway.local_addr().unwrap().port();

    let auth = Some(Credentials::new("bob", "pass456"));
    let fwd = Arc::new(ForwardHandler::new(pool, auth));

    tokio::spawn(async move {
        while let Ok((socket, _)) = gateway.accept().await {
            let f = fwd.clone();
            tokio::spawn(async move {
                if let Ok(InboundSession::Http(handshake)) = sniff_inbound(socket).await {
                    if let Ok(proxy::inbound::http::HttpInboundSession::Connect(session)) = handshake.parse().await {
                        let _ = f.handle_http_connect(session).await;
                    }
                }
            });
        }
    });

    // 1. Without auth header -> 407 Proxy Authentication Required
    {
        let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
        let req = format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\n\r\n");
        client.write_all(req.as_bytes()).await.unwrap();

        let mut resp = [0u8; 256];
        let n = client.read(&mut resp).await.unwrap();
        let s = String::from_utf8_lossy(&resp[..n]);
        assert!(s.contains("407 Proxy Authentication Required"));
    }

    // 2. With valid auth -> 200 Connection Established and tunnel works
    {
        let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
        // bob:pass456 in base64 is Ym9iOnBhc3M0NTY=
        let req = format!(
            "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\nProxy-Authorization: Basic Ym9iOnBhc3M0NTY=\r\n\r\n"
        );
        client.write_all(req.as_bytes()).await.unwrap();

        let mut resp = [0u8; 256];
        let n = client.read(&mut resp).await.unwrap();
        let s = String::from_utf8_lossy(&resp[..n]);
        println!("Received from gateway: {:?}", s);
        assert!(s.contains("200 Connection Established"));

        // Echo payload
        client.write_all(b"tunnel data").await.unwrap();
        let mut data = [0u8; 11];
        client.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"tunnel data");
    }
}

#[tokio::test]
async fn test_tcp_fragmentation_connect() {
    let (_echo_l, echo_port) = run_mock_echo_server().await;

    let pool = Arc::new(
        ProxyPool::new(vec![Arc::new(ProxyNode::new(
            1,
            "direct",
            OutboundNodeConfig {
                dial_addrs: Arc::from(vec![].into_boxed_slice()),
                protocol: OutboundProtocol::Direct,
            },
        ))]),
    );

    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = gateway.local_addr().unwrap().port();

    let fwd = Arc::new(ForwardHandler::new(pool, None));

    tokio::spawn(async move {
        while let Ok((socket, _)) = gateway.accept().await {
            let f = fwd.clone();
            tokio::spawn(async move {
                if let Ok(InboundSession::Http(handshake)) = sniff_inbound(socket).await {
                    if let Ok(proxy::inbound::http::HttpInboundSession::Connect(session)) = handshake.parse().await {
                        let _ = f.handle_http_connect(session).await;
                    }
                }
            });
        }
    });

    let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();

    // Send CONNECT byte by byte to test fragmentation resistance
    let req = format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\n\r\n");
    for b in req.as_bytes() {
        client.write_all(&[*b]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let mut resp = [0u8; 256];
    let n = client.read(&mut resp).await.unwrap();
    let s = String::from_utf8_lossy(&resp[..n]);
    assert!(s.contains("200 Connection Established"));
}

#[tokio::test]
async fn test_reverse_proxy_path_routing() {
    // Run an HTTP mock upstream
    let http_upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = http_upstream.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = http_upstream.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let body = "Hello from Reverse Upstream";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    let pool = Arc::new(
        ProxyPool::new(vec![Arc::new(ProxyNode::new(
            1,
            "direct",
            OutboundNodeConfig {
                dial_addrs: Arc::from(vec![].into_boxed_slice()),
                protocol: OutboundProtocol::Direct,
            },
        ))]),
    );

    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = gateway.local_addr().unwrap().port();

    let rev = Arc::new(ReverseProxyHandler::new(pool, None, None, None));

    tokio::spawn(async move {
        while let Ok((socket, _)) = gateway.accept().await {
            let r = rev.clone();
            tokio::spawn(async move {
                if let Ok(InboundSession::Http(handshake)) = sniff_inbound(socket).await {
                    if let Ok(proxy::inbound::http::HttpInboundSession::Reverse(req)) = handshake.parse().await {
                        let _ = r.handle(req).await;
                    }
                }
            });
        }
    });

    let mut client = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
    let req = format!("GET /target/127.0.0.1:{upstream_port}/api/hello HTTP/1.1\r\nHost: gateway\r\n\r\n");
    client.write_all(req.as_bytes()).await.unwrap();

    let mut resp = [0u8; 512];
    let n = client.read(&mut resp).await.unwrap();
    let s = String::from_utf8_lossy(&resp[..n]);
    assert!(s.contains("200 OK"));
    assert!(s.contains("Hello from Reverse Upstream"));
}

#[tokio::test]
async fn test_sticky_session_and_same_ip_rotation() {
    let (_echo_l, echo_port) = run_mock_echo_server().await;
    let target = TargetAddr::from_ip("127.0.0.1".parse().unwrap(), echo_port);

    // Verified public egress IP: 198.51.100.42
    let shared_egress_ip: std::net::IpAddr = "198.51.100.42".parse().unwrap();

    let node1 = Arc::new(ProxyNode::new(
        1,
        "us-node-1",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec!["104.20.1.1:1080".parse().unwrap()].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));
    node1.set_probed_egress_ip(shared_egress_ip);

    let node2 = Arc::new(ProxyNode::new(
        2,
        "us-node-2",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec!["104.20.1.1:1081".parse().unwrap()].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));
    node2.set_probed_egress_ip(shared_egress_ip);

    let pool = ProxyPool::new(vec![node1.clone(), node2.clone()])
        .with_sticky_ttl(Duration::from_secs(60));
    pool.leases().update_node_egress_ip(node1.id, None, Some(shared_egress_ip));
    pool.leases().update_node_egress_ip(node2.id, None, Some(shared_egress_ip));

    // Request 1: first request from account "user_A" assigns a lease
    let conn1 = pool.connect_sticky("user_A", &target).await.unwrap();
    let assigned_id = conn1.node.id;

    // Request 2: subsequent request from "user_A" MUST route to the same node
    let conn2 = pool.connect_sticky("user_A", &target).await.unwrap();
    assert_eq!(conn2.node.id, assigned_id, "Sticky session should route to same node");

    // Simulate node failure: mark the assigned node cooling down
    let assigned_node = if assigned_id == 1 { &node1 } else { &node2 };
    assigned_node.mark_failure(1, Duration::from_secs(60)); // enters cooldown

    // Request 3: assigned node is now cooling down -> MUST failover to candidate sharing verified Egress IP!
    let conn3 = pool.connect_sticky("user_A", &target).await.unwrap();
    let backup_id = if assigned_id == 1 { 2 } else { 1 };
    assert_eq!(conn3.node.id, backup_id, "Should failover to backup node with same Egress IP");
}

#[test]
fn test_account_header_rule_trie_matcher() {
    use proxy::reverse::rules::{AccountMatcher, AccountRule};

    let rules = vec![
        AccountRule {
            url_prefix: "api.twitter.com/v1".into(),
            headers: vec!["x-account-id".into(), "authorization".into()],
        },
        AccountRule {
            url_prefix: "api.twitter.com/v1/auth".into(),
            headers: vec!["x-auth-account".into()],
        },
        AccountRule {
            url_prefix: "*".into(),
            headers: vec!["x-default-account".into()],
        },
    ];

    let matcher = AccountMatcher::new(&rules);

    // 1. Longest prefix match: /v1/auth/token should match "api.twitter.com/v1/auth"
    let headers = matcher.match_url("api.twitter.com:443", "/v1/auth/token").unwrap();
    assert_eq!(headers, &["x-auth-account"]);

    // 2. Shorter prefix match: /v1/users should match "api.twitter.com/v1"
    let headers = matcher.match_url("api.twitter.com", "/v1/users").unwrap();
    assert_eq!(headers, &["x-account-id", "authorization"]);

    // 3. Fallback to wildcard rule
    let headers = matcher.match_url("other.domain.com", "/hello").unwrap();
    assert_eq!(headers, &["x-default-account"]);

    // 4. Resolve account from headers function
    let account = matcher.resolve_account("api.twitter.com", "/v1/users", |name| {
        if name == "authorization" {
            Some("Bearer my-token-user")
        } else {
            None
        }
    });
    assert_eq!(account, Some("Bearer my-token-user".into()));
}

#[tokio::test]
async fn test_admin_api_endpoints_and_rule_sandbox() {
    let pool = Arc::new(ProxyPool::new(vec![Arc::new(ProxyNode::new(
        1,
        "direct-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ))]));

    let rules_runtime = Arc::new(proxy::reverse::rules::AccountMatcherRuntime::new(&[
        proxy::reverse::rules::AccountRule {
            url_prefix: "api.github.com".into(),
            headers: vec!["x-github-user".into()],
        },
        proxy::reverse::rules::AccountRule {
            url_prefix: "*".into(),
            headers: vec!["authorization".into()],
        },
    ]));

    let state = proxy::api::AppState {
        pool,
        rules_runtime,
        security: Arc::new(proxy::security::SecurityFilter::default()),
        admin_token: None,
        proxy_port: 1099,
    };

    let router = proxy::api::create_api_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // 1. System Info
    let info: serde_json::Value = client
        .get(format!("{base}/api/v1/system/info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["status"], "healthy");
    assert_eq!(info["total_nodes"], 1);
    assert_eq!(info["anonymous_allowed"], true);

    // 2. Nodes List
    let nodes: serde_json::Value = client
        .get(format!("{base}/api/v1/nodes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nodes["total"], 1);
    assert_eq!(nodes["items"][0]["tag"], "direct-node");

    // 3. Rule Sandbox Resolve
    let mut req_body = serde_json::json!({
        "url": "https://api.github.com/repos/rust-lang/rust",
        "headers": {
            "x-github-user": "octocat",
            "authorization": "Bearer token123"
        }
    });

    let res: serde_json::Value = client
        .post(format!("{base}/api/v1/rules:resolve"))
        .json(&req_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["extracted_account"], "octocat");

    // 4. Fallback rule resolve
    req_body["url"] = serde_json::json!("https://example.com/api");
    let res: serde_json::Value = client
        .post(format!("{base}/api/v1/rules:resolve"))
        .json(&req_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["extracted_account"], "Bearer token123");

    // 5. Embedded Web UI & SPA Fallback Tests
    let ui_resp = client.get(format!("{base}/ui/")).send().await.unwrap();
    assert_eq!(ui_resp.status(), 200);
    let ui_html = ui_resp.text().await.unwrap();
    assert!(ui_html.contains("html"), "Should return embedded index.html");

    // SPA History Route fallback: /ui/nodes has no extension -> should return index.html
    let spa_resp = client.get(format!("{base}/ui/nodes")).send().await.unwrap();
    assert_eq!(spa_resp.status(), 200);

    // Missing file with extension -> should return 404
    let missing_resp = client.get(format!("{base}/ui/missing-image.png")).send().await.unwrap();
    assert_eq!(missing_resp.status(), 404);

    // 6. Dynamic Rules API Test (list, upsert, delete)
    let rules_list: Vec<serde_json::Value> = client
        .get(format!("{base}/api/v1/rules"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rules_list.len(), 2);

    // Upsert a new rule
    let upsert_status = client
        .put(format!("{base}/api/v1/rules/api.slack.com"))
        .json(&serde_json::json!({ "headers": ["x-slack-team"] }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(upsert_status, 200);

    let rules_after_upsert: Vec<serde_json::Value> = client
        .get(format!("{base}/api/v1/rules"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rules_after_upsert.len(), 3);

    // Delete the rule
    let del_status = client
        .delete(format!("{base}/api/v1/rules/api.slack.com"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(del_status, 200);

    // 7. Prometheus /metrics Endpoint Test
    let metrics_resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(metrics_resp.status(), 200);
    let metrics_text = metrics_resp.text().await.unwrap();
    assert!(metrics_text.contains("proxy_nodes_total"));
    assert!(metrics_text.contains("proxy_nodes_healthy"));
    assert!(metrics_text.contains("proxy_node_latency_ewma_ms"));
}

#[tokio::test]
async fn test_td_ewma_score_and_p2c_selection() {
    use proxy::pool::node::ProxyNode;
    use proxy::pool::selector::{PowerOfTwoChoices, Selector};
    use proxy::outbound::{OutboundNodeConfig, OutboundProtocol};
    use std::time::Duration;

    let node1 = Arc::new(ProxyNode::new(
        1,
        "fast-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));

    let node2 = Arc::new(ProxyNode::new(
        2,
        "slow-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));

    // Fast node has low latency samples
    node1.mark_success_with_rtt(Duration::from_millis(10));
    node1.mark_success_with_rtt(Duration::from_millis(15));

    // Slow node has high latency samples
    node2.mark_success_with_rtt(Duration::from_millis(800));
    node2.mark_success_with_rtt(Duration::from_millis(900));

    // Node 1 should have much lower score than Node 2
    assert!(node1.calculate_score() < node2.calculate_score());

    let selector = PowerOfTwoChoices;
    let nodes = vec![node1.clone(), node2.clone()];

    // Over multiple choices, fast node should be selected consistently
    let mut selected_node1 = 0;
    for _ in 0..100 {
        if let Some(chosen) = selector.select(&nodes) {
            if chosen.id == 1 {
                selected_node1 += 1;
            }
        }
    }
    assert_eq!(selected_node1, 100, "Fast node with lower TD-EWMA score must win in P2C");
}

#[tokio::test]
async fn test_single_port_multiplexing_and_resin_reverse_proxy() {
    use proxy::{
        forward::ForwardHandler,
        inbound::{InboundSession, sniff_inbound},
        outbound::{OutboundNodeConfig, OutboundProtocol},
        pool::{ProxyNode, ProxyPool},
        reverse::{ReverseProxyHandler, rules::{AccountMatcherRuntime, AccountRule}},
    };

    // 1. Mock upstream HTTP service
    let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n > 0 {
                    let req_str = String::from_utf8_lossy(&buf[..n]);
                    let body = format!("ECHO: {}", req_str.lines().next().unwrap_or(""));
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            });
        }
    });

    // 2. Setup gateway pool & rules
    let pool = Arc::new(ProxyPool::new(vec![Arc::new(ProxyNode::new(
        0,
        "direct",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ))]));

    let rules_runtime = Arc::new(AccountMatcherRuntime::new(&[AccountRule {
        url_prefix: "*".into(),
        headers: vec!["Authorization".into()],
    }]));

    let proxy_token = "secret-proxy-token".to_string();

    let forward_handler = Arc::new(ForwardHandler::new(pool.clone(), None));
    let reverse_handler = Arc::new(ReverseProxyHandler::new(
        pool.clone(),
        Some(rules_runtime.clone()),
        Some(proxy_token.clone()),
        None,
    ));

    // 3. Bind single gateway port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = listener.local_addr().unwrap().port();

    let api_state = proxy::api::AppState {
        pool: pool.clone(),
        rules_runtime: rules_runtime.clone(),
        security: Arc::new(proxy::security::SecurityFilter::new(false)),
        admin_token: None,
        proxy_port: gateway_port,
    };
    let api_router = proxy::api::create_api_router(api_state);

    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let fwd = forward_handler.clone();
            let rev = reverse_handler.clone();
            let router = api_router.clone();

            tokio::spawn(async move {
                match sniff_inbound(socket).await {
                    Ok(InboundSession::Socks5(handshake)) => {
                        let _ = fwd.handle_socks5(handshake).await;
                    }
                    Ok(InboundSession::Http(handshake)) => {
                        match handshake.parse().await {
                            Ok(proxy::inbound::http::HttpInboundSession::Connect(session)) => {
                                let _ = fwd.handle_http_connect(session).await;
                            }
                            Ok(proxy::inbound::http::HttpInboundSession::Admin(admin_stream)) => {
                                proxy::api::serve_single_connection(admin_stream, router).await;
                            }
                            Ok(proxy::inbound::http::HttpInboundSession::Reverse(rev_req)) => {
                                if let Err(e) = rev.handle(rev_req).await {
                                    eprintln!("[DEBUG rev.handle ERR]: {e:?}");
                                }
                            }
                            Err(e) => {
                                eprintln!("[DEBUG handshake.parse ERR]: {e:?}");
                            }
                        }
                    }
                    Err(_) => {}
                }
            });
        }
    });

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();

    // Test A: Admin endpoint on the SAME single port
    let info_resp: serde_json::Value = client
        .get(format!("http://127.0.0.1:{gateway_port}/api/v1/system/info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info_resp["status"], "healthy");

    // Test B: Prometheus /metrics on the SAME single port
    let metrics_resp = client
        .get(format!("http://127.0.0.1:{gateway_port}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics_resp.contains("proxy_nodes_total"));

    // Test C: Web UI /ui/ on the SAME single port
    let ui_resp = client
        .get(format!("http://127.0.0.1:{gateway_port}/ui/"))
        .send()
        .await
        .unwrap();
    assert_eq!(ui_resp.status(), 200);

    // Test D: Resin V1 reverse proxy request: /{token}/{platform.account}/{protocol}/{host}/{path}
    let resin_url = format!(
        "http://127.0.0.1:{gateway_port}/{proxy_token}/myPlatform.alice/http/127.0.0.1:{echo_port}/api/greet?foo=bar"
    );
    let rev_resp = client.get(&resin_url).send().await.unwrap();
    assert_eq!(rev_resp.status(), 200);
    let rev_text = rev_resp.text().await.unwrap();
    assert!(rev_text.contains("ECHO: GET /api/greet?foo=bar HTTP/1.1"));

    // Verify Sticky lease was created for "alice"
    assert!(pool.leases().get_lease("alice").is_some());

    // Test E: Reverse proxy with WRONG token should fail with connection refused or 403
    let wrong_token_url = format!(
        "http://127.0.0.1:{gateway_port}/invalidToken/myPlatform.bob/http/127.0.0.1:{echo_port}/test"
    );
    let wrong_resp = client.get(&wrong_token_url).send().await;
    // Client should receive error or disconnected
    assert!(wrong_resp.is_err() || !wrong_resp.unwrap().status().is_success());
}

#[tokio::test]
async fn test_security_ssrf_and_domain_blacklist() {
    use proxy::{
        outbound::{OutboundNodeConfig, OutboundProtocol},
        pool::{ProxyNode, ProxyPool},
        protocol::TargetAddr,
        reverse::rules::AccountMatcherRuntime,
        security::SecurityFilter,
    };

    let sec = SecurityFilter::new(true);

    // 1. SSRF Private IP check
    let private_target = TargetAddr::parse_authority("127.0.0.1:8080").unwrap();
    assert!(sec.check_target(&private_target).is_err(), "Localhost IP must be blocked");

    let cloud_meta = TargetAddr::parse_authority("169.254.169.254:80").unwrap();
    assert!(sec.check_target(&cloud_meta).is_err(), "Cloud metadata must be blocked");

    let internal_lan = TargetAddr::parse_authority("192.168.1.1:443").unwrap();
    assert!(sec.check_target(&internal_lan).is_err(), "LAN IP must be blocked");

    // Public IP allowed
    let public_target = TargetAddr::parse_authority("1.1.1.1:443").unwrap();
    assert!(sec.check_target(&public_target).is_ok());

    // 2. Domain Blacklist
    sec.add_blocked_domain("evil.com");
    sec.add_blocked_domain("*.badsite.org");

    let evil_target = TargetAddr::parse_authority("evil.com:443").unwrap();
    assert!(sec.check_target(&evil_target).is_err(), "Direct domain match must be blocked");

    let wildcard_target = TargetAddr::parse_authority("sub.badsite.org:443").unwrap();
    assert!(sec.check_target(&wildcard_target).is_err(), "Wildcard suffix match must be blocked");

    let safe_target = TargetAddr::parse_authority("github.com:443").unwrap();
    assert!(sec.check_target(&safe_target).is_ok());

    // 3. REST API control test: block & unblock node, blacklist management
    let node = Arc::new(ProxyNode::new(
        42,
        "test-node",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec![].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));
    assert!(node.is_available());

    let pool = Arc::new(ProxyPool::new(vec![node.clone()]));
    let sec_filter = Arc::new(SecurityFilter::new(true));

    let state = proxy::api::AppState {
        pool: pool.clone(),
        rules_runtime: Arc::new(AccountMatcherRuntime::new(&[])),
        security: sec_filter.clone(),
        admin_token: None,
        proxy_port: 8080,
    };

    let router = proxy::api::create_api_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Test A: Block node 42
    let res: serde_json::Value = client
        .post(format!("{base}/api/v1/nodes/42/actions/block"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["status"], "blocked");
    assert!(!node.is_available(), "Blocked node must not be available for routing");

    // Test B: Unblock node 42
    let res: serde_json::Value = client
        .post(format!("{base}/api/v1/nodes/42/actions/unblock"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["status"], "healthy");
    assert!(node.is_available(), "Unblocked node must be restored to available");

    // Test C: Manage blacklist via REST API
    let add_res: serde_json::Value = client
        .post(format!("{base}/api/v1/security/blacklist"))
        .json(&serde_json::json!({ "domain": "malicious.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(add_res["domain"], "malicious.com");

    let list_res: Vec<String> = client
        .get(format!("{base}/api/v1/security/blacklist"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list_res.contains(&"malicious.com".to_string()));

    let del_res: serde_json::Value = client
        .delete(format!("{base}/api/v1/security/blacklist/malicious.com"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(del_res["success"], true);
}

#[tokio::test]
async fn test_admin_api_token_auth() {
    let pool = Arc::new(ProxyPool::new(vec![]));
    let rules_runtime = Arc::new(proxy::reverse::rules::AccountMatcherRuntime::new(&[]));
    let security = Arc::new(proxy::security::SecurityFilter::default());

    let state = proxy::api::AppState {
        pool,
        rules_runtime,
        security,
        admin_token: Some("superSecretAdminToken".into()),
        proxy_port: 8080,
    };

    let router = proxy::api::create_api_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // 1. Without Token -> 401 Unauthorized
    let resp = client.get(format!("{base}/api/v1/system/info")).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    // 2. With Wrong Token -> 401 Unauthorized
    let resp = client
        .get(format!("{base}/api/v1/system/info"))
        .header("Authorization", "Bearer wrongToken")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // 3. With Valid Bearer Token -> 200 OK
    let resp = client
        .get(format!("{base}/api/v1/system/info"))
        .header("Authorization", "Bearer superSecretAdminToken")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 4. With Valid Query Token (?token=...) -> 200 OK
    let resp = client
        .get(format!("{base}/api/v1/system/info?token=superSecretAdminToken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
