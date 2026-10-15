# Network Protocol & Gateway Pitfalls & Architecture (EXPERIENCES/PROTOCOL_GATEWAY.md)

### Pitfall 1: TCP Packet Fragmentation Causes Sniffer Misclassification
- **Symptom**: Downstream clients streaming CONNECT requests byte-by-byte or under slow network conditions trigger premature connection resets (`ConnectionReset` / broken pipe) or 404 instead of tunneling.
- **Cause**: Using `stream.peek(&mut [0u8; 8])` and checking `buf.starts_with(b"CONNECT ")` assumes the entire word arrives in the first TCP segment. In reality, TCP has no message boundaries; `peek` may return only `b"C"`, causing premature fallback to reverse proxy.
- **Fix**: Peek strictly 1 byte: `0x05` goes to SOCKS5; any other byte routes into the HTTP subsystem. The HTTP handshake then buffers the request line incrementally until complete, safely inspecting `method == "CONNECT"`.
- **Scope**: Any single-port multi-protocol listener over streaming transports (TCP/TLS).
- **Verification**: Integration test `test_tcp_fragmentation_connect` in `tests/gateway_v1.rs` transmitting CONNECT byte-by-byte with delays.

---

### Pitfall 2: Early Success Commit Destroys Upstream Retry Semantics
- **Symptom**: Transparent proxy retry cannot be implemented, and clients receive broken tunnels immediately after receiving HTTP 200 or SOCKS5 0x00 when upstream proxy nodes fail.
- **Cause**: The gateway committed success (200 OK / SOCKS5 REP 0x00) immediately after parsing downstream headers, before dialing the upstream proxy. Once downstream enters tunnel mode, any failure cannot be gracefully retried or cleanly reported as 502/REP failure.
- **Fix**: Pipeline split into: `sniff -> authenticate -> parse target -> dial upstream with retry -> commit success -> relay residual + raw socket`. If all upstream retries fail, reply with HTTP 502 Bad Gateway or SOCKS5 REP error code.
- **Scope**: All forward tunneling gateways (HTTP CONNECT, SOCKS5).
- **Verification**: `test_pool_retry_on_first_node_failure` and `test_socks5_delayed_commit_and_auth` in `tests/gateway_v1.rs`.

---

### Pitfall 3: Rust Logical Negation Operator Precedence with Unsigned Integers
- **Symptom**: Correctly formatted HTTP Basic authorization headers are silently rejected with `407 Proxy Authentication Required`.
- **Cause**: Writing `if !trimmed.len() >= 6` in Rust applies bitwise NOT `!` to `usize` (`!20usize == 18446744073709551595`), which is always `>= 6`, causing the condition to always be true and returning `None`.
- **Fix**: Write explicitly `if trimmed.len() < 6`.
- **Scope**: Rust length checks and boundary assertions.
- **Verification**: `test_http_connect_delayed_commit_and_auth` in `tests/gateway_v1.rs`.

---

### Pitfall 4: Conflating Upstream Proxy DNS with Target Address DNS
- **Symptom**: Either SOCKS5h fails to provide geo-unblocking / remote DNS privacy, or concurrent gateway connections get bottlenecked by blocking local DNS queries.
- **Cause**: Treating upstream proxy addresses and downstream target addresses with the same `Endpoint` abstraction.
- **Fix**:
  1. Upstream proxy hosts are resolved into `Arc<[SocketAddr]>` upon configuration loading / snapshot building, completely eliminating runtime DNS latency in the hot path.
  2. Downstream target hosts (`TargetAddr`) maintain `TargetHost::Domain` intact so that SOCKS5h can forward the domain name directly to the remote exit node.
- **Scope**: Multi-hop proxy pools and protocol converters.
- **Verification**: `tests/outbound.rs` and `test_socks5_delayed_commit_and_auth`.

---

### Pitfall 5: Obscura Browser Automation Local SSRF Guard Blocks Loopback Addresses
- **Symptom**: Calling `browser_navigate` to `http://127.0.0.1:...` or `http://localhost:...` during E2E UI testing fails with `Network error: Access to private/internal IP address 127.0.0.1 is not allowed`.
- **Cause**: Obscura MCP server enforces SSRF protection by default, blocking navigation to RFC 1918 private subnets and 127.0.0.1 loopback addresses.
- **Fix**:
  1. For headless UI testing of local embedded services, start Obscura with `OBSCURA_ALLOW_PRIVATE_NETWORK=1` or `--allow-private-network`;
  2. In automated Rust integration tests, verify embedded single-binary asset responses, MIME types, and SPA History Fallback directly via HTTP clients (`reqwest`).
- **Scope**: All local frontend/UI automation and browser testing against dev servers.
- **Verification**: Integration test `test_admin_api_endpoints_and_rule_sandbox` step 5 verifying `GET /ui/`, `/ui/nodes`, and 404 behavior.
