//! Multipart upload handling.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::random_token;
use crate::util::find_bytes;

pub(crate) fn sanitize_filename(name: &str) -> Option<String> {
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

pub(crate) fn unique_dest(dir: &Path, filename: &str, allow_overwrite: bool) -> PathBuf {
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
pub(crate) fn multipart_boundary(content_type: &str) -> Option<String> {
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
pub(crate) fn parse_multipart_file(body: &[u8], boundary: &str) -> Result<(String, Vec<u8>), String> {
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



#[derive(Clone)]
pub(crate) struct UploadConfig {
    pub(crate) dir: PathBuf,
    pub(crate) max_size: Option<u64>,
    pub(crate) allow_overwrite: bool,
    pub(crate) upload_only: bool,
}

/// Shared lifetime / transfer limits. Any thread may call record_* after success.


pub(crate) fn handle_upload(
    body: &[u8],
    headers: &HashMap<String, String>,
    uc: &UploadConfig,
    verbose: bool,
) -> Result<(PathBuf, u64), String> {
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
        eprintln!("  stored upload as {} ({} bytes)", dest.display(), data.len());
    }
    Ok((dest, data.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sanitize_filename_rejects_traversal() {
        assert_eq!(sanitize_filename("ok.txt").as_deref(), Some("ok.txt"));
        assert_eq!(sanitize_filename("../etc/passwd"), None);
        assert_eq!(sanitize_filename("a/b"), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("."), None);
    }

    #[test]
    fn multipart_boundary_extract() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=----abc"),
            Some("----abc".into())
        );
        assert_eq!(multipart_boundary("text/plain"), None);
    }

    #[test]
    fn unique_dest_does_not_clobber() {
        let dir = std::env::temp_dir().join(format!("http-share-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let a = unique_dest(&dir, "f.txt", false);
        fs::write(&a, b"1").unwrap();
        let b = unique_dest(&dir, "f.txt", false);
        assert_ne!(a, b);
        assert!(b.file_name().unwrap().to_string_lossy().contains("f-"));
        let c = unique_dest(&dir, "f.txt", true);
        assert_eq!(c, dir.join("f.txt"));
        let _ = fs::remove_dir_all(&dir);
    }
}
