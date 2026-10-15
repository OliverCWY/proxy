use bytes::BytesMut;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{
    error::{InboundError, OutboundError},
    protocol::{Credentials, TargetAddr},
};

const MAX_HEADER_SIZE: usize = 16 * 1024; // 16 KB max for request headers

pub struct HttpHandshake {
    stream: BufReader<TcpStream>,
}

pub enum HttpInboundSession {
    Connect(PendingHttpSession),
    Reverse(ReverseHttpRequest),
}

impl HttpHandshake {
    pub fn new(stream: BufReader<TcpStream>) -> Self {
        Self { stream }
    }

    /// Read and parse HTTP headers until end of headers, then determine if it's CONNECT or normal HTTP (reverse proxy)
    pub async fn parse(mut self) -> Result<HttpInboundSession, InboundError> {
        let mut buf = BytesMut::with_capacity(2048);

        // Read until \r\n\r\n or \n\n
        let header_end_pos = loop {
            let mut chunk = [0u8; 1024];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(InboundError::PrematureEof);
            }
            buf.extend_from_slice(&chunk[..n]);

            if let Some(pos) = find_header_end(&buf) {
                break pos;
            }

            if buf.len() > MAX_HEADER_SIZE {
                return Err(InboundError::HeaderTooLarge);
            }
        };

        // Parse with httparse
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);

        let parsed_len = req
            .parse(&buf[..header_end_pos])
            .map_err(|e| InboundError::MalformedRequest(format!("HTTP parse error: {e:?}")))?;

        if parsed_len.is_partial() {
            return Err(InboundError::MalformedRequest("Incomplete HTTP headers".into()));
        }

        let method = req.method.unwrap_or_default();
        let path = req.path.unwrap_or("/");

        if method.eq_ignore_ascii_case("CONNECT") {
            let target = TargetAddr::parse_authority(path).ok_or_else(|| {
                InboundError::MalformedRequest(format!("Invalid CONNECT authority: {path}"))
            })?;

            let mut credentials = None;
            for h in req.headers.iter() {
                if h.name.eq_ignore_ascii_case("Proxy-Authorization") {
                    if let Some(creds) = Credentials::parse_http_basic(h.value) {
                        credentials = Some(creds);
                        break;
                    }
                }
            }

            let residual_data = if header_end_pos < buf.len() {
                buf.split_off(header_end_pos).freeze()
            } else {
                bytes::Bytes::new()
            };

            Ok(HttpInboundSession::Connect(PendingHttpSession {
                stream: self.stream,
                target,
                credentials,
                residual_data,
            }))
        } else {
            // Reverse proxy HTTP request
            Ok(HttpInboundSession::Reverse(ReverseHttpRequest {
                stream: self.stream,
                header_bytes: buf.split_to(header_end_pos).freeze(),
                body_residual: buf.freeze(),
            }))
        }
    }
}

pub struct ReverseHttpRequest {
    pub stream: BufReader<TcpStream>,
    pub header_bytes: bytes::Bytes,
    pub body_residual: bytes::Bytes,
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len() {
        if i + 4 <= buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
        if i + 2 <= buf.len() && &buf[i..i + 2] == b"\n\n" {
            return Some(i + 2);
        }
    }
    None
}

pub struct PendingHttpSession {
    stream: BufReader<TcpStream>,
    pub target: TargetAddr,
    pub credentials: Option<Credentials>,
    pub residual_data: bytes::Bytes,
}

impl PendingHttpSession {
    pub fn target(&self) -> &TargetAddr {
        &self.target
    }

    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    /// Reject downstream client with 407 Proxy Authentication Required
    pub async fn reject_auth(mut self) -> Result<(), InboundError> {
        let resp = b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                     Proxy-Authenticate: Basic realm=\"Proxy\"\r\n\
                     Content-Length: 0\r\n\
                     Connection: close\r\n\r\n";
        self.stream.write_all(resp).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Commit success response: HTTP/1.1 200 Connection Established
    pub async fn commit_success(mut self) -> Result<(BufReader<TcpStream>, bytes::Bytes), InboundError> {
        let resp = b"HTTP/1.1 200 Connection Established\r\n\r\n";
        self.stream.write_all(resp).await?;
        self.stream.flush().await?;
        Ok((self.stream, self.residual_data))
    }

    /// Commit failure response: HTTP/1.1 502 Bad Gateway
    pub async fn commit_fail(mut self, error: &OutboundError) -> Result<(), InboundError> {
        let body = format!("Gateway Error: {error}\r\n");
        let resp = format!(
            "HTTP/1.1 502 Bad Gateway\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            body.len(),
            body
        );
        self.stream.write_all(resp.as_bytes()).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
