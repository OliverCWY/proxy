use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use memchr::memchr;
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, net::TcpStream};

use super::{TcpTarget, utils::invalid_data};
use super::TargetMachine::*;
use super::TargetMachineContainer;
use super::MAX_EXPECTED_TARGET_MACHINE_LENGTH;

type BodyStream = BufReader<TcpStream>;

#[derive(Default)]
pub struct Auth {
    pub user: String,
    pub pass: String
}

pub enum Inbound {
    Forward(TcpTarget, Option<Auth>, BodyStream),
    Reverse(TcpStream)
}

const MAX_ALLOWED_HEADER_LINE: u64 = 1<<16;
const MAX_ALLOWED_HEADER_SIZE: usize = 1<<20;

type HandlerResult = anyhow::Result<(TcpTarget, Option<Auth>)>;

/// Returns: client can provide auth
async fn handle_socks5_handshake(reader: &mut BufReader<TcpStream>) -> anyhow::Result<bool> {
    let ver = reader.read_u8().await?;
    if ver != 0x05 {
        return Err(invalid_data("Incorrect signature"));
    }
    let nmtd = reader.read_u8().await? as usize;
    let mut buf = [0u8; 255];
    if reader.read_exact(&mut buf[..nmtd]).await? != nmtd {
        return Err(invalid_data("NMETHODS does not match!"));
    };
    
    // TODO: SIMD?
    Ok(buf.into_iter().any(|b| b == 2))
}

async fn handle_socks5_auth(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Auth> {
    todo!()
}

async fn handle_socks5_request(reader: &mut BufReader<TcpStream>) -> anyhow::Result<TcpTarget> {
    let ver = reader.read_u8().await?;
    let cmd = reader.read_u8().await?;
    let _rsv = reader.read_u8().await?;
    let atyp = reader.read_u8().await?;
    dbg!(ver, cmd, _rsv, atyp);

    if ver != 0x05 || cmd != 0x01 {
        return Err(invalid_data("Incorrect signature"));
    }
    let host = match atyp {
        0x01 => {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes).await?;
            let ip = IpAddr::from(bytes);
            Ip(ip)
        }
        0x03 => {
            let len = reader.read_u8().await? as usize;
            dbg!(len);
            if len >= MAX_EXPECTED_TARGET_MACHINE_LENGTH {
                return Err(invalid_data("Domain too long!"));
            }
            let mut buffer = [0u8; MAX_EXPECTED_TARGET_MACHINE_LENGTH];
            reader.read_exact(&mut buffer[..len]).await?;
            Domain(TargetMachineContainer::from(&buffer[..len]))
        }
        0x04 => {
            let mut bytes = [0u8; 16];
            reader.read_exact(&mut bytes).await?;
            let ip = IpAddr::from(bytes);
            Ip(ip)
        }
        _ => return Err(invalid_data(format!("Invalid atyp: {}", atyp))),
    };

    dbg!(&host);

    let port = reader.read_u16().await?;

    dbg!(port);

    Ok(TcpTarget { host, port })
}

async fn parse_socks5(reader: &mut BufReader<TcpStream>, requires_auth: bool) -> HandlerResult {
    let supports_auth = handle_socks5_handshake(reader).await?;

    // TODO: probably check auth while making handshake, rather than delay until actual request?
    let auth = if requires_auth {
        reader.write_all(&mut [0x05, 0x02]).await?;
        if !supports_auth {
            return Err(invalid_data("Requires Auth!"));
        }
        let auth = Some(handle_socks5_auth(reader).await?);
        reader.write_all(&mut [0x01, 0x00]).await?;
        auth
    } else {
        reader.write_all(&mut [0x05, 0x00]).await?;
        None
    };
    let target = handle_socks5_request(reader).await?;
    Ok((target, auth))
}

