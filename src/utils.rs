const ALLOWED_PROXY_PREFIXES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

pub fn is_valid_proxy(proxy: &str) -> bool {
    ALLOWED_PROXY_PREFIXES
        .iter()
        .any(|pre| proxy.starts_with(*pre))
}
