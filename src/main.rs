//! http-share — minimal HTTP(S) file sharing utility for ad-hoc transfers.
//!
//! Only files and directories given on the command line are exposed.
//! Virtual root is `/`; individual files land at the root by basename;
//! directories keep their hierarchy under their basename.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rustls::ServerConfig;
use rustls::{Certificate, PrivateKey};

// ---------------------------------------------------------------------------
// Simple CLI (manual — no clap)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Args {
    paths: Vec<PathBuf>,
    port: u16,
    bind: String,
    verbose: bool,
    follow_symlinks: bool,
    public: bool,
    open: bool,
    https: bool,
    dynamic_cert: bool,
    regenerate_cert: bool,
    user: Option<String>,
    password: Option<String>,
}

fn print_usage(program: &str) {
    eprintln!(
        "Usage: {program} [OPTIONS] <PATH>...

Share only the files and directories you explicitly list.
Never exposes the current working directory implicitly.

Options:
  -p, --port PORT          Port to listen on (default: 8000)
      --bind ADDRESS       Address to bind (default: 0.0.0.0)
  -v, --verbose            Verbose logging
      --follow-symlinks    Follow symbolic links
      --public             Disable authentication
      --user USER          Username for Basic Auth
      --password PASS      Password for Basic Auth
      --random-password    Generate random credentials (default when not --public)
      --https              Serve over HTTPS with a self-signed certificate
      --http               Serve plain HTTP (default)
      --dynamic-cert       Use an ephemeral certificate (not stored)
      --regenerate-cert    Replace the persistent self-signed certificate
      --open               Open primary share URL in the default browser
  -h, --help               Print help
"
    );
}

fn parse_args() -> Args {
    let mut args: Vec<String> = env::args().collect();
    let program = args.first().cloned().unwrap_or_else(|| "http-share".into());
    args.remove(0);

    let mut paths = Vec::new();
    let mut port: u16 = 8000;
    let mut bind = "0.0.0.0".to_string();
    let mut verbose = false;
    let mut follow_symlinks = false;
    let mut public = false;
    let mut open = false;
    let mut https = false;
    let mut dynamic_cert = false;
    let mut regenerate_cert = false;
    let mut user: Option<String> = None;
    let mut password: Option<String> = None;
    let mut random_password = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => {
                print_usage(&program);
                std::process::exit(0);
            }
            "-p" | "--port" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --port requires a value");
                    std::process::exit(1);
                }
                port = args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: invalid port");
                    std::process::exit(1);
                });
            }
            "--bind" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --bind requires a value");
                    std::process::exit(1);
                }
                bind = args[i].clone();
            }
            "-v" | "--verbose" => verbose = true,
            "--follow-symlinks" => follow_symlinks = true,
            "--public" => public = true,
            "--open" => open = true,
            "--https" => https = true,
            "--http" => https = false,
            "--dynamic-cert" => dynamic_cert = true,
            "--regenerate-cert" => regenerate_cert = true,
            "--random-password" => random_password = true,
            "--user" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --user requires a value");
                    std::process::exit(1);
                }
                user = Some(args[i].clone());
            }
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --password requires a value");
                    std::process::exit(1);
                }
                password = Some(args[i].clone());
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option {s}");
                print_usage(&program);
                std::process::exit(1);
            }
            _ => paths.push(PathBuf::from(a)),
        }
        i += 1;
    }

    if paths.is_empty() {
        eprintln!("error: at least one path is required");
        print_usage(&program);
        std::process::exit(1);
    }

    // Default: authenticated with random credentials unless --public
    if !public && user.is_none() && password.is_none() {
        random_password = true;
    }
    if random_password {
        if user.is_none() {
            user = Some("share".to_string());
        }
        if password.is_none() {
            password = Some(random_token(16));
        }
    }
    if !public {
        if user.is_none() || password.is_none() {
            eprintln!("error: authentication requires --user and --password (or --random-password / default)");
            std::process::exit(1);
        }
    }

    Args {
        paths,
        port,
        bind,
        verbose,
        follow_symlinks,
        public,
        open,
        https,
        dynamic_cert,
        regenerate_cert,
        user,
        password,
    }
}

