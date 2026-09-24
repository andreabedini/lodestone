# CLAUDE.md

Guidance for working in this repository.

## What this is

Lodestone is a self-hosted manager for Minecraft / multiplayer game servers.

- **`core/`** — Rust backend (`lodestone_core`): an axum 0.6 + tokio HTTP/WebSocket
  server. This is the default workspace member and where most work happens.
- **`dashboard/`** — Next.js 13 + React 18 + TypeScript web UI. TypeScript API
  types in `dashboard/src/bindings/` are **generated from Rust** via `ts-rs`
  (`#[derive(TS)]`) — don't hand-edit them; regenerate from the Rust side.
- **`dashboard/src-tauri/`** — Tauri 1.4 desktop wrapper (second workspace member).
- Embedded **Deno runtime** (`deno_core`) runs user "macros"; `bollard` drives
  Docker; `playit.gg` provides tunneling.

## Build & test

```bash
# Backend (default member) — prefer `check` for fast iteration
cargo check -p lodestone_core
cargo build -p lodestone_core
cargo test --no-fail-fast -- --test-threads=1   # what CI runs; ~44 unit tests, no integration tests
cargo clippy

# Dashboard
cd dashboard && npm install         # runs patch-package via postinstall
npm run dev                         # next dev on :3001
npm run build                       # next build && next export
```

`.env` holds `DATABASE_URL=sqlite://dev.db` (sqlx). Primary instance state lives as
JSON files in instance dirs (`.lodestone_config`); the SQLite DB stores only the
event log.

## ⚠️ The Deno/V8 stack is old — read before upgrading

`rust-toolchain.toml` pins **Rust 1.96.0**. The old Deno stack (`deno_core 0.190`,
`deno_runtime 0.116`, `v8 0.73`, 2023) only builds on it because of a local patch:

- **`vendor/v8/`** is the published `v8 0.73.0` crate (only `Cargo.toml`, `build.rs`,
  `src/`, `tools/download_file.py`), wired in via `[patch.crates-io]` in the root
  `Cargo.toml`. The upstream crate asserts `size_of::<TypeId>() == size_of::<u64>()`,
  which fails with `E0080` on rustc ≥ 1.72 (TypeId is 128-bit). The fix is in
  `TypeIdHasher` in `vendor/v8/src/isolate.rs`, marked `LODESTONE PATCH`. The build
  still downloads the prebuilt `librusty_v8` 0.73.0 from GitHub release assets
  (`github.com` + `release-assets.githubusercontent.com`).
- Remove `vendor/v8` and the patch once the Deno stack is upgraded to a `deno_core`
  that pulls `v8 ≥ 0.74` (which handles 128-bit TypeId upstream).

**Known dependency walls (held by the old Deno/swc crates, not by rustc):**

- `serde` must stay old: `serde 1.0.229` (which splits out `serde_core`) breaks the old
  `swc_common` with `unresolved import serde::__private`. `1.0.193` is known good.
  Because `time ≥ 0.3.46` requires the newer serde, **`time` is capped at 0.3.44**
  (so RUSTSEC-2026-0009, fixed in 0.3.47, stays open until the Deno upgrade).
- Plain `cargo update` / `cargo update -p X` may pull the newer serde now that
  `rust-version` is 1.96. After any update, check `Cargo.lock` still has
  `serde 1.0.193` and run `cargo check`; pin with `cargo update -p X --precise <ver>`.
- Do not bump `rust-toolchain.toml` further without re-running the full test suite:
  `vendor/v8` is only verified on 1.96.0 (and `cargo check` on 1.98.1).

## Sandbox / environment notes

- Forked **git dependencies** (`sqlx`, `safe_path_subset`, `playit-agent` ×2) live
  under `~/.cargo/git/`. Fetching them needs that dir writable and network access to
  `github.com` + `crates.io`. If a build fails with "Read-only file system" on
  `~/.cargo/git`, the sandbox needs `~/.cargo/git/` in `filesystem.allowWrite`.
- `cargo` is deterministic: prefer regenerating `Cargo.lock` via targeted
  `cargo update -p <crate>` over hand-editing it.

## Security posture (important context, not yet fixed)

The README advertises strong sandboxing, but be aware:

- **Macros run with `Permissions::allow_all()`** (`core/src/macro_executor.rs`) — user
  macro JS has full host access. Don't describe macros as sandboxed until this is fixed.
- **Global file manager** (`core/src/handlers/global_fs.rs`) is arbitrary host
  read/write by design, gated only by the `ReadGlobalFile`/`WriteGlobalFile`
  permission + a `safe_mode` toggle. Treat `can_write_global_file` as root-equivalent.
- CORS is `allow_origin(Any)`; the monitor WebSocket (`handlers/monitor.rs`) is
  unauthenticated. Auth is Bearer-token (header), so CSRF risk is limited.

Run `cargo audit` (or cross-reference `Cargo.lock` against the RustSec advisory DB)
before claiming the dependency tree is clean.

The full assessment (security issues S1–S7, maintenance plan, latest audit results)
is in `docs/codebase-assessment.md` — update it when you fix one of its items.

## Conventions

- Errors: `thiserror` + `color-eyre`; handlers return `Result<_, error::Error>` which
  maps `ErrorKind` → HTTP status. Avoid adding `.unwrap()`/`.expect()` in request
  paths (there are already ~330 in `core/src`; don't make it worse).
- Prefer `rg` over `grep -r`.
