// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! CLI argument parsing (no clap).

use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const VERSION: &str = env!("HTTP_SHARE_VERSION");

pub(crate) struct Args {
    pub(crate) shares: Vec<crate::vfs::ShareSpec>,
    pub(crate) port: u16,
    pub(crate) bind: String,
    pub(crate) verbose: bool,
    /// Print the virtual filesystem tree after it is built.
    pub(crate) tree: bool,
    pub(crate) follow_symlinks: bool,
    pub(crate) public: bool,
    pub(crate) open: bool,
    pub(crate) qr: bool,
    pub(crate) rq: bool,
    pub(crate) https: bool,
    pub(crate) dynamic_cert: bool,
    pub(crate) regenerate_cert: bool,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) incoming: Option<PathBuf>,
    pub(crate) upload_only: bool,
    pub(crate) max_upload_size: Option<u64>,
    pub(crate) allow_overwrite: bool,
    /// When true (default), mount --incoming as virtual /incoming/ for browsing.
    pub(crate) browse_uploads: bool,
    pub(crate) one_shot: bool,
    pub(crate) expire: Option<Duration>,
    pub(crate) max_downloads: Option<u64>,
    pub(crate) max_uploads: Option<u64>,
}

pub(crate) fn print_usage(program: &str) {
    eprintln!(
        "Usage: {program} [OPTIONS] <PATH>...

Share only the files and directories you explicitly list.
Never exposes the current working directory implicitly.

Shared files are served at the site root. Uploads go to /incoming/.

Share selection (rsync-like):
  PATH                 Share PATH as /basename
  PATH/                Share *contents* of directory PATH at the root
  --map PATH VIRT      Expose PATH as /VIRT (repeatable; VIRT may be a/b/c)

Network:
  -p, --port PORT          Port to listen on (default: 8000)
      --bind ADDRESS       Address to bind (default: 0.0.0.0)
  -v, --verbose            Verbose logging
      --tree               Print the virtual filesystem tree (can be noisy)
      --open               Open primary share URL in the default browser
      --qr                 Print a terminal QR code (query-auth URL when auth is on)
      --rq                 Like --qr but with reversed QR colors

Share selection:
      --map PATH VIRT      Expose PATH as /VIRT (repeatable; deep paths ok)
      --follow-symlinks    Follow symbolic links when sharing paths

Authentication:
      --public             Disable authentication
      --user USER          Username for Basic Auth / query auth
      --password PASS      Password for Basic Auth / query auth
      --random-password    Generate random credentials (default when not --public)

TLS:
      --https              Serve over HTTPS with a self-signed certificate
      --http               Serve plain HTTP (default)
      --dynamic-cert       Use an ephemeral certificate (not stored)
      --regenerate-cert    Replace the persistent self-signed certificate

Uploads:
      --incoming DIR       Accept uploads into DIR (browsable at /incoming/)
      --upload-only        Only accept uploads (no shared-path downloads)
      --max-upload-size N  Max upload size (e.g. 10M, 1G; default unlimited)
      --allow-overwrite    Allow uploaded files to replace existing ones
      --no-browse-uploads  Do not expose uploaded files under /incoming/

Lifetime:
      --one-shot           Stop after the first successful download or upload
      --expire DURATION    Stop after DURATION (e.g. 30s, 5m, 1h)
      --max-downloads N    Stop after N successful file downloads
      --max-uploads N      Stop after N successful uploads

  -V, --version            Print version and exit
  -h, --help               Print help

URL layout:
  /                  Shared files (CLI paths) and navigation links
  /<name>            Shared file or directory
  /incoming/         Uploaded files (when --incoming, unless --no-browse-uploads)
  /upload            Upload form (when --incoming)
  /message           POST a short text note to the host (also form on pages)
  /certificate.pem   HTTPS server certificate (when --https)

Auth: HTTP Basic Auth, or query parameters ?user=…&password=… (or ?u=…&p=…)
for QR scanners that do not support userinfo in URLs. --qr uses the query form.
"
    );
}

