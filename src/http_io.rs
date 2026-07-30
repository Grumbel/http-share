//! Low-level HTTP response writing and file serving.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::util::{find_bytes, mime_for, parse_range};

pub(crate) fn write_response(
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

pub(crate) fn html_headers(content_len: usize, set_cookie: &Option<String>) -> Vec<(&str, String)> {
    let mut h = vec![
        ("Content-Type", "text/html; charset=utf-8".into()),
        ("Content-Length", content_len.to_string()),
        ("Cache-Control", "no-store".into()),
    ];
    if let Some(c) = set_cookie {
        h.push(("Set-Cookie", c.clone()));
    }
    h
}

pub(crate) fn serve_file(
    stream: &mut dyn Write,
    path: &Path,
    range_header: Option<&str>,
    head_only: bool,
    set_cookie: &Option<String>,
) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let len = meta.len();
    let mime = mime_for(path);

    if let Some(rh) = range_header {
        if let Some((start, end)) = parse_range(rh, len) {
            let to_read = end - start + 1;
            let mut headers = vec![
                ("Content-Type", mime.to_string()),
                ("Content-Length", to_read.to_string()),
                ("Content-Range", format!("bytes {start}-{end}/{len}")),
                ("Accept-Ranges", "bytes".into()),
                ("Cache-Control", "no-store".into()),
            ];
            if let Some(c) = set_cookie {
                headers.push(("Set-Cookie", c.clone()));
            }
            if head_only {
                write_response(stream, 206, "Partial Content", &headers, b"")?;
                return Ok(0);
            }
            file.seek(SeekFrom::Start(start))?;
            let mut buf = vec![0u8; to_read as usize];
            file.read_exact(&mut buf)?;
            write_response(stream, 206, "Partial Content", &headers, &buf)?;
            return Ok(to_read);
        }
    }

    write!(stream, "HTTP/1.1 200 OK\r\n")?;
    write!(stream, "Content-Type: {mime}\r\n")?;
    write!(stream, "Content-Length: {len}\r\n")?;
    write!(stream, "Accept-Ranges: bytes\r\n")?;
    write!(stream, "Cache-Control: no-store\r\n")?;
    if let Some(c) = set_cookie {
        write!(stream, "Set-Cookie: {c}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    if head_only {
        return Ok(0);
    }

    let mut sent: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
        sent += n as u64;
    }
    Ok(sent)
}

pub(crate) fn read_http_message(stream: &mut dyn Read, max_body: u64) -> Option<(String, Vec<u8>)> {
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
