pub mod rules;
pub mod v1;

use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use crate::{
    error::InboundError,
    inbound::http::ReverseHttpRequest,
    pool::ProxyPool,
    protocol::{Credentials, TargetHost},
    reverse::{rules::AccountMatcherRuntime, v1::parse_resin_path},
};

pub struct ReverseProxyHandler {
    pool: Arc<ProxyPool>,
    rules_runtime: Option<Arc<AccountMatcherRuntime>>,
    security: Option<Arc<crate::security::SecurityFilter>>,
    proxy_token: Option<String>,
    auth: Option<Credentials>,
}

impl ReverseProxyHandler {
    pub fn new(
        pool: Arc<ProxyPool>,
        rules_runtime: Option<Arc<AccountMatcherRuntime>>,
        proxy_token: Option<String>,
        auth: Option<Credentials>,
    ) -> Self {
        Self {
            pool,
            rules_runtime,
            security: None,
            proxy_token,
            auth,
        }
    }

    pub fn with_security(mut self, security: Arc<crate::security::SecurityFilter>) -> Self {
        self.security = Some(security);
        self
    }

    pub fn auth(&self) -> Option<&Credentials> {
        self.auth.as_ref()
    }

    /// Handle standard incoming HTTP request (GET, POST, etc.) for reverse proxying.
    /// Supports:
    /// 1. Resin V1 Path routing: `/{token}/{platform.account}/{protocol}/{host}/{path}`
    /// 2. Legacy path routing: `/target/{target_authority}/{rest_of_path}`
    pub async fn handle(
        &self,
        req_session: ReverseHttpRequest,
    ) -> Result<(), InboundError> {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);
        let parsed_len = req
            .parse(&req_session.header_bytes)
            .map_err(|e| InboundError::MalformedRequest(format!("HTTP parse error: {e:?}")))?;

        if parsed_len.is_partial() {
            return Err(InboundError::MalformedRequest("Incomplete HTTP request".into()));
        }

        let path = req.path.unwrap_or("/");
        let method = req.method.unwrap_or("GET");

        // Parse Resin V1 path routing
        let parsed_target = parse_resin_path(
            path,
            self.proxy_token.as_deref(),
            req.headers,
            self.rules_runtime.as_ref(),
        )?;

        let target = parsed_target.target;
        let target_path = parsed_target.target_path;

        // Fast Security Check (SSRF & Domain Blacklist)
        if let Some(sec) = &self.security {
            if let Err(reason) = sec.check_target(&target) {
                return Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    reason,
                )));
            }
        }

        // Resolve account: first from URL segment, fallback to Trie rules & headers
        let effective_account = if let Some(acc) = parsed_target.account {
            Some(acc)
        } else if let Some(rules) = &self.rules_runtime {
            let host_str = match &target.host {
                TargetHost::Domain(d) => d.to_string(),
                TargetHost::Ip(ip) => ip.to_string(),
            };
            rules.load().resolve_account(&host_str, &target_path, |name| {
                for h in req.headers.iter() {
                    if h.name.eq_ignore_ascii_case(name) {
                        return std::str::from_utf8(h.value).ok();
                    }
                }
                None
            })
        } else {
            None
        };

        // Forward to target through proxy pool (using sticky session if account is present!)
        let mut conn = if let Some(account) = effective_account {
            self.pool.connect_sticky(&account, &target).await.map_err(|e| {
                InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Sticky proxy connect failed: {e}"),
                ))
            })?
        } else {
            self.pool.connect_with_retry(&target).await.map_err(|e| {
                InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Proxy pool connect failed: {e}"),
                ))
            })?
        };

        // Reconstruct request line with target_path
        let mut fwd_headers = Vec::with_capacity(512);
        use std::io::Write;
        write!(&mut fwd_headers, "{method} {target_path} HTTP/1.1\r\n").ok();

        // Copy original headers, modifying Host and stripping internal proxy headers
        let host_val = target.to_http_authority_bytes();
        write!(&mut fwd_headers, "Host: {}\r\n", String::from_utf8_lossy(&host_val)).ok();

        for h in req.headers.iter() {
            if !h.name.eq_ignore_ascii_case("Host")
                && !h.name.eq_ignore_ascii_case("Proxy-Connection")
                && !h.name.eq_ignore_ascii_case("Proxy-Authorization")
                && !h.name.eq_ignore_ascii_case("X-Resin-Account")
            {
                fwd_headers.extend_from_slice(h.name.as_bytes());
                fwd_headers.extend_from_slice(b": ");
                fwd_headers.extend_from_slice(h.value);
                fwd_headers.extend_from_slice(b"\r\n");
            }
        }
        fwd_headers.extend_from_slice(b"\r\n");

        conn.stream.write_all(&fwd_headers).await?;

        // Forward any body that was read
        if !req_session.body_residual.is_empty() {
            conn.stream.write_all(&req_session.body_residual).await?;
        }

        // Forward residual buffer in stream
        let unconsumed = req_session.stream.buffer();
        if !unconsumed.is_empty() {
            conn.stream.write_all(unconsumed).await?;
        }

        let mut client_socket = req_session.stream.into_inner();
        let relay_res = tokio::io::copy_bidirectional(&mut client_socket, &mut conn.stream).await;
        conn.record_relay_result(&relay_res, 3, std::time::Duration::from_secs(10));

        Ok(())
    }
}
