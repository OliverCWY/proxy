use std::sync::Arc;

use memchr::{memchr, memchr3};

use crate::proxies::{Endpoint, outbound::SimpleProxy};

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub(crate) enum Proto {
    Http,
    Https,
    Socks5,
    Socks5h,
}

fn generate_http_auth(auth: &[u8]) -> Arc<[u8]> {
    use base64::prelude::*;
    let base64_auth = BASE64_STANDARD.encode(auth);
    let mut http_auth = Vec::from(b"Proxy-Authorization: Basic ");
    http_auth.reserve(base64_auth.len());
    http_auth.extend(base64_auth.as_bytes());
    http_auth.into()
}

fn generate_socks5_auth(auth: &[u8]) -> Arc<[u8]> {
    let mut socks5_auth = Vec::new();
    let (user, pass) = match memchr(b':', auth) {
        None => (auth, &auth[..0]),
        Some(pos) => (&auth[..pos], &auth[pos + 1..]),
    };
    socks5_auth.reserve(1 + 1 + user.len() + 1 + pass.len());
    socks5_auth.push(0x01);
    socks5_auth.push(user.len() as u8);
    socks5_auth.extend_from_slice(user);
    socks5_auth.push(pass.len() as u8);
    socks5_auth.extend_from_slice(pass);
    socks5_auth.into()
}

pub fn parse_proxy(proxy: &str) -> Option<SimpleProxy> {
    let bytes = proxy.as_bytes();
    if bytes.len() < 7 {
        return None;
    }
    let proto_len = memchr3(b':', b'/', b'/', bytes)?;
    let main_start = proto_len + 3;
    let proto = match &bytes[..proto_len] {
        b"http" => Proto::Http,
        b"https" => Proto::Https,
        b"socks5" => Proto::Socks5,
        b"socks5h" => Proto::Socks5h,
        _ => return None,
    };
    let main = &bytes[main_start..];
    let (auth, endpoint_str) = match memchr(b'@', main) {
        Some(sep) => (Some(&main[..sep]), &main[sep + 1..]),
        None => (None, main),
    };
    let endpoint = Endpoint::try_parse(unsafe { str::from_utf8_unchecked(endpoint_str) }, proto)?;
    match proto {
        Proto::Http => Some(SimpleProxy::Http {
            auth_header: auth.map_or_default(generate_http_auth),
            endpoint,
        }),
        Proto::Socks5 | Proto::Socks5h => Some(SimpleProxy::Socks5 {
            auth: auth.map_or_default(generate_socks5_auth),
            endpoint,
            dns: proto == Proto::Socks5h,
        }),
        Proto::Https => None,
    }
}
