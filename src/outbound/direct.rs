use std::time::Duration;
use tokio::{net::TcpStream, time::timeout};

use crate::{
    error::{OutboundError, OutboundErrorKind},
    protocol::TargetAddr,
};

pub async fn connect_direct(
    target: &TargetAddr,
    connect_timeout: Duration,
) -> Result<TcpStream, OutboundError> {
    let addrs = target
        .resolve()
        .await
        .map_err(|e| OutboundError::new(OutboundErrorKind::DnsResolutionFailed(e.to_string()), None))?;

    if addrs.is_empty() {
        return Err(OutboundError::new(
            OutboundErrorKind::DnsResolutionFailed("No IP addresses found".into()),
            None,
        ));
    }

    let stream = timeout(connect_timeout, TcpStream::connect(addrs.as_slice()))
        .await
        .map_err(|_| OutboundError::new(OutboundErrorKind::Timeout, None))?
        .map_err(|e| OutboundError::new(OutboundErrorKind::TcpConnectFailed(e.to_string()), None))?;

    Ok(stream)
}
