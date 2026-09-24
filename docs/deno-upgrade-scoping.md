# Deno stack upgrade — scoping

- **Date:** 2026-09-24
- **Status:** scoping only, no code changed yet
- **Related:** `docs/codebase-assessment.md` §4 Phase 3 item 13, §7.1; S1 (macro sandbox)

## 1. Summary

We currently run `deno_core 0.190` / `deno_runtime 0.116` / `deno_ast 0.27` (mid-2023), plus a local `vendor/v8` patch so that it builds on Rust 1.96. The upgrade is worth doing:

- it clears RUSTSEC-2026-0009 (`time`), because it lifts the old-`serde` cap;
- it removes `vendor/v8`;
- it clears the `deno_crypto` advisories.

**Recommendation:** move to **`deno_core 0.412` + `deno_ast 0.53` + `deno_web`/`deno_webidl`**, with our own `extension!` entry point. **Do not use `deno_runtime`.** We would embed the JS glue and add a compatibility shim so that existing macros keep working.

Estimated effort: **about 1.5–2 focused weeks**, most of it in the runtime/glue rewrite and in testing, not in the op conversion.

Three product decisions need an answer before work starts (§6).

## 2. Target versions

Checked against the crates.io index on 2026-09-24. `deno_core` 0.412 was built and run on rustc 1.96.0 in a scratch project, using an `#[op2]` async serde op, a string op, a fast op with an error, and `extension!` with state.

| crate | now | target | notes |
|---|---|---|---|
| deno_core | 0.190.0 | **0.412.0** | uses the `deno_v8` 0.4 facade, which wraps `v8 150.4.0`; edition 2024 |
| deno_ast | 0.27.1 | **0.53.3** | `swc_common 17.0.1`; no `serde::__private` |
| deno_web | (via runtime) | **0.290** | now includes console, URL and timers (`deno_console`/`deno_url` are frozen at 0.222 and merged in) |
| deno_webidl | (via runtime) | **0.259** | required by deno_web |
| deno_fs / deno_io | (via runtime) | 0.169 / 0.169 | only if we keep `Deno.*` fs (decision A) |
| deno_fetch | (via runtime) | 0.283 | only if we keep `fetch` (decision B) |
| deno_runtime | 0.116.0 | **drop** | see §3 |
| deno_graph | 0.49.0 | **drop** | only used in the copied `deno_errors` classifier |
| import_map | 0.15.0 | **drop** | same |

None of these crates declares a `rust_version`. Upstream builds them with 1.91–1.95, so 1.96 is fine.

Effects on the resolved tree:
- `serde` goes to 1.0.229 and `time` to 0.3.55, which fixes RUSTSEC-2026-0009.
- v8 150 handles 128-bit `TypeId` itself, so `vendor/v8` and the `[patch.crates-io]` entry can be deleted.

`denoland/deno_core` was archived on 2026-02-27. Core, ops, serde_v8 and v8 now live in `denoland/deno` under `libs/`.

## 3. Why not `deno_runtime`

1. **It hides our ops from user JS.** Since 0.144, `99_main.js` removes every op that is not on Deno's allowlist from `Deno[Deno.internal].core.ops`. All of our glue reads ops from there.
2. **It conflicts with sqlite linking.** `deno_runtime ≥ 0.261` pulls `libsqlite3-sys 0.38`, and no sqlx release accepts that version. Cargo allows only one package that links `sqlite3`, so this blocks retiring the sqlx fork.
   - Found from resolution data; not built.
   - Versions 0.230–0.260 would work, but they are already stale.
3. **Weight and attack surface:** 697 packages, against 165 for `deno_core` alone. It has no features to trim, so deno_node, deno_kv, webgpu, ffi, napi and deno_crypto are all mandatory. It also exposes things we don't want, e.g. `Deno.exit`, which today kills the whole Lodestone process from a macro.
4. **Its embedding API churns heavily.** `WorkerServiceOptions` is generic over npm/node resolver types even when you don't use them.

What we lose by dropping it is the ready-made `globalThis.Deno` namespace (`Deno.args`, `Deno.errors`, `Deno.build`, `Deno.*` fs). We would provide the subset we choose to support (decision A).

## 4. What has to change

### 4.1 Current usage (inventory)

