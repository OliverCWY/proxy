use std::net::SocketAddr;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum InboundError {
    #[error("I/O error during inbound operation: {0}")]
    Io(#[from] std::io::Error),

    #[error("Malformed request: {0}")]
    MalformedRequest(String),

    #[error("Unsupported protocol version: {0}")]
    UnsupportedVersion(u8),

    #[error("Unsupported command: {0}")]
    UnsupportedCommand(u8),

    #[error("Unsupported address type: {0}")]
    UnsupportedAddressType(u8),

    #[error("No supported authentication method")]
    NoAcceptableAuthMethod,

    #[error("Authentication required but none provided")]
    AuthRequired,

    #[error("Authentication failed")]
    AuthFailed,

    #[error("Client closed connection prematurely")]
    PrematureEof,

    #[error("Header section exceeded limit")]
    HeaderTooLarge,
}

#[derive(Error, Debug, Clone)]
pub enum OutboundErrorKind {
    #[error("DNS resolution failed: {0}")]
    DnsResolutionFailed(String),

    #[error("TCP connect to upstream failed: {0}")]
    TcpConnectFailed(String),

    #[error("Upstream proxy authentication failed")]
    UpstreamAuthFailed,

    #[error("Upstream HTTP proxy returned non-2xx status: {0}")]
    UpstreamHttpStatus(u16),

    #[error("Upstream SOCKS5 proxy returned error reply: 0x{0:02x}")]
    UpstreamSocks5Reply(u8),

    #[error("Protocol error while talking to upstream: {0}")]
    UpstreamProtocolError(String),

    #[error("Connection timed out")]
    Timeout,

    #[error("Upstream closed connection prematurely")]
    PrematureEof,
}

#[derive(Error, Debug)]
#[error("{kind}")]
pub struct OutboundError {
    pub kind: OutboundErrorKind,
    pub upstream_addr: Option<SocketAddr>,
}

impl OutboundError {
    pub fn new(kind: OutboundErrorKind, upstream_addr: Option<SocketAddr>) -> Self {
        Self { kind, upstream_addr }
    }

    pub fn is_retryable(&self) -> bool {
        match &self.kind {
            OutboundErrorKind::TcpConnectFailed(_) => true,
            OutboundErrorKind::Timeout => true,
            OutboundErrorKind::PrematureEof => true,
            OutboundErrorKind::UpstreamHttpStatus(status) => {
                // 502 Bad Gateway, 503 Service Unavailable, 504 Gateway Timeout are retryable
                *status == 502 || *status == 503 || *status == 504 || *status == 500
            }
            OutboundErrorKind::UpstreamSocks5Reply(rep) => {
                // 0x01 general failure, 0x03 net unreachable, 0x04 host unreachable, 0x05 conn refused, 0x06 TTL expired
                matches!(*rep, 0x01 | 0x03 | 0x04 | 0x05 | 0x06)
            }
            OutboundErrorKind::DnsResolutionFailed(_) => false,
            OutboundErrorKind::UpstreamAuthFailed => false, // bad credentials won't fix with retry
            OutboundErrorKind::UpstreamProtocolError(_) => true,
        }
    }
}

impl From<std::io::Error> for OutboundError {
    fn from(err: std::io::Error) -> Self {
        let kind = match err.kind() {
            std::io::ErrorKind::TimedOut => OutboundErrorKind::Timeout,
            std::io::ErrorKind::UnexpectedEof => OutboundErrorKind::PrematureEof,
            _ => OutboundErrorKind::TcpConnectFailed(err.to_string()),
        };
        Self {
            kind,
            upstream_addr: None,
        }
    }
}

#[derive(Error, Debug)]
pub enum PoolError {
    #[error("Proxy pool is empty or no healthy nodes available")]
    NoHealthyProxy,

    #[error("All retry attempts failed: {0:?}")]
    AllRetriesFailed(Vec<OutboundError>),
}
