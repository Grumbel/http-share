<!--
SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Proposal: `http-share`

`http-share` is a minimal HTTP(S) file sharing utility for ad-hoc file transfers.

Its purpose is to expose a selected set of files and directories over HTTP or HTTPS directly from the command line, without requiring installation, configuration, or a web server setup. The focus is on making temporary file sharing as simple as possible while avoiding accidental exposure of unrelated files.

Example:

```sh
http-share file.txt another-file.txt photos/ --port 8080
```

## Design goals

* **Minimal.** A small command-line interface with sensible defaults.
* **Zero configuration.** Works immediately without prior setup.
* **Ephemeral.** Intended for temporary file transfers rather than permanent hosting.
* **Safe by default.** Only explicitly specified files are shared.
* **Cross-platform.** Primary target is Linux, with portability to other platforms where practical.

## Core functionality

The server shall:

* Serve only files and directories explicitly provided on the command line.
* Never expose the current working directory implicitly.
* Present the shared paths through a virtual filesystem rooted at `/`.
* Preserve directory hierarchy for shared directories.
* Place individually shared files into the virtual root regardless of their original location.
* Stream files directly from disk without loading them completely into memory.
* Support HTTP range requests to allow interrupted downloads to resume.
* Generate a simple HTML directory listing for directories.

Symbolic links should either:

* only be followed when they resolve within an explicitly shared directory, or
* require an explicit `--follow-symlinks` option.

## Network options

Common networking options should include:

* `--bind ADDRESS`
* `--port PORT`
* `--http`
* `--https`
* `--open` (open the share URL in the default browser)
* `--qr` (display the primary access URL as a terminal QR code)
* `--verbose`

On startup, the server should print one or more usable URLs, for example:

```text
http://192.168.1.15:8000/
http://hostname.local:8000/
```

When authentication is enabled, the printed URLs should already contain the credentials.

## HTTPS

HTTPS should require no manual setup.

On first use, a self-signed certificate and private key shall be generated and stored persistently, for example under:

```text
~/.config/http-share/
    certificate.pem
    private-key.pem
```

The same certificate shall be reused across sessions to avoid repeated browser warnings caused by changing certificates.

The certificate shall also be made available for download from a well-known endpoint (for example `/certificate.pem`) so that clients can install and trust it.

Suggested options:

* `--https`
* `--http`
* `--dynamic-cert` (generate a temporary certificate for this session)
* `--regenerate-cert` (replace the persistent certificate)

## Authentication

Authentication should be simple enough that all required information can be communicated as a single URL.

HTTP Basic Authentication should be supported, allowing URLs of the form:

```text
https://user:password@host/
```

Unless explicitly disabled, the server should automatically generate a random username and password on startup and include them in the printed URLs and QR code.

Suggested options:

* `--user USER`
* `--password PASSWORD`
* `--random-password` (generate random credentials; default)
* `--public` (disable authentication entirely)

The default behavior should be authenticated sharing using randomly generated credentials. Public, unauthenticated file sharing should require an explicit `--public` option.

## Upload mode

Optional upload support shall be provided via:

```text
--incoming DIRECTORY
```

When enabled:

* uploaded files are stored in the specified directory;
* existing files are never overwritten unless explicitly requested;
* downloads remain available unless disabled via an additional option.

Additional options may include:

* `--upload-only`
* `--max-upload-size SIZE`

A simple HTML upload page is sufficient.

## Lifetime management

Since the tool is intended for temporary sharing, automatic termination options are desirable.

Suggested options include:

* `--one-shot` — terminate the server after the first successful download or upload.
* `--expire DURATION` — terminate after the specified amount of time.
* `--max-downloads N` — terminate after *N* completed downloads.
* `--max-uploads N` — terminate after *N* completed uploads.

## Packaging

The repository should include:

* `Cargo.toml`
* `flake.nix`

The Nix flake should provide:

* `packages.default`
* `apps.default`
* `devShell`
* `checks`
* formatter configuration

The project should build cleanly using both Cargo and Nix.

## Nice-to-have features

* Automatic LAN address detection and printing of all reachable URLs.
* mDNS/Bonjour advertisement.
* IPv4 and IPv6 support.
* Read-only mode by default.
* Optional ZIP or TAR download when multiple files are shared.
* Download statistics.
* Appropriate cache-control headers (for example `Cache-Control: no-store`).
* Graceful shutdown on Ctrl+C.

## Implementation

* Language: Rust.
* Small dependency footprint.
* Straightforward, maintainable codebase.
* Linux as the primary target platform.

## Inspiration

A similar tool exists for SFTP:

https://github.com/grumbel/sftp-share

