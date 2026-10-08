use async_trait::async_trait;
use smallvec::{SmallVec, smallvec};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

use crate::proxies::{
    Endpoint, MAX_EXPECTED_AUTH_HEADER_LENGTH, MAX_EXPECTED_DOMAIN_SIZE,
    MAX_EXPECTED_HTTP_CONNECT_HEADER_SIZE, utils::invalid_data,
};

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

pub type Auth = SmallVec<[u8; MAX_EXPECTED_AUTH_HEADER_LENGTH]>;

pub enum SimpleProxy {
    NoProxy,
    Http {
        /// In the format
        /// b"Proxy-Authorization: {base64_encoded}\r\n";
        /// Or completely empty for no-auth
        auth_header: Auth,
        host: Endpoint,
    },
    Socks5 {
        /// In the format
        /// [0x01, ULEN] :: UNAME :: [PLEN] :: PASSWD
        /// Or completely empty for no-auth
        auth: Auth,
        host: Endpoint,
        dns: bool,
    },
}

impl SimpleProxy {
    async fn connect_http_outbound(
        proxy: &Endpoint,
        auth_header: &Auth,
        target: &Endpoint,
    ) -> anyhow::Result<TcpStream> {
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

        let mut bytes = SmallVec::<[u8; MAX_EXPECTED_HTTP_CONNECT_HEADER_SIZE]>::new();
        strs.into_iter().for_each(|s| bytes.extend_from_slice(s));
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

    async fn handle_socks5_auth(auth: &Auth, stream: &mut TcpStream) -> anyhow::Result<()> {
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
    ) -> anyhow::Result<TcpStream> {
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

        let mut connect_buffer: SmallVec<[u8; 128]> = smallvec![0x05, 0x01, 0x00];
        target.fill_socks5_connect_buffer(&mut connect_buffer);
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
                dbg!(len);
                if len >= MAX_EXPECTED_DOMAIN_SIZE {
                    return Err(invalid_data("Domain too long!"));
                }
                let mut buffer = [0u8; MAX_EXPECTED_DOMAIN_SIZE];
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

#[async_trait]
impl OutboundProxy for SimpleProxy {
    async fn connect(&self, target: &Endpoint) -> anyhow::Result<impl IoStream> {
        use SimpleProxy::*;
        match self {
            NoProxy => Ok(TcpStream::connect(target.get_socket_addr().await?.as_slice()).await?),
            Http { auth_header, host } => {
                Self::connect_http_outbound(host, auth_header, target).await
            }
            Socks5 {
                auth,
                host,
                dns: _dns,
            } => Self::connect_socks5_outbound(host, auth, target).await,
        }
    }
}
