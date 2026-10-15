use base64::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Credentials {
    pub user: String,
    pub pass: String,
}

impl Credentials {
    pub fn new(user: impl Into<String>, pass: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            pass: pass.into(),
        }
    }

    pub fn matches(&self, user: &str, pass: &str) -> bool {
        self.user == user && self.pass == pass
    }

    /// Precompute HTTP Basic Auth header: "Proxy-Authorization: Basic <base64>\r\n"
    pub fn to_http_basic_header(&self) -> Arc<[u8]> {
        let mut raw = Vec::with_capacity(self.user.len() + 1 + self.pass.len());
        raw.extend_from_slice(self.user.as_bytes());
        raw.push(b':');
        raw.extend_from_slice(self.pass.as_bytes());

        let encoded = BASE64_STANDARD.encode(&raw);
        let mut header = Vec::with_capacity(32 + encoded.len());
        header.extend_from_slice(b"Proxy-Authorization: Basic ");
        header.extend_from_slice(encoded.as_bytes());
        header.extend_from_slice(b"\r\n");
        header.into()
    }

    /// Precompute RFC 1929 SOCKS5 auth payload: [0x01, ulen, user..., plen, pass...]
    pub fn to_socks5_auth_payload(&self) -> Arc<[u8]> {
        let u_bytes = self.user.as_bytes();
        let p_bytes = self.pass.as_bytes();
        let ulen = u_bytes.len().min(255) as u8;
        let plen = p_bytes.len().min(255) as u8;

        let mut payload = Vec::with_capacity(3 + ulen as usize + plen as usize);
        payload.push(0x01); // Auth subnegotiation version
        payload.push(ulen);
        payload.extend_from_slice(&u_bytes[..ulen as usize]);
        payload.push(plen);
        payload.extend_from_slice(&p_bytes[..plen as usize]);
        payload.into()
    }

    /// Parse HTTP "Basic <base64>" header value
    pub fn parse_http_basic(header_val: &[u8]) -> Option<Self> {
        let trimmed = header_val.trim_ascii();
        if trimmed.len() < 6 {
            return None;
        }
        let (prefix, b64) = trimmed.split_at(6);
        if !prefix.eq_ignore_ascii_case(b"Basic ") {
            return None;
        }
        let decoded = BASE64_STANDARD.decode(b64.trim_ascii()).ok()?;
        let colon = memchr::memchr(b':', &decoded)?;
        let user = String::from_utf8(decoded[..colon].to_vec()).ok()?;
        let pass = String::from_utf8(decoded[colon + 1..].to_vec()).ok()?;
        Some(Self { user, pass })
    }
}