fn random_token(len: usize) -> String {
    // Prefer /dev/urandom; fall back to a weak time-based mix if unavailable.
    let mut buf = vec![0u8; len];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((t >> ((i % 8) * 8)) as u8).wrapping_add(i as u8).wrapping_mul(31);
        }
    }
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    buf.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// Virtual filesystem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Vfs {
    files: HashMap<String, PathBuf>,
    dirs: HashMap<String, PathBuf>,
}

impl Vfs {
    fn from_paths(paths: &[PathBuf], follow_symlinks: bool) -> io::Result<Self> {
        let mut files = HashMap::new();
        let mut dirs = HashMap::new();

        for p in paths {
            let meta = fs::symlink_metadata(p).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot access {}: {e}", p.display()))
            })?;

            let path = if follow_symlinks || !meta.file_type().is_symlink() {
                if follow_symlinks {
                    p.canonicalize().map_err(|e| {
                        io::Error::new(e.kind(), format!("canonicalize {}: {e}", p.display()))
                    })?
                } else if p.is_absolute() {
                    p.clone()
                } else {
                    env::current_dir()?.join(p)
                }
            } else if p.is_absolute() {
                p.clone()
            } else {
                env::current_dir()?.join(p)
            };

            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".into());

            if path.is_dir() {
                if files.contains_key(&name) || dirs.contains_key(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("name collision for directory '{name}'"),
                    ));
                }
                dirs.insert(name, path);
            } else if path.is_file() {
                if files.contains_key(&name) || dirs.contains_key(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("name collision for file '{name}'"),
                    ));
                }
                files.insert(name, path);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is neither a regular file nor a directory", p.display()),
                ));
            }
        }

        Ok(Vfs { files, dirs })
    }

    fn resolve(&self, req_path: &str) -> Option<Resolved> {
        let req_path = req_path.trim_start_matches('/');
        if req_path.is_empty() || req_path == "." {
            return Some(Resolved::Index);
        }

        if let Some(real) = self.files.get(req_path) {
            return Some(Resolved::File(real.clone()));
        }

        let mut parts = req_path.splitn(2, '/');
        let first = parts.next()?;
        let rest = parts.next().unwrap_or("");

        if let Some(dir_root) = self.dirs.get(first) {
            if rest.is_empty() {
                return Some(Resolved::Dir(dir_root.clone(), first.to_string()));
            }
            let candidate = dir_root.join(rest);
            let cand_canon = candidate.canonicalize().ok()?;
            let root_canon = dir_root.canonicalize().ok()?;
            if !cand_canon.starts_with(&root_canon) {
                return None;
            }
            if cand_canon.is_file() {
                return Some(Resolved::File(cand_canon));
            }
            if cand_canon.is_dir() {
                return Some(Resolved::Dir(cand_canon, req_path.to_string()));
            }
        }

        None
    }
}

#[derive(Debug)]
enum Resolved {
    Index,
    File(PathBuf),
    Dir(PathBuf, String),
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn mime_for(path: &Path) -> &'static str {
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

fn percent_decode(s: &str) -> String {
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn encode_path_component(s: &str) -> String {
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

fn listing_html(vfs: &Vfs, virt_path: &str, real_dir: Option<&Path>) -> String {
    let mut items: Vec<(String, String, bool)> = Vec::new();

    if let Some(dir) = real_dir {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let href = if virt_path.is_empty() || virt_path == "/" {
                    format!("/{}", encode_path_component(&name))
                } else {
                    format!(
                        "/{}/{}",
                        virt_path.trim_matches('/'),
                        encode_path_component(&name)
                    )
                };
                let display = if is_dir {
                    format!("{name}/")
                } else {
                    name
                };
                items.push((href, display, is_dir));
            }
        }
    } else {
        for name in vfs.files.keys() {
            items.push((
                format!("/{}", encode_path_component(name)),
                name.clone(),
                false,
            ));
        }
        for name in vfs.dirs.keys() {
            items.push((
                format!("/{}/", encode_path_component(name)),
                format!("{name}/"),
                true,
            ));
        }
    }

    items.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));

    let mut body = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>http-share</title>
