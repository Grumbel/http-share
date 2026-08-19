// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! HTML directory listings, error pages, message form, and upload form.


use crate::auth::with_auth_query;
use crate::cli::VERSION;
use crate::util::{encode_path_component, format_bytes, html_escape};
use crate::vfs::Vfs;


fn site_footer(auth_q: &str) -> String {
    let action = with_auth_query("/message", auth_q);
    format!(
        r#"
<form class="msg-form" method="POST" action="{action}">
  <label for="msg">Message to host</label>
  <div class="msg-row">
    <input id="msg" type="text" name="message" maxlength="500" placeholder="Optional note for the person running http-share…">
    <button type="submit">Send</button>
  </div>
</form>
<footer>http-share {ver}</footer>
"#,
        action = action,
        ver = VERSION,
    )
}

const SHARED_STYLE: &str = r#"
  body { font-family: system-ui, sans-serif; margin: 2rem; max-width: 48rem; color: #222; }
  h1 { font-size: 1.2rem; margin-bottom: 1rem; }
  a { color: #06c; }
  .nav { margin: 1.25rem 0; font-size: 0.95rem; }
  .nav a { margin-right: 1rem; }
  footer { margin-top: 2rem; font-size: 0.85rem; color: #888; }
  .msg-form { margin-top: 2rem; padding-top: 1rem; border-top: 1px solid #eee; }
  .msg-form label { display: block; font-size: 0.9rem; color: #555; margin-bottom: 0.35rem; }
  .msg-row { display: flex; gap: 0.5rem; flex-wrap: wrap; }
  .msg-row input[type=text] { flex: 1; min-width: 12rem; padding: 0.4rem 0.5rem; font-size: 0.95rem; }
  .msg-row button { padding: 0.4rem 0.9rem; font-size: 0.95rem; cursor: pointer; }
  .msg { margin-top: 1rem; padding: 0.75rem; background: #f0f7ff; border-radius: 4px; }
  .err { background: #fff0f0; }
"#;

/// Simple HTML error page (404 / 405 / 400) with path and reason.
pub(crate) fn error_html(status: u16, reason: &str, detail: &str, auth_q: &str) -> String {
    let home = with_auth_query("/", auth_q);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{status} {reason} — http-share</title>
<style>{style}</style>
</head>
<body>
<h1>{status} {reason}</h1>
<p>{detail}</p>
<p class="nav"><a href="{home}">← Home</a></p>
<footer>http-share {ver}</footer>
</body>
</html>"#,
        status = status,
        reason = html_escape(reason),
        detail = html_escape(detail),
        home = home,
        style = SHARED_STYLE,
        ver = VERSION,
    )
}

pub(crate) fn message_result_html(ok: bool, message: &str, auth_q: &str) -> String {
    let cls = if ok { "msg" } else { "msg err" };
    let home = with_auth_query("/", auth_q);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Message — http-share</title>
<style>{style}</style>
</head>
<body>
<h1>Message</h1>
<div class="{cls}">{msg}</div>
<p class="nav"><a href="{home}">← Home</a></p>
<footer>http-share {ver}</footer>
</body>
</html>"#,
        cls = cls,
        msg = html_escape(message),
        home = home,
        style = SHARED_STYLE,
        ver = VERSION,
    )
}

pub(crate) fn listing_html(
    vfs: &Vfs,
    virt_path: &str,
    show_upload: bool,
    show_cert: bool,
    auth_q: &str,
    incoming_name: &str,
) -> String {
    // (href, display, is_dir, size_label)
    let mut items: Vec<(String, String, bool, String)> = Vec::new();

    let list_key = if virt_path.is_empty() || virt_path == "/" {
        ""
    } else {
        virt_path.trim_matches('/')
    };
    if let Some(entries) = vfs.list(list_key) {
        let prefix = if list_key.is_empty() {
            String::new()
        } else {
            format!("/{list_key}")
        };
        for e in entries {
            // At root, the incoming browse dir is shown via nav, not as a list row
            if list_key.is_empty() && e.name == incoming_name {
                continue;
            }
            let href = if e.is_dir {
                format!("{prefix}/{}/", encode_path_component(&e.name))
            } else {
                format!("{prefix}/{}", encode_path_component(&e.name))
            };
            let display = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let size_label = e.size.map(format_bytes).unwrap_or_default();
            items.push((href, display, e.is_dir, size_label));
        }
    }

    // Keep query-auth credentials on every link (QR → browse without losing session)
    if !auth_q.is_empty() {
        for item in &mut items {
            item.0 = with_auth_query(&item.0, auth_q);
        }
    }

    let title = if virt_path == "/" || virt_path.is_empty() {
        "Shared files"
    } else {
        virt_path.trim_matches('/')
    };

    let title_esc = html_escape(title);
    let mut body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{0} — http-share</title>
<style>
  body {{ font-family: system-ui, sans-serif; margin: 2rem; max-width: 48rem; color: #222; }}
  h1 {{ font-size: 1.2rem; margin-bottom: 1rem; }}
  ul {{ list-style: none; padding: 0; margin: 0; }}
  li {{ padding: 0.35rem 0; border-bottom: 1px solid #eee; display: flex; gap: 0.75rem; align-items: baseline; }}
  li a {{ text-decoration: none; color: #06c; flex: 1; min-width: 0; word-break: break-all; }}
  li a:hover {{ text-decoration: underline; }}
  .dir {{ font-weight: 600; }}
  .size {{ color: #666; font-size: 0.9rem; font-variant-numeric: tabular-nums; white-space: nowrap; }}
  .nav {{ margin: 1.25rem 0; font-size: 0.95rem; }}
  .nav a {{ color: #06c; margin-right: 1rem; }}
  footer {{ margin-top: 1rem; font-size: 0.85rem; color: #888; }}
  footer a {{ color: #06c; }}
  .msg-form {{ margin-top: 2rem; padding-top: 1rem; border-top: 1px solid #eee; }}
  .msg-form label {{ display: block; font-size: 0.9rem; color: #555; margin-bottom: 0.35rem; }}
  .msg-row {{ display: flex; gap: 0.5rem; flex-wrap: wrap; }}
  .msg-row input[type=text] {{ flex: 1; min-width: 12rem; padding: 0.4rem 0.5rem; font-size: 0.95rem; }}
  .msg-row button {{ padding: 0.4rem 0.9rem; font-size: 0.95rem; cursor: pointer; }}
</style>
</head>
<body>
<h1>{0}</h1>
<ul>
"#,
        title_esc
    );

    if virt_path != "/" && !virt_path.is_empty() {
        let parent = {
            let t = virt_path.trim_matches('/');
            if let Some(i) = t.rfind('/') {
                format!("/{}/", &t[..i])
            } else {
                "/".to_string()
            }
        };
        let parent = with_auth_query(&parent, auth_q);
        body.push_str(&format!(
            r#"<li><a href="{}" class="dir">../</a><span class="size"></span></li>"#,
            html_escape(&parent)
        ));
    }

    for (href, display, is_dir, size_label) in &items {
        let class = if *is_dir { r#" class="dir""# } else { "" };
        body.push_str(&format!(
            r#"<li><a href="{}"{}>{}</a><span class="size">{}</span></li>"#,
            html_escape(href),
            class,
            html_escape(display),
            html_escape(size_label)
        ));
    }

    if items.is_empty() {
        body.push_str(r#"<li style="color:#888">No files</li>"#);
    }

    body.push_str(r#"</ul>"#);

    let mut nav = Vec::new();
    if virt_path != "/" && !virt_path.is_empty() {
        nav.push(format!(
            r#"<a href="{}">Home</a>"#,
            with_auth_query("/", auth_q)
        ));
    }
    if show_upload {
        nav.push(format!(
            r#"<a href="{}">Upload a file…</a>"#,
            with_auth_query("/upload", auth_q)
        ));
    }
    if vfs.has_top_level(incoming_name) && !virt_path.starts_with(incoming_name) {
        nav.push(format!(
            r#"<a href="{}">Browse /{}/</a>"#,
            with_auth_query(&format!("/{incoming_name}/"), auth_q),
            incoming_name
        ));
    }
    if show_cert {
        nav.push(format!(
            r#"<a href="{}">Download certificate.pem</a>"#,
            with_auth_query("/certificate.pem", auth_q)
        ));
    }
    if !nav.is_empty() {
        body.push_str(r#"<p class="nav">"#);
        body.push_str(&nav.join(" · "));
        body.push_str("</p>");
    }

    body.push_str(&site_footer(auth_q));
    body.push_str("
</body>
</html>");
    body
}

pub(crate) fn upload_form_html(auth_q: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Upload — http-share</title>
<style>{style}
  form.upload {{ margin-top: 1.5rem; }}
  input[type=file] {{ display: block; margin: 1rem 0; }}
  form.upload button {{ padding: 0.5rem 1.2rem; font-size: 1rem; cursor: pointer; }}
</style>
</head>
<body>
<h1>Upload a file</h1>
<p class="nav"><a href="{home}">← Home</a></p>
<form class="upload" method="POST" action="{action}" enctype="multipart/form-data">
  <input type="file" name="file" required>
  <button type="submit">Upload</button>
</form>
{footer}
</body>
</html>"#,
        style = SHARED_STYLE,
        home = with_auth_query("/", auth_q),
        action = with_auth_query("/upload", auth_q),
        footer = site_footer(auth_q),
    )
}

pub(crate) fn upload_result_html(
    ok: bool,
    message: &str,
    browse_href: Option<&str>,
    auth_q: &str,
    incoming_name: &str,
) -> String {
    let cls = if ok { "msg" } else { "msg err" };
    let file_link = match browse_href {
        Some(href) if ok => format!(
            r#"<p><a href="{0}">Open uploaded file</a> · <a href="{1}">Browse /{2}/</a></p>"#,
            html_escape(&with_auth_query(href, auth_q)),
            html_escape(&with_auth_query(&format!("/{incoming_name}/"), auth_q)),
            html_escape(incoming_name),
        ),
        _ => String::new(),
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Upload — http-share</title>
<style>{style}</style>
</head>
<body>
<h1>Upload</h1>
<p class="nav"><a href="{upload}">Upload another</a> · <a href="{home}">Home</a></p>
<div class="{cls}">{msg}</div>
{file_link}
{footer}
</body>
</html>"#,
        style = SHARED_STYLE,
        cls = cls,
        msg = html_escape(message),
        file_link = file_link,
        upload = with_auth_query("/upload", auth_q),
        home = with_auth_query("/", auth_q),
        footer = site_footer(auth_q),
    )
}