| area | where | size |
|---|---|---|
| Ops, all legacy `#[op]`, serde in/out, `anyhow::Error` | `deno_ops/events` (13), `deno_ops/instance_control` (26), `deno_ops/prelude` (1), `generic/bridge/procedure_call.rs` (3), tests (2) | 46 + 2 |
| Extensions (`Extension::builder`, no ESM) | the 3 `register_*` fns, `generic/macro.rs:30`, `generic/mod.rs:68`, test | 6 |
| Runtime (`MainWorker::from_options`, `bootstrap`, `execute_script` ×2, `execute_main_module`, `run_event_loop(false)`) | `macro_executor.rs:304-549` | ~250 lines |
| Module loader (file + http via reqwest, deno_ast transpile) | `macro_executor.rs:74-216` | ~140 lines |
| Error classifier copied from the Deno CLI (deno_graph / import_map) | `macro_executor.rs:1210-1275` | delete |
| JS glue (`Deno[Deno.internal].core.ops` / `core.opAsync`) | `events.ts`, `instance_control.ts`, `prelude.ts`, `generic/js/main/libs/procedure_bridge.ts` | ~510 lines TS |
| Threading: one OS thread per macro, `LocalSet` on the shared runtime handle, kill via `IsolateHandle` | `macro_executor.rs:331-514` | unchanged in principle |

Total Rust surface: about 2,750 lines across 7 files.

### 4.2 API changes and how they map to our code

| change | landed in | our work |
|---|---|---|
| `#[op]` → `#[op2]`, with `#[serde]`/`#[string]` annotations; fast-compatible ops must be marked `(fast)` | 0.192 / `#[op]` removed in 0.225 | mechanical, 46 ops |
| `core.opAsync("x")` → `core.ops.x()` returns a promise | 0.243 | glue rewrite, or the compat shim (§5) |
| `Extension::builder` → `extension!(…, ops=[…], esm_entry_point, esm=[…], state=…)`; `init_ops_and_esm` → `init()` | 0.258 / 0.344 | 6 sites |
| `anyhow::Error` is no longer a valid op error; use `JsErrorBox` or `#[derive(JsError)]` | 0.329 | one `MacroOpError` type + `From` impls; `get_error_class_fn` goes away |
| `ModuleLoader`: `resolve` → `Result<_, JsErrorBox>`; `load(spec, Option<&ModuleLoadReferrer>, ModuleLoadOptions) -> ModuleLoadResponse`; `ModuleSource::new(…, ModuleSourceCode::String(..), &spec, None)` | 0.246–0.256+ | port from the upstream `examples/ts_module_loader.rs`, which is nearly a drop-in replacement |
| `deno_ast` transpile: `transpile(&TranspileOptions, &TranspileModuleOptions, &EmitOptions)?.into_source().text` | 0.53 | a few lines |
| `run_event_loop(PollEventLoopOptions)`, `load_main_es_module`, `mod_evaluate` → `Result<(), CoreError>`, `ModuleCode` removed, `FastString::Owned` gone | 0.232–0.267 | small |
| `v8::IsolateHandle::terminate_execution` | unchanged | none; replace the `"Uncaught Error: execution terminated"` string match with a flag set by `abort_macro` |

## 5. Proposed design

- **Crates:** `deno_core`, `deno_ast` (`transpiling`), `deno_webidl` and `deno_web`. Optionally `deno_fs` + `deno_io` (decision A) and `deno_fetch` (decision B).
- **One `lodestone` extension** built with `extension!`:
  - it holds all ops, plus `EventBroadcaster` / `ProcedureBridge` state passed in as options;
  - its `esm_entry_point` installs the globals macros see: `console`, timers, `URL`, `TextEncoder`/`TextDecoder` from deno_web, and a small `globalThis.Deno` with `args`, `build`, `errors`, and the fs subset if decision A says so.
  - Bootstrap values that are currently injected as script text (`__macro_pid`, `__instance_uuid`, and `LodestoneConfig`) keep working the same way.
- **Compatibility shim for existing macros:** the entry point also defines
  `Deno[Deno.internal] = { core: { ops: <lodestone ops>, opAsync: (name, ...a) => ops[name](...a) } }`.
  - The glue on upstream `dev`, and therefore `lodestone-macro-lib`, keeps working unchanged. This only works because we control the bootstrap now that `deno_runtime` is gone.
  - Ops keep their current names (`next_event`, …). Check them against deno_core's built-in op names, because duplicates panic.
- **Embed the glue:** ship `events.ts`, `instance_control.ts`, `prelude.ts` and the generic `js/main` tree inside the binary. The module loader maps `https://raw.githubusercontent.com/Lodestone-Team/lodestone/dev/core/src/…` and `…/lodestone-macro-lib/main/…` to the embedded copies.
  - This removes a live supply-chain dependency on a repo we don't control.
  - It also decouples us from upstream's glue.
