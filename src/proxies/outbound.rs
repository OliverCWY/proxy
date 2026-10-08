use std::io::Write;

use async_trait::async_trait;
use smallvec::SmallVec;
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, net::TcpStream};

use crate::proxies::{TargetMachine, MAX_EXPECTED_AUTH_HEADER_LENGTH, TcpTarget, utils::invalid_data};

pub trait IoStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized + 'static> IoStream for T {}

#[async_trait]
trait OutboundProxy: std::marker::Sync {
    async fn connect(&self, target: TcpTarget) -> anyhow::Result<impl IoStream>;
}

#[async_trait]
pub trait OutboundProxyDyn {
    async fn connect(&self, target: TcpTarget) -> anyhow::Result<Box<dyn IoStream>>;
}

#[async_trait]
impl<T: OutboundProxy> OutboundProxyDyn for T {
    async fn connect(&self, target: TcpTarget) -> anyhow::Result<Box<dyn IoStream>> {
        Ok(Box::new(OutboundProxy::connect(self, target).await?))
    }
}

type Auth = SmallVec<[u8; MAX_EXPECTED_AUTH_HEADER_LENGTH]>;

pub enum SimpleProxy {
    NoProxy,
    Http {
         /// In the format 
         /// b"Proxy-Authorization: {user}:{pass} \n";
         /// Or completely empty for no-auth
        auth_header: Auth,
        host: TcpTarget
    },
    Socks5 {
        /// In base64 format
        auth: Auth,
        host: TargetMachine,
        dns: bool
    }
}

impl SimpleProxy {
    async fn connect_http_outbound<T: IoStream>(auth_header: &Auth, host: &TargetMachine, port: u16) -> anyhow::Result<T> {
        let stream = TcpStream::connect(addr);
        
        stream.write_all(b"CONNECT ").await?;
        stream.write_all(host).await?;

        let mut buffer = [0u8; 4];
        let mut port_str = &mut buffer[..];
        write!(&mut port_str, ":{port}")?;
        stream.write_all(port_str).await?;

        stream.write_all(b" HTTP/1.1\r\nHost: ").await?;
        stream.write_all(host).await?;
        stream.write_all(port_str).await?;
        stream.write_all(b"\r\nProxy-Connection: keep-alive\r\n").await?;
        stream.write_all(auth_header).await?;
        stream.write_all(b"\r\n\r\n").await?;

        let success_bytes = b"HTTP/1.1 200 Connection Established\r\n\r\n";
        let mut buffer = [0u8; 39];
        stream.read_exact(&mut buffer).await?;
        if buffer != *success_bytes {
            return Err(invalid_data("Upstream connection failed"));
        }

        Ok(stream)
    }

    async fn connect_socks5_outbound<T: IoStream>(auth: &Auth, host: &TargetMachine, port: u16, dns: bool, target: TcpTarget, stream: T) -> anyhow::Result<T> {
        stream.write_all();
        
        Ok(stream)
    }
}

#[async_trait]
impl OutboundProxy for SimpleProxy {
    async fn connect(&self, target: TcpTarget) -> anyhow::Result<impl IoStream> {
        use SimpleProxy::*;
        match self {
            Direct => {
                
            },
            Http {auth_header, host, port} => Self::connect_http_outbound(auth_header, host, *port, stream).await,
            Socks5 {auth, host, port, dns} => Self::connect_socks5_outbound(auth, host, *port, *dns, stream).await
        }
    }
}