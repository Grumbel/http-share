# TODO: http-share

Minimal HTTP(S) file sharing utility for ad-hoc file transfers (Rust).

Derived from PROPOSAL.md.

**Scope note:** Trusted user-to-user / ad-hoc transfers only. Lifetime limits (`--one-shot`, `--expire`, `--max-downloads`, `--max-uploads`) stop the process after temporary sharing — not a multi-user hosting platform.

## Phase 0: Project scaffolding

- [x] Create `Cargo.toml` with minimal dependencies
- [x] Create basic `src/main.rs` skeleton (CLI parse + working server)
- [x] Create `flake.nix` (packages.default, apps.default, devShell, checks, formatter)
- [x] Ensure project builds with `cargo build`
- [ ] Ensure project builds with `nix build` (nix not available in this environment)

## Phase 1: Core server & virtual filesystem

- [x] CLI: accept list of files/directories to share + `--port`, `--bind`
- [x] Virtual filesystem rooted at `/`
- [x] Never expose CWD implicitly; only explicitly given paths
- [x] Stream file responses from disk
- [x] HTTP range request support (single-range + suffix)
- [x] Simple HTML directory listing for directories
- [x] Symlink policy: `--follow-symlinks`
- [x] Harden path traversal / symlink escape further

## Phase 2: Network & discovery

- [x] `--bind ADDRESS`, `--port PORT`
- [x] Print reachable URLs on startup
- [x] `--open` — open primary URL in the default browser
- [x] `--qr` — print terminal QR code of primary URL
- [x] `--verbose`
- [x] IPv6 support (bind / print)
- [x] Graceful shutdown on Ctrl+C

## Phase 3: HTTPS

- [x] `--https` / `--http` (default HTTP)
- [x] Persistent self-signed cert under `~/.config/http-share/`
- [x] Reuse certificate across sessions
- [x] `--dynamic-cert` (ephemeral cert for this run)
- [x] `--regenerate-cert`
- [x] Serve certificate at `/certificate.pem`

## Phase 4: Authentication

- [x] HTTP Basic Auth
- [x] Default: generate random user + password; include in printed URLs
- [x] `--user`, `--password`, `--random-password` (default), `--public`
- [x] Credentials appear in printed URLs when auth is enabled

## Phase 5: Upload mode

- [x] `--incoming DIRECTORY`
- [x] Simple HTML upload form
- [x] Never overwrite existing files unless explicitly requested
- [x] `--upload-only`, `--max-upload-size`, `--allow-overwrite`
- [x] Downloads still available unless disabled

## Phase 6: Lifetime management

- [x] `--one-shot` — stop after first successful download or upload
- [x] `--expire DURATION` — stop after duration (Ns/Nm/Nh/Nd)
- [x] `--max-downloads N`
- [x] `--max-uploads N`


## Phase 7: Nice-to-haves (later)

- [x] Automatic LAN address detection (partial)
- [ ] mDNS / Bonjour advertisement
- [ ] Optional ZIP/TAR of multiple shared files
- [x] Download statistics
- [x] Cache-Control: no-store
- [x] Read-only by default

## Phase 8: UX / URL space / mobile

- [x] Directory listings show file sizes
- [x] Link to download `/certificate.pem` from HTML pages (when HTTPS)
- [x] Uploaded files browsable under `/incoming/` (default with `--incoming`)
- [x] `--no-browse-uploads` to hide uploaded files from the VFS
- [x] Query-parameter auth (`?user=` / `?password=` or `?u=` / `?p=`) for QR / Android
- [x] `--qr` encodes query-auth URL (not `user:pass@host` userinfo)
- [ ] Optional stricter URL layout: shared files under `/share/` (or `/pub/`) instead of virtual root
- [ ] Optional separate virtual root name for uploads (`/incoming/` is the current choice)
- [ ] Post-upload result page: direct link to the new file under `/incoming/…`

## Design constraints (always)

- Minimal CLI, sensible defaults
- Zero configuration for common case
- Ephemeral / temporary sharing
- Safe by default (only explicit paths)
- Small dependency footprint (rustls/rcgen/base64, pinned for rustc 1.75)
- Linux primary target; portable where practical

## Current status (2026-07-30)

- Phases 0–6 complete (scaffolding through lifetime management).
- Phase 5 upload mode: `--incoming`, `/upload`, multipart, unique names, size limits.
- Phase 6 lifetime: `--one-shot`, `--expire`, `--max-downloads`, `--max-uploads`.
- Default: random Basic Auth credentials embedded in printed URLs.
- Phase 8 (partial): sizes in listings, cert download link, `/incoming/` browse +
  `--no-browse-uploads`, query-param auth + Android-friendly QR URLs.
- URL layout today: CLI shares at `/<name>`, uploads at `/incoming/`, form at `/upload`,
  cert at `/certificate.pem`. Full `/share/` prefix still optional (see Phase 8).
