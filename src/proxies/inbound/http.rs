use std::net::IpAddr::{self, V4, V6};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::proxies::{
    Endpoint,
    Host::{self, Domain},
    inbound::{
        InboundAuth,
        InboundError::{self, IncorrectFormat},
        InboundResult,
    },
    outbound::OutboundError,
};

pub struct HttpInitState {
    stream: TcpStream,
}

const MAX_ALLOWED_HEADER_SIZE: usize = 1 << 15;

async fn read_line(
    reader: &mut BufReader<TcpStream>,
    buffer: &mut Vec<u8>,
    header_size: &mut usize,
) -> InboundResult<()> {
    // TODO: create zero_copied (when whole line fits into buffer) Cow buffer, but strictly within upper bound
    reader.read_until(b'\n', buffer).await?;
    if buffer.is_empty() {
        return Err(IncorrectFormat("Unexpected EOF").into());
    }
    *header_size += buffer.len();
    if *header_size > MAX_ALLOWED_HEADER_SIZE {
        return Err(IncorrectFormat("Header too large").into());
    }
    buffer.pop();
    if Some(&b'\r') == buffer.last() {
        buffer.pop();
    }

    Ok(())
}

impl HttpInitState {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    pub async fn handshake(self) -> InboundResult<(HttpPendingAuthState, Option<InboundAuth>)> {
        let mut stream = BufReader::new(self.stream);
        let reader = &mut stream;
        let mut header_size = 0usize;
        let mut line = Vec::new();

        fn incorrect_format(msg: &'static str, data: &[u8]) -> InboundError {
            let mut data_copied = Vec::with_capacity(data.len());
            data_copied.copy_from_slice(data);
            InboundError::IncorrectFormatWithData(msg, data_copied.into_boxed_slice())
        }

        read_line(reader, &mut line, &mut header_size).await?;
        let err_fn = || incorrect_format("Incorrect CONNECT format", &line);

        let colon_pos = memchr::memrchr(b':', &line).ok_or_else(err_fn)?;
        let host_start = memchr::memrchr(b' ', &line[..colon_pos]).ok_or_else(err_fn)? + 1;
        let port_end = memchr::memchr(b' ', &line[colon_pos..]).ok_or_else(err_fn)? + colon_pos;
        let host = String::from_utf8(Vec::from(&line[host_start..colon_pos]))?;
        let port = String::from_utf8_lossy(&line[colon_pos + 1..port_end]).parse::<u16>()?;

        let host = host
            .parse::<IpAddr>()
            .map(|ip| match ip {
                V4(ip) => Host::Ipv4(ip),
                V6(ip) => Host::Ipv6(ip),
            })
            .or_else(|_| -> InboundResult<Host> {
                let domain = str::from_utf8(&line[host_start..colon_pos])?;
                Ok(Domain(domain.into()))
            })?;

        let target = Endpoint { host, port };

        let mut auth = None;

        line.clear();
        loop {
            read_line(reader, &mut line, &mut header_size).await?;
            if line.is_empty() {
                return Ok((HttpPendingAuthState { stream, target }, auth));
            }
            let err_fn = || incorrect_format("Incorrect header line: ", &line);
            let pos = memchr::memchr(b':', &line).ok_or_else(err_fn)?;
            let value_start = if pos + 1 < line.len() && line[pos + 1] == b' ' {
                pos + 2
            } else {
                pos + 1
            };
            let (name, value) = (&line[..pos], &line[value_start..]);
            dbg!(
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(value)
            );

            // TODO: should collect other headers?
            if name == b"Proxy-Authorization" {
                if !value.starts_with(b"Basic ") {
                    return Err(IncorrectFormat("Authorization does not start with Bearer ").into());
                }
                use base64::prelude::*;
                let auth_bytes = BASE64_STANDARD.decode(&value[6..])?;
                let (user, pass) = match memchr::memchr(b':', &auth_bytes) {
                    None => (str::from_utf8(&auth_bytes)?.to_string(), "".to_string()),
                    Some(colon_pos) => {
                        let user = str::from_utf8(&auth_bytes[..colon_pos])?;
                        let pass = str::from_utf8(&auth_bytes[colon_pos + 1..])?;
                        (user.to_string(), pass.to_string())
                    }
                };
                auth = Some(InboundAuth { user, pass });
            };
            line.clear()
        }
    }
}

pub struct HttpPendingAuthState {
    stream: BufReader<TcpStream>,
    target: Endpoint,
}

impl HttpPendingAuthState {
    pub fn get_target(self) -> (HttpReadyState, Endpoint) {
        (
            HttpReadyState {
                stream: self.stream,
            },
            self.target,
        )
    }
    pub async fn reject(mut self) -> anyhow::Result<()> {
        self.stream.write_all(b"401 Unauthorized").await?;
        Ok(())
    }
}

pub struct HttpReadyState {
    stream: BufReader<TcpStream>,
}

impl HttpReadyState {
    pub async fn success(mut self) -> InboundResult<BufReader<TcpStream>> {
        self.stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        Ok(self.stream)
    }
    pub async fn fail(mut self, _error: OutboundError) -> InboundResult<()> {
        self.stream.write_all(b"502 Bad Gateway").await?;
        Ok(())
    }
}
