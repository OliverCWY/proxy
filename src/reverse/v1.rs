use std::sync::Arc;
use crate::{
    error::InboundError,
    protocol::TargetAddr,
    reverse::rules::AccountMatcherRuntime,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReverseTarget {
    pub platform: Option<String>,
    pub account: Option<String>,
    pub protocol: String,
    pub target: TargetAddr,
    pub target_path: String,
}

/// Parse Resin V1 / Legacy reverse proxy request path:
/// Format: `/{PROXY_TOKEN}/{Platform.Account}/{protocol}/{host}/{path...}?{query}`
pub fn parse_resin_path(
    raw_path: &str,
    expected_proxy_token: Option<&str>,
    headers: &[httparse::Header],
    rules: Option<&Arc<AccountMatcherRuntime>>,
) -> Result<ParsedReverseTarget, InboundError> {
    let path = raw_path.trim_start_matches('/');
    if path.is_empty() {
        return Err(InboundError::MalformedRequest("Empty reverse proxy path".into()));
    }

    // Split into up to 5 segments: [token, identity, protocol, host, rest]
    let segments: Vec<&str> = path.splitn(5, '/').collect();

    // Support legacy /target/{authority}/{path...} as fallback
    if segments.len() >= 2 && segments[0] == "target" {
        let authority = segments[1];
        let rest = if segments.len() >= 3 {
            format!("/{}", segments[2..].join("/"))
        } else {
            "/".to_string()
        };
        let target = TargetAddr::parse_authority(authority).ok_or_else(|| {
            InboundError::MalformedRequest(format!("Invalid target authority: {authority}"))
        })?;
        return Ok(ParsedReverseTarget {
            platform: None,
            account: None,
            protocol: "http".into(),
            target,
            target_path: rest,
        });
    }

    if segments.len() < 4 {
        return Err(InboundError::MalformedRequest(
            "Invalid Resin reverse proxy path. Expected: /{token}/{platform.account}/{protocol}/{host}/{path}".into(),
        ));
    }

    let token_seg = segments[0];
    let identity_seg = segments[1];
    let protocol_seg = segments[2].to_ascii_lowercase();
    let host_seg = segments[3];
    let rest_path = if segments.len() == 5 {
        format!("/{}", segments[4])
    } else {
        "/".to_string()
    };

    // 1. Token validation
    if let Some(expected) = expected_proxy_token {
        if !expected.is_empty() && token_seg != expected {
            return Err(InboundError::AuthFailed);
        }
    }

    // 2. Identity parsing (Platform.Account or Platform:Account)
    let (platform, mut account) = parse_identity(identity_seg);

    // 3. Protocol validation
    if protocol_seg != "http" && protocol_seg != "https" {
        return Err(InboundError::MalformedRequest(format!(
            "Invalid protocol: '{protocol_seg}', must be 'http' or 'https'"
        )));
    }

    // 4. Host validation & TargetAddr construction
    let target = parse_target_host(host_seg, &protocol_seg)?;

    // 5. Account resolution fallback
    // Priority: URL account -> X-Resin-Account header -> Rule Trie matching
    if account.is_none() {
        account = extract_account_fallback(&target, &rest_path, headers, rules);
    }

    Ok(ParsedReverseTarget {
        platform,
        account,
        protocol: protocol_seg,
        target,
        target_path: rest_path,
    })
}

fn parse_identity(identity: &str) -> (Option<String>, Option<String>) {
    let trimmed = identity.trim();
    if trimmed.is_empty() || trimmed == "_" {
        return (None, None);
    }

    // Prefer '.' as V1 delimiter, fallback to ':'
    let delimiter_idx = trimmed.find('.').or_else(|| trimmed.find(':'));
    if let Some(idx) = delimiter_idx {
        let plat = trimmed[..idx].trim();
        let acc = trimmed[idx + 1..].trim();

        let plat_opt = if plat.is_empty() || plat == "_" { None } else { Some(plat.to_string()) };
        let acc_opt = if acc.is_empty() || acc == "_" { None } else { Some(acc.to_string()) };
        (plat_opt, acc_opt)
    } else {
        (Some(trimmed.to_string()), None)
    }
}

fn parse_target_host(host_str: &str, protocol: &str) -> Result<TargetAddr, InboundError> {
    let default_port = if protocol == "https" { 443 } else { 80 };

    // Check if host_str already contains port
    if let Some(target) = TargetAddr::parse_authority(host_str) {
        return Ok(target);
    }

    // If no port, append default port
    let with_port = if host_str.starts_with('[') && host_str.ends_with(']') {
        format!("{host_str}:{default_port}")
    } else {
        format!("{host_str}:{default_port}")
    };

    TargetAddr::parse_authority(&with_port).ok_or_else(|| {
        InboundError::MalformedRequest(format!("Invalid target host: '{host_str}'"))
    })
}

fn extract_account_fallback(
    target: &TargetAddr,
    target_path: &str,
    headers: &[httparse::Header],
    rules: Option<&Arc<AccountMatcherRuntime>>,
) -> Option<String> {
    // Priority 1: X-Resin-Account or x-account-id header
    for h in headers {
        if h.name.eq_ignore_ascii_case("X-Resin-Account") || h.name.eq_ignore_ascii_case("x-account-id") {
            if let Ok(val) = std::str::from_utf8(h.value) {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // Priority 2: AccountMatcher rule trie
    if let Some(matcher_runtime) = rules {
        let matcher = matcher_runtime.load();
        let host = target.host.to_string();

        if let Some(headers_to_inspect) = matcher.match_url(&host, target_path) {
            for h_name in headers_to_inspect {
                for req_h in headers {
                    if req_h.name.eq_ignore_ascii_case(h_name) {
                        if let Ok(val) = std::str::from_utf8(req_h.value) {
                            let trimmed = val.trim();
                            if !trimmed.is_empty() {
                                return Some(trimmed.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}