async fn read_line(reader: &mut BufReader<TcpStream>, buffer: &mut Vec<u8>, header_size: &mut usize) -> anyhow::Result<()> {
    // TODO: create zero_copied (when whole line fits into buffer) Cow buffer, but strictly within upper bound
    reader.read_until(b'\n', buffer).await?;
    if buffer.is_empty() {
        return Err(invalid_data("User closed connection"));
    }
    *header_size += buffer.len();
    if *header_size > MAX_ALLOWED_HEADER_SIZE {
        return Err(invalid_data("Header too large"));
    }
    buffer.pop();
    if Some(&b'\r') == buffer.last() {
        buffer.pop();
    }

    Ok(())
}

async fn parse_http(reader: &mut BufReader<TcpStream>) -> anyhow::Result<(TcpTarget, Option<Auth>)> {
    let mut header_size = 0usize;
    let mut line = Vec::new();

    fn incorrect_header(line: &[u8]) -> anyhow::Error {
        invalid_data(
            format!("Incorrect format for header line: {}", String::from_utf8_lossy(line))
        ).into()
    }

    read_line(reader, &mut line, &mut header_size).await?;
    let err_fn = || incorrect_header(&line);

    // TODO: support ipv6
    let colon_pos = memchr::memchr(b':', &line).ok_or_else(err_fn)?;
    let host_start = memchr::memrchr(b' ', &line[..colon_pos]).ok_or_else(err_fn)?;
    let port_end = memchr::memchr(b' ', &line[colon_pos..]).ok_or_else(err_fn)? + colon_pos;
    let host = String::from_utf8(Vec::from(&line[host_start..colon_pos]))?;
    let port = String::from_utf8_lossy(&line[colon_pos+1..port_end]).parse::<u16>()?;

    let host = host.parse::<IpAddr>().map(Ip).unwrap_or_else(|_| 
        Domain(TargetMachineContainer::from(&line[host_start..colon_pos]))
    );

    let target = TcpTarget {
        host, port
    };

    let mut auth = None;

    line.clear();
    loop {
        read_line(reader, &mut line, &mut header_size).await?;
        if line.is_empty() {
            return Ok((target, auth));
        }
        let err_fn = || incorrect_header(&line);
        let pos = memchr(b':', &line).ok_or_else(err_fn)?;
        let value_start = if pos+1 < line.len() && line[pos+1] == b' ' {
            pos + 2
        } else {
            pos + 1
        };
        let (name, value) = (&line[..pos], &line[value_start..]);
        dbg!(String::from_utf8_lossy(name), String::from_utf8_lossy(value));

        // TODO: should collect other headers?
        if name == b"Proxy-Authorization" {
            if !value.starts_with(b"Basic ") {
                return Err(incorrect_header(&line));
            }
            use base64::prelude::*;
            let auth_bytes = BASE64_STANDARD.decode(&value[6..])?;
            let colon_pos = memchr::memchr(b':', &auth_bytes).ok_or_else(err_fn)?;
            let user = String::from_utf8(Vec::from(&auth_bytes[..colon_pos]))?;
            let pass = String::from_utf8(Vec::from(&auth_bytes[colon_pos+1..]))?;
            auth = Some(Auth {user, pass});
        };
        line.clear()
    }
}

// TODO: should accept config to determine whether socks5 should request for auth
pub async fn parse_inbound_request(mut client: TcpStream) -> Result<Inbound, (anyhow::Error, TcpStream)> {
    let mut buf = [0u8; 8];
    if let Err(e) = client.peek(&mut buf).await {
        return Err((e.into(), client))
    }
    match buf[0] {
        0x05 => {
            let mut reader = BufReader::new(client);
            match parse_socks5(&mut reader, false).await {
                Err(e) => Err((e, reader.into_inner())),
                Ok((target, auth)) => Ok(Inbound::Forward(target, auth, reader))
            }
        },
        b'C' if buf.starts_with(b"CONNECT ") => {
            let mut reader = BufReader::new(client);
            match parse_http(&mut reader).await {
                Err(e) => Err((e, reader.into_inner())),
                Ok((target, auth)) => Ok(Inbound::Forward(target, auth, reader))
            }
        },
        _ => Ok(Inbound::Reverse(client))
    }
}