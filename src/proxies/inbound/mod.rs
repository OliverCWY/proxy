use derive_more::From;
use thiserror::Error;
use tokio::{io::BufReader, net::TcpStream};

use crate::proxies::{
    Endpoint,
    inbound::{
        InboundError::NoAuthProvided,
        http::{HttpAuthedState, HttpInitState, HttpReadyState},
        socks5::{Socks5AuthedState, Socks5InitState, Socks5ReadyState},
    },
    outbound::OutboundError,
};

pub struct InboundAuth {
    pub user: String,
    pub pass: String,
}

mod http;
mod socks5;

#[derive(Error, Debug)]
pub enum InboundError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Incorret format: {0}")]
    IncorrectFormat(&'static str),
    #[error("Incorret format: {0}: {1:?}")]
    IncorrectFormatWithData(&'static str, Box<[u8]>),
    #[error("No auth provided")]
    NoAuthProvided,
}

pub type InboundResult<T> = anyhow::Result<T>;

#[derive(From)]
pub enum InboundType {
    Forward(InboundInitState),
    Reverse(TcpStream),
}

#[derive(From)]
pub enum InboundInitState {
    Http(HttpInitState),
    Socks5(Socks5InitState),
}

#[derive(From)]
pub enum InboundAuthedState {
    Http(HttpAuthedState),
    Socks5(Socks5AuthedState),
}

#[derive(From)]
pub enum InboundReadyState {
    Http(HttpReadyState),
    Socks5(Socks5ReadyState),
}

impl InboundType {
    pub async fn new(stream: TcpStream) -> InboundResult<Self> {
        let mut buf = [0u8; 8];
        stream.peek(&mut buf).await?;
        let inbound_type = match buf[0] {
            0x05 => InboundInitState::Http(HttpInitState::new(stream)).into(),
            b'C' if buf.starts_with(b"CONNECT ") => {
                InboundInitState::Socks5(Socks5InitState::new(stream)).into()
            }
            _ => Self::Reverse(stream),
        };

        Ok(inbound_type)
    }
}

impl InboundInitState {
    pub async fn handshake_no_auth(self) -> InboundResult<InboundAuthedState> {
        match self {
            Self::Http(http) => Ok(http.handshake().await?.0.into()),
            Self::Socks5(socks5) => Ok(socks5.handshake_no_auth().await?.into()),
        }
    }
    pub async fn handshake_must_auth(self) -> InboundResult<(InboundAuthedState, InboundAuth)> {
        match self {
            Self::Http(http) => {
                let (next, auth) = http.handshake().await?;
                if let Some(auth) = auth {
                    Ok((next.into(), auth))
                } else {
                    Err(NoAuthProvided.into())
                }
            }
            Self::Socks5(socks5) => socks5
                .handshake_must_auth()
                .await
                .map(|(next, auth)| (next.into(), auth)),
        }
    }
}

impl InboundAuthedState {
    pub async fn reject(self) -> InboundResult<()> {
        todo!()
    }
    pub async fn get_target(self) -> InboundResult<(InboundReadyState, Endpoint)> {
        match self {
            Self::Http(http) => {
                let (next, endpoint) = http.get_target();
                Ok((next.into(), endpoint))
            }
            Self::Socks5(socks5) => {
                let (next, endpoint) = socks5.get_target().await?;
                Ok((next.into(), endpoint))
            }
        }
    }
}

impl InboundReadyState {
    pub async fn fail(self, error: OutboundError) -> InboundResult<()> {
        match self {
            Self::Http(http) => http.fail(error).await,
            Self::Socks5(socks5) => socks5.fail(error).await,
        }
    }
    pub async fn success(self) -> InboundResult<BufReader<TcpStream>> {
        match self {
            Self::Http(http) => http.success().await,
            Self::Socks5(socks5) => socks5.success().await,
        }
    }
}
