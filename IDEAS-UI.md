<!--
SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Draft: UI companions (GUI / TUI)

**Status: deferred — not planned for near-term work.**  
Collected so the ideas are not lost. The primary product remains a **CLI one-liner**.

## Why this is on hold

`http-share` is valuable because it is small: share paths, print a URL, optional QR, stop with Ctrl+C. A full desktop or terminal UI competes with LocalSend / Syncthing / file-manager shares and adds packaging, platform quirks, and dual-surface maintenance (CLI flags vs UI controls).

Anything beyond the CLI should stay **optional** and preferably a **separate binary** (or separate repo) that reuses a shared core, not an “everything app” mode inside the main tool.

## Out of scope for the core CLI

| Idea | Notes |
|------|--------|
| Qt (or other) full GUI | High cost (tooling, releases, HiDPI/Wayland/tray). |
| Runtime add/remove of files and directories while the server runs | Needs locking around the VFS, conflict policy, and mid-transfer semantics. |
| Built-in chat with replies | Today `/api/message` is host-side log only; two-way chat is a different protocol. |
| mDNS / ZIP / tar of shares | Explicitly skipped for now (see project notes). |

## Possible directions (if revisited)

### 1. Thin GUI — “CLI options as a form” (most plausible)

Mirror existing flags; **no** live VFS edits.

- Path list, port/bind, auth/public, HTTPS, incoming dir, map options  
- **Start / Stop** server (same core as the CLI)  
- **Show QR** / copy URL on button press  
- Optional system tray later (icon, open browser, quit) — tray is cross-platform pain; treat as phase 2  

**Libraries (Rust), rough fit:**

| Stack | Role |
|-------|------|
| egui + eframe | Simple control panel; good prototype speed |
| Tauri | Web form + Rust backend; easy QR in HTML |
| iced / Slint | Cleaner declarative forms; more structure |
| gtk-rs | Native Linux; better tray story if Linux-only |
| Qt | Capable but heavy for this scope |

Prefer a **separate package** `http-share-gui` that depends on shared library code (or subprocess-invokes the CLI for an even thinner first cut).

### 2. Optional TUI status panel

Lighter than a GUI, still secondary to the one-liner.

- Live log / transfer counters  
- Display primary URL and QR in-terminal (overlap with `--qr` / `--rq`)  
- **Not** a file manager; avoid in-session mount editing unless the core grows a deliberate control API  

### 3. Dynamic mounts without a GUI

If power users need “change shares without restart”, prefer a **narrow control channel** (e.g. optional socket/FIFO commands) over a full UI. Only if real demand appears.

### 4. Message “replies”

Keep host-visible messages in the terminal (and optional log file). Do not expand into a messaging product unless requirements are explicit.

## Architecture constraint (if anything is built)

```text
http-share-core   (VFS, HTTP, auth, TLS, upload)
       ↑
       ├── http-share        (CLI — primary)
       └── http-share-gui    (optional companion; same options, Start/Stop/QR)
```

- Do not gate the CLI on GUI dependencies.  
- Stabilize CLI/API surface (`--map`, `--map-api`, auth, etc.) before investing in a form UI.  
- Prefer restart-to-reconfigure over live tree mutation for a first GUI.

## Decision

**Skip for now.** Reopen this document only if there is a clear user need that the one-liner + `--qr`/`--rq` + `--tree` cannot cover.
