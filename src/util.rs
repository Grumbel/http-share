// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared string/path helpers and small HTTP utilities.

use std::collections::HashMap;
use std::path::Path;

pub(crate) fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" | "rs" | "toml" | "py" | "c" | "h" | "cpp" | "pem" => {
            "text/plain; charset=utf-8"
        }
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

pub(crate) fn percent_decode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn encode_path_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub(crate) fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let header = header.strip_prefix("bytes=")?;
    let header = header.split(',').next()?.trim();
    let mut parts = header.splitn(2, '-');
    let start_s = parts.next()?.trim();
    let end_s = parts.next()?.trim();
    let (start, end) = if start_s.is_empty() {
        let n: u64 = end_s.parse().ok()?;
        if n == 0 || total == 0 {
            return None;
        }
        let start = total.saturating_sub(n);
        (start, total.saturating_sub(1))
    } else {
        let start: u64 = start_s.parse().ok()?;
        let end: u64 = if end_s.is_empty() {
            total.saturating_sub(1)
        } else {
            end_s.parse().ok()?
        };
        (start, end)
    };
    if start > end || end >= total {
        return None;
    }
    Some((start, end))
}

pub(crate) fn parse_query(q: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = percent_decode(k);
        if !map.contains_key(&key) {
            map.insert(key, percent_decode(v));
        }
    }
    map
}

/// Percent-encode a single URL path/query component (RFC 3986 unreserved left as-is).
pub(crate) fn url_encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%2e%2e"), "..");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(2048).contains("K"));
        assert!(format_bytes(2 * 1024 * 1024).contains("M"));
    }

    #[test]
    fn parse_range_suffix_and_open() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=50-", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=0-0", 1), Some((0, 0)));
        assert_eq!(parse_range("invalid", 100), None);
    }

    #[test]
    fn parse_query_map() {
        let q = parse_query("user=alice&password=s%65cret&u=x");
        assert_eq!(q.get("user").map(String::as_str), Some("alice"));
        assert_eq!(q.get("password").map(String::as_str), Some("secret"));
        // first value wins
        assert_eq!(q.get("u").map(String::as_str), Some("x"));
    }

    #[test]
    fn url_encode_reserved() {
        let s = url_encode_component("a b/c");
        assert!(s.contains("%20"));
        assert!(s.contains("%2F") || s.contains("%2f") || s.contains("/"));
    }

    #[test]
    fn find_bytes_locates_needle() {
        assert_eq!(find_bytes(b"abcdef", b"cd"), Some(2));
        assert_eq!(find_bytes(b"abc", b"z"), None);
    }
}
