use std::io::Write;
use std::net::{IpAddr, SocketAddr};

use bytes::BufMut;
use smallvec::SmallVec;
use tokio::net::lookup_host;

pub mod inbound;
pub mod outbound;
pub mod utils;

const MAX_EXPECTED_HTTP_CONNECT_HEADER_SIZE: usize = 512;
pub const MAX_EXPECTED_AUTH_HEADER_LENGTH: usize = 512;
pub const MAX_EXPECTED_DOMAIN_SIZE: usize = 64;

// Awkward, probably use something like Box<[u8]> later to avoid copying during arc-swap...
type HostContainer = SmallVec<[u8; MAX_EXPECTED_DOMAIN_SIZE]>;
type TargetContainer = SmallVec<[u8; MAX_EXPECTED_DOMAIN_SIZE + 5]>;

#[derive(Debug)]
pub enum Host {
    Ip(IpAddr),
    Domain(HostContainer),
}

pub struct Endpoint {
    pub host: Host,
    pub port: u16,
}

// todo: precompote all these
impl Endpoint {
    pub async fn get_socket_addr(&self) -> std::io::Result<Vec<SocketAddr>> {
        use Host::*;
        match &self.host {
            Ip(ip) => lookup_host((*ip, self.port)).await.map(Iterator::collect),
            Domain(domain) => {
                lookup_host(dbg!(unsafe { str::from_utf8_unchecked(domain) }, self.port))
                    .await
                    .map(Iterator::collect)
            }
        }
    }

    pub fn get_str_bytes(&self) -> anyhow::Result<TargetContainer> {
        let mut res = TargetContainer::new();
        use Host::*;
        match &self.host {
            Ip(ip) => write!((&mut res).writer(), "{}", ip)?,
            Domain(domain) => res.extend_from_slice(domain),
        }
        res.push(b':');
        let mut port = itoa::Buffer::new();
        let port = port.format(self.port);
        res.extend_from_slice(port.as_bytes());

        Ok(res)
    }

    pub fn fill_socks5_connect_buffer(&self, buffer: &mut SmallVec<[u8; 128]>) {
        use Host::*;
        match &self.host {
            Ip(IpAddr::V4(ip)) => {
                buffer.push(0x01);
                buffer.extend(ip.octets());
            }
            Ip(IpAddr::V6(ip)) => {
                buffer.push(0x04);
                buffer.extend(ip.octets());
            }
            Domain(domain) => {
                buffer.push(0x03);
                buffer.push(domain.len() as u8);
                buffer.extend_from_slice(domain);
            }
        }
        buffer.extend(self.port.to_be_bytes());
    }
}
