use std::{ptr::addr_eq, sync::Arc};

use arrayvec::ArrayVec;
use async_trait::async_trait;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

use crate::proxies::{Endpoint, MAX_ALLOWED_DOMAIN_SIZE, utils::invalid_data};

pub type OutboundError = anyhow::Error;
pub type OutboundResult<T> = Result<T, OutboundError>;

pub trait IoStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized + 'static> IoStream for T {}

#[async_trait]
trait OutboundProxy: std::marker::Sync {
    async fn connect(&self, target: &Endpoint) -> anyhow::Result<impl IoStream>;
}

#[async_trait]
pub trait OutboundProxyDyn {
    async fn connect(&self, target: &Endpoint) -> anyhow::Result<Box<dyn IoStream>>;
}

#[async_trait]
impl<T: OutboundProxy> OutboundProxyDyn for T {
    async fn connect(&self, target: &Endpoint) -> anyhow::Result<Box<dyn IoStream>> {
        Ok(Box::new(OutboundProxy::connect(self, target).await?))
    }
}

pub type Auth = Arc<[u8]>;

// Clone for every thread to have an Arc of auth and endpoint
#[derive(Clone, Debug)]
pub enum SimpleProxy {
    NoProxy,
    Http {
        /// In the format
        /// b"Proxy-Authorization: {base64_encoded}\r\n";
        /// Or completely empty for no-auth
        auth_header: Auth,
        endpoint: Endpoint,
    },
    Socks5 {
        /// In the format
        /// [0x01, ULEN] :: UNAME :: [PLEN] :: PASSWD
        /// Or completely empty for no-auth
        auth: Auth,
        endpoint: Endpoint,
        use_server_proxy: bool,
    },
}

impl SimpleProxy {
    async fn connect_http_outbound(
        proxy: &Endpoint,
        auth_header: &Auth,
        target: &Endpoint,
    ) -> OutboundResult<TcpStream> {
        let mut stream = TcpStream::connect(proxy.get_socket_addr().await?.as_slice()).await?;

        let connect = b"CONNECT ";
        let http_1 = b" HTTP/1.1\r\n";
        let host = b"Host: ";
        let connection = b"\r\nProxy-Connection: keep-alive\r\n";
        let end = b"\r\n";
        let target = target.get_str_bytes()?;

        let strs = [
            connect as &[u8],
            &target,
            http_1,
            host,
            &target,
            connection,
            auth_header,
            end,
        ];

        let mut bytes = ArrayVec::<u8, 270>::new();
        for s in strs {
            bytes.try_extend_from_slice(s)?;
        }
        stream.write_all(&bytes).await?;

        dbg!(String::from_utf8_lossy(&bytes));

        let mut resp_buf = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).await.is_ok() {
            resp_buf.push(byte[0]);
            if resp_buf.ends_with(b"\r\n\r\n") || resp_buf.ends_with(b"\n\n") {
                break;
            }
            if resp_buf.len() > 2048 {
                return Err(invalid_data("Upstream HTTP response headers too long"));
            }
        }

        let first_line = resp_buf.split(|&b| b == b'\n').next().unwrap_or(&[]);
        if !first_line.windows(3).any(|w| w == b"200") {
            return Err(invalid_data(format!(
                "Upstream HTTP proxy error: {}",
                String::from_utf8_lossy(first_line).trim()
            )));
        }

        Ok(stream)
    }

    async fn handle_socks5_auth(auth: &Auth, stream: &mut TcpStream) -> OutboundResult<()> {
        stream.write_all(auth).await?;
        let mut auth_res = [0u8; 2];
        stream.read_exact(&mut auth_res).await?;
        if auth_res == [0x01, 0x00] {
            Ok(())
        } else {
            Err(invalid_data("auth failed"))
        }
    }

    async fn connect_socks5_outbound(
        proxy: &Endpoint,
        auth: &Auth,
        target: &Endpoint,
        use_server_proxy: bool,
    ) -> OutboundResult<TcpStream> {
        let mut stream = TcpStream::connect(proxy.get_socket_addr().await?.as_slice()).await?;

        // Round 1: negotiation
        if auth.is_empty() {
            stream.write_all(&[0x05, 1, 0]).await?;
            let nego_res = {
                let mut nego_res = [0u8; 2];
                stream.read_exact(&mut nego_res).await?;
                nego_res
            };
            if nego_res != [0x05, 0x00] {
                return Err(invalid_data("negotiation failed"));
            }
        } else {
            stream.write_all(&[0x05, 2, 0, 2]).await?;
            let nego_res = {
                let mut nego_res = [0u8; 2];
                stream.read_exact(&mut nego_res).await?;
                nego_res
            };
            if nego_res[0] != 0x05 {
                return Err(invalid_data("negotiation failed"));
            }
            match nego_res[1] {
                0 => (), // no auth
                2 => Self::handle_socks5_auth(auth, &mut stream).await?,
                _ => return Err(invalid_data("unsupported auth method")),
            }
        }

        let mut connect_buffer = ArrayVec::<u8, 270>::new();
        connect_buffer.extend([0x05, 0x01, 0x00]);

        let mut target_bytes_written = false;
        if !use_server_proxy {
            let addrs = target.get_socket_addr().await?;
            if let Some(addr) = addrs.into_iter().next() {
                Endpoint::from(addr).fill_socks5_connect_buffer(&mut connect_buffer);
                target_bytes_written = true;
            }
        }
        if !target_bytes_written {
            target.fill_socks5_connect_buffer(&mut connect_buffer);
        }

        stream.write_all(&connect_buffer).await?;

        let ver = stream.read_u8().await?;
        let rep = stream.read_u8().await?;
        let _rsv = stream.read_u8().await?;
        let atyp = stream.read_u8().await?;
        dbg!(ver, rep, _rsv, atyp);

        if ver != 0x05 || rep != 0x00 {
            return Err(invalid_data("Incorrect signature"));
        }

        match atyp {
            0x01 => {
                let mut bytes = [0u8; 4];
                stream.read_exact(&mut bytes).await?;
            }
            0x03 => {
                let len = stream.read_u8().await? as usize;
                let mut buffer = [0u8; MAX_ALLOWED_DOMAIN_SIZE];
                stream.read_exact(&mut buffer[..len]).await?;
            }
            0x04 => {
                let mut bytes = [0u8; 16];
                stream.read_exact(&mut bytes).await?;
            }
            _ => return Err(invalid_data(format!("Invalid atyp: {}", atyp))),
        };

        let _port = stream.read_u16().await?;
        Ok(stream)
    }
}

impl SimpleProxy {
    pub async fn connect_inner(&self, target: &Endpoint) -> anyhow::Result<TcpStream> {
        use SimpleProxy::*;
        match self {
            NoProxy => Ok(TcpStream::connect(target.get_socket_addr().await?.as_slice()).await?),
            Http {
                auth_header,
                endpoint,
            } => Self::connect_http_outbound(endpoint, auth_header, target).await,
            Socks5 {
                auth,
                endpoint,
                use_server_proxy,
            } => Self::connect_socks5_outbound(endpoint, auth, target, *use_server_proxy).await,
        }
    }
}

#[async_trait]
impl OutboundProxy for SimpleProxy {
    async fn connect(&self, target: &Endpoint) -> OutboundResult<impl IoStream> {
        self.connect_inner(target).await
    }
}