<style>
  body { font-family: system-ui, sans-serif; margin: 2rem; max-width: 42rem; color: #222; }
  h1 { font-size: 1.2rem; margin-bottom: 1rem; }
  ul { list-style: none; padding: 0; margin: 0; }
  li { padding: 0.3rem 0; border-bottom: 1px solid #eee; }
  a { text-decoration: none; color: #06c; }
  a:hover { text-decoration: underline; }
  .dir { font-weight: 600; }
  footer { margin-top: 2rem; font-size: 0.85rem; color: #888; }
</style>
</head>
<body>
<h1>Shared files</h1>
<ul>
"#,
    );

    if virt_path != "/" && !virt_path.is_empty() {
        body.push_str(r#"<li><a href=".." class="dir">../</a></li>"#);
    }

    for (href, display, is_dir) in &items {
        let class = if *is_dir { r#" class="dir""# } else { "" };
        body.push_str(&format!(
            r#"<li><a href="{}"{}>{}</a></li>"#,
            html_escape(href),
            class,
            html_escape(display)
        ));
    }

    body.push_str(
        r#"</ul>
<footer>http-share</footer>
</body>
</html>"#,
    );
    body
}

fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
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

fn write_response(
    stream: &mut dyn Write,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status} {reason}\r\n")?;
    for (k, v) in headers {
        write!(stream, "{k}: {v}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    stream.write_all(body)?;
    Ok(())
}

fn serve_file(
    stream: &mut dyn Write,
    path: &Path,
    range_header: Option<&str>,
    head_only: bool,
) -> io::Result<()> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let len = meta.len();
    let mime = mime_for(path);

    if let Some(rh) = range_header {
        if let Some((start, end)) = parse_range(rh, len) {
            let to_read = (end - start + 1) as usize;
            let headers = [
                ("Content-Type", mime.to_string()),
                ("Content-Length", to_read.to_string()),
                ("Content-Range", format!("bytes {start}-{end}/{len}")),
                ("Accept-Ranges", "bytes".into()),
                ("Cache-Control", "no-store".into()),
            ];
            if head_only {
                return write_response(stream, 206, "Partial Content", &headers, b"");
            }
            file.seek(SeekFrom::Start(start))?;
            let mut buf = vec![0u8; to_read];
            file.read_exact(&mut buf)?;
            return write_response(stream, 206, "Partial Content", &headers, &buf);
        }
    }

    let headers = [
        ("Content-Type", mime.to_string()),
        ("Content-Length", len.to_string()),
        ("Accept-Ranges", "bytes".into()),
        ("Cache-Control", "no-store".into()),
    ];
    write!(stream, "HTTP/1.1 200 OK\r\n")?;
    for (k, v) in &headers {
        write!(stream, "{k}: {v}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    if head_only {
        return Ok(());
    }

    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
    }
    Ok(())
}

fn check_basic_auth(headers: &HashMap<String, String>, user: &str, pass: &str) -> bool {
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

fn unauthorized(stream: &mut dyn Write) -> io::Result<()> {
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

fn handle_request(
    stream: &mut dyn Write,
    req: &str,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(&str, &str)>,
    cert_pem: Option<&[u8]>,
) {
    let mut lines = req.lines();
    let request_line = match lines.next() {
        Some(l) => l,
        None => return,
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let _version = parts.next().unwrap_or("");

    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    if method != "GET" && method != "HEAD" {
        let _ = write_response(
            stream,
            405,
            "Method Not Allowed",
            &[("Allow", "GET, HEAD".into())],
            b"Method Not Allowed",
        );
        return;
    }

    if let Some((user, pass)) = auth {
        if !check_basic_auth(&headers, user, pass) {
            let _ = unauthorized(stream);
            return;
        }
    }

    let path_only = raw_path.split('?').next().unwrap_or("/");
    let decoded = percent_decode(path_only);

    if verbose {
        eprintln!("→ {method} {decoded}");
    }

    // Well-known certificate endpoint (no auth required for install convenience?
    // Proposal: serve cert so clients can trust it. Keep auth for consistency when enabled,
    // except we already passed auth above. If public, open; if auth, already checked.)
    if decoded == "/certificate.pem" || decoded == "certificate.pem" {
        if let Some(pem) = cert_pem {
            let headers = [
                ("Content-Type", "application/x-pem-file".into()),
                ("Content-Length", pem.len().to_string()),
                ("Content-Disposition", "attachment; filename=\"certificate.pem\"".into()),
                ("Cache-Control", "no-store".into()),
            ];
            if method == "HEAD" {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, pem);
            }
            return;
        }
    }

    let head_only = method == "HEAD";
    let range_header = headers.get("range").map(|s| s.as_str());

    match vfs.resolve(&decoded) {
        Some(Resolved::Index) => {
            let html = listing_html(vfs, "/", None);
            let headers = [
                ("Content-Type", "text/html; charset=utf-8".into()),
                ("Content-Length", html.len().to_string()),
                ("Cache-Control", "no-store".into()),
            ];
            if head_only {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
        }
        Some(Resolved::File(path)) => {
            if let Err(e) = serve_file(stream, &path, range_header, head_only) {
                if verbose {
                    eprintln!("  error serving {}: {e}", path.display());
                }
            }
        }
        Some(Resolved::Dir(real, virt)) => {
            let html = listing_html(vfs, &virt, Some(&real));
            let headers = [
                ("Content-Type", "text/html; charset=utf-8".into()),
                ("Content-Length", html.len().to_string()),
                ("Cache-Control", "no-store".into()),
            ];
            if head_only {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
        }
        None => {
            let _ = write_response(stream, 404, "Not Found", &[], b"Not Found");
        }
    }
}

fn handle_client_plain(
    mut stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(300)));

    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
    handle_request(
        &mut stream,
        &req,
        vfs,
        verbose,
        auth_ref,
        cert_pem.as_deref(),
    );
}

fn handle_client_tls(
    stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
    tls_config: Arc<ServerConfig>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(300)));

    let conn = match rustls::ServerConnection::new(tls_config) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                eprintln!("tls session error: {e}");
            }
            return;
        }
    };
    let mut tls = rustls::StreamOwned::new(conn, stream);

    let mut buf = [0u8; 8192];
    let n = match tls.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
    handle_request(&mut tls, &req, vfs, verbose, auth_ref, cert_pem.as_deref());
}

// ---------------------------------------------------------------------------
// Certificate management
// ---------------------------------------------------------------------------

fn config_dir() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("http-share")
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config").join("http-share")
    } else {
        PathBuf::from("/tmp/http-share-config")
    }
}

