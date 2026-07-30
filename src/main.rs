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
use std::sync::atomic::{AtomicBool, Ordering};
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
    qr: bool,
    https: bool,
    dynamic_cert: bool,
    regenerate_cert: bool,
    user: Option<String>,
    password: Option<String>,
    incoming: Option<PathBuf>,
    upload_only: bool,
    max_upload_size: Option<u64>,
    allow_overwrite: bool,
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
      --qr                 Print a terminal QR code of the primary URL
      --incoming DIR       Accept uploads into DIR
      --upload-only        Only accept uploads (no downloads of shared paths)
      --max-upload-size N  Max upload size (e.g. 10M, 1G; default unlimited)
      --allow-overwrite    Allow uploaded files to replace existing ones
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
    let mut qr = false;
    let mut https = false;
    let mut dynamic_cert = false;
    let mut regenerate_cert = false;
    let mut user: Option<String> = None;
    let mut password: Option<String> = None;
    let mut random_password = false;
    let mut incoming: Option<PathBuf> = None;
    let mut upload_only = false;
    let mut max_upload_size: Option<u64> = None;
    let mut allow_overwrite = false;

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
            "--qr" => qr = true,
            "--https" => https = true,
            "--http" => https = false,
            "--dynamic-cert" => dynamic_cert = true,
            "--regenerate-cert" => regenerate_cert = true,
            "--random-password" => random_password = true,
            "--upload-only" => upload_only = true,
            "--allow-overwrite" => allow_overwrite = true,
            "--incoming" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --incoming requires a directory path");
                    std::process::exit(1);
                }
                incoming = Some(PathBuf::from(&args[i]));
            }
            "--max-upload-size" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --max-upload-size requires a value");
                    std::process::exit(1);
                }
                max_upload_size = Some(parse_size(&args[i]).unwrap_or_else(|e| {
                    eprintln!("error: invalid --max-upload-size: {e}");
                    std::process::exit(1);
                }));
            }
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

    if paths.is_empty() && incoming.is_none() {
        eprintln!("error: at least one path is required (or --incoming DIR)");
        print_usage(&program);
        std::process::exit(1);
    }
    if upload_only && incoming.is_none() {
        eprintln!("error: --upload-only requires --incoming DIR");
        std::process::exit(1);
    }
    if let Some(ref dir) = incoming {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("error: cannot create incoming directory {}: {e}", dir.display());
            std::process::exit(1);
        }
        let meta = fs::metadata(dir).unwrap_or_else(|e| {
            eprintln!("error: cannot access incoming directory {}: {e}", dir.display());
            std::process::exit(1);
        });
        if !meta.is_dir() {
            eprintln!("error: --incoming path is not a directory: {}", dir.display());
            std::process::exit(1);
        }
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
        qr,
        https,
        dynamic_cert,
        regenerate_cert,
        user,
        password,
        incoming,
        upload_only,
        max_upload_size,
        allow_overwrite,
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".into());
    }
    let (num_str, mult) = match s.as_bytes().last().map(|b| b.to_ascii_lowercase()) {
        Some(b @ (b'k' | b'm' | b'g' | b't')) => {
            let m = match b {
                b'k' => 1024u64,
                b'm' => 1024 * 1024,
                b'g' => 1024 * 1024 * 1024,
                b't' => 1024 * 1024 * 1024 * 1024,
                _ => 1,
            };
            (&s[..s.len() - 1], m)
        }
        _ => (s, 1u64),
    };
    let n: u64 = num_str.trim().parse().map_err(|_| format!("not a number: {num_str}"))?;
    n.checked_mul(mult).ok_or_else(|| "size overflow".into())
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

