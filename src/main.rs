//! http-share — minimal HTTP(S) file sharing utility for ad-hoc transfers.
//!
//! Only files and directories given on the command line are exposed.
//! FTP-style URL layout: shared paths under `/pub/`, uploads under `/incoming/`,
//! landing page at `/`.

mod auth;
mod cli;
mod html;
mod http_io;
mod net;
mod qr;
mod server;
mod state;
mod tls;
mod upload;
mod util;
mod vfs;

use std::env;
use std::io;
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cli::parse_args;
use net::{host_for_url, is_ipv6, is_loopback, local_ips, open_browser};
use qr::qr_print;
use server::{handle_client_plain, handle_client_tls};
use state::{LifetimeState, TransferStats};
use rustls::ServerConfig;
use tls::{load_or_create_cert, make_tls_config};
use upload::UploadConfig;
use vfs::Vfs;

use auth::build_share_url;
use util::url_encode_component;

use state::CTRL_C_RUNNING;

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

    let mut vfs = match Vfs::from_paths(&args.paths, args.follow_symlinks) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // Mount --incoming as virtual /incoming/ so uploads are browsable (unless disabled).
    if let Some(ref dir) = args.incoming {
        if args.browse_uploads {
            let path = if args.follow_symlinks {
                dir.canonicalize().unwrap_or_else(|_| dir.clone())
            } else if dir.is_absolute() {
                dir.clone()
            } else {
                env::current_dir()
                    .map(|c| c.join(dir))
                    .unwrap_or_else(|_| dir.clone())
            };
            if let Err(e) = vfs.add_dir("incoming", path) {
                eprintln!("error: cannot mount /incoming/: {e}");
                eprintln!("  tip: avoid sharing a path whose basename is 'incoming', or pass --no-browse-uploads");
                std::process::exit(1);
            }
        }
    }

    let vfs = Arc::new(vfs);

    if args.verbose {
        eprintln!(
            "sharing {} file(s), {} directory(ies)",
            vfs.files.len(),
            vfs.dirs.len()
        );
        for (n, p) in &vfs.files {
            eprintln!("  file  /pub/{n} → {}", p.display());
        }
        for (n, p) in &vfs.dirs {
            if n == "incoming" {
                eprintln!("  dir   /{n}/ → {}", p.display());
            } else {
                eprintln!("  dir   /pub/{n}/ → {}", p.display());
            }
        }
        if let Some(ref d) = args.incoming {
            eprintln!(
                "  incoming uploads → {} (browse={})",
                d.display(),
                args.browse_uploads
            );
        }
    }

    let upload_cfg: Option<UploadConfig> = args.incoming.as_ref().map(|dir| UploadConfig {
        dir: dir.clone(),
        max_size: args.max_upload_size,
        allow_overwrite: args.allow_overwrite,
        upload_only: args.upload_only,
    });

    let lifetime = if args.one_shot
        || args.expire.is_some()
        || args.max_downloads.is_some()
        || args.max_uploads.is_some()
    {
        Some(Arc::new(LifetimeState::new(
            args.one_shot,
            args.expire,
            args.max_downloads,
            args.max_uploads,
        )))
    } else {
        None
    };

    let stats = Arc::new(TransferStats::new());

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
        let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
        let url = build_share_url(scheme, &host, args.port, auth_ref, false);
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
        let auth_ref = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
        primary_url = Some(build_share_url(scheme, &host, args.port, auth_ref, false));
        if !bind_all_v4 && !bind_all_v6 {
            println!("  {}", primary_url.as_ref().unwrap());
        }
    }
    if auth.is_some() {
        println!("  authentication: HTTP Basic Auth (credentials in URLs above)");
        println!("  QR / mobile tip: use query form ?user=…&password=… (see --qr)");
    } else {
        println!("  authentication: disabled (--public)");
    }
    if args.https {
        println!("  certificate: {scheme}://…/certificate.pem (also linked from directory pages)");
    }
    if let Some(ref uc) = upload_cfg {
        println!(
            "  uploads: {} → {}",
            if uc.upload_only { "only" } else { "enabled" },
            uc.dir.display()
        );
        if args.browse_uploads {
            println!("  uploaded files: {scheme}://…/incoming/");
        } else {
            println!("  uploaded files: not browsable (--no-browse-uploads)");
        }
        if let Some(max) = uc.max_size {
            println!("  max upload size: {max} bytes");
        }
        println!("  upload form: {scheme}://…/upload");
    }
    if let Some(ref lt) = lifetime {
        if lt.one_shot {
            println!("  lifetime: one-shot (stop after first successful transfer)");
        }
        if let Some(d) = lt.expire {
            println!("  lifetime: expire after {d:?}");
        }
        if let Some(n) = lt.max_downloads {
            println!("  lifetime: max-downloads {n}");
        }
        if let Some(n) = lt.max_uploads {
            println!("  lifetime: max-uploads {n}");
        }
    }
    if args.open {
        if let Some(ref url) = primary_url {
            open_browser(url);
        }
    }
    if args.qr {
        // Prefer query-parameter credentials for QR: many Android scanners do not
        // preserve userinfo (user:pass@host) when opening the URL.
        let qr_url = if let Some(ref url) = primary_url {
            if let Some((ref u, ref p)) = auth {
                if let Some(rest) = url.split("://").nth(1) {
                    let after_at = rest.split('@').last().unwrap_or(rest);
                    let hostport = after_at.trim_end_matches('/');
                    format!(
                        "{scheme}://{hostport}/?user={}&password={}",
                        url_encode_component(u),
                        url_encode_component(p)
                    )
                } else {
                    url.clone()
                }
            } else {
                url.clone()
            }
        } else {
            String::new()
        };
        if !qr_url.is_empty() {
            println!("QR code (query-auth URL, Android-friendly):");
            println!("  {qr_url}");
            qr_print(&qr_url);
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
        if let Some(ref lt) = lifetime {
            if lt.should_stop() {
                CTRL_C_RUNNING.store(false, Ordering::SeqCst);
                break;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let vfs = Arc::clone(&vfs);
                let verbose = args.verbose;
                let auth = auth.clone();
                let cert_pem = cert_pem.clone();
                let tls_config = tls_config.clone();
                let upload = upload_cfg.clone();
                let lifetime = lifetime.clone();
                let stats = Arc::clone(&stats);
                thread::spawn(move || {
                    if let Some(cfg) = tls_config {
                        handle_client_tls(stream, &vfs, verbose, auth, cert_pem, cfg, upload, lifetime, stats);
                    } else {
                        handle_client_plain(stream, &vfs, verbose, auth, cert_pem, upload, lifetime, stats);
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
    if let Some(ref lt) = lifetime {
        if let Some(reason) = lt.stop_reason() {
            println!("Shutting down ({reason}).");
        } else {
            println!("Shutting down.");
        }
    } else {
        println!("Shutting down.");
    }
    println!("  {}", stats.summary_line());
}
