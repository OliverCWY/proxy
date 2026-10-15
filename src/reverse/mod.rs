pub mod rules;

use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use crate::{
    error::InboundError,
    inbound::http::ReverseHttpRequest,
    pool::ProxyPool,
    protocol::{Credentials, TargetAddr},
};

pub struct ReverseProxyHandler {
    pool: Arc<ProxyPool>,
    auth: Option<Credentials>,
}

impl ReverseProxyHandler {
    pub fn new(pool: Arc<ProxyPool>, auth: Option<Credentials>) -> Self {
        Self { pool, auth }
    }

    pub fn auth(&self) -> Option<&Credentials> {
        self.auth.as_ref()
    }

    /// Handle standard incoming HTTP request (GET, POST, etc.) for reverse proxying.
    /// Supports:
    /// 1. Path-based target routing: `/target/{target_authority}/{rest_of_path}`
    /// 2. Header-based routing: `Host` header resolution.
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

        // Parse path routing or host routing
        let (target, target_path) = parse_reverse_target(path, req.headers)?;

        // Forward to target through proxy pool
        let (mut upstream, _upstream_residual, _node) = self
            .pool
            .connect_with_retry(&target)
            .await
            .map_err(|e| {
                InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Proxy pool connect failed: {e}"),
                ))
            })?;

        // Reconstruct request line with target_path
        let mut fwd_headers = Vec::with_capacity(512);
        use std::io::Write;
        write!(&mut fwd_headers, "{method} {target_path} HTTP/1.1\r\n").ok();

        // Copy original headers, modifying Host
        let host_val = target.to_http_authority_bytes();
        write!(&mut fwd_headers, "Host: {}\r\n", String::from_utf8_lossy(&host_val)).ok();

        for h in req.headers.iter() {
            if !h.name.eq_ignore_ascii_case("Host") && !h.name.eq_ignore_ascii_case("Proxy-Connection") {
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

fn parse_reverse_target(
    path: &str,
    headers: &[httparse::Header],
) -> Result<(TargetAddr, String), InboundError> {
    // 1. Path-based: /target/example.com:443/hello?foo=bar
    if let Some(stripped) = path.strip_prefix("/target/") {
        let (target_str, remaining_path) = match stripped.find('/') {
            Some(idx) => (&stripped[..idx], &stripped[idx..]),
            None => (stripped, "/"),
        };

        let target = TargetAddr::parse_authority(target_str).ok_or_else(|| {
            InboundError::MalformedRequest(format!("Invalid target authority: {target_str}"))
        })?;

        return Ok((target, remaining_path.to_string()));
    }

    // 2. Host-header based
    for h in headers {
        if h.name.eq_ignore_ascii_case("Host") {
            let host_str = std::str::from_utf8(h.value)
                .map_err(|_| InboundError::MalformedRequest("Host header not UTF-8".into()))?;
            let target = TargetAddr::parse_authority(host_str).ok_or_else(|| {
                InboundError::MalformedRequest(format!("Invalid Host header: {host_str}"))
            })?;
            return Ok((target, path.to_string()));
        }
    }

    Err(InboundError::MalformedRequest(
        "Could not determine reverse proxy target from path or Host header".into(),
    ))
}
