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
    let (mut stream, _residual, node) = res.unwrap();
    assert_eq!(node.id, 2);

    // Verify echo
    stream.write_all(b"ping").await.unwrap();
    let mut resp = [0u8; 4];
    stream.read_exact(&mut resp).await.unwrap();
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

    let rev = Arc::new(ReverseProxyHandler::new(pool, None));

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

    // Node 1 & Node 2 both share the same egress IP: 104.20.1.1
    let node1 = Arc::new(ProxyNode::new(
        1,
        "us-node-1",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec!["104.20.1.1:1080".parse().unwrap()].into_boxed_slice()),
            protocol: OutboundProtocol::Direct, // for test echo, use Direct
        },
    ));

    let node2 = Arc::new(ProxyNode::new(
        2,
        "us-node-2",
        OutboundNodeConfig {
            dial_addrs: Arc::from(vec!["104.20.1.1:1081".parse().unwrap()].into_boxed_slice()),
            protocol: OutboundProtocol::Direct,
        },
    ));

    let pool = ProxyPool::new(vec![node1.clone(), node2.clone()])
        .with_sticky_ttl(Duration::from_secs(60));

    // Request 1: first request from account "user_A" assigns a lease
    let (_, _, picked1) = pool.connect_sticky("user_A", &target).await.unwrap();
    let assigned_id = picked1.id;

    // Request 2: subsequent request from "user_A" MUST route to the same node
    let (_, _, picked2) = pool.connect_sticky("user_A", &target).await.unwrap();
    assert_eq!(picked2.id, assigned_id, "Sticky session should route to same node");

    // Simulate node failure: mark the assigned node cooling down
    let assigned_node = if assigned_id == 1 { &node1 } else { &node2 };
    assigned_node.mark_failure(1, Duration::from_secs(60)); // enters cooldown

    // Request 3: assigned node is now cooling down -> MUST failover to candidate sharing the same Egress IP!
    let (_, _, picked3) = pool.connect_sticky("user_A", &target).await.unwrap();
    let backup_id = if assigned_id == 1 { 2 } else { 1 };
    assert_eq!(picked3.id, backup_id, "Should failover to backup node with same Egress IP");
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