fn listing_html(vfs: &Vfs, virt_path: &str, real_dir: Option<&Path>, show_upload: bool) -> String {
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

    body.push_str(r#"</ul>"#);
    if show_upload {
        body.push_str(
            r#"<p style="margin-top:1.5rem"><a href="/upload">Upload a file…</a></p>"#,
        );
    }
    body.push_str(
        r#"
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


fn upload_form_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Upload — http-share</title>
<style>
  body { font-family: system-ui, sans-serif; margin: 2rem; max-width: 28rem; color: #222; }
  h1 { font-size: 1.2rem; }
  form { margin-top: 1.5rem; }
  input[type=file] { display: block; margin: 1rem 0; }
  button { padding: 0.5rem 1.2rem; font-size: 1rem; cursor: pointer; }
  a { color: #06c; }
  .msg { margin-top: 1rem; padding: 0.75rem; background: #f0f7ff; border-radius: 4px; }
  .err { background: #fff0f0; }
</style>
</head>
<body>
<h1>Upload a file</h1>
<p><a href="/">← Shared files</a></p>
<form method="POST" action="/upload" enctype="multipart/form-data">
  <input type="file" name="file" required>
  <button type="submit">Upload</button>
</form>
</body>
</html>"#.to_string()
}

fn upload_result_html(ok: bool, message: &str) -> String {
    let cls = if ok { "msg" } else { "msg err" };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Upload — http-share</title>
<style>
  body {{ font-family: system-ui, sans-serif; margin: 2rem; max-width: 28rem; color: #222; }}
  h1 {{ font-size: 1.2rem; }}
  a {{ color: #06c; }}
  .msg {{ margin-top: 1rem; padding: 0.75rem; background: #f0f7ff; border-radius: 4px; }}
  .err {{ background: #fff0f0; }}
</style>
</head>
<body>
<h1>Upload</h1>
<p><a href="/upload">Upload another</a> · <a href="/">Shared files</a></p>
<div class="{cls}">{msg}</div>
</body>
</html>"#,
        cls = cls,
        msg = html_escape(message)
    )
}

/// Sanitize a client-provided filename: basename only, no path separators, no empty.
fn sanitize_filename(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    // Reject path components
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    // Strip any leading dots-only weirdness beyond hidden files
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned)
}

fn unique_dest(dir: &Path, filename: &str, allow_overwrite: bool) -> PathBuf {
    let dest = dir.join(filename);
    if allow_overwrite || !dest.exists() {
        return dest;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..10000 {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-overflow{ext}"))
}

/// Extract boundary from Content-Type: multipart/form-data; boundary=...
fn multipart_boundary(content_type: &str) -> Option<String> {
    let ct = content_type.to_ascii_lowercase();
    if !ct.starts_with("multipart/form-data") {
        return None;
    }
    for part in content_type.split(';') {
        let part = part.trim();
        if let Some(b) = part
            .strip_prefix("boundary=")
            .or_else(|| part.strip_prefix("Boundary="))
        {
            let b = b.trim().trim_matches('"');
            if !b.is_empty() {
                return Some(b.to_string());
            }
        }
    }
    None
}

/// Parse a single file part from multipart body. Returns (filename, file_bytes).
fn parse_multipart_file(body: &[u8], boundary: &str) -> Result<(String, Vec<u8>), String> {
    let delim = format!("--{boundary}");
    let delim_bytes = delim.as_bytes();
    // Find first boundary
    let mut pos = find_bytes(body, delim_bytes).ok_or("missing multipart boundary")?;
    pos += delim_bytes.len();
    // Skip optional CRLF after boundary
    if body.get(pos..pos + 2) == Some(b"\r\n") {
        pos += 2;
    }

    // Walk parts until we find a file field
    while pos < body.len() {
        // End marker --boundary--
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        // Headers until blank line
        let headers_end = find_bytes(&body[pos..], b"\r\n\r\n")
            .ok_or("multipart: missing header terminator")?;
        let headers = std::str::from_utf8(&body[pos..pos + headers_end])
            .map_err(|_| "multipart: non-utf8 headers")?;
        pos += headers_end + 4;

        let mut filename: Option<String> = None;
        let mut is_file_field = false;
        for line in headers.lines() {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                // name="file"; filename="x.txt"
                if let Some(fn_start) = line.find("filename=") {
                    let rest = &line[fn_start + 9..];
                    let fname = rest.trim().trim_matches('"').trim_matches('\'');
                    filename = Some(fname.to_string());
                    is_file_field = true;
                }
                if lower.contains("name=\"file\"") || lower.contains("name=file") {
                    is_file_field = true;
                }
            }
        }

        // Body until next boundary (preceded by CRLF)
        let next_delim = format!("\r\n--{boundary}");
        let next = find_bytes(&body[pos..], next_delim.as_bytes())
            .ok_or("multipart: missing next boundary")?;
        let file_data = &body[pos..pos + next];
        pos += next + next_delim.len();
        // After boundary: either -- (end) or CRLF
        if body.get(pos..pos + 2) == Some(b"--") {
            // end
        } else if body.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }

        if is_file_field {
            let name = filename.unwrap_or_else(|| "upload.bin".into());
            return Ok((name, file_data.to_vec()));
        }
    }
    Err("multipart: no file field found".into())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[derive(Clone)]
struct UploadConfig {
    dir: PathBuf,
    max_size: Option<u64>,
    allow_overwrite: bool,
    upload_only: bool,
}


fn handle_request(
    stream: &mut dyn Write,
    req_head: &str,
    body: &[u8],
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(&str, &str)>,
    cert_pem: Option<&[u8]>,
    upload: Option<&UploadConfig>,
) {
    let mut lines = req_head.lines();
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

    if let Some((user, pass)) = auth {
        if !check_basic_auth(&headers, user, pass) {
            let _ = unauthorized(stream);
            return;
        }
    }

    let path_only = raw_path.split('?').next().unwrap_or("/");
    let decoded = percent_decode(path_only);

    if verbose {
        eprintln!("→ {method} {decoded} (body {} bytes)", body.len());
    }

    // Certificate endpoint
    if method == "GET" || method == "HEAD" {
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
    }

    // Upload endpoints
    if let Some(uc) = upload {
        if decoded == "/upload" || decoded == "upload" {
            if method == "GET" || method == "HEAD" {
                let html = upload_form_html();
                let headers = [
                    ("Content-Type", "text/html; charset=utf-8".into()),
                    ("Content-Length", html.len().to_string()),
                    ("Cache-Control", "no-store".into()),
                ];
                if method == "HEAD" {
                    let _ = write_response(stream, 200, "OK", &headers, b"");
                } else {
                    let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
                }
                return;
            }
            if method == "POST" {
                let result = handle_upload(body, &headers, uc, verbose);
                let (ok, msg) = match result {
                    Ok(path) => (true, format!("Saved as {}", path.display())),
                    Err(e) => (false, e),
                };
                let html = upload_result_html(ok, &msg);
                let status = if ok { 201 } else { 400 };
                let reason = if ok { "Created" } else { "Bad Request" };
                let headers = [
                    ("Content-Type", "text/html; charset=utf-8".into()),
                    ("Content-Length", html.len().to_string()),
                    ("Cache-Control", "no-store".into()),
                ];
                let _ = write_response(stream, status, reason, &headers, html.as_bytes());
                return;
            }
        }
    }

    // Methods for download
    if method != "GET" && method != "HEAD" {
        let allow = if upload.is_some() {
            "GET, HEAD, POST"
        } else {
            "GET, HEAD"
        };
        let _ = write_response(
            stream,
            405,
            "Method Not Allowed",
            &[("Allow", allow.into())],
            b"Method Not Allowed",
        );
        return;
    }

    // Upload-only: block downloads of shared content
    if upload.map(|u| u.upload_only).unwrap_or(false) {
        // Still allow the index to point at upload
        if decoded == "/" || decoded.is_empty() || decoded == "." {
            let html = if upload.is_some() {
                format!(
                    r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>http-share</title>
<style>body{{font-family:system-ui,sans-serif;margin:2rem}}</style></head>
<body><h1>Upload only</h1>
<p><a href="/upload">Upload a file…</a></p>
<footer>http-share</footer></body></html>"#
                )
            } else {
                "Not Found".into()
            };
            let headers = [
                ("Content-Type", "text/html; charset=utf-8".into()),
                ("Content-Length", html.len().to_string()),
                ("Cache-Control", "no-store".into()),
            ];
            let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            return;
        }
        let _ = write_response(stream, 404, "Not Found", &[], b"Not Found");
        return;
    }

    let head_only = method == "HEAD";
    let range_header = headers.get("range").map(|s| s.as_str());
    let show_upload = upload.is_some();

    match vfs.resolve(&decoded) {
        Some(Resolved::Index) => {
            let html = listing_html(vfs, "/", None, show_upload);
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
            let html = listing_html(vfs, &virt, Some(&real), show_upload);
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

fn handle_upload(
    body: &[u8],
    headers: &HashMap<String, String>,
    uc: &UploadConfig,
    verbose: bool,
) -> Result<PathBuf, String> {
    if let Some(max) = uc.max_size {
        if body.len() as u64 > max {
            return Err(format!(
                "upload too large ({} bytes, max {} bytes)",
                body.len(),
                max
            ));
        }
    }

    let ct = headers
        .get("content-type")
        .map(|s| s.as_str())
        .unwrap_or("");
    let boundary = multipart_boundary(ct)
        .ok_or_else(|| "expected multipart/form-data with boundary".to_string())?;

    let (raw_name, data) = parse_multipart_file(body, &boundary)?;
    if data.is_empty() {
        return Err("empty file".into());
    }
    if let Some(max) = uc.max_size {
        if data.len() as u64 > max {
            return Err(format!(
                "file too large ({} bytes, max {} bytes)",
                data.len(),
                max
            ));
        }
    }

    let filename = sanitize_filename(&raw_name)
        .ok_or_else(|| format!("invalid filename: {raw_name:?}"))?;
    let dest = unique_dest(&uc.dir, &filename, uc.allow_overwrite);

    // Write via temp then rename for atomicity where possible
    let tmp = uc.dir.join(format!(
        ".upload-{}.tmp",
        random_token(8)
    ));
    {
        let mut f = File::create(&tmp).map_err(|e| format!("create: {e}"))?;
        f.write_all(&data).map_err(|e| format!("write: {e}"))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, &dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename: {e}")
    })?;

    if verbose {
        eprintln!("  uploaded {} ({} bytes)", dest.display(), data.len());
    }
    Ok(dest)
}

fn read_http_message(stream: &mut dyn Read, max_body: u64) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    // Read until header terminator
    loop {
        let n = match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return None,
        };
        buf.extend_from_slice(&tmp[..n]);
        if find_bytes(&buf, b"\r\n\r\n").is_some() {
            break;
        }
        if buf.len() > 64 * 1024 {
            return None; // headers too large
        }
    }
    let header_end = find_bytes(&buf, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut body = buf[header_end + 4..].to_vec();

    // Content-Length
    let mut content_length: Option<usize> = None;
    for line in head.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
    }

    if let Some(cl) = content_length {
        if cl as u64 > max_body {
            // Still try to drain? Just reject with empty body marker — caller checks size
            return Some((head, body)); // body may be partial; handle_upload checks max
        }
        while body.len() < cl {
            let n = match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            body.extend_from_slice(&tmp[..n]);
            if body.len() > max_body as usize {
                break;
            }
        }
        if body.len() > cl {
            body.truncate(cl);
        }
    }
    Some((head, body))
}

fn handle_client_plain(
    mut stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
    upload: Option<UploadConfig>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(300)));

    let max_body = upload
        .as_ref()
        .and_then(|u| u.max_size)
        .unwrap_or(256 * 1024 * 1024); // 256 MiB default cap when unlimited
    let (head, body) = match read_http_message(&mut stream, max_body) {
        Some(x) => x,
        None => return,
    };
    let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
    handle_request(
        &mut stream,
        &head,
        &body,
        vfs,
        verbose,
        auth_ref,
        cert_pem.as_deref(),
        upload.as_ref(),
    );
}

fn handle_client_tls(
    stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
    tls_config: Arc<ServerConfig>,
    upload: Option<UploadConfig>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
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

    let max_body = upload
        .as_ref()
        .and_then(|u| u.max_size)
        .unwrap_or(256 * 1024 * 1024);
    let (head, body) = match read_http_message(&mut tls, max_body) {
        Some(x) => x,
        None => return,
    };
    let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
    handle_request(
        &mut tls,
        &head,
        &body,
        vfs,
        verbose,
        auth_ref,
        cert_pem.as_deref(),
        upload.as_ref(),
    );
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
fn host_for_url(ip: &str) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]")
    } else {
        ip.to_string()
    }
}

fn is_loopback(ip: &str) -> bool {
    ip == "127.0.0.1" || ip == "::1" || ip == "localhost"
}

fn is_ipv6(ip: &str) -> bool {
    ip.contains(':')
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
// Minimal QR code (byte mode, ECC level L) — pure Rust, no extra deps
// Supports versions 1–6 (enough for typical share URLs with auth).
// ---------------------------------------------------------------------------

fn qr_print(text: &str) {
    match qr_encode(text.as_bytes()) {
        Some(matrix) => {
            let n = matrix.len();
            let border = 2;
            println!();
            for y in -(border as isize)..(n as isize + border as isize) {
                print!("  ");
                for x in -(border as isize)..(n as isize + border as isize) {
                    let on = if x >= 0 && y >= 0 && (x as usize) < n && (y as usize) < n {
                        matrix[y as usize][x as usize]
                    } else {
                        false
                    };
                    if on {
                        print!("██");
                    } else {
                        print!("  ");
                    }
                }
                println!();
            }
            println!();
        }
        None => {
            eprintln!("(QR: data too long for built-in encoder)");
        }
    }
}

fn qr_encode(data: &[u8]) -> Option<Vec<Vec<bool>>> {
    const CAP: [usize; 7] = [0, 19, 34, 55, 80, 108, 136];
    const SIZE: [usize; 7] = [0, 21, 25, 29, 33, 37, 41];
    const ECC_CW: [usize; 7] = [0, 7, 10, 15, 20, 26, 36];
    const NBLOCKS: [usize; 7] = [0, 1, 1, 1, 1, 1, 2];

    let need = data.len() + 3;
    let mut version = 0;
    for v in 1..=6 {
        if CAP[v] >= need {
            version = v;
            break;
        }
    }
    if version == 0 {
        return None;
    }

    let size = SIZE[version];
    let data_cw = CAP[version];
    let ecc_cw = ECC_CW[version];
    let nblocks = NBLOCKS[version];

    let mut bits: Vec<bool> = Vec::new();
    for b in [false, true, false, false] {
        bits.push(b);
    }
    let len = data.len() as u16;
    for i in (0..8).rev() {
        bits.push((len >> i) & 1 == 1);
    }
    for &byte in data {
        for i in (0..8).rev() {
            bits.push((byte >> i) & 1 == 1);
        }
    }
    for _ in 0..4 {
        if bits.len() >= data_cw * 8 {
            break;
        }
        bits.push(false);
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut pad = true;
    while bits.len() / 8 < data_cw {
        let p: u8 = if pad { 0xEC } else { 0x11 };
        pad = !pad;
        for i in (0..8).rev() {
            bits.push((p >> i) & 1 == 1);
        }
    }
    bits.truncate(data_cw * 8);

    let data_bytes: Vec<u8> = bits
        .chunks(8)
        .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8))
        .collect();

    let gen = rs_generator(ecc_cw / nblocks.max(1));
    let block_data_len = data_cw / nblocks;
    let block_ecc_len = ecc_cw / nblocks;
    let mut ecc_blocks: Vec<Vec<u8>> = Vec::new();
    for b in 0..nblocks {
        let start = b * block_data_len;
        let end = if b + 1 == nblocks { data_cw } else { start + block_data_len };
        let block = &data_bytes[start..end];
        ecc_blocks.push(rs_encode(block, &gen));
    }

    let mut final_bytes: Vec<u8> = Vec::new();
    let max_d = (data_bytes.len() + nblocks - 1) / nblocks;
    for i in 0..max_d {
        for b in 0..nblocks {
            let start = b * block_data_len;
            let end = if b + 1 == nblocks { data_cw } else { start + block_data_len };
            if i < end - start {
                final_bytes.push(data_bytes[start + i]);
            }
        }
    }
    for i in 0..block_ecc_len {
        for b in 0..nblocks {
            final_bytes.push(ecc_blocks[b][i]);
        }
    }

    let mut matrix = vec![vec![None::<bool>; size]; size];

    place_finder(&mut matrix, 0, 0);
    place_finder(&mut matrix, size - 7, 0);
    place_finder(&mut matrix, 0, size - 7);

    for i in 0..8 {
        if i < size {
            if matrix[7][i].is_none() { matrix[7][i] = Some(false); }
            if matrix[i][7].is_none() { matrix[i][7] = Some(false); }
            if matrix[size - 8][i].is_none() { matrix[size - 8][i] = Some(false); }
            if matrix[i][size - 8].is_none() { matrix[i][size - 8] = Some(false); }
            if matrix[size - 1 - i][7].is_none() { matrix[size - 1 - i][7] = Some(false); }
            if matrix[7][size - 1 - i].is_none() { matrix[7][size - 1 - i] = Some(false); }
        }
    }

    for i in 8..size - 8 {
        if matrix[6][i].is_none() { matrix[6][i] = Some(i % 2 == 0); }
        if matrix[i][6].is_none() { matrix[i][6] = Some(i % 2 == 0); }
    }

    if version >= 2 {
        let positions: &[usize] = match version {
            2 => &[6, 18],
            3 => &[6, 22],
            4 => &[6, 26],
            5 => &[6, 30],
            6 => &[6, 34],
            _ => &[],
        };
        for &r in positions {
            for &c in positions {
                if (r == 6 && c == 6) || (r == 6 && c == size - 7) || (r == size - 7 && c == 6) {
                    continue;
                }
                place_alignment(&mut matrix, r, c);
            }
        }
    }

    matrix[size - 8][8] = Some(true);

    for i in 0..9 {
        if matrix[8][i].is_none() { matrix[8][i] = Some(false); }
        if matrix[i][8].is_none() { matrix[i][8] = Some(false); }
    }
    for i in 0..8 {
        if matrix[8][size - 1 - i].is_none() { matrix[8][size - 1 - i] = Some(false); }
        if matrix[size - 1 - i][8].is_none() { matrix[size - 1 - i][8] = Some(false); }
    }

    let mut bit_idx = 0;
    let total_bits = final_bytes.len() * 8;
    let mut col = size as isize - 1;
    let mut upward = true;
    while col > 0 {
        if col == 6 { col -= 1; }
        let row_range: Vec<isize> = if upward {
            (0..size as isize).rev().collect()
        } else {
            (0..size as isize).collect()
        };
        for row in row_range {
            for dc in [0, -1] {
                let c = col + dc;
                if c < 0 || c >= size as isize { continue; }
                if matrix[row as usize][c as usize].is_some() { continue; }
                let bit = if bit_idx < total_bits {
                    let byte = final_bytes[bit_idx / 8];
                    let b = (byte >> (7 - (bit_idx % 8))) & 1 == 1;
                    bit_idx += 1;
                    b
                } else {
                    false
                };
                let mask = (row + c) % 2 == 0;
                matrix[row as usize][c as usize] = Some(bit ^ mask);
            }
        }
        upward = !upward;
        col -= 2;
    }

    let format: u16 = 0b111011111000100;
    for i in 0..6 {
        matrix[8][i] = Some((format >> (14 - i)) & 1 == 1);
    }
    matrix[8][7] = Some((format >> 8) & 1 == 1);
    matrix[8][8] = Some((format >> 7) & 1 == 1);
    matrix[7][8] = Some((format >> 6) & 1 == 1);
    for i in 0..6 {
        matrix[5 - i][8] = Some((format >> (5 - i)) & 1 == 1);
    }
    for i in 0..7 {
        matrix[size - 1 - i][8] = Some((format >> (14 - i)) & 1 == 1);
    }
    for i in 0..8 {
        matrix[8][size - 8 + i] = Some((format >> (7 - i)) & 1 == 1);
    }

    Some(
        matrix
            .into_iter()
            .map(|row| row.into_iter().map(|c| c.unwrap_or(false)).collect())
            .collect(),
    )
}

fn place_finder(m: &mut [Vec<Option<bool>>], row: usize, col: usize) {
    for dr in 0..7 {
        for dc in 0..7 {
            let on = dr == 0 || dr == 6 || dc == 0 || dc == 6
                || (dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4);
            m[row + dr][col + dc] = Some(on);
        }
    }
}

fn place_alignment(m: &mut [Vec<Option<bool>>], cx: usize, cy: usize) {
    for dr in -2isize..=2 {
        for dc in -2isize..=2 {
            let r = (cx as isize + dr) as usize;
            let c = (cy as isize + dc) as usize;
            let on = dr.abs() == 2 || dc.abs() == 2 || (dr == 0 && dc == 0);
            m[r][c] = Some(on);
        }
    }
}

fn rs_generator(nsym: usize) -> Vec<u8> {
    let mut g = vec![1u8];
    for i in 0..nsym {
        let mut ng = vec![0u8; g.len() + 1];
        let alpha = gf_pow(2, i as u32);
        for (j, &c) in g.iter().enumerate() {
            ng[j] ^= c;
            ng[j + 1] ^= gf_mul_correct(c, alpha);
        }
        g = ng;
    }
    g
}

fn gf_pow(mut base: u8, mut exp: u32) -> u8 {
    let mut r = 1u8;
    while exp > 0 {
        if exp & 1 != 0 {
            r = gf_mul_correct(r, base);
        }
        base = gf_mul_correct(base, base);
        exp >>= 1;
    }
    r
}

fn gf_mul_correct(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = (a & 0x80) != 0;
        a <<= 1;
        if hi {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    p
}

fn rs_encode(data: &[u8], gen: &[u8]) -> Vec<u8> {
    let nsym = gen.len() - 1;
    let mut res = vec![0u8; data.len() + nsym];
    res[..data.len()].copy_from_slice(data);
    for i in 0..data.len() {
        let coef = res[i];
        if coef != 0 {
            for j in 0..gen.len() {
                res[i + j] ^= gf_mul_correct(gen[j], coef);
            }
        }
    }
    res[data.len()..].to_vec()
}

static CTRL_C_RUNNING: AtomicBool = AtomicBool::new(true);

#[cfg(unix)]
extern "C" fn sigint_handler(_: i32) {
    CTRL_C_RUNNING.store(false, Ordering::SeqCst);
}

fn install_ctrlc_handler() {
    #[cfg(unix)]
    {
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        unsafe {
            signal(2, sigint_handler as usize); // SIGINT
            signal(15, sigint_handler as usize); // SIGTERM
        }
    }
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
        if let Some(ref d) = args.incoming {
            eprintln!("  incoming uploads → {}", d.display());
        }
    }

    let upload_cfg: Option<UploadConfig> = args.incoming.as_ref().map(|dir| UploadConfig {
        dir: dir.clone(),
        max_size: args.max_upload_size,
        allow_overwrite: args.allow_overwrite,
        upload_only: args.upload_only,
    });

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
    // Support IPv6 bind addresses such as "::" or "[::]"
    let bind_host = args.bind.trim_matches(|c| c == '[' || c == ']');
    let addr = if is_ipv6(bind_host) {
        format!("[{bind_host}]:{}", args.port)
    } else {
        format!("{bind_host}:{}", args.port)
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("http-share listening on {scheme}://{addr}/");

    let bind_all_v4 = bind_host == "0.0.0.0";
    let bind_all_v6 = bind_host == "::";
    let mut primary_url: Option<String> = None;

    for ip in local_ips() {
        let show = if bind_all_v4 {
            !is_ipv6(&ip)
        } else if bind_all_v6 {
            is_ipv6(&ip) || ip == "::1"
        } else {
            ip == bind_host || (bind_host == "127.0.0.1" && ip == "127.0.0.1")
        };
        if !show {
            continue;
        }
        let host = host_for_url(&ip);
        let url = if let Some((ref u, ref p)) = auth {
            format!(
                "{scheme}://{}:{}@{host}:{}/",
                url_encode_component(u),
                url_encode_component(p),
                args.port
            )
        } else {
            format!("{scheme}://{host}:{}/", args.port)
        };
        if primary_url.is_none() && !is_loopback(&ip) {
            primary_url = Some(url.clone());
        }
        println!("  {url}");
    }
    if primary_url.is_none() {
        let fallback_ip = if bind_all_v6 || is_ipv6(bind_host) {
            "::1"
        } else {
            "127.0.0.1"
        };
        let host = host_for_url(fallback_ip);
        primary_url = Some(if let Some((ref u, ref p)) = auth {
            format!(
                "{scheme}://{}:{}@{host}:{}/",
                url_encode_component(u),
                url_encode_component(p),
                args.port
            )
        } else {
            format!("{scheme}://{host}:{}/", args.port)
        });
        if !bind_all_v4 && !bind_all_v6 {
            println!("  {}", primary_url.as_ref().unwrap());
        }
    }
    if auth.is_some() {
        println!("  authentication: HTTP Basic Auth (credentials embedded in URLs above)");
    } else {
        println!("  authentication: disabled (--public)");
    }
    if args.https {
        println!("  certificate available at {scheme}://…/certificate.pem");
    }
    if let Some(ref uc) = upload_cfg {
        println!("  uploads: {} → {}", 
            if uc.upload_only { "only" } else { "enabled" },
            uc.dir.display());
        if let Some(max) = uc.max_size {
            println!("  max upload size: {max} bytes");
        }
        println!("  upload form: {scheme}://…/upload");
    }
    if args.open {
        if let Some(ref url) = primary_url {
            open_browser(url);
        }
    }
    if args.qr {
        if let Some(ref url) = primary_url {
            println!("QR code for primary URL:");
            qr_print(url);
        }
    }
    println!("Press Ctrl+C to stop.");

    install_ctrlc_handler();
    CTRL_C_RUNNING.store(true, Ordering::SeqCst);

    if let Err(e) = listener.set_nonblocking(true) {
        if args.verbose {
            eprintln!("warning: could not set non-blocking accept: {e}");
        }
    }

    while CTRL_C_RUNNING.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let vfs = Arc::clone(&vfs);
                let verbose = args.verbose;
                let auth = auth.clone();
                let cert_pem = cert_pem.clone();
                let tls_config = tls_config.clone();
                let upload = upload_cfg.clone();
                thread::spawn(move || {
                    if let Some(cfg) = tls_config {
                        handle_client_tls(stream, &vfs, verbose, auth, cert_pem, cfg, upload);
                    } else {
                        handle_client_plain(stream, &vfs, verbose, auth, cert_pem, upload);
                    }
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                if args.verbose {
                    eprintln!("accept error: {e}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    println!("Shutting down.");
}
