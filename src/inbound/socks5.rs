use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{
    error::{InboundError, OutboundError, OutboundErrorKind},
    protocol::{Credentials, TargetAddr},
};

pub struct Socks5Handshake {
    stream: BufReader<TcpStream>,
}

impl Socks5Handshake {
    pub fn new(stream: BufReader<TcpStream>) -> Self {
        Self { stream }
    }

    /// Perform SOCKS5 method negotiation and optional RFC 1929 authentication.
    /// Returns the target destination requested by client, and extracted credentials (if any).
    pub async fn negotiate(
        mut self,
        require_auth: bool,
    ) -> Result<PendingSocks5Session, InboundError> {
        let reader = &mut self.stream;

        // 1. Version identifier / method selection message
        let ver = reader.read_u8().await?;
        if ver != 0x05 {
            return Err(InboundError::UnsupportedVersion(ver));
        }

        let nmethods = reader.read_u8().await? as usize;
        if nmethods == 0 {
            reader.write_all(&[0x05, 0xFF]).await?;
            return Err(InboundError::NoAcceptableAuthMethod);
        }

        let mut methods = [0u8; 255];
        reader.read_exact(&mut methods[..nmethods]).await?;
        let supported = &methods[..nmethods];

        let has_no_auth = supported.contains(&0x00);
        let has_user_pass = supported.contains(&0x02);

        let selected_method = if require_auth {
            if has_user_pass {
                0x02
            } else {
                reader.write_all(&[0x05, 0xFF]).await?;
                return Err(InboundError::NoAcceptableAuthMethod);
            }
        } else if has_no_auth {
            0x00
        } else if has_user_pass {
            0x02
        } else {
            reader.write_all(&[0x05, 0xFF]).await?;
            return Err(InboundError::NoAcceptableAuthMethod);
        };

        reader.write_all(&[0x05, selected_method]).await?;

        // 2. Subnegotiation authentication if 0x02 selected
        let credentials = if selected_method == 0x02 {
            let auth_ver = reader.read_u8().await?;
            if auth_ver != 0x01 {
                return Err(InboundError::UnsupportedVersion(auth_ver));
            }

            let ulen = reader.read_u8().await? as usize;
            let mut u_buf = [0u8; 255];
            reader.read_exact(&mut u_buf[..ulen]).await?;

            let plen = reader.read_u8().await? as usize;
            let mut p_buf = [0u8; 255];
            reader.read_exact(&mut p_buf[..plen]).await?;

            let user = String::from_utf8(u_buf[..ulen].to_vec())
                .map_err(|_| InboundError::MalformedRequest("Username is not valid UTF-8".into()))?;
            let pass = String::from_utf8(p_buf[..plen].to_vec())
                .map_err(|_| InboundError::MalformedRequest("Password is not valid UTF-8".into()))?;

            Some(Credentials::new(user, pass))
        } else {
            None
        };

        Ok(PendingSocks5Session {
            stream: self.stream,
            credentials,
            requires_auth_reply: selected_method == 0x02,
        })
    }
}

pub struct PendingSocks5Session {
    stream: BufReader<TcpStream>,
    credentials: Option<Credentials>,
    requires_auth_reply: bool,
}

impl PendingSocks5Session {
    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    /// Reject client authentication according to RFC 1929: [0x01, 0x01] (failure)
    pub async fn reject_auth(mut self) -> Result<(), InboundError> {
        if self.requires_auth_reply {
            self.stream.write_all(&[0x01, 0x01]).await?;
            self.stream.flush().await?;
        }
        Ok(())
    }

    /// Complete authentication (if pending) and read the CONNECT request
    pub async fn read_request(mut self) -> Result<(CommittedSocks5State, TargetAddr), InboundError> {
        if self.requires_auth_reply {
            // Send RFC 1929 success: [0x01, 0x00]
            self.stream.write_all(&[0x01, 0x00]).await?;
        }

        let reader = &mut self.stream;

        let ver = reader.read_u8().await?;
        if ver != 0x05 {
            return Err(InboundError::UnsupportedVersion(ver));
        }

        let cmd = reader.read_u8().await?;
        if cmd != 0x01 {
            // Only CONNECT is supported
            // Reply: 0x07 (Command not supported)
            reader.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
            return Err(InboundError::UnsupportedCommand(cmd));
        }

        let _rsv = reader.read_u8().await?;
        let atyp = reader.read_u8().await?;

        let target_addr = match atyp {
            0x01 => {
                // IPv4
                let mut octets = [0u8; 4];
                reader.read_exact(&mut octets).await?;
                let port = reader.read_u16().await?;
                TargetAddr::from_ip(std::net::IpAddr::V4(octets.into()), port)
            }
            0x03 => {
                // Domain name
                let len = reader.read_u8().await? as usize;
                let mut domain_buf = vec![0u8; len];
                reader.read_exact(&mut domain_buf).await?;
                let port = reader.read_u16().await?;
                let domain = String::from_utf8(domain_buf)
                    .map_err(|_| InboundError::MalformedRequest("Domain is not valid UTF-8".into()))?;
                TargetAddr::from_domain(domain, port)
            }
            0x04 => {
                // IPv6
                let mut octets = [0u8; 16];
                reader.read_exact(&mut octets).await?;
                let port = reader.read_u16().await?;
                TargetAddr::from_ip(std::net::IpAddr::V6(octets.into()), port)
            }
            _ => {
                reader.write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
                return Err(InboundError::UnsupportedAddressType(atyp));
            }
        };

        Ok((
            CommittedSocks5State {
                stream: self.stream,
            },
            target_addr,
        ))
    }
}

pub struct CommittedSocks5State {
    stream: BufReader<TcpStream>,
}

impl CommittedSocks5State {
    /// Commit success to downstream client: 0x00 (Success)
    pub async fn commit_success(mut self) -> Result<BufReader<TcpStream>, InboundError> {
        // [VER, REP, RSV, ATYP, BND.ADDR (0.0.0.0), BND.PORT (0)]
        self.stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        self.stream.flush().await?;
        Ok(self.stream)
    }

    /// Commit failure to downstream client with proper REP code
    pub async fn commit_fail(mut self, error: &OutboundError) -> Result<(), InboundError> {
        let rep = match &error.kind {
            OutboundErrorKind::DnsResolutionFailed(_) => 0x04, // Host unreachable
            OutboundErrorKind::TcpConnectFailed(_) => 0x05,    // Connection refused
            OutboundErrorKind::Timeout => 0x04,               // Host unreachable / timed out
            OutboundErrorKind::UpstreamSocks5Reply(r) => *r,
            _ => 0x01,                                       // General failure
        };

        self.stream
            .write_all(&[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        self.stream.flush().await?;
        Ok(())
    }
}
