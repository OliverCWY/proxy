use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use arrayvec::ArrayVec;
use bytes::{BufMut, Bytes, BytesMut};
use memchr::memrchr;
use tokio::net::lookup_host;

use crate::utils::Proto;

pub mod inbound;
pub mod outbound;
pub mod utils;

pub const MAX_EXPECTED_AUTH_HEADER_LENGTH: usize = 512;
pub const MAX_EXPECTED_DOMAIN_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
    Domain(Arc<str>),
}

impl From<IpAddr> for Host {
    fn from(value: IpAddr) -> Self {
        match value {
            IpAddr::V4(ip) => Self::Ipv4(ip),
            IpAddr::V6(ip) => Self::Ipv6(ip),
        }
    }
}

impl From<&str> for Host {
    fn from(value: &str) -> Self {
        match value.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => Self::Ipv4(ip),
            Ok(IpAddr::V6(ip)) => Self::Ipv6(ip),
            Err(_) => Self::Domain(value.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: Host,
    pub port: u16,
}

impl Endpoint {
    pub(crate) fn try_parse(endpoint_str: &str, proto: Proto) -> Option<Self> {
        let endpoint_bytes = endpoint_str.as_bytes();
        let (host, port): (Host, u16) = match memrchr(b':', endpoint_bytes) {
            None => (
                endpoint_str.into(),
                match proto {
                    Proto::Http => 80,
                    Proto::Https => 443,
                    Proto::Socks5 | Proto::Socks5h => return None,
                },
            ),
            Some(pos) => {
                let host = &endpoint_str[..pos];
                let port = endpoint_str[pos + 1..].parse().ok()?;

                (host.into(), port)
            }
        };

        Some(Self { host, port })
    }
}

impl From<SocketAddr> for Endpoint {
    fn from(value: SocketAddr) -> Self {
        let host = value.ip().into();
        Endpoint {
            host,
            port: value.port(),
        }
    }
}

// todo: precompote all these
impl Endpoint {
    pub async fn get_socket_addr(&self) -> std::io::Result<Vec<SocketAddr>> {
        use Host::*;
        match &self.host {
            Ipv4(ip) => lookup_host((*ip, self.port)).await.map(Iterator::collect),
            Ipv6(ip) => lookup_host((*ip, self.port)).await.map(Iterator::collect),
            Domain(domain) => lookup_host((domain.as_ref(), self.port))
                .await
                .map(Iterator::collect),
        }
    }

    pub fn get_str_bytes(&self) -> anyhow::Result<Bytes> {
        let mut res = BytesMut::with_capacity(64);
        use Host::*;
        match &self.host {
            Ipv4(ip) => write!((&mut res).writer(), "{}", ip)?,
            Ipv6(ip) => write!((&mut res).writer(), "[{}]", ip)?,
            Domain(domain) => res.extend_from_slice(domain.as_bytes()),
        }
        res.put(b":".as_slice());
        let mut port = itoa::Buffer::new();
        let port = port.format(self.port);
        res.put_slice(port.as_bytes());

        Ok(res.freeze())
    }

    pub fn fill_socks5_connect_buffer(&self, buffer: &mut ArrayVec<u8, 270>) {
        use Host::*;
        match &self.host {
            Ipv4(ip) => {
                buffer.push(0x01);
                buffer.extend(ip.octets());
            }
            Ipv6(ip) => {
                buffer.push(0x04);
                buffer.extend(ip.octets());
            }
            Domain(domain) => {
                buffer.push(0x03);
                buffer.push(domain.len() as u8);
                buffer.extend(domain.as_bytes().iter().copied());
            }
        }
        buffer.extend(self.port.to_be_bytes());
    }
}
