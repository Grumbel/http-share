// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

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
use crate::html::{error_html, listing_html, message_result_html, upload_form_html, upload_result_html};
use crate::http_io::{html_headers, read_http_message, serve_file, write_response};
use crate::state::{LifetimeState, TransferStats};
use crate::upload::{handle_upload, UploadConfig};
use crate::util::{encode_path_component, percent_decode, parse_query};
use crate::vfs::{Resolved, Vfs};


fn send_error(
    stream: &mut dyn Write,
    status: u16,
    reason: &str,
    detail: &str,
    auth_q: &str,
    set_cookie: &Option<String>,
    extra_headers: &[(&str, String)],
) {
    let html = error_html(status, reason, detail, auth_q);
    let mut headers = html_headers(html.len(), set_cookie);
    for (k, v) in extra_headers {
        headers.push((k, v.clone()));
    }
    let _ = write_response(stream, status, reason, &headers, html.as_bytes());
}

/// Extract `message` from an `application/x-www-form-urlencoded` body.
fn form_message_field(body: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    let map = parse_query(s);
    map.get("message").map(|m| m.trim().to_string())
}

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
            set_cookie = Some(auth_set_cookie(user, pass, cert_pem.is_some()));
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

    // Human-readable message to the host process (always available)
    if decoded == "/message" || decoded == "message" {
        if method == "POST" {
            const MAX_MSG: usize = 500;
            let raw = form_message_field(body).unwrap_or_default();
            if raw.is_empty() {
                let html = message_result_html(false, "Empty message — nothing was sent.", &auth_q);
                let headers = html_headers(html.len(), &set_cookie);
                let _ = write_response(stream, 400, "Bad Request", &headers, html.as_bytes());
                return;
            }
            if raw.chars().count() > MAX_MSG {
                let html = message_result_html(
                    false,
                    &format!("Message too long (max {MAX_MSG} characters)."),
                    &auth_q,
                );
                let headers = html_headers(html.len(), &set_cookie);
                let _ = write_response(stream, 400, "Bad Request", &headers, html.as_bytes());
                return;
            }
            // Always surface messages — that is the point of the feature
            eprintln!("[message] {raw}");
            let html = message_result_html(true, "Message delivered to the host.", &auth_q);
            let headers = html_headers(html.len(), &set_cookie);
            let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            return;
        }
        if method == "GET" || method == "HEAD" {
            // Redirect-like: show a tiny page pointing back home (form lives on listings)
            let html = message_result_html(
                false,
                "Use the message form on the directory pages to send a note to the host.",
                &auth_q,
            );
            let headers = html_headers(html.len(), &set_cookie);
            if method == "HEAD" {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
            return;
        }
    }

    // Methods for download / browse
    if method != "GET" && method != "HEAD" {
        let allow = "GET, HEAD, POST";
        send_error(
            stream,
            405,
            "Method Not Allowed",
            &format!("Method {method} is not allowed for this path. Allowed: {allow}."),
            &auth_q,
            &set_cookie,
            &[("Allow", allow.into())],
        );
        return;
    }

    // Upload-only: block shared-path downloads; still allow / and /incoming
    if upload.map(|u| u.upload_only).unwrap_or(false) {
        let path_trim = decoded.trim_start_matches('/');
        let is_incoming = path_trim == "incoming" || path_trim.starts_with("incoming/");
        let is_root = path_trim.is_empty() || path_trim == ".";
        // Allow root (landing), incoming, and special endpoints handled above
        if !is_root && !is_incoming {
            send_error(
                stream,
                404,
                "Not Found",
                &format!(
                    "Path {decoded} is not available in upload-only mode (shared downloads disabled)."
                ),
                &auth_q,
                &set_cookie,
                &[],
            );
            return;
        }
    }

    let head_only = method == "HEAD";
    let range_header = headers.get("range").map(|s| s.as_str());
    let show_upload = upload.is_some();

    match vfs.resolve(&decoded) {
        Some(Resolved::Index) => {
            // Root lists shared CLI paths (and nav links to incoming/upload/cert)
            let html = listing_html(vfs, "", show_upload, show_cert, &auth_q);
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
                    eprintln!("  error serving {}: {e}", path.display());
                    send_error(
                        stream,
                        500,
                        "Internal Server Error",
                        &format!("Failed to read {}: {e}", path.display()),
                        &auth_q,
                        &set_cookie,
                        &[],
                    );
                }
            }
        }
        Some(Resolved::Dir(_, ref virt)) | Some(Resolved::VirtualDir(ref virt)) => {
            let html = listing_html(vfs, &virt, show_upload, show_cert, &auth_q);
            let headers = html_headers(html.len(), &set_cookie);
            if head_only {
                let _ = write_response(stream, 200, "OK", &headers, b"");
            } else {
                let _ = write_response(stream, 200, "OK", &headers, html.as_bytes());
            }
        }
        None => {
            if verbose {
                eprintln!("  404 {decoded}");
            }
            send_error(
                stream,
                404,
                "Not Found",
                &format!("No shared file or directory at {decoded}."),
                &auth_q,
                &set_cookie,
                &[],
            );
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
            eprintln!("tls session error: {e}");
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

