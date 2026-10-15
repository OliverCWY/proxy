pub mod direct;
pub mod http;
pub mod socks5;

use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpStream;

use crate::{
    error::OutboundError,
    protocol::TargetAddr,
};

pub use socks5::SocksDnsMode;

#[derive(Debug, Clone)]
pub enum OutboundProtocol {
    Direct,
    Http {
        auth_header: Option<Arc<[u8]>>,
    },
    Socks5 {
        auth_payload: Option<Arc<[u8]>>,
        dns_mode: SocksDnsMode,
    },
}

#[derive(Debug, Clone)]
pub struct OutboundNodeConfig {
    pub dial_addrs: Arc<[SocketAddr]>,
    pub protocol: OutboundProtocol,
}

pub struct OutboundConnector;

impl OutboundConnector {
    pub async fn connect(
        config: &OutboundNodeConfig,
        target: &TargetAddr,
        connect_timeout: Duration,
    ) -> Result<(TcpStream, bytes::Bytes), OutboundError> {
        match &config.protocol {
            OutboundProtocol::Direct => {
                let stream = direct::connect_direct(target, connect_timeout).await?;
                Ok((stream, bytes::Bytes::new()))
            }
            OutboundProtocol::Http { auth_header } => {
                http::connect_http_upstream(
                    &config.dial_addrs,
                    auth_header.as_ref(),
                    target,
                    connect_timeout,
                )
                .await
            }
            OutboundProtocol::Socks5 {
                auth_payload,
                dns_mode,
            } => {
                let stream = socks5::connect_socks5_upstream(
                    &config.dial_addrs,
                    auth_payload.as_ref(),
                    target,
                    *dns_mode,
                    connect_timeout,
                )
                .await?;
                Ok((stream, bytes::Bytes::new()))
            }
        }
    }
}