fn generate_self_signed() -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut params = rcgen::CertificateParams::new(vec![
        "localhost".into(),
        "http-share.local".into(),
    ]);
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::new(127, 0, 0, 1),
        )));
    // Also common LAN-ish; browsers still warn for self-signed either way.
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "http-share");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "http-share");

    let cert = rcgen::Certificate::from_params(params).map_err(|e| e.to_string())?;
    let cert_pem = cert.serialize_pem().map_err(|e| e.to_string())?.into_bytes();
    let key_pem = cert.serialize_private_key_pem().into_bytes();
    Ok((cert_pem, key_pem))
}

fn load_or_create_cert(
    dynamic: bool,
    regenerate: bool,
) -> Result<(Vec<u8>, Vec<u8>, Option<PathBuf>), String> {
    if dynamic {
        let (c, k) = generate_self_signed()?;
        return Ok((c, k, None));
    }

    let dir = config_dir();
    let cert_path = dir.join("certificate.pem");
    let key_path = dir.join("private-key.pem");

    if regenerate || !cert_path.exists() || !key_path.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("create config dir: {e}"))?;
        let (c, k) = generate_self_signed()?;
        fs::write(&cert_path, &c).map_err(|e| format!("write cert: {e}"))?;
        fs::write(&key_path, &k).map_err(|e| format!("write key: {e}"))?;
        // Restrict key permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));
        }
        return Ok((c, k, Some(cert_path)));
    }

    let c = fs::read(&cert_path).map_err(|e| format!("read cert: {e}"))?;
    let k = fs::read(&key_path).map_err(|e| format!("read key: {e}"))?;
    Ok((c, k, Some(cert_path)))
}

fn make_tls_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>, String> {
    let mut cert_reader = BufReader::new(cert_pem);
    let certs: Vec<Certificate> = rustls_pemfile::certs(&mut cert_reader)
        .map_err(|e| format!("parse cert: {e}"))?
        .into_iter()
        .map(Certificate)
        .collect();
    if certs.is_empty() {
        return Err("no certificates found in pem".into());
    }

    let mut key_reader = BufReader::new(key_pem);
    let keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
        .map_err(|e| format!("parse key: {e}"))?;
    let key = keys
        .into_iter()
        .next()
        .map(PrivateKey)
        .ok_or_else(|| "no private key found in pem".to_string())?;

    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("tls config: {e}"))?;
    Ok(Arc::new(config))
}

