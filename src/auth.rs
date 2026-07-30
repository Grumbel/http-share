//! HTTP Basic, query-parameter, and session-cookie authentication.

use std::collections::HashMap;
use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::http_io::write_response;
use crate::util::url_encode_component;

pub(crate) fn check_basic_auth(headers: &HashMap<String, String>, user: &str, pass: &str) -> bool {
    let Some(auth) = headers.get("authorization") else {
        return false;
    };
    let Some(token) = auth.strip_prefix("Basic ").or_else(|| auth.strip_prefix("basic ")) else {
        return false;
    };
    let Ok(decoded) = B64.decode(token.trim()) else {
        return false;
    };
    let Ok(s) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((u, p)) = s.split_once(':') else {
        return false;
    };
    u == user && p == pass
}

pub(crate) fn check_auth(
    headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    user: &str,
    pass: &str,
) -> bool {
    if check_basic_auth(headers, user, pass) {
        return true;
    }
    if query_credentials_match(query, user, pass) {
        return true;
    }
    check_cookie_auth(headers, user, pass)
}

pub(crate) fn query_credentials_match(query: &HashMap<String, String>, user: &str, pass: &str) -> bool {
    let q_user = query
        .get("user")
        .or_else(|| query.get("u"))
        .map(|s| s.as_str());
    let q_pass = query
        .get("password")
        .or_else(|| query.get("pass"))
        .or_else(|| query.get("p"))
        .map(|s| s.as_str());
    matches!((q_user, q_pass), (Some(u), Some(p)) if u == user && p == pass)
}

/// Cookie value is base64(`user:pass`), same encoding as Basic Auth.
pub(crate) fn check_cookie_auth(headers: &HashMap<String, String>, user: &str, pass: &str) -> bool {
    let Some(cookie_hdr) = headers.get("cookie") else {
        return false;
    };
    for part in cookie_hdr.split(';') {
        let part = part.trim();
        let Some((name, val)) = part.split_once('=') else {
            continue;
        };
        if name.trim() != "http_share_auth" {
            continue;
        }
        let Ok(decoded) = B64.decode(val.trim()) else {
            return false;
        };
        let Ok(s) = String::from_utf8(decoded) else {
            return false;
        };
        let Some((u, p)) = s.split_once(':') else {
            return false;
        };
        return u == user && p == pass;
    }
    false
}

pub(crate) fn auth_set_cookie(user: &str, pass: &str) -> String {
    let token = B64.encode(format!("{user}:{pass}"));
    format!("http_share_auth={token}; Path=/; HttpOnly; SameSite=Lax")
}

/// Query suffix to append to internal links so navigation keeps credentials
/// when the client does not yet send the session cookie (first page after QR).
pub(crate) fn auth_query_suffix(user: &str, pass: &str) -> String {
    format!(
        "?user={}&password={}",
        url_encode_component(user),
        url_encode_component(pass)
    )
}

pub(crate) fn with_auth_query(href: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        return href.to_string();
    }
    if href.contains('?') {
        // suffix starts with '?'; append as '&…'
        format!("{}&{}", href, &suffix[1..])
    } else {
        format!("{href}{suffix}")
    }
}

/// Build a share URL. `for_qr` uses query-string credentials (Android-friendly)
/// instead of URL userinfo, which many QR scanners drop or mishandle.
pub(crate) fn build_share_url(
    scheme: &str,
    host: &str,
    port: u16,
    auth: Option<(&str, &str)>,
    for_qr: bool,
) -> String {
    match auth {
        Some((u, p)) if for_qr => format!(
            "{scheme}://{host}:{port}/?user={}&password={}",
            url_encode_component(u),
            url_encode_component(p)
        ),
        Some((u, p)) => format!(
            "{scheme}://{}:{}@{host}:{port}/",
            url_encode_component(u),
            url_encode_component(p)
        ),
        None => format!("{scheme}://{host}:{port}/"),
    }
}

pub(crate) fn unauthorized(stream: &mut dyn Write) -> io::Result<()> {
    write_response(
        stream,
        401,
        "Unauthorized",
        &[
            ("WWW-Authenticate", r#"Basic realm="http-share""#.into()),
            ("Content-Type", "text/plain; charset=utf-8".into()),
        ],
        b"Unauthorized",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn query_credentials_match_long_and_short() {
        let mut q = HashMap::new();
        q.insert("user".into(), "alice".into());
        q.insert("password".into(), "secret".into());
        assert!(query_credentials_match(&q, "alice", "secret"));
        assert!(!query_credentials_match(&q, "alice", "nope"));

        let mut q2 = HashMap::new();
        q2.insert("u".into(), "bob".into());
        q2.insert("p".into(), "x".into());
        assert!(query_credentials_match(&q2, "bob", "x"));
    }

    #[test]
    fn with_auth_query_appends() {
        assert_eq!(with_auth_query("/pub/", ""), "/pub/".to_string());
        let s = with_auth_query("/pub/", "?user=a&password=b");
        assert!(s.starts_with("/pub/?"));
        assert!(s.contains("user=a"));
        let s2 = with_auth_query("/x?y=1", "?user=a&password=b");
        assert!(s2.contains("y=1") && s2.contains("user=a"));
    }

    #[test]
    fn basic_auth_header() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let token = B64.encode("alice:secret");
        let mut h = HashMap::new();
        h.insert("authorization".into(), format!("Basic {token}"));
        assert!(check_basic_auth(&h, "alice", "secret"));
        assert!(!check_basic_auth(&h, "alice", "wrong"));
    }

    #[test]
    fn cookie_auth_roundtrip() {
        let cookie = auth_set_cookie("alice", "secret");
        // "http_share_auth=...; Path=/; ..."
        let val = cookie.split(';').next().unwrap();
        let mut h = HashMap::new();
        h.insert("cookie".into(), val.to_string());
        assert!(check_cookie_auth(&h, "alice", "secret"));
        assert!(!check_cookie_auth(&h, "alice", "nope"));
    }
}
