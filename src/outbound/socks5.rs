use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

use crate::{
    error::{OutboundError, OutboundErrorKind},
    protocol::TargetAddr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocksDnsMode {
    Local,
    Remote,
}

pub async fn connect_socks5_upstream(
    proxy_addrs: &[SocketAddr],
    auth_payload: Option<&Arc<[u8]>>,
    target: &TargetAddr,
    dns_mode: SocksDnsMode,
    connect_timeout: Duration,
) -> Result<TcpStream, OutboundError> {
    // 1. Connect TCP to upstream SOCKS5 proxy
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

    // 2. Client Greeting: Send supported methods
    if auth_payload.is_some() {
        // Support NO AUTH (0x00) and USER/PASS (0x02)
        stream
            .write_all(&[0x05, 0x02, 0x00, 0x02])
            .await
            .map_err(|e| OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer))?;
    } else {
        // NO AUTH only (0x00)
        stream
            .write_all(&[0x05, 0x01, 0x00])
            .await
            .map_err(|e| OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer))?;
    }

    let mut method_res = [0u8; 2];
    stream.read_exact(&mut method_res).await.map_err(|e| {
        OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
    })?;

    if method_res[0] != 0x05 {
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamProtocolError(format!(
                "Invalid SOCKS version in handshake reply: {}",
                method_res[0]
            )),
            peer,
        ));
    }

    let chosen_method = method_res[1];
    if chosen_method == 0xFF {
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamAuthFailed,
            peer,
        ));
    }

    // 3. Subnegotiation authentication if chosen 0x02
    if chosen_method == 0x02 {
        let auth = auth_payload.ok_or_else(|| {
            OutboundError::new(
                OutboundErrorKind::UpstreamProtocolError("Proxy requested auth, but none provided".into()),
                peer,
            )
        })?;

        stream.write_all(auth).await.map_err(|e| {
            OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
        })?;

        let mut auth_res = [0u8; 2];
        stream.read_exact(&mut auth_res).await.map_err(|e| {
            OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
        })?;

        if auth_res != [0x01, 0x00] {
            return Err(OutboundError::new(
                OutboundErrorKind::UpstreamAuthFailed,
                peer,
            ));
        }
    }

    // 4. Send CONNECT Request
    let mut req = Vec::with_capacity(32);
    req.extend_from_slice(&[0x05, 0x01, 0x00]); // VER=5, CMD=1 (CONNECT), RSV=0

    match dns_mode {
        SocksDnsMode::Local => {
            // Local resolution: resolve target to IP first
            let addrs = target.resolve().await.map_err(|e| {
                OutboundError::new(OutboundErrorKind::DnsResolutionFailed(e.to_string()), peer)
            })?;
            let first_addr = addrs.first().ok_or_else(|| {
                OutboundError::new(
                    OutboundErrorKind::DnsResolutionFailed("No IP resolved for target".into()),
                    peer,
                )
            })?;
            let local_target = TargetAddr::from_ip(first_addr.ip(), first_addr.port());
            local_target.write_socks5_dest(&mut req);
        }
        SocksDnsMode::Remote => {
            // Remote resolution: write domain directly into SOCKS5 frame
            target.write_socks5_dest(&mut req);
        }
    }

    stream.write_all(&req).await.map_err(|e| {
        OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
    })?;

    // 5. Read SOCKS5 Reply
    let mut reply_header = [0u8; 4];
    stream.read_exact(&mut reply_header).await.map_err(|e| {
        OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
    })?;

    if reply_header[0] != 0x05 {
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamProtocolError("Invalid SOCKS version in connect reply".into()),
            peer,
        ));
    }

    let rep = reply_header[1];
    if rep != 0x00 {
        return Err(OutboundError::new(
            OutboundErrorKind::UpstreamSocks5Reply(rep),
            peer,
        ));
    }

    // Drain bound address and port
    let atyp = reply_header[3];
    match atyp {
        0x01 => {
            let mut addr_port = [0u8; 4 + 2];
            stream.read_exact(&mut addr_port).await.map_err(|e| {
                OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
            })?;
        }
        0x03 => {
            let dlen = stream.read_u8().await.map_err(|e| {
                OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
            })? as usize;
            let mut domain_port = vec![0u8; dlen + 2];
            stream.read_exact(&mut domain_port).await.map_err(|e| {
                OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
            })?;
        }
        0x04 => {
            let mut addr_port = [0u8; 16 + 2];
            stream.read_exact(&mut addr_port).await.map_err(|e| {
                OutboundError::new(OutboundErrorKind::UpstreamProtocolError(e.to_string()), peer)
            })?;
        }
        _ => {
            return Err(OutboundError::new(
                OutboundErrorKind::UpstreamProtocolError(format!("Invalid ATYP {atyp} in reply")),
                peer,
            ));
        }
    }

    Ok(stream)
}
