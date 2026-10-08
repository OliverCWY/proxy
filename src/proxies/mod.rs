use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use smallvec::SmallVec;

pub mod inbound;
pub mod outbound;
pub mod utils;

type TargetMachineContainer = SmallVec<[u8; MAX_EXPECTED_TARGET_MACHINE_LENGTH]>;

#[derive(Debug)]
pub enum TargetMachine {
    Ip(IpAddr),
    Domain(TargetMachineContainer)
}

pub struct TcpTarget {
    pub host: TargetMachine,
    pub port: u16,
    addr: SmallVec<[SocketAddr; 2]>
}

impl TcpTarget {
    pub fn new(host: TargetMachine, port: u16) -> Self {
        
    }
}

pub const MAX_EXPECTED_AUTH_HEADER_LENGTH: usize = 32;
pub const MAX_EXPECTED_TARGET_MACHINE_LENGTH: usize = 16;