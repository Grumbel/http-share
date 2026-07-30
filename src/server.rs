//! Request handling and per-connection workers.

use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;

use crate::auth::{
    auth_query_suffix, auth_set_cookie, check_auth, query_credentials_match, unauthorized,
};
use crate::html::{landing_html, listing_html, upload_form_html, upload_result_html};
use crate::http_io::{html_headers, read_http_message, serve_file, write_response};
use crate::state::{LifetimeState, TransferStats};
use crate::upload::{handle_upload, UploadConfig};
use crate::util::{encode_path_component, percent_decode, parse_query};
use crate::vfs::{Resolved, Vfs};

pub(crate) fn handle_request(
    stream: &mut dyn Write,
    req_head: &str,
    body: &[u8],
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(&str, &str)>,
    cert_pem: Option<&[u8]>,
    upload: Option<&UploadConfig>,
    lifetime: Option<&LifetimeState>,
    stats: Option<&TransferStats>,
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

    let path_only = raw_path.split('?').next().unwrap_or("/");
    let query_str = raw_path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let query = parse_query(query_str);
    let decoded = percent_decode(path_only);

    // When the client used query-auth, keep credentials on HTML links and set a
    // session cookie so subsequent navigations (and POSTs) work without re-auth.
    let mut auth_q = String::new();
    let mut set_cookie: Option<String> = None;
    if let Some((user, pass)) = auth {
        if !check_auth(&headers, &query, user, pass) {
            let _ = unauthorized(stream);
            return;
        }
        if query_credentials_match(&query, user, pass) {
            auth_q = auth_query_suffix(user, pass);
            set_cookie = Some(auth_set_cookie(user, pass));
        }
    }

    if verbose {
        eprintln!("→ {method} {decoded} (body {} bytes)", body.len());
    }

    let show_cert = cert_pem.is_some();

    // Certificate endpoint
    if method == "GET" || method == "HEAD" {
        if decoded == "/certificate.pem" || decoded == "certificate.pem" {
            if let Some(pem) = cert_pem {
                let mut headers = vec![
                    ("Content-Type", "application/x-pem-file".into()),
                    ("Content-Length", pem.len().to_string()),
                    ("Content-Disposition", "attachment; filename=\"certificate.pem\"".into()),
                    ("Cache-Control", "no-store".into()),
                ];
                if let Some(c) = &set_cookie {
                    headers.push(("Set-Cookie", c.clone()));
                }
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
                let html = upload_form_html(&auth_q);
                let headers = html_headers(html.len(), &set_cookie);
                if method == "HEAD" {
                    let _ = write_response(stream, 200, "OK", &headers, b"");
                } else {
                    let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
                }
                return;
            }
            if method == "POST" {
                let result = handle_upload(body, &headers, uc, verbose);
                let (ok, msg, browse_href) = match result {
                    Ok((path, nbytes)) => {
                        if let Some(lt) = lifetime {
                            lt.record_upload();
                        }
                        if let Some(st) = stats {
                            st.record_upload(nbytes);
                        }
                        if verbose {
                            eprintln!("  upload {} ({} bytes)", path.display(), nbytes);
                        }
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let href = format!("/incoming/{}", encode_path_component(&name));
                        (
                            true,
                            format!("Saved as {name} ({} bytes)", nbytes),
                            Some(href),
                        )
                    }
                    Err(e) => (false, e, None),
                };
                let html = upload_result_html(ok, &msg, browse_href.as_deref(), &auth_q);
                let status = if ok { 201 } else { 400 };
                let reason = if ok { "Created" } else { "Bad Request" };
                let headers = html_headers(html.len(), &set_cookie);
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

    // Upload-only: block /pub/ downloads; still allow / and /incoming
    if upload.map(|u| u.upload_only).unwrap_or(false) {
        let path_trim = decoded.trim_start_matches('/');
        let is_pub = path_trim == "pub" || path_trim.starts_with("pub/");
        if is_pub {
            let _ = write_response(stream, 404, "Not Found", &[], b"Not Found");
            return;
        }
    }

    let head_only = method == "HEAD";
    let range_header = headers.get("range").map(|s| s.as_str());
    let show_upload = upload.is_some();
    let has_shared = !vfs.files.is_empty()
        || vfs.dirs.keys().any(|k| k != "incoming");
    let has_incoming = vfs.dirs.contains_key("incoming");

    match vfs.resolve(&decoded) {
        Some(Resolved::Index) => {
            let html = landing_html(show_upload, show_cert, has_shared, has_incoming, &auth_q);
            let headers = html_headers(html.len(), &set_cookie);
            if head_only {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
        }
        Some(Resolved::PubIndex) => {
            if upload.map(|u| u.upload_only).unwrap_or(false) {
                let _ = write_response(stream, 404, "Not Found", &[], b"Not Found");
                return;
            }
            let html = listing_html(vfs, "pub", None, show_upload, show_cert, &auth_q);
            let headers = html_headers(html.len(), &set_cookie);
            if head_only {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
        }
        Some(Resolved::File(path)) => {
            match serve_file(stream, &path, range_header, head_only, &set_cookie) {
                Ok(nbytes) => {
                    // Count successful GET of a real file (not HEAD, not listings)
                    if !head_only {
                        if let Some(lt) = lifetime {
                            lt.record_download();
                        }
                        if let Some(st) = stats {
                            st.record_download(nbytes);
                        }
                        if verbose {
                            eprintln!("  served {} ({} bytes)", path.display(), nbytes);
                        }
                    }
                }
                Err(e) => {
                    if verbose {
                        eprintln!("  error serving {}: {e}", path.display());
                    }
                }
            }
        }
        Some(Resolved::Dir(real, virt)) => {
            let html = listing_html(vfs, &virt, Some(&real), show_upload, show_cert, &auth_q);
            let headers = html_headers(html.len(), &set_cookie);
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

pub(crate) fn handle_client_plain(
    mut stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
    upload: Option<UploadConfig>,
    lifetime: Option<Arc<LifetimeState>>,
    stats: Arc<TransferStats>,
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
        lifetime.as_deref(),
        Some(stats.as_ref()),
    );
}

pub(crate) fn handle_client_tls(
    stream: TcpStream,
    vfs: &Vfs,
    verbose: bool,
    auth: Option<(String, String)>,
    cert_pem: Option<Vec<u8>>,
    tls_config: Arc<ServerConfig>,
    upload: Option<UploadConfig>,
    lifetime: Option<Arc<LifetimeState>>,
    stats: Arc<TransferStats>,
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
        lifetime.as_deref(),
        Some(stats.as_ref()),
    );
}

// ---------------------------------------------------------------------------
// Certificate management
// ---------------------------------------------------------------------------
