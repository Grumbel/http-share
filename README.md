<!--
SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# http-share

Minimal HTTP(S) file sharing for ad-hoc transfers. Share only the paths you list — nothing else.

```sh
http-share photos/ report.pdf
```

Prints a URL (with credentials by default). Open it on another device on the same network. When you’re done, Ctrl+C.

**Version:** see the top-level `VERSION` file (currently development builds use a `-dev` suffix).

## Install

### From source

Requires Rust (rustc **1.75+** recommended; dependencies are pinned for that MSRV).

```sh
git clone <repo-url> http-share
cd http-share
cargo build --release
# binary: target/release/http-share
```

### Nix

```sh
nix run .# -- photos/
# or
nix build
./result/bin/http-share --help
```

## Quick start

```sh
# Share a file (auth on by default — credentials appear in the printed URL)
http-share ./document.pdf

# Share several paths
http-share ./notes.txt ./photos/

# No password (local trusted network only)
http-share --public ./document.pdf

# HTTPS with a persistent self-signed certificate
http-share --https ./document.pdf

# QR code in the terminal (handy for phones)
http-share --qr ./document.pdf

# Open the URL in your browser
http-share --open ./document.pdf
```

## URL layout

Shared content is organized FTP-style:

| Path | What it is |
|------|------------|
| `/` | Landing page with links |
| `/` | Listing of files and directories you passed on the command line |
| `/<name>` | A shared file or folder |
| `/incoming/` | Uploaded files (when uploads are enabled) |
| `/upload` | Upload form |
| `/certificate.pem` | Server TLS certificate (HTTPS only) |

Example: `http-share report.pdf photos/` serves `report.pdf` at `/report.pdf` and the folder at `/photos/`.

## Authentication

**Default:** random username (`share`) and password. Printed URLs already include credentials.

```text
http://share:Ab3x…@192.168.1.15:8000/
```

| Flag | Effect |
|------|--------|
| *(default)* | Random password |
| `--user NAME` / `--password PASS` | Fixed credentials |
| `--random-password` | Force random credentials |
| `--public` | No authentication |

### Phones and QR codes

Many Android QR scanners do not handle `user:pass@host` in URLs. Use `--qr`: the code encodes a **query-parameter** form instead:

```text
http://192.168.1.15:8000/?user=share&password=…
```

After the first successful login that way, the server sets a short session cookie and keeps credentials on page links so browsing continues to work.

You can also type query auth manually: `?user=…&password=…` or short `?u=…&p=…`.

## HTTPS

```sh
http-share --https ./file.pdf
```

- First run generates a self-signed certificate under `~/.config/http-share/`.
- Later runs reuse it (fewer new browser warnings).
- Download the cert from `/certificate.pem` (linked from directory pages) if you want to trust it on a client.
- `--dynamic-cert` — one-off cert, not stored.
- `--regenerate-cert` — replace the stored cert.

Browsers will still warn about a self-signed certificate; that is expected for ad-hoc use.

## Uploads

```sh
http-share --incoming /tmp/inbox ./readme.txt
```

- Clients use the form at `/upload` or POST multipart to `/upload`.
- Files land in the incoming directory and are listed under `/incoming/` (unless you pass `--no-browse-uploads`).
- Existing files are **not** overwritten unless you pass `--allow-overwrite` (otherwise a unique suffix is added).

| Flag | Effect |
|------|--------|
| `--incoming DIR` | Enable uploads into `DIR` |
| `--upload-only` | Only uploads (no shared-path downloads) |
| `--max-upload-size 100M` | Cap upload size (`K`/`M`/`G`) |
| `--allow-overwrite` | Replace existing names |
| `--no-browse-uploads` | Hide `/incoming/` from the browser |

## Lifetime limits

Useful so a share stops by itself:

```sh
http-share --one-shot ./secret.zip          # exit after first file download or upload
http-share --expire 30m ./photos/           # exit after 30 minutes
http-share --max-downloads 5 ./file.bin     # exit after 5 successful downloads
http-share --max-uploads 10 --incoming /tmp/in
```

Durations accept `s` / `m` / `h` / `d` (e.g. `30s`, `5m`, `1h`).

## Network options

| Flag | Default | Notes |
|------|---------|--------|
| `-p`, `--port` | `8000` | Listen port |
| `--bind` | `0.0.0.0` | Listen address |
| `--http` | yes | Plain HTTP |
| `--https` | | TLS |
| `-v`, `--verbose` | | Log requests |
| `--open` | | Open primary URL in the browser |
| `--qr` | | Print terminal QR (query-auth URL when auth is on) |
| `--follow-symlinks` | off | Follow symlinks that stay inside the share root |

On start, usable URLs are printed (LAN address when detectable). Ctrl+C shuts down cleanly and prints transfer statistics.

## Safety model

- **Only paths you list** are shared. The current directory is never exposed implicitly.
- Symlinks outside the share root are not served; `--follow-symlinks` still requires the target to stay within the shared tree.
- Path traversal (`..`, encoded variants) is rejected.
- Uploads use basename-only names; `../`-style filenames are rejected.
- Intended for **trusted, temporary** peer-to-peer use — not a multi-user hosting platform.

## Examples

```sh
# One file, stop after first download
http-share --one-shot --qr ./boarding-pass.pdf

# Photo drop for the LAN, HTTPS, size limit
http-share --https --incoming ~/inbox --max-upload-size 20M --expire 2h

# Public read-only share of a directory
http-share --public -p 9000 ./exports/

# Fixed password for a known peer
http-share --user alice --password hunter2 ./dataset.tar.gz
```

## See also

- `PROPOSAL.md` — design goals and rationale  
- `TODO.md` — implementation status  
- `AGENTS.md` — notes for contributors and coding agents  
- `VERSION` — product version (source of truth)

```sh
http-share --help
http-share --version
```

## License

Copyright 2026 Ingo Ruhnke &lt;grumbel@gmail.com&gt;.

This project is licensed under the **GNU General Public License v3.0 or later**.
See [`LICENSES/GPL-3.0-or-later.txt`](LICENSES/GPL-3.0-or-later.txt) and the SPDX headers in each file.