pub(crate) fn parse_args() -> Args {
    let mut args: Vec<String> = env::args().collect();
    let program = args.first().cloned().unwrap_or_else(|| "http-share".into());
    args.remove(0);

    let mut shares: Vec<crate::vfs::ShareSpec> = Vec::new();
    let mut port: u16 = 8000;
    let mut bind = "0.0.0.0".to_string();
    let mut verbose = false;
    let mut tree = false;
    let mut follow_symlinks = false;
    let mut public = false;
    let mut open = false;
    let mut qr = false;
    let mut rq = false;
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
    let mut browse_uploads = true;
    let mut one_shot = false;
    let mut expire: Option<Duration> = None;
    let mut max_downloads: Option<u64> = None;
    let mut max_uploads: Option<u64> = None;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-V" | "--version" => {
                println!("http-share {VERSION}");
                std::process::exit(0);
            }
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
            "--tree" => tree = true,
            "--follow-symlinks" => follow_symlinks = true,
            "--map" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --map requires PATH and VIRT");
                    std::process::exit(1);
                }
                let path = args[i].clone();
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --map requires PATH and VIRT");
                    std::process::exit(1);
                }
                let virt = args[i].clone();
                match crate::vfs::ShareSpec::map(&path, &virt) {
                    Ok(s) => shares.push(s),
                    Err(e) => {
                        eprintln!("error: --map: {e}");
                        std::process::exit(1);
                    }
                }
            },
            "--public" => public = true,
            "--open" => open = true,
            "--qr" => qr = true,
            "--rq" => rq = true,
            "--https" => https = true,
            "--http" => https = false,
            "--dynamic-cert" => dynamic_cert = true,
            "--regenerate-cert" => regenerate_cert = true,
            "--random-password" => random_password = true,
            "--upload-only" => upload_only = true,
            "--allow-overwrite" => allow_overwrite = true,
            "--no-browse-uploads" => browse_uploads = false,
            "--one-shot" => one_shot = true,
            "--expire" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --expire requires a duration (e.g. 30s, 5m, 1h)");
                    std::process::exit(1);
                }
                expire = Some(parse_duration(&args[i]).unwrap_or_else(|e| {
                    eprintln!("error: invalid --expire: {e}");
                    std::process::exit(1);
                }));
            }
            "--max-downloads" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --max-downloads requires a number");
                    std::process::exit(1);
                }
                max_downloads = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: invalid --max-downloads");
                    std::process::exit(1);
                }));
            }
            "--max-uploads" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --max-uploads requires a number");
                    std::process::exit(1);
                }
                max_uploads = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: invalid --max-uploads");
                    std::process::exit(1);
                }));
            }
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
            _ => match crate::vfs::ShareSpec::parse(a) {
                Ok(s) => shares.push(s),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            },
        }
        i += 1;
    }

    if shares.is_empty() && incoming.is_none() {
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
        shares,
        port,
        bind,
        verbose,
        tree,
        follow_symlinks,
        public,
        open,
        qr,
        rq,
        https,
        dynamic_cert,
        regenerate_cert,
        user,
        password,
        incoming,
        upload_only,
        max_upload_size,
        allow_overwrite,
        browse_uploads,
        one_shot,
        expire,
        max_downloads,
        max_uploads,
    }
}

pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    // bare number = seconds
    if let Ok(n) = s.parse::<u64>() {
        return Ok(Duration::from_secs(n));
    }
    let (num_str, mult) = match s.as_bytes().last() {
        Some(b's') => (&s[..s.len() - 1], 1u64),
        Some(b'm') => (&s[..s.len() - 1], 60u64),
        Some(b'h') => (&s[..s.len() - 1], 3600u64),
        Some(b'd') => (&s[..s.len() - 1], 86400u64),
        _ => return Err(format!("use Ns, Nm, Nh, or Nd (got {s})")),
    };
    let n: u64 = num_str.trim().parse().map_err(|_| format!("not a number: {num_str}"))?;
    let secs = n.checked_mul(mult).ok_or_else(|| "duration overflow".to_string())?;
    if secs == 0 {
        return Err("duration must be > 0".into());
    }
    Ok(Duration::from_secs(secs))
}

pub(crate) fn parse_size(s: &str) -> Result<u64, String> {
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

pub(crate) fn random_token(len: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(2 * 86400));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("nope").is_err());
    }

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("10K").unwrap(), 10 * 1024);
        assert_eq!(parse_size("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_size("").is_err());
        assert!(parse_size("xx").is_err());
    }
}