- **Module loader:** port it from `ts_module_loader.rs`. Keep file and https; still no npm:, node: or jsr: (unchanged from today). Optionally add an on-disk cache for remote modules later.
- **Permissions (S1):** there is no `deno_runtime` permission container. Our own ops don't need one because they go through `app_state()`. If we add `deno_fs`, we implement its permission trait scoped to the instance directory. That closes the fs part of S1 as a by-product; it's not a separate project.
- **Remove:** `deno_runtime`, `deno_graph`, `import_map`, the `deno_errors` module, `vendor/v8` + patch, the dead `add_default_permissions` and the unused `permissions` parameter on `spawn`.

## 6. Decisions needed

- **A. `Deno.*` filesystem API.**
  - The wiki's headline example (auto-backup) uses `deno.land/std/fs/copy`, which needs `Deno.stat`, `mkdir`, `copyFile`, `readDir`, …
  - Options:
    1. `deno_fs` + shim, scoped to the instance dir (recommended);
    2. a few of our own fs ops;
    3. drop it, which breaks that example.
- **B. Network (`fetch`, `WebSocket`).**
  - Nothing in-repo uses these. The macro-lib `discord_bot` example and user-supplied generic ("atom") instance code might.
  - Options: add `deno_fetch` now, or later on demand.
- **C. How far to promise compatibility.**
  - The wiki says a macro can "do anything a normal Deno program can do". It also marks the API as beta, with breaking changes allowed at any time.
  - Proposal: keep the compat shim and `Deno.args`/fs, drop `Deno.run`/`Command`/`env`/`exit`, and update the wiki.

## 7. Plan

**Phase 0 — prepare, on the current stack (about 2–3 days)**

1. Add tests that run JS through `MacroExecutor` for each op category: events, instance control (against a test instance), prelude, and the procedure bridge. Also add tests for the loader (file, TS, JSON, http) and for abort.
2. Fix the bugs found while scoping:
   - `send_command` is called with 2 args from TS but takes 3 in Rust (`instance_control.ts:44`);
   - `DashMap` `Ref`s are held across `.await` in `instance_control`;
   - termination is detected by comparing error strings.
3. Embed the glue and add the URL redirect in the current loader. This can ship on its own.

**Phase 1 — dependencies (about 0.5 day)**

4. Swap the crates as in §2 and delete `vendor/v8` + the patch.
5. `cargo update` `serde` and `time`, and confirm `time ≥ 0.3.47`.
6. Check that the `libsqlite3-sys` link does not conflict with the sqlx fork (it shouldn't without `deno_runtime`, but verify).

**Phase 2 — ops and extension (about 1–2 days)**

7. Convert the 46 ops to `#[op2]`, add the `MacroOpError` type, and merge the 6 builders into `extension!`.

**Phase 3 — runtime, loader, shim (about 3–4 days)**

8. Replace `MainWorker` with `JsRuntime` plus our extension, the ESM entry point, the `Deno` shim and the compat shim.
9. Port the loader.
10. Handle termination with a flag.
11. Optionally add `deno_fs` with scoped permissions (decision A).

**Phase 4 — verify and document (about 1–2 days)**

12. Run the Phase 0 tests and the auto-backup example end to end, plus one macro-lib example against the embedded glue.
13. Run `cargo audit`.
14. Update `CLAUDE.md` (toolchain section, serde cap), `docs/codebase-assessment.md` (§7, S1) and the wiki.

## 8. Risks

- **The Phase 0 tests are the real safety net.** Today only 2 tests exercise the runtime.
- **The `deno_web` wiring is unverified.** The scratch build ran `deno_core` alone. Do a spike on it in Phase 3 before committing to the design.
- **deno_core's embedding API still churns.** Upstream says it is "subject to rapid and breaking changes", and since the repo merge releases come with the Deno CLI. Pin exact versions and plan periodic bumps.
- **Global op names:** a clash with a built-in op panics at startup. This shows up immediately in tests.
- **Remote glue on upstream `dev` can change at any time.** Embedding it in Phase 0 removes that risk.

## 9. References

- op2 reference: https://docs.rs/deno_core/latest/deno_core/attr.op2.html (see also `valid_args.md` / `valid_retvals.md` in the crate)
- Embedding guide: https://deno.com/blog/roll-your-own-javascript-runtime
- Loader template: `examples/ts_module_loader.rs` in the `deno_core` 0.412 crate; source at https://github.com/denoland/deno/tree/main/libs/core
- Ops hidden from `Deno[Deno.internal]`: https://github.com/denoland/deno/discussions/18591
- op2 migration tracking: https://github.com/denoland/deno/issues/19915
- A reference for using `deno_core` + extensions without `deno_runtime`: `rustyscript` (not usable as a dependency, because it is about 60 `deno_core` versions behind)
