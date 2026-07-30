# AGENTS.md — http-share

Guidance for humans and coding agents working on this repository.

## What this is

`http-share` is a **minimal HTTP(S) file sharing utility** for ad-hoc, trusted peer-to-peer transfers. Share only the paths you list on the CLI; no implicit CWD exposure, no permanent hosting.

Authoritative design: `PROPOSAL.md`.  
Task tracking: `TODO.md`.

## Hard constraints

- **Language:** Rust, edition 2021, target **rustc 1.75** (deps pinned in `Cargo.toml` for that reason).
- **No clap / heavy frameworks.** Manual CLI parsing in `src/main.rs`.
- **Small dependency footprint.** Current deps only:
  - `rustls` 0.20.9, `rustls-pemfile` 1.0.2, `rcgen` 0.9.3, `base64` 0.21.5, `time` 0.3.20
  - Prefer pure-std solutions (QR encoder, multipart parser, signal handling, lifetime limits are all in-tree).
- **Linux primary**; portable where practical.
- **Trusted user-to-user only.** Optional lifetime limits (`--one-shot`, `--expire`, `--max-downloads`, `--max-uploads`) stop the process after temporary use — not multi-user hosting.
- **Safe by default:** only explicit paths; auth on by default (random credentials); no overwrite on upload unless `--allow-overwrite`.

## Layout

```
src/main.rs     # entire application (single file by design)
Cargo.toml      # pinned deps for rustc 1.75
Cargo.lock
flake.nix       # Nix package / app / devShell / checks
PROPOSAL.md     # product design
TODO.md         # phased checklist + current status
AGENTS.md       # this file
```

Keep the single-file style unless a change clearly benefits from a module split.

## Architecture (main.rs)

| Area | Notes |
|------|--------|
| CLI | `Args`, `parse_args()`, `print_usage()` — extend here for new flags |
| VFS | `Vfs`, `Resolved` — shared under `/pub/`; extra mounts (e.g. `incoming`); landing at `/` |
| HTTP | Manual request parse; `handle_request`; range GETs; HTML listings |
| TLS | Persistent self-signed cert under `~/.config/http-share/`; `--dynamic-cert` / `--regenerate-cert` |
| Auth | HTTP Basic **or** query `?user=&password=` / `?u=&p=`; default random user `share`; `--public` disables |
| Upload | `--incoming DIR`; GET/POST `/upload`; browsable at `/incoming/` unless `--no-browse-uploads` |
| QR | Pure-Rust byte-mode ECC-L versions 1–6; `--qr` uses query-auth URL (Android-friendly) |
| Shutdown | Non-blocking accept + SIGINT/SIGTERM → `CTRL_C_RUNNING` |
| Lifetime | `LifetimeState` atomics; download/upload success or expire stops server |
| Stats | `TransferStats` — counts + bytes; summary on shutdown |

### Important behaviors

- **Paths:** at least one path **or** `--incoming DIR` required.
- **URL layout (FTP-style):** `/` landing; CLI shares under `/pub/`; uploads under `/incoming/`; form at `/upload`; cert at `/certificate.pem`.
- **URLs:** userinfo credentials in printed URLs; `--qr` uses `?user=&password=` (many Android QR readers drop userinfo).
- **Listings:** show file sizes; link to cert download when HTTPS; link to `/incoming/` when mounted.
- **Uploads:** basename-only filenames; reject `../`; unique `name-N.ext` unless `--allow-overwrite`.
- **Upload-only:** shared path downloads return 404; `/incoming/` still allowed if browsing is on; index links to `/upload`.
- **Body size:** `--max-upload-size` (e.g. `10M`); if unset, practical read cap is 256 MiB.
- **Lifetime:** `--one-shot` / `--expire` / `--max-downloads` / `--max-uploads` clear `CTRL_C_RUNNING` after limits; directory listings and HEAD do not count as downloads.
- **Path safety:** reject `..`, NUL, backslash; component walk with symlink_metadata; out-of-tree symlinks never served; root symlinks need `--follow-symlinks`.
- **Stats:** `TransferStats` tracks download/upload counts and bytes; printed on shutdown.

## Build & run

```sh
cargo build
cargo build --release
./target/release/http-share --public file.txt
./target/release/http-share --https --incoming /tmp/in photos/
./target/release/http-share --one-shot --expire 10m file.txt
```

Nix: `nix build` / `nix run` (flake present; may be unavailable in some agent environments).

**Note:** Some sandboxes mount the workdir without `exec`. Build under `/tmp` if `cargo` cannot run build scripts.

## What is done (as of 2026-07-30)

- Phases 0–6 complete (see `TODO.md`).
- Open items of interest:
  - Further path-traversal / symlink hardening
  - mDNS / Bonjour
  - Optional ZIP/TAR of multiple shared files
  - Download statistics
  - `nix build` verification

## How to extend

1. Read `PROPOSAL.md` + the relevant Phase in `TODO.md`.
2. Prefer **no new crates**. If a crate is unavoidable, pin an exact version compatible with rustc 1.75 / edition 2021.
3. Add CLI flags in `parse_args` + usage text; thread config through `Args` → `main` → handlers.
4. Keep responses streaming from disk; do not load whole files into memory for downloads.
5. Update `TODO.md` checkboxes and the “Current status” block when finishing a phase.
6. **After every change, propose a detailed git commit message** before finishing.
   Do not only say "done" — output a ready-to-use message the human can paste.
   Format:
   - Subject line ≤ ~72 chars, imperative mood (e.g. `Add --qr terminal QR codes`)
   - Blank line, then body bullets covering user-visible behavior, flags, and risks
   - Reference the phase/TODO item when relevant
   Example shape:
   ```
   Phase 5: add upload mode (--incoming, form, multipart POST)

   - --incoming DIR accepts multipart uploads into DIR
   - GET/POST /upload with simple HTML form; link from listings
   - Never overwrite by default (unique -N suffix); --allow-overwrite opt-in
   ```

## Testing habits

Manual smoke tests are the norm (no test suite yet):

```sh
# share + upload
http-share --public --incoming /tmp/in --port 8000 ./some-file
curl -F 'file=@./local.txt' http://127.0.0.1:8000/upload
curl -O http://127.0.0.1:8000/some-file

# lifetime
http-share --public --one-shot ./file   # exits after first GET of a file
http-share --public --expire 30s ./file
http-share --public --max-downloads 3 ./file
```

When changing HTTP parsing or upload, exercise multipart, overwrite policy, and path-traversal filenames (`../../etc/passwd` must fail).

## Do not

- Turn this into long-lived multi-user hosting or a session database.
- Implicitly serve the current working directory.
- Bump rustls/rcgen to edition-2024 crates without raising the MSRV story.
- Expand scope into “nice-to-haves” unless the user asks.
