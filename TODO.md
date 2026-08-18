<!--
SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
SPDX-License-Identifier: GPL-3.0-or-later
-->

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
- [x] Shared files at site root; uploads under `/incoming/`
- [x] Root listing of shared paths with nav links to incoming / upload / cert
- [x] Post-upload result page links to `/incoming/<file>`
- [x] Grouped `--help` output (Network, Auth, TLS, Uploads, Lifetime, …)

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
- Phase 8 complete: shared files at root + `/incoming/`, sizes, cert link,
  query-auth QR, grouped `--help`, post-upload file links.
## Open / agent TODO

- [x] Remove `/pub/` subdirectory; shared paths live at the virtual root
- [x] `http-share .` flattens CWD contents into the virtual root (not under CWD basename)
- [ ] add a text field that allows sending human readable messages to the server
- [ ] need better reporting of errors
- [ ] do we have a recursive flag? do we need one? (dirs are already recursive via safe_join)
