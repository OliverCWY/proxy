pub mod http;
pub mod socks5;

use tokio::{io::BufReader, net::TcpStream};

use crate::error::InboundError;

pub enum InboundSession {
    Socks5(socks5::Socks5Handshake),
    Http(http::HttpHandshake),
}

/// Sniff protocol from the first incoming byte:
/// - 0x05 -> SOCKS5
/// - other -> HTTP (CONNECT for forward proxy, GET/POST/... for reverse proxy)
pub async fn sniff_inbound(stream: TcpStream) -> Result<InboundSession, InboundError> {
    let reader = BufReader::new(stream);
    let mut first_byte = [0u8; 1];

    let n = reader.get_ref().peek(&mut first_byte).await?;
    if n == 0 {
        return Err(InboundError::PrematureEof);
    }

    if first_byte[0] == 0x05 {
        Ok(InboundSession::Socks5(socks5::Socks5Handshake::new(reader)))
    } else {
        Ok(InboundSession::Http(http::HttpHandshake::new(reader)))
    }
}
