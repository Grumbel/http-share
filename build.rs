// Embed the top-level VERSION file as HTTP_SHARE_VERSION for --version.
// VERSION is the sole product-version source of truth (see AGENTS.md).
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let version_path = std::path::Path::new(&manifest_dir).join("VERSION");
    let version = std::fs::read_to_string(&version_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", version_path.display()))
        .lines()
        .next()
        .unwrap_or("0.0.0-dev")
        .trim()
        .to_string();
    // Allow packaging (e.g. Nix) to override with a +g<rev> suffix.
    let version = std::env::var("HTTP_SHARE_VERSION_OVERRIDE").unwrap_or(version);
    println!("cargo:rustc-env=HTTP_SHARE_VERSION={version}");
    println!("cargo:rerun-if-changed=VERSION");
    println!("cargo:rerun-if-env-changed=HTTP_SHARE_VERSION_OVERRIDE");
}
