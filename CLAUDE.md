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
- Embedded **Deno** (`deno_core`, no `deno_runtime`) runs user "macros"; `bollard` drives
  Docker; `playit.gg` provides tunneling.

## Build & test

```bash
# Backend (default member) — prefer `check` for fast iteration
cargo check -p lodestone_core
cargo build -p lodestone_core
cargo test --no-fail-fast -- --test-threads=1   # what CI runs; ~155 unit tests, no integration tests
cargo clippy

# Dashboard
cd dashboard && npm install         # runs patch-package via postinstall
npm run dev                         # next dev on :3001
npm run build                       # next build && next export
```

**End-to-end:** `core/tests/e2e.sh` (zsh, ~35 s, needs network) builds nothing; it runs
`target/debug/lodestone_core` on a throwaway data dir, creates and starts a real vanilla
server, runs a test macro and the `auto-backup` example, then tears down. In the Claude
sandbox each Bash command has its own network namespace, so the server and the client must
run in one command (the script does); declare the Mojang, Adoptium, GitHub and deno.land
hosts for that command.

`.env` holds `DATABASE_URL=sqlite://dev.db` (sqlx). Primary instance state lives as
JSON files in instance dirs (`.lodestone_config`); the SQLite DB stores only the
event log.

## The macro runtime (Deno stack) — read before upgrading

`rust-toolchain.toml` pins **Rust 1.96.0**. Macros run on `deno_core` without
`deno_runtime` (upgraded 2026-09-24; see `docs/deno-upgrade-scoping.md`):

- **Crates, pinned exactly** in `core/Cargo.toml`: `deno_core`, `deno_ast`,
  `deno_error`, `deno_webidl`, `deno_web`, `deno_io`, `deno_fs`, `deno_fetch`,
  `deno_net`, `deno_permissions`. deno_core's embedding API breaks between
  releases, so **upgrade them together**: pick one Deno CLI release, take the
  versions it uses, bump all pins, fix the compile errors, then run the macro
  tests (below). Do not add `deno_runtime` (it hides our ops, and it pulls a
  `libsqlite3-sys` that conflicts with the sqlx fork).
- **Threading:** one OS thread per macro (`macro-<pid>`), each with its **own
  current-thread tokio runtime** (deno_core's async ops `tokio::spawn` `!Send`
  futures, which is only sound there). Op bodies that touch the app state or an
  instance must go through `deno_ops::run_on_shared`, which runs them on the
  shared runtime: instance code spawns tasks (server supervision) and owns IO
  that must outlive the macro. Termination: `abort_macro` sets a flag, calls
  `terminate_execution()` and wakes the macro's event loop through a `Notify`.
- **Where things are:** ops in `core/src/deno_ops/` (`#[op2]`, error type
  `MacroOpError`; the procedure bridge ops in
  `implementations/generic/bridge/procedure_call.rs`); the `lodestone`
  extension in `core/src/macro_executor/extension.rs`; the ESM entry point in
  `core/src/macro_executor/bootstrap.js` (web globals, the `Deno` namespace —
  its `DENO_API` block is the one list of what macros get — and the compat
  shim `Deno[Deno.internal].core.{ops,opAsync}` that the glue uses); the module
  loader and permissions in `core/src/macro_executor/{loader,permissions}.rs`.
- **Permissions:** `Deno.*` fs is limited to the macro's root (the instance
  directory; `MacroExecutor::spawn`'s `fs_root`), with `..` and symlink
  escapes denied; no root means no fs access. Network (`fetch`) is open.
- **Extension JS is embedded by `core/build.rs`** (`DENO_EXTENSION_CRATES`). deno_core
  otherwise reads it from the build machine's cargo registry at runtime. A new Deno
  extension crate must be added there, or building a runtime fails with "not
  embedded in this build".
- Op names have no `op_` prefix (the glue uses them). A test
  (`lodestone_op_names_do_not_collide`) checks they don't clash with deno ops.

## Sandbox / environment notes

- The `v8` build script downloads a prebuilt `librusty_v8` archive per profile
  (debug and release are separate downloads) from GitHub (`github.com` +
  `release-assets.githubusercontent.com`).
- Forked **git dependencies** (`sqlx`, `safe_path_subset`, `playit-agent` ×2) live
  under `~/.cargo/git/`. Fetching them needs that dir writable and network access to
  `github.com` + `crates.io`. If a build fails with "Read-only file system" on
  `~/.cargo/git`, the sandbox needs `~/.cargo/git/` in `filesystem.allowWrite`.
- `cargo` is deterministic: prefer regenerating `Cargo.lock` via targeted
  `cargo update -p <crate>` over hand-editing it.

## Security posture (important context, not fully fixed)

The README advertises strong sandboxing, but be aware:

- **Macros are only partly sandboxed.** The `Deno.*` fs API is scoped to the instance
  directory, and `Deno.run`/`Command`/`env`/`exit` don't exist. But `fetch` can reach
  any host (including LAN and localhost services), module imports can read any local
  file (`import x from "file:///…" with { type: "json" }`), and the ops can start, stop
  and command every instance. Don't describe macros as a security boundary.
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
- **Macro JS glue is embedded.** `core/build.rs` embeds `src/deno_ops/**`,
  `deno_bindings/` and `src/implementations/generic/js/**` into the binary, and the
  module loader serves any `https://raw.githubusercontent.com/Lodestone-Team/lodestone/dev/core/…`
  URL from those copies (`core/src/embedded_glue.rs`; add other branches to
  `EMBEDDED_GLUE_URL_PREFIXES`). Editing a glue file changes what macros get, with
  no push needed. `lodestone-macro-lib` itself is still fetched from GitHub.
- **Macro runtime tests** live in `core/src/macro_runtime_tests.rs`. They check only
  JS-visible behaviour (return values, events, exit status), so they must keep passing
  across the Deno upgrade. `crate::init_test_app_state()` installs a global `AppState`
  for tests that need `app_state()`.
- `cargo test` regenerates the `ts-rs` bindings and touches `core/test.db`; don't
  commit those incidental changes.
