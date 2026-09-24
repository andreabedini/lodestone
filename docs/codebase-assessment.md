# Lodestone — Codebase Assessment, Maintenance Plan & Roadmap

- **Written:** 2026-05-29
- **Last updated:** 2026-09-24. File references, the toolchain findings and the dependency audit were re-checked against the code.

> [!NOTE]
> This is an assessment of the [Lodestone](https://github.com/Lodestone-Team/lodestone) self-hosted game-server manager. Upstream has been inactive since **2024-09-09**, when the last commit landed on `main`. This repository is a personal fork, maintained to run a single Minecraft server. The goal is to flag urgent security and architecture issues and to propose a maintenance plan and roadmap. The headline security claims were checked directly against the source.

## 1. Snapshot

| | |
|---|---|
| **Backend** | Rust / axum 0.6 / tokio, 92 `.rs` files in `core/src`, single `lodestone_core` binary |
| **Frontend** | Next.js 13 + React 18 + TypeScript (~235 TS/TSX files in `dashboard/src`), Tauri 1.4 desktop wrapper |
| **Notable subsystems** | Embedded **Deno runtime** for user "macros", SQLite event log (forked sqlx), Docker via `bollard`, playit.gg tunneling, UPnP |
| **Last upstream commit** | **2024-09-09** |
| **Toolchain pin** | **Rust 1.96.0**, made possible by a one-function patch to the vendored `v8 0.73` crate (`vendor/v8`); see §7.1 |
| **Tests** | ~44 backend unit tests, **0 frontend tests**, no integration tests |
| **Dependency health** | Several git forks. Most dependencies are about 2 years behind. Latest audit: 25 RustSec vulnerabilities and 314 npm advisories (§7) |

The architecture is sound. It has clean trait-based layering (handlers → traits → implementations). The TypeScript bindings are generated from Rust via `ts-rs`, so the API types can't drift. The core is event-driven, and permissions are fine-grained. The problems are **stale dependencies, an over-permissive macro sandbox, thin testing, and error handling that panics too easily**. The underlying design is not the problem.

## 2. Urgent issues — Security

### 🔴 S1 — The Deno macro sandbox runs with `allow_all`

`core/src/macro_executor.rs:350-352`:

```rust
PermissionsContainer::new(
    // TODO: limit permissions
    Permissions::allow_all(),
)
```

User-supplied macro JS/TS runs with **full host access**: filesystem, network, subprocess spawning and environment. The README advertises "priority on safety and security" and lists macros as a headline feature; this setting directly contradicts that. Macros can also arrive inside instances and extensions pulled from URLs. Anyone who can author a macro can therefore execute code on the host. **This is the most important single issue.**

### 🔴 S2 — The global file manager is arbitrary host read/write by design

`core/src/handlers/global_fs.rs` decodes a base64 path and passes it straight to `PathBuf::from(...)` with **no scoping**. Instance file operations, by contrast, go through `scoped_join_win_safe`.

**Calibration note:** these endpoints *are* authenticated (`AuthBearer`), and they *are* gated behind the `ReadGlobalFile` / `WriteGlobalFile` permissions plus a `safe_mode` toggle. This is therefore **not** an unauthenticated path-traversal bug. It is a *deliberate* "browse the whole machine" feature. It is still urgent. Anyone holding `can_write_global_file` can write anywhere on the host, which leads to a full compromise via cron jobs, SSH keys or overwritten binaries. Meanwhile, the dashboard presents it as an ordinary file browser. **Treat `can_write_global_file` as equivalent to root on the host.**

### 🟠 S3 — Unauthenticated monitor WebSocket

The `monitor` handler at `core/src/handlers/monitor.rs:23` serves the route `/monitor/:uuid` (`:86`). It takes only `ws`, `State` and `Path(uuid)`, with **no `AuthBearer`**. Anyone who knows or guesses an instance UUID can stream live CPU, RAM, disk and player telemetry. The UUIDs are v4 and hard to brute-force, so the practical risk is moderate. It is still a clear missing-auth gap, and it is inconsistent with every other handler.

### 🟠 S4 — Stale dependency tree

The project was pinned to Rust 1.70 until 2026-09-24 (now 1.96, §7.1). It still uses axum 0.6 and `rand 0.6.5` (from 2019). It also uses an old `deno_core`/`deno_runtime` and several unmaintained crates (`ansi_term`, `tempdir`). The May 2026 quick wins cleared the openssl, bytes, h2, mio, tar and whoami advisories, and pinned `bollard` to 0.15. A real `cargo audit` run (§7.2) still reports **25 vulnerabilities** in the workspace, and most of them are reachable from `lodestone_core`. Most of the remaining fixes are blocked behind the Deno, axum and sqlx-fork upgrades.

### 🟡 S5 — CORS `allow_origin(Any)`

`core/src/lib.rs:661-671`. **Calibration note:** auth is a **Bearer token** sent in a header (read from localStorage), not a cookie. A malicious origin therefore *cannot* ride a victim's credentials, which largely mitigates classic CSRF. The real effect of allowing any origin is that it amplifies the *unauthenticated* endpoints (S3, S7, system info) and enables DNS-rebinding-style abuse. Severity: medium, not critical.

### 🟡 S6 — Panics in request and crypto paths (availability / DoS)

There are ~330 `unwrap()`/`expect()` calls in `core/src`. They include `auth/hashed_password.rs:14,44` (`.unwrap()` on hash and verify) and `serde_json::to_string(..).unwrap()` inside the WebSocket send loops (`handlers/events.rs:195,255` and `handlers/monitor.rs:51,67`). A corrupted hash or an event that fails to serialize panics the task, and can crash the service. That is a DoS surface in a tool whose whole job is uptime.

### 🟡 S7 — Unauthenticated playit.gg control endpoints

`core/src/handlers/playitgg.rs` registers the routes, and the handlers in `core/src/playitgg/mod.rs` take only `State`, with no `AuthBearer`. The routes are:

- `start_cli`, `stop_cli`, `cli_is_running`
- `generate_signup_link`, `verify_key`, `get_tunnels`

An unauthenticated caller can therefore disrupt connectivity and list tunnels.

## 3. Urgent issues — Architecture / Maintainability

- **A1 — Testing is near-zero.** There are ~44 unit tests and no integration tests. Nothing covers the file manager, permission enforcement or instance lifecycle, and the frontend has no tests at all. Refactoring or upgrading safely is impossible without a safety net, so **this gates everything else.**
- **A2 — Git-forked dependencies with unknown deltas.** `sqlx` and `safe_path_subset` are forked, and `playit-agent` is pulled twice (branches `v0.9` and `master`). How reproducible builds are and how to upgrade is unclear. The sqlx fork is the worst case: it blocks moving to a modern sqlx.
- **A3 — Oversized files.** `minecraft/configurable.rs` (2,087 lines), `macro_executor.rs` (1,275) and `handlers/instance_fs.rs` (1,066) are expensive to change and hard to test.
- **A4 — One hand-written SQL migration and no migration framework.** Evolving the schema requires manual coordination (`core/migrations/2023-1-1.sql`).
- **A5 — Little maintainer documentation.** There is no `ARCHITECTURE.md` and no documented locking order for `AppState`, which has 12 `Arc<Mutex/RwLock/DashMap>` fields (`core/src/lib.rs:103-122`). `CLAUDE.md` (added 2026-05-29) now covers the build, the toolchain constraints and the security posture.

## 4. Maintenance plan (phased)

The goal is a fork that is **safe to keep running and cheap to keep current**, not a rewrite.

> [!IMPORTANT]
> Because this is a personal fork running one server, priorities differ from those of a public project:
> - **S3, S5 and S7 matter as soon as the API is reachable from outside the LAN**, and they are cheap to fix. Do them first.
> - **S1 and S2 are latent** while you are the only macro author and the only holder of `can_write_global_file`. They become urgent the moment another person gets an account.
> - **The desktop app is not used.** Tauri-only advisories and Tauri upgrades have low priority.

### Phase 0 — Stabilize & measure

1. ✅ **Green build on a modern toolchain** — done 2026-09-24 on Rust 1.96.0 by patching `v8 0.73` locally instead of upgrading Deno (§7.1).
2. ✅ **Run `cargo audit`** — done 2026-09-24 (§7.2). Still to do: install `cargo audit` + `cargo deny` as CI gates.
3. ✅ **Pin `bollard`** to 0.15 (was `*`). Still to do: inventory the git-forked dependencies, recording *why* each was forked and whether upstream now covers it.
4. ✅ **`npm audit`** on the dashboard — done 2026-09-24 (§7.3).

### Phase 1 — Close urgent security gaps

5. **S3/S7:** Add `AuthBearer` and permission checks to the monitor WebSocket and the playit.gg routes. Add a router-level test asserting that every non-public route rejects an empty token.
6. **S5:** Replace `allow_origin(Any)` with a configurable origin allowlist, defaulting to the bundled dashboard origin.
7. **S6:** Remove `unwrap()` from `auth/`, the WebSocket send loops and `handlers/`, converting to `?`/`Result`. Replace `rand 0.6.5` with `rand 0.8` and `OsRng` for all secret generation (`core/src/util.rs:455`).
8. **S1:** Replace `Permissions::allow_all()` with a least-privilege `PermissionsContainer`: restrict macro filesystem access to the instance directory, deny subprocesses and environment access, and allowlist network destinations. This is the highest-value single change once anyone else can author macros.
9. **S2:** Put `can_write_global_file` behind an explicit confirmation in the UI, turn `safe_mode` on by default, and document that the permission is equivalent to root on the host. Consider an opt-in path allowlist.

### Phase 2 — Build the safety net

10. **Integration tests** on the highest-risk surfaces first: the permission enforcement matrix, file-manager scoping, auth/JWT, and instance start/stop. Target ~30–40 tests that pin current behaviour.
11. **Frontend:** add Vitest and React Testing Library, and smoke-test the auth flow and the file browser. Wire both into CI.
12. Add **`ARCHITECTURE.md`**: module map, `AppState` locking order and event flow.

### Phase 3 — Dependency modernization

13. **Upgrade the Deno stack** (`deno_core` / `deno_runtime` / `deno_ast`, which bring a newer `v8`). Scoped in `docs/deno-upgrade-scoping.md` (2026-09-24): target `deno_core` 0.412 without `deno_runtime`. It is no longer needed for the toolchain (§7.1), but it still clears the `deno_crypto` advisories (rsa, ring, aes-gcm, curve25519-dalek) and makes S1 easier to harden. It also lifts the old-`serde` cap (which holds `time` below its RUSTSEC-2026-0009 fix) and lets `vendor/v8` be deleted.
14. Upgrade in dependency order (no longer blocked by the toolchain): `axum 0.6 → 0.7+` (router and extractor API changes; brings hyper 1 / h2 0.4 and a fixed tungstenite), Next.js 13 → 14/15, React Query 4 → 5, and Tauri 1.4 → 2.x if the desktop app is ever used.
15. **Retire the sqlx fork**: move to upstream modern sqlx and a real migration setup (`sqlx migrate`).
16. Resolve the dual `playit-agent` dependency to a single version.

### Ongoing hygiene

- Dependabot or Renovate on both `Cargo.toml` and `package.json`, with `serde` held at 1.0.193 until the Deno upgrade (§7.1).
- CI gates: `cargo audit`, `cargo deny`, `cargo clippy -D warnings`, `npm audit`, plus the new test suites.
- A documented release and versioning process (currently 0.5.1, with no changelog).

## 5. Roadmap (improvements, once stable)

**Near-term (3–6 months)**

- Turn the **least-privilege macro sandbox** into a first-class capability model: each macro declares its permissions, and the user sees them before it runs.
- Split the oversized files: `minecraft/configurable.rs` into per-modloader modules, and the Deno setup, module loader and permissions out of `macro_executor.rs`.
- A structured **audit log / event viewer** (the README's "Event viewer" to-do). The SQLite event log already exists; it just needs surfacing.

**Mid-term (6–12 months)**

- Finish the **Docker instance** integration, which is already work in progress, on the pinned `bollard`.
- Plugin and mod management (a README future feature). Design it on top of the hardened macro/extension system, not as a parallel one.
- Proper **secrets management** for the JWT signing keys and playit keys, instead of JSON on disk. Consider rotation.
- Observability: `tracing` is already present. Add a metrics endpoint and a dashboard panel.

**Longer-term / strategic**

- **Fork strategy:** settled as a hard fork (personal use). Upstream or eliminate each forked dependency over time.
- Re-evaluate the embedded Deno runtime against a lighter sandbox (e.g. WASM) if macro security proves hard to bound. Weigh this against the cost of the Deno upgrade in Phase 3: dropping or replacing Deno would also remove the `vendor/v8` patch and the `serde` cap.

> [!NOTE]
> See also the *Kubernetes Instance Backend Feasibility* note (in the personal notes vault, not in this repository). It is a code-grounded look at running each instance as a Kubernetes pod, and is the strictly larger version of the "finish the Docker instance integration" item above. Both need the same `ServerBackend`/`ConsoleTransport` extraction. That work is currently **shelved**.

## 6. Bottom line

Nothing is on fire *right now*, as long as the instance isn't exposed to the internet and you don't run untrusted macros. **Three things are genuinely urgent:**

- the unauthenticated monitor and playit.gg endpoints (S3/S7);
- the `allow_all` macro sandbox (S1);
- the dependency tree (S4), whose remaining advisories are mostly blocked on the Deno upgrade.

The codebase is well-structured enough to be worth maintaining. The limiting factor is the **absence of tests** (A1), which is why Phase 2 must come before any serious upgrade work.

**Suggested next actions:** the remaining quick wins (§7.4), then S3/S7/S5.

---

## 7. Toolchain and dependency audit

### 7.1 Toolchain — unblocked by patching `v8`

Verified with `cargo +<ver> check -p lodestone_core --locked --keep-going` (latest run 2026-09-24):

| Toolchain | Result | Blocker(s) |
|-----------|--------|-----------|
| 1.70.0 (old pin) | ✅ builds | — |
| 1.93 / 1.96.0, unpatched | ❌ fails | `v8 0.73` — `E0080: assertion failed: size_of::<TypeId>() == size_of::<u64>()` **and** `time 0.3.20` — `E0282` |
| **1.96.0 (current pin)**, patched | ✅ builds, tests pass | — (121/124 tests pass; the 3 failures are PaperMC's live API changing shape, unrelated) |
| 1.98.1, patched | ✅ `cargo check` | not pinned: not installed via rustup here |

> [!IMPORTANT]
> Earlier versions of this note said moving off 1.70 required the Deno upgrade. **It does not.** The only `v8` breakage is one compile-time assertion in `TypeIdHasher` (`src/isolate.rs`), since `TypeId` became 128-bit in Rust 1.72. The fix (2026-09-24):
>
> - `vendor/v8/` holds the published `v8 0.73.0` crate (`Cargo.toml`, `build.rs`, `src/`, `tools/download_file.py`), wired in with `[patch.crates-io]` in the root `Cargo.toml`. `TypeIdHasher` now folds any `write`/`write_u64` input instead of assuming exactly one 64-bit write; the size assertion is removed. It still links the same prebuilt `librusty_v8` 0.73.0 binary.
> - `time 0.3.20 → 0.3.44` fixes the `E0282` inference error on rustc ≥ 1.80.
> - Macro tests (`macro_executor::tests`, which execute JS in V8) pass on 1.96.0.

New wall: the old `swc_common` (via `deno_ast`) breaks on `serde 1.0.229` (`unresolved import serde::__private`), so `serde` is held at **1.0.193**, which caps `time` at 0.3.44 (0.3.46+ needs the newer serde). See `CLAUDE.md`.

### 7.2 Rust dependencies — `cargo audit` (2026-09-24)

`cargo-audit 0.22.2` against the whole workspace `Cargo.lock`: **25 vulnerabilities, 22 unmaintained, 13 unsound.** The *Path* column comes from `cargo tree -p lodestone_core -i <crate>`. Being in the tree does not by itself mean the vulnerable code is reachable.

**Fixed since the first audit (2026-05-29):** openssl (0.10.45 → 0.10.75; 7 advisories), bytes, mio, tar, whoami, plus the h2 advisories that were known at the time. Separately, the `bollard = "*"` wildcard is gone.

#### Vulnerabilities in `lodestone_core`'s dependency tree

| Crate | Ver | Advisory | Issue | Fixed in | Path / blocker |
|-------|-----|----------|-------|----------|----------------|
| **rsa** | 0.7.2 | RUSTSEC-2023-0071 | Marvin Attack (timing key recovery) | **no fix** | `deno_crypto` — only reachable from macro WebCrypto. *(The first audit wrongly attributed this to sqlx.)* |
| aes-gcm | 0.10.1 | RUSTSEC-2023-0096 | Plaintext exposed on tag-verification failure | ≥ 0.10.3 | `deno_crypto` → Deno upgrade |
| ring | 0.16.20 | RUSTSEC-2025-0009 | AES panic with overflow checks | ≥ 0.17.12 | `deno_crypto`, rustls 0.20 |
| curve25519-dalek | 2.1.3, 3.2.0 | RUSTSEC-2024-0344 | Timing variability | ≥ 4.1.3 | `deno_crypto` / `x25519-dalek` |
| rustls | 0.20.8, 0.21.1 | RUSTSEC-2024-0336 | Infinite loop on network input | ≥ 0.21.11 | 0.20: `axum-server`, `playit-agent` v0.9, sqlx fork; 0.21: `deno_tls` |
| rustls-webpki | 0.100.1 | RUSTSEC-2023-0053, 2026-0098/0099/0104 | CPU DoS; name-constraint bypasses; CRL panic | ≥ 0.101.4 / 0.103.12 | via rustls |
| webpki | 0.22.0 | RUSTSEC-2023-0052 | CPU DoS in path building | ≥ 0.22.2 | via rustls 0.20 |
| tungstenite | 0.18.0 | RUSTSEC-2023-0065 | Remote DoS | ≥ 0.20.1 | `axum 0.6` → axum upgrade |
| h2 | 0.3.27 | RUSTSEC-2026-0258 | Unbounded empty DATA frames | ≥ 0.4.16 | hyper 0.14 → axum upgrade |
| time | 0.3.44 | RUSTSEC-2026-0009 | DoS via stack exhaustion | ≥ 0.3.47 | needs newer `serde`, which breaks old `swc_common` → Deno upgrade |
| time | 0.1.45 | RUSTSEC-2020-0071 | Potential segfault (`localtime_r`) | ≥ 0.2.23 | `chrono 0.4.22` (direct), `playit-agent` |
| tracing-subscriber | 0.3.16 | RUSTSEC-2025-0055 | ANSI-escape log injection | ≥ 0.3.20 | **quick win** (0.3.20 declares MSRV 1.65) |
| remove_dir_all | 0.5.3 | RUSTSEC-2023-0018 | TOCTOU link-following race | ≥ 0.8.0 | only via `tempdir` → **quick win** (§7.4) |
| crossbeam-epoch | 0.9.14 | RUSTSEC-2026-0204 | Invalid pointer deref in `fmt::Pointer` | ≥ 0.9.20 | `rayon` → **quick win** (0.9.20 declares MSRV 1.61) |
| idna | 0.2.3, 0.3.0 | RUSTSEC-2024-0421 | Punycode label confusion | ≥ 1.0.0 | old `url` versions → dependency modernization |

#### Desktop (Tauri) only

`quick-xml` 0.23.1 / 0.29.0 (RUSTSEC-2026-0194/0195, DoS). Low priority while the desktop app is not used. Most of the Tauri-side warnings clear with Tauri 1.4 → 2.x.

#### Unmaintained / unsound (35)

- **Unmaintained, in core's tree:** `ansi_term` (direct; `lib.rs:494`), `tempdir` (direct; tests only), `atty`, `instant`, `paste`, `proc-macro-error`, `bincode`, `rustls-pemfile`, `ring 0.16`, `rand_os`, `adler`, `smartstring`, `dlopen_derive`, and the `unic-*` family (via Deno/swc).
- **Unmaintained, desktop only:** `derivative`, `fxhash`, `kuchiki`, `safemem`.
- **Unsound:** `tokio 1.35.1` (RUSTSEC-2025-0023), `anyhow 1.0.71`, `memmap2 0.5.10`, `spin 0.9.6`, `lexical`/`lexical-core`, `keccak`, `anstream`, `atty`, `rand 0.8.5` (core) / `0.7.3` (desktop), `glib`, `cxx` (desktop).

### 7.3 Dashboard — `npm audit` (2026-09-24)

**314 advisories: 52 critical, 144 high, 104 moderate, 14 low**, across 2,454 packages. Most of them are in the build and dev toolchain (Storybook, Babel, webpack, ESLint, the `crypto-browserify` polyfill chain). The dashboard ships as a static export (`next build && next export`), so Next.js *server* advisories do not apply at runtime.

The runtime-relevant direct dependencies to look at first are **`axios`** (critical) and **`jsonwebtoken`** (high). After those, `react-router-dom`, `formik` and `yup`. The rest clears with the Next.js 13 → 14/15 and Storybook upgrades in Phase 3.

### 7.4 Remaining quick wins

Each item has to be verified with `cargo check` / `cargo test` after the bump, and `Cargo.lock` must still have `serde 1.0.193` (§7.1). With the toolchain at 1.96, MSRV is no longer the constraint; `openssl` can also move past 0.10.75 now.

1. `cargo update -p tracing-subscriber --precise 0.3.20` (or the newest version that still builds): clears RUSTSEC-2025-0055.
2. Replace the test-only `tempdir::TempDir::new(..)` calls (`util.rs`, `global_settings.rs`, `auth/user.rs`) with `tempfile::tempdir()`. `tempfile` is already a dependency. Then drop `tempdir`, which removes `remove_dir_all 0.5.3` along with it.
3. `cargo update -p crossbeam-epoch`: clears RUSTSEC-2026-0204.
4. Bump `chrono` and check whether `time 0.1` drops out of the tree. Newer chrono no longer needs it, but `playit-agent` also pulls chrono; confirm with `cargo tree -i time@0.1.45`.
5. Replace `ansi_term` (two call sites in `lib.rs`) with a maintained crate or plain ANSI codes.
6. Dashboard: bump `axios` and `jsonwebtoken` within their current majors where possible.

Beyond this list, the remaining advisories are gated on three larger projects: the **Deno stack** (`serde`/`time`, `deno_crypto` chain), the **axum 0.7 migration** (h2, tungstenite, rustls 0.20 via axum-server) and **retiring the sqlx fork**.

---

## Appendix — Key file references

| Area | File:line |
|------|-----------|
| Deno sandbox `allow_all` | `core/src/macro_executor.rs:350-352` |
| Global FS raw path | `core/src/handlers/global_fs.rs` (124, 163, 200, 236, 311, 346, 382, 417) |
| Unauthenticated monitor WS | `core/src/handlers/monitor.rs:23`, route `:86` |
| Unauthenticated playit.gg | routes `core/src/handlers/playitgg.rs:11-18`, handlers `core/src/playitgg/mod.rs` |
| CORS `Any` | `core/src/lib.rs:661-671` |
| Password hash unwraps | `core/src/auth/hashed_password.rs:14,44` |
| WS serialize unwraps | `core/src/handlers/events.rs:195,255`, `core/src/handlers/monitor.rs:51,67` |
| App state / locking | `core/src/lib.rs:103-122` |
| Weak RNG | `core/Cargo.toml` (`rand = "0.6.5"`), `core/src/util.rs:455` |
| Forked deps | `core/Cargo.toml` (sqlx, safe_path_subset, playit-agent ×2) |
| Toolchain pin | `rust-toolchain.toml`; rationale in `CLAUDE.md` |
