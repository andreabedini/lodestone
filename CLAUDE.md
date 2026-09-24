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

## ⚠️ The toolchain is pinned by the Deno/V8 stack — read before upgrading

`rust-toolchain.toml` pins **Rust 1.70.0**. This is **not arbitrary**: the vendored
`v8 0.73` / `deno_core 0.190` (2023) fail to compile on any modern rustc with
`E0080: size_of::<TypeId>() == size_of::<u64>()` (verified broken on 1.93 and 1.96).

**Consequences — do not fight these blindly:**

1. **Do not bump `rust-toolchain.toml`** without first upgrading the whole Deno stack
   (`deno_core`, `deno_runtime`, `deno_ast`, which pull a newer `v8`). That is a real
   project, not a one-liner.
2. **Dependency upgrades are MSRV-constrained.** Many patched crate versions now
   require rustc ≥1.80/1.81 and therefore **will not build on 1.70**. Known walls:
   - `time ≥0.3.36` pulls `deranged` (needs rustc 1.81) — stay on `time 0.3.20`.
   - `openssl ≥0.10.76` / `openssl-sys ≥0.9.112` need rustc 1.80 — the last
     1.70-compatible patched pair is **`openssl 0.10.75` + `openssl-sys 0.9.111`**.
   When `cargo update -p X` breaks the build with "requires rustc 1.8x", find the
   newest version that still declares MSRV 1.70 (check the registry index's
   `rust_version` field) and pin it with `cargo update -p X --precise <ver>`.

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
