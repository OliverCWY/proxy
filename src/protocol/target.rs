use std::{
    fmt,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tokio::net::lookup_host;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetHost {
    Ip(IpAddr),
    Domain(Arc<str>),
}

impl fmt::Display for TargetHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetHost::Ip(IpAddr::V4(ip)) => write!(f, "{ip}"),
            TargetHost::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
            TargetHost::Domain(domain) => write!(f, "{domain}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetAddr {
    pub host: TargetHost,
    pub port: u16,
}

impl TargetAddr {
    pub fn new(host: TargetHost, port: u16) -> Self {
        Self { host, port }
    }

    pub fn from_ip(ip: IpAddr, port: u16) -> Self {
        Self {
            host: TargetHost::Ip(ip),
            port,
        }
    }

    pub fn from_domain(domain: impl Into<Arc<str>>, port: u16) -> Self {
        Self {
            host: TargetHost::Domain(domain.into()),
            port,
        }
    }

    /// Parse host:port string (IPv4:port, [IPv6]:port, or domain:port)
    pub fn parse_authority(authority: &str) -> Option<Self> {
        let authority = authority.trim();
        if authority.is_empty() {
            return None;
        }

        // Handle IPv6 bracket notation [::1]:port
        if authority.starts_with('[') {
            let close_bracket = authority.find(']')?;
            let ip_str = &authority[1..close_bracket];
            let colon_port = &authority[close_bracket + 1..];
            if !colon_port.starts_with(':') {
                return None;
            }
            let port = colon_port[1..].parse::<u16>().ok()?;
            let ip = ip_str.parse::<Ipv6Addr>().ok()?;
            return Some(Self::from_ip(IpAddr::V6(ip), port));
        }

        // Standard host:port
        let colon = authority.rfind(':')?;
        let host_str = &authority[..colon];
        let port = authority[colon + 1..].parse::<u16>().ok()?;

        if let Ok(ip) = host_str.parse::<IpAddr>() {
            Some(Self::from_ip(ip, port))
        } else if !host_str.is_empty() && host_str.len() <= 255 {
            Some(Self::from_domain(host_str, port))
        } else {
            None
        }
    }

    /// Resolve this target address into SocketAddr list (used for direct connection or local DNS mode)
    pub async fn resolve(&self) -> std::io::Result<Vec<SocketAddr>> {
        match &self.host {
            TargetHost::Ip(ip) => Ok(vec![SocketAddr::new(*ip, self.port)]),
            TargetHost::Domain(domain) => {
                let addrs = lookup_host((domain.as_ref(), self.port)).await?;
                Ok(addrs.collect())
            }
        }
    }

    /// Format as "host:port" bytes for HTTP CONNECT
    pub fn to_http_authority_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);
        use std::io::Write;
        write!(&mut buf, "{}:{}", self.host, self.port).ok();
        buf
    }

    /// Encode target address into SOCKS5 address format (ATYP + ADDR + PORT)
    pub fn write_socks5_dest(&self, buf: &mut Vec<u8>) {
        match &self.host {
            TargetHost::Ip(IpAddr::V4(ip)) => {
                buf.push(0x01); // ATYP: IPv4
                buf.extend_from_slice(&ip.octets());
            }
            TargetHost::Ip(IpAddr::V6(ip)) => {
                buf.push(0x04); // ATYP: IPv6
                buf.extend_from_slice(&ip.octets());
            }
            TargetHost::Domain(domain) => {
                buf.push(0x03); // ATYP: Domain
                let bytes = domain.as_bytes();
                let len = bytes.len().min(255) as u8;
                buf.push(len);
                buf.extend_from_slice(&bytes[..len as usize]);
            }
        }
        buf.extend_from_slice(&self.port.to_be_bytes());
    }
}

impl fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}