// ---------------------------------------------------------------------------
// Network helpers
// ---------------------------------------------------------------------------

fn local_ips() -> Vec<String> {
    let mut ips = vec!["127.0.0.1".to_string()];
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for part in s.split_whitespace() {
                if part.contains('.') && part != "127.0.0.1" {
                    ips.push(part.to_string());
                }
            }
        }
    }
    ips
}

fn open_browser(url: &str) {
    for (cmd, arg) in [("xdg-open", url), ("open", url), ("wslview", url)] {
        if std::process::Command::new(cmd).arg(arg).spawn().is_ok() {
            return;
        }
    }
    let _ = std::process::Command::new("gio").args(["open", url]).spawn();
}

fn url_encode_component(s: &str) -> String {
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();

    let vfs = match Vfs::from_paths(&args.paths, args.follow_symlinks) {
        Ok(v) => Arc::new(v),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if args.verbose {
        eprintln!(
            "sharing {} file(s), {} directory(ies)",
            vfs.files.len(),
            vfs.dirs.len()
        );
        for (n, p) in &vfs.files {
            eprintln!("  file  /{n} → {}", p.display());
        }
        for (n, p) in &vfs.dirs {
            eprintln!("  dir   /{n}/ → {}", p.display());
        }
    }

    let auth: Option<(String, String)> = if args.public {
        None
    } else {
        Some((
            args.user.clone().unwrap(),
            args.password.clone().unwrap(),
        ))
    };

    let (cert_pem, tls_config): (Option<Vec<u8>>, Option<Arc<ServerConfig>>) = if args.https {
        match load_or_create_cert(args.dynamic_cert, args.regenerate_cert) {
            Ok((c, k, path)) => {
                if let Some(p) = path {
                    if args.verbose {
                        eprintln!("using certificate {}", p.display());
                    }
                } else if args.verbose {
                    eprintln!("using ephemeral (dynamic) certificate");
                }
                match make_tls_config(&c, &k) {
                    Ok(cfg) => (Some(c), Some(cfg)),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("error: certificate: {e}");
                std::process::exit(1);
            }
        }
    } else {
        (None, None)
    };

    let scheme = if args.https { "https" } else { "http" };
    let addr = format!("{}:{}", args.bind, args.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("http-share listening on {scheme}://{addr}/");
    let mut primary_url: Option<String> = None;
    for ip in local_ips() {
        if args.bind == "0.0.0.0" || args.bind == ip {
            let url = if let Some((ref u, ref p)) = auth {
                format!(
                    "{scheme}://{}:{}@{ip}:{}/",
                    url_encode_component(u),
                    url_encode_component(p),
                    args.port
                )
            } else {
                format!("{scheme}://{ip}:{}/", args.port)
            };
            if primary_url.is_none() && ip != "127.0.0.1" {
                primary_url = Some(url.clone());
            }
            println!("  {url}");
        }
    }
    if primary_url.is_none() {
        primary_url = Some(if let Some((ref u, ref p)) = auth {
            format!(
                "{scheme}://{}:{}@127.0.0.1:{}/",
                url_encode_component(u),
                url_encode_component(p),
                args.port
            )
        } else {
            format!("{scheme}://127.0.0.1:{}/", args.port)
        });
    }
    if auth.is_some() {
        println!("  authentication: HTTP Basic Auth (credentials embedded in URLs above)");
    } else {
        println!("  authentication: disabled (--public)");
    }
    if args.https {
        println!("  certificate available at {scheme}://…/certificate.pem");
    }
    if args.open {
        if let Some(ref url) = primary_url {
            open_browser(url);
        }
    }
    println!("Press Ctrl+C to stop.");

    let running = Arc::new(AtomicBool::new(true));
    let _ = running;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let vfs = Arc::clone(&vfs);
                let verbose = args.verbose;
                let auth = auth.clone();
                let cert_pem = cert_pem.clone();
                let tls_config = tls_config.clone();
                thread::spawn(move || {
                    if let Some(cfg) = tls_config {
                        handle_client_tls(stream, &vfs, verbose, auth, cert_pem, cfg);
                    } else {
                        handle_client_plain(stream, &vfs, verbose, auth, cert_pem);
                    }
                });
            }
            Err(e) => {
                if args.verbose {
                    eprintln!("accept error: {e}");
                }
            }
        }
    }
}
