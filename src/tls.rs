//! Self-signed certificate load/generate and rustls config.

use std::env;
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use rustls::{Certificate, PrivateKey, ServerConfig};

pub(crate) fn config_dir() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("http-share")
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config").join("http-share")
    } else {
        PathBuf::from("/tmp/http-share-config")
    }
}

pub(crate) fn generate_self_signed() -> Result<(Vec<u8>, Vec<u8>), String> {
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

pub(crate) fn load_or_create_cert(
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

pub(crate) fn make_tls_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>, String> {
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

