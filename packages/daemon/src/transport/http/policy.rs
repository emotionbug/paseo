use http::{HeaderMap, HeaderValue};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub(super) fn bearer(value: Option<&str>) -> Option<&str> {
    let mut parts = value?.split_whitespace();
    if parts.next()? != "Bearer" {
        return None;
    }
    let token = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(token)
}

pub(super) fn valid_ws_protocols(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get("sec-websocket-protocol") else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    value.split(',').map(str::trim).all(|protocol| {
        !protocol.is_empty()
            && protocol
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            && seen.insert(protocol)
    })
}

pub(super) fn ws_protocol(headers: &HeaderMap, password: bool) -> Option<String> {
    let protocols = headers.get("sec-websocket-protocol")?.to_str().ok()?;
    if password {
        protocols
            .split(',')
            .map(str::trim)
            .find(|p| p.starts_with("paseo.bearer."))
            .map(str::to_owned)
    } else {
        protocols
            .split(',')
            .map(str::trim)
            .find(|p| !p.is_empty())
            .map(str::to_owned)
    }
}

pub(super) fn ws_bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(token) = bearer(headers.get("authorization").and_then(|v| v.to_str().ok())) {
        return Some(format!("Bearer {token}"));
    }
    headers
        .get("sec-websocket-protocol")?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .find_map(|p| {
            p.strip_prefix("paseo.bearer.")
                .map(|t| format!("Bearer {t}"))
        })
}

pub(super) fn hostname(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']')?.0
    } else {
        host.split(':').next()?
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

pub(super) fn host_allowed(host: Option<&str>, config: &Value) -> bool {
    let Some(host) = host.and_then(hostname) else {
        return false;
    };
    if config.as_bool() == Some(true)
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok()
    {
        return true;
    }
    config.as_array().is_some_and(|patterns| {
        patterns.iter().filter_map(Value::as_str).any(|pattern| {
            let pattern = pattern.trim().to_ascii_lowercase();
            if let Some(base) = pattern.strip_prefix('.') {
                !base.is_empty() && (host == base || host.ends_with(&format!(".{base}")))
            } else {
                host == pattern
            }
        })
    })
}

fn loopback_alias(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub(super) fn same_origin(origin: &str, host: &str) -> bool {
    if origin == format!("http://{host}") || origin == format!("https://{host}") {
        return true;
    }
    let Ok(origin) = url::Url::parse(origin) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https") {
        return false;
    }
    let Ok(request) = url::Url::parse(&format!("{}://{host}", origin.scheme())) else {
        return false;
    };
    origin.port_or_known_default() == request.port_or_known_default()
        && origin.host_str().is_some_and(loopback_alias)
        && request.host_str().is_some_and(loopback_alias)
}

pub(super) fn allowed_origin(origin: &str, config: &Value, listen: &Value) -> bool {
    if origin == "paseo://app" {
        return true;
    }
    if config["corsAllowedOrigins"]
        .as_array()
        .is_some_and(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|v| v == "*" || v == origin)
        })
    {
        return true;
    }
    if listen["type"] == "tcp" {
        let Some(port) = listen["port"].as_u64() else {
            return false;
        };
        for host in [
            listen["host"].as_str().unwrap_or(""),
            "localhost",
            "127.0.0.1",
        ] {
            if origin == format!("http://{}:{port}", authority_host(host)) {
                return true;
            }
        }
    }
    false
}

pub(super) fn cors(headers: &mut HeaderMap, origin: Option<&str>, config: &Value, listen: &Value) {
    if let Some(origin) = origin.filter(|origin| allowed_origin(origin, config, listen)) {
        if let Ok(value) = HeaderValue::from_str(origin) {
            headers.insert("access-control-allow-origin", value);
            headers.insert(
                "access-control-allow-methods",
                HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
            );
            headers.insert(
                "access-control-allow-headers",
                HeaderValue::from_static("Content-Type, Authorization"),
            );
            headers.insert(
                "access-control-allow-credentials",
                HeaderValue::from_static("true"),
            );
        }
    }
}

pub(super) fn activity_peer(ip: Option<IpAddr>) -> bool {
    matches!(ip, Some(IpAddr::V4(ip)) if ip == Ipv4Addr::LOCALHOST)
        || matches!(ip, Some(IpAddr::V6(ip)) if ip == Ipv6Addr::LOCALHOST || ip.to_ipv4_mapped() == Some(Ipv4Addr::LOCALHOST))
}

pub(super) fn authority_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn bearer_is_case_sensitive_and_single_token() {
        assert_eq!(bearer(Some(" Bearer\tsecret ")), Some("secret"));
        for value in ["bearer secret", "Bearer", "Bearer one two", "Basic secret"] {
            assert_eq!(bearer(Some(value)), None);
        }
    }
    #[test]
    fn websocket_credentials_keep_header_precedence_and_reject_bad_protocols() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer header-token"),
        );
        headers.insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("paseo.bearer.protocol.token"),
        );
        assert_eq!(ws_bearer(&headers).as_deref(), Some("Bearer header-token"));
        assert_eq!(
            ws_protocol(&headers, true).as_deref(),
            Some("paseo.bearer.protocol.token")
        );
        assert!(valid_ws_protocols(&headers));
        headers.insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("same, same"),
        );
        assert!(!valid_ws_protocols(&headers));
        headers.insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("bad protocol"),
        );
        assert!(!valid_ws_protocols(&headers));
    }
    #[test]
    fn host_policy_preserves_defaults_and_suffix_boundaries() {
        for host in [
            "localhost:6767",
            "svc.localhost",
            "[::1]:6767",
            "192.168.1.8",
        ] {
            assert!(host_allowed(Some(host), &Value::Null));
        }
        assert!(host_allowed(
            Some("a.example.com"),
            &json!([".example.com"])
        ));
        assert!(!host_allowed(
            Some("notexample.com"),
            &json!([".example.com"])
        ));
        assert!(!host_allowed(None, &json!(true)));
    }
    #[test]
    fn origin_aliases_require_matching_port() {
        assert!(same_origin("http://localhost:6767", "127.0.0.1:6767"));
        assert!(same_origin("https://[::1]", "localhost:443"));
        assert!(!same_origin("http://localhost:6768", "127.0.0.1:6767"));
        assert!(!same_origin("file://localhost", "localhost"));
    }
    #[test]
    fn activity_never_trusts_forwarded_or_ipc_peers() {
        assert!(activity_peer(Some("::ffff:127.0.0.1".parse().unwrap())));
        assert!(!activity_peer(Some("127.0.0.2".parse().unwrap())));
        assert!(!activity_peer(None));
        assert!(!activity_peer(Some("10.0.0.1".parse().unwrap())));
    }
}
