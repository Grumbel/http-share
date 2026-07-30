//! LAN address detection and browser open helper.

pub(crate) fn local_ips() -> Vec<String> {
    let mut ips = Vec::new();
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for part in s.split_whitespace() {
                if !part.is_empty() {
                    ips.push(part.to_string());
                }
            }
        }
    }
    if let Ok(output) = std::process::Command::new("ip")
        .args(["-o", "addr", "show", "scope", "global"])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for line in s.lines() {
                let mut parts = line.split_whitespace();
                let _ = parts.next();
                let _ = parts.next();
                let family = parts.next().unwrap_or("");
                let addr = parts.next().unwrap_or("").split('/').next().unwrap_or("");
                if (family == "inet" || family == "inet6") && !addr.is_empty() {
                    if !ips.iter().any(|x| x == addr) {
                        ips.push(addr.to_string());
                    }
                }
            }
        }
    }
    if !ips.iter().any(|x| x == "127.0.0.1") {
        ips.push("127.0.0.1".to_string());
    }
    if !ips.iter().any(|x| x == "::1") {
        ips.push("::1".to_string());
    }
    ips
}

/// Host part of a URL: bracket IPv6 addresses.
pub(crate) fn host_for_url(ip: &str) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]")
    } else {
        ip.to_string()
    }
}

pub(crate) fn is_loopback(ip: &str) -> bool {
    ip == "127.0.0.1" || ip == "::1" || ip == "localhost"
}

pub(crate) fn is_ipv6(ip: &str) -> bool {
    ip.contains(':')
}

pub(crate) fn open_browser(url: &str) {
    for (cmd, arg) in [("xdg-open", url), ("open", url), ("wslview", url)] {
        if std::process::Command::new(cmd).arg(arg).spawn().is_ok() {
            return;
        }
    }
    let _ = std::process::Command::new("gio").args(["open", url]).spawn();
}
