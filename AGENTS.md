<!--
SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
SPDX-License-Identifier: GPL-3.0-or-later
-->

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
src/
  main.rs       # entry point, accept loop, signal handler
  cli.rs        # Args, parse_args, print_usage, duration/size parsers
  vfs.rs        # virtual filesystem (root + /incoming/), path safety
  util.rs       # percent-encoding, mime, ranges, query parse, helpers
  auth.rs       # Basic / query / cookie auth
  html.rs       # landing, listings, upload form HTML
  http_io.rs    # write_response, serve_file, read_http_message
  upload.rs     # multipart parser, sanitize, unique names
  state.rs      # LifetimeState, TransferStats, CTRL_C_RUNNING
  server.rs     # handle_request, plain/TLS client workers
  tls.rs        # self-signed cert load/generate, rustls config
  net.rs        # LAN IP detection, open browser
  qr.rs         # pure-Rust QR encoder
Cargo.toml / Cargo.lock / build.rs / VERSION / flake.nix
PROPOSAL.md / TODO.md / AGENTS.md / README.md
```

Prefer **small modules** over a single giant `main.rs` so agents and humans can
read, test, and change one concern at a time. Unit tests live in `#[cfg(test)]`
modules next to the code they cover (`cargo test`).

## Architecture (main.rs)

| Area | Notes |
|------|--------|
| CLI | `Args`, `parse_args()`, `print_usage()` — extend here for new flags |
| VFS | `Vfs`, `ShareSpec`, `Resolved` — `PATH` / `PATH/` / `--map PATH VIRT`; extra mounts (e.g. `incoming`); listing at `/` |
| HTTP | Manual request parse; `handle_request`; range GETs; HTML listings |
| TLS | Persistent self-signed cert under `~/.config/http-share/`; `--dynamic-cert` / `--regenerate-cert` |
| Auth | HTTP Basic **or** query `?user=&password=` / `?u=&p=`; default random user `share`; `--public` disables |
| Upload | `--incoming DIR`; GET/POST `/upload`; browsable at `/incoming/` unless `--no-browse-uploads` |
| QR / auth | Query-auth (`?user=`/`?password=`) for QR; on success set `http_share_auth` cookie and rewrite HTML links so navigation keeps credentials |
| Shutdown | Non-blocking accept + SIGINT/SIGTERM → `CTRL_C_RUNNING` |
| Lifetime | `LifetimeState` atomics; download/upload success or expire stops server |
| Stats | `TransferStats` — counts + bytes; summary on shutdown |

### Important behaviors

- **Paths:** at least one path **or** `--incoming DIR` required.
- **URL layout:** `/` lists shared CLI paths; uploads under `/incoming/`; form at `/upload`; cert at `/certificate.pem`.
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



## Version Number Handling

Single source of truth: top-level **`VERSION`** file (e.g. `0.1.0-dev`).  
Do **not** hard-code the product version elsewhere; derive it at build time.

### Rules

* **`VERSION` is the only source of truth.** No duplicated version strings in docs or source that can drift.
* **In git (development):** the version always carries a `-dev` suffix (e.g. `0.1.0-dev`).
* **At release:** strip `-dev`, commit `VERSION` (e.g. `0.1.0`), tag the tree as **`v` + VERSION** (e.g. `v0.1.0`). The tag must match `VERSION` with a leading `v`.
* **`--version` CLI flag** must print the product version (from `VERSION` / build embedding). GUI About dialogs, if any, use the same string.
* **`Cargo.toml` `package.version`:** Cargo requires a version field. Prefer keeping it equal to the base in `VERSION` (without flake’s `+g…` suffix) via the release workflow. Runtime/`--version` must **not** rely on `CARGO_PKG_VERSION` alone — embed from `VERSION` through `build.rs` so the file remains authoritative even if Cargo.toml lags.
* **`flake.nix`:** read `VERSION`, then append the short git revision when available (Nix `self.shortRev` / `dirtyShortRev`), e.g. `0.1.0-dev+gd910b1c` or `0.1.0-dev+gfb40b2c-dirty`.

### How it flows (Rust)

| Layer | Behavior |
|-------|----------|
| `VERSION` | One line, e.g. `0.1.0-dev` |
| `build.rs` | Reads `VERSION`, sets `cargo:rustc-env=HTTP_SHARE_VERSION=…`, `rerun-if-changed=VERSION` |
| `src/main.rs` | `const VERSION: &str = env!("HTTP_SHARE_VERSION");` and `--version` prints it |
| `Cargo.toml` | `version` kept in sync with `VERSION` at release (no `+g` git suffix) |
| `flake.nix` | `versionBase` from `VERSION`; package/app version = `"${versionBase}+g${gitRev}"` |

Nix may pass an override (e.g. full `version` with `+g…`) into the build via `HTTP_SHARE_VERSION` env if packaging needs the dirty/rev suffix in the binary; otherwise the binary shows the clean `VERSION` line and only the Nix package name/version carries `+g…`. Prefer one consistent policy: either binary always matches `VERSION`, or binary matches flake’s expanded version when built under Nix. Document the choice in the flake comment.

### `build.rs` example

```rust
// build.rs — embed VERSION as HTTP_SHARE_VERSION for --version
fn main() {
    let version = std::fs::read_to_string("VERSION")
        .expect("VERSION file missing")
        .lines()
        .next()
        .unwrap_or("0.0.0-dev")
        .trim()
        .to_string();
    println!("cargo:rustc-env=HTTP_SHARE_VERSION={version}");
    println!("cargo:rerun-if-changed=VERSION");
}
```

### CLI example

```rust
// in parse_args / flag handling
"--version" | "-V" => {
    println!("http-share {}", env!("HTTP_SHARE_VERSION"));
    std::process::exit(0);
}
```

### `flake.nix` example

```nix
pkgs = nixpkgs.legacyPackages.${system};
lib = pkgs.lib;
versionBase = lib.strings.removeSuffix "\n" (builtins.readFile ./VERSION);
gitRev = "${self.shortRev or self.dirtyShortRev or "dirty"}";
version = "${versionBase}+g${gitRev}";
# use `version` for package version; optionally pass versionBase or version
# into the crate build via env for --version
```

### Release checklist

1. Set `VERSION` to the release number **without** `-dev` (e.g. `0.1.0`).
2. Align `Cargo.toml` `package.version` with that same number.
3. Commit (`Release 0.1.0` or similar).
4. Tag `v0.1.0` (always `v` + contents of `VERSION`).
5. On the next commit, set `VERSION` back to `0.2.0-dev` (or the next `-dev` line) so mainline stays marked development.

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
