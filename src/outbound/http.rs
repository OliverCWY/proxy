use std::{net::SocketAddr, sync::Arc, time::Duration};
use bytes::BytesMut;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

use crate::{
    error::{OutboundError, OutboundErrorKind},
    protocol::TargetAddr,
};

pub async fn connect_http_upstream(
    proxy_addrs: &[SocketAddr],
    auth_header: Option<&Arc<[u8]>>,
    target: &TargetAddr,
    connect_timeout: Duration,
) -> Result<(TcpStream, bytes::Bytes), OutboundError> {
    // 1. Connect TCP to upstream HTTP proxy
    let mut stream = timeout(connect_timeout, TcpStream::connect(proxy_addrs))
        .await
        .map_err(|_| OutboundError::new(OutboundErrorKind::Timeout, None))?
        .map_err(|e| {
            OutboundError::new(
                OutboundErrorKind::TcpConnectFailed(e.to_string()),
                proxy_addrs.first().copied(),
            )
        })?;

    let peer = stream.peer_addr().ok();

    // 2. Build CONNECT request
    let authority = target.to_http_authority_bytes();
    let mut req = Vec::with_capacity(128 + authority.len());
    req.extend_from_slice(b"CONNECT ");
    req.extend_from_slice(&authority);
    req.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    req.extend_from_slice(&authority);
    req.extend_from_slice(b"\r\nProxy-Connection: keep-alive\r\n");

    if let Some(auth) = auth_header {
        req.extend_from_slice(auth);
    }
    req.extend_from_slice(b"\r\n");

    // 3. Send CONNECT request
    stream.write_all(&req).await.map_err(|e| {
        OutboundError::new(
            OutboundErrorKind::UpstreamProtocolError(e.to_string()),
            peer,
        )
    })?;

    // 4. Read response with httparse in chunks
    let mut buf = BytesMut::with_capacity(1024);
    let mut header_end = None;

    while header_end.is_none() {
        let mut chunk = [0u8; 1024];
        let n = stream.read(&mut chunk).await.map_err(|e| {
            OutboundError::new(
                OutboundErrorKind::UpstreamProtocolError(e.to_string()),
                peer,
            )
        })?;
        if n == 0 {
            return Err(OutboundError::new(OutboundErrorKind::PrematureEof, peer));
        }
        buf.extend_from_slice(&chunk[..n]);

        for i in 0..buf.len() {
            if i + 4 <= buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
                header_end = Some(i + 4);
                break;
            }
            if i + 2 <= buf.len() && &buf[i..i + 2] == b"\n\n" {
                header_end = Some(i + 2);
                break;
            }
        }

        if buf.len() > 16 * 1024 {
            return Err(OutboundError::new(
                OutboundErrorKind::UpstreamProtocolError("Upstream headers exceeded 16KB".into()),
                peer,
            ));
        }
    }

    let end_idx = header_end.unwrap();
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers);
    let res = resp
        .parse(&buf[..end_idx])
        .map_err(|e| OutboundError::new(OutboundErrorKind::UpstreamProtocolError(format!("{e:?}")), peer))?;

    if res.is_partial() {
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamProtocolError("Incomplete response line".into()),
            peer,
        ));
    }

    let code = resp.code.unwrap_or(0);
    if !(200..300).contains(&code) {
        if code == 407 {
            return Err(OutboundError::new(OutboundErrorKind::UpstreamAuthFailed, peer));
        }
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamHttpStatus(code),
            peer,
        ));
    }

    // Residual data after headers
    let residual = if end_idx < buf.len() {
        buf.split_off(end_idx).freeze()
    } else {
        bytes::Bytes::new()
    };

    Ok((stream, residual))
}
