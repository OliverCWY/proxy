use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::proxies::{
    Endpoint, Host,
    inbound::{
        InboundAuth,
        InboundError::{IncorrectFormat, IncorrectFormatWithData},
        InboundResult,
    },
    outbound::OutboundError,
};

pub struct Socks5InitState {
    stream: BufReader<TcpStream>,
}

// possibly better to return [u8; 255], but sufficient for now
struct HandShakeMethods {
    no_auth: bool,
    pass: bool,
}

impl Socks5InitState {
    pub(super) fn new(stream: TcpStream) -> Self {
        Self {
            stream: BufReader::new(stream),
        }
    }

    // wait for rfc to be merged
    async fn no_supported_method(&mut self) -> InboundResult<()> {
        self.stream.write_all(&[0x05, 0xFF]).await?;
        Err(IncorrectFormat("No supported method is provided").into())
    }

    async fn handshake(&mut self) -> InboundResult<HandShakeMethods> {
        let reader = &mut self.stream;
        let ver = reader.read_u8().await?;
        if ver != 0x05 {
            return Err(
                IncorrectFormatWithData("Incorrect ver", vec![ver].into_boxed_slice()).into(),
            );
        }
        let nmtd = reader.read_u8().await? as usize;
        if nmtd == 0 {
            self.no_supported_method().await?;
        }

        let reader = &mut self.stream;
        let mut buf = [0u8; 255];
        let nmtd_actual = reader.read_exact(&mut buf[..nmtd]).await?;
        if nmtd_actual != nmtd {
            return Err(IncorrectFormatWithData(
                "Unmatched NMethod",
                // safety: nmtd_actual <= nmtd
                vec![nmtd_actual as u8].into_boxed_slice(),
            )
            .into());
        };

        // TODO: SIMD?
        let (no_auth, pass) = buf[..nmtd]
            .iter()
            .fold((false, false), |(no_auth, pass), &item| {
                (no_auth || item == 0, pass || item == 2)
            });

        Ok(HandShakeMethods { no_auth, pass })
    }

    pub(super) async fn handshake_no_auth(mut self) -> InboundResult<Socks5PendingAuthState> {
        let methods = self.handshake().await?;
        if !methods.no_auth {
            self.no_supported_method().await?;
        }
        self.stream.write_all(&[0x05, 0x00]).await?;
        Ok(Socks5PendingAuthState {
            stream: self.stream,
            pass_authed: false,
        })
    }

    pub(super) async fn handshake_must_auth(
        mut self,
    ) -> InboundResult<(Socks5PendingAuthState, InboundAuth)> {
        let methods = self.handshake().await?;
        if !methods.pass {
            self.no_supported_method().await?;
        }
        let reader = &mut self.stream;
        reader.write_all(&[0x05, 0x02]).await?;
        let ver = reader.read_u8().await?;
        if ver != 0x01 {
            return Err(
                IncorrectFormatWithData("Incorrect version", vec![ver].into_boxed_slice()).into(),
            );
        }
        let ulen = reader.read_u8().await? as usize;
        let mut user = [0u8; 255];
        reader.read_exact(&mut user[..ulen]).await?;
        let plen = reader.read_u8().await? as usize;
        let mut pass = [0u8; 255];
        reader.read_exact(&mut pass[..plen]).await?;
        let user = str::from_utf8(&user[..ulen])?.to_string();
        let pass = str::from_utf8(&pass[..plen])?.to_string();

        Ok((
            Socks5PendingAuthState {
                stream: self.stream,
                pass_authed: true,
            },
            InboundAuth { user, pass },
        ))
    }

    // TODO: possible may_auth
}

pub struct Socks5PendingAuthState {
    stream: BufReader<TcpStream>,
    pass_authed: bool,
}

impl Socks5PendingAuthState {
    pub(super) async fn get_target(mut self) -> InboundResult<(Socks5ReadyState, Endpoint)> {
        let reader = &mut self.stream;
        if self.pass_authed {
            reader.write_all(&[0x01, 0x00]).await?;
        }

        let ver = reader.read_u8().await?;
        let cmd = reader.read_u8().await?;
        let _rsv = reader.read_u8().await?;
        let atyp = reader.read_u8().await?;
        dbg!(ver, cmd, _rsv, atyp);

        if ver != 0x05 || cmd != 0x01 {
            return Err(IncorrectFormat("Incorrect version").into());
        }

        let host = match atyp {
            0x01 => {
                let mut bytes = [0u8; 4];
                reader.read_exact(&mut bytes).await?;
                let ip = Ipv4Addr::from(bytes);
                Host::Ipv4(ip)
            }
            0x03 => {
                let len = reader.read_u8().await? as usize;
                let mut buffer = [0u8; 256];
                reader.read_exact(&mut buffer[..len]).await?;
                let domain = str::from_utf8(&buffer[..len])?;
                Host::Domain(domain.into())
            }
            0x04 => {
                let mut bytes = [0u8; 16];
                reader.read_exact(&mut bytes).await?;
                let ip = Ipv6Addr::from(bytes);
                Host::Ipv6(ip)
            }
            _ => {
                return Err(
                    IncorrectFormatWithData("Invalid atyp", vec![atyp].into_boxed_slice()).into(),
                );
            }
        };

        dbg!(&host);

        let port = reader.read_u16().await?;

        dbg!(port);

        let endpoint = Endpoint { host, port };

        Ok((
            Socks5ReadyState {
                stream: self.stream,
            },
            endpoint,
        ))
    }
    pub(super) async fn reject(mut self) -> InboundResult<()> {
        self.stream.write_all(&[0x01, 0x01]).await?;
        Ok(())
    }
}

pub struct Socks5ReadyState {
    stream: BufReader<TcpStream>,
}

impl Socks5ReadyState {
    pub(super) async fn success(mut self) -> InboundResult<BufReader<TcpStream>> {
        self.stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        Ok(self.stream)
    }
    pub(super) async fn fail(mut self, _error: OutboundError) -> InboundResult<()> {
        self.stream
            .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        self.stream.flush().await?;
        Ok(())
    }
}
