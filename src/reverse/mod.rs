pub mod rules;
pub mod v1;

use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use crate::{
    error::InboundError,
    inbound::http::ReverseHttpRequest,
    pool::ProxyPool,
    protocol::Credentials,
    reverse::{rules::AccountMatcherRuntime, v1::parse_resin_path},
};

pub struct ReverseProxyHandler {
    pool: Arc<ProxyPool>,
    rules_runtime: Option<Arc<AccountMatcherRuntime>>,
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
            proxy_token,
            auth,
        }
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

        // Forward to target through proxy pool (using sticky session if account is present!)
        let (mut upstream, _upstream_residual, _node) = if let Some(account) = parsed_target.account {
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

        upstream.write_all(&fwd_headers).await?;

        // Forward any body that was read
        if !req_session.body_residual.is_empty() {
            upstream.write_all(&req_session.body_residual).await?;
        }

        // Forward residual buffer in stream
        let unconsumed = req_session.stream.buffer();
        if !unconsumed.is_empty() {
            upstream.write_all(unconsumed).await?;
        }

        let mut client_socket = req_session.stream.into_inner();
        let _ = tokio::io::copy_bidirectional(&mut client_socket, &mut upstream).await;

        Ok(())
    }
}
