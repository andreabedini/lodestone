# Macro runtime — notes for macro authors

- **Applies to:** builds from 2026-09-24 on (the Deno upgrade; background in
  `docs/deno-upgrade-scoping.md`)

Macros are TypeScript or JavaScript modules. Lodestone runs them on an embedded
V8 (`deno_core`). It is **not** the Deno CLI: a macro gets web APIs, a small
`Deno` namespace, and Lodestone's API, and nothing else.

## What a macro can use

- **Web APIs:** `console`, `setTimeout`/`setInterval` (and `clear*`),
  `queueMicrotask`, `fetch`, `Request`, `Response`, `Headers`, `URL`,
  `URLSearchParams`, `TextEncoder`/`TextDecoder`, `atob`/`btoa`,
  `structuredClone`, `AbortController`/`AbortSignal`, `Event`/`EventTarget`,
  `DOMException`, `performance`, `Blob`/`File`, and
  `ReadableStream`/`WritableStream`/`TransformStream`. Console output goes to
  Lodestone's stdout (`log`, `info`, `debug`) and stderr (`warn`, `error`).
- **`Deno`:** `Deno.args`, `Deno.build` (`os`, `arch`), `Deno.errors`,
  `Deno.inspect`, `Deno.cwd()`, and the file-system functions:
  `readTextFile`, `writeTextFile`, `readFile`, `writeFile`, `readDir`, `stat`,
  `lstat`, `mkdir`, `remove`, `rename`, `copyFile`, `realPath`, `readLink`,
  `symlink`, `utime`, `truncate`, `makeTempDir`, `makeTempFile`, `open`,
  `create` (each with its `…Sync` variant).
- **Lodestone's API:** the glue modules (`prelude.ts`, `events.ts`,
  `instance_control.ts`) and `lodestone-macro-lib`, unchanged. The glue is
  served from the copy built into Lodestone, not from GitHub.
- **Imports:** local files and `http(s)` URLs (for example `deno.land/std`),
  TypeScript, TSX/JSX, JavaScript and JSON. No `npm:`, `node:` or `jsr:`.

## Files: only the instance directory

A macro can read and write **only its instance's directory**:

- `Deno.cwd()` is the instance directory, and relative paths resolve against
  it, so `Deno.readTextFile("server.properties")` reads the instance's file.
- Anything outside is denied: absolute paths elsewhere, `../` paths, and
  symlinks inside the instance directory that point outside it.
- A denied access throws an error that is `instanceof
  Deno.errors.PermissionDenied` (its class is `NotCapable`, as in Deno 2).
- Code with no instance (such as reading a generic instance's setup manifest
  before the instance exists) has no file access, and `Deno.cwd()` throws.

`deno.land/std` fs helpers, such as `fs/copy.ts` for backups, work inside the
instance directory.

## What was removed

These existed when macros ran on `deno_runtime` and are gone:

- `Deno.run`, `Deno.Command` (no subprocesses), `Deno.env`, `Deno.exit`
  (it used to stop the whole Lodestone process);
- `Deno.core` (use the glue, or `Deno[Deno.internal].core.ops`, which only
  holds Lodestone's ops);
- the rest of the Deno namespace: networking sockets (`Deno.connect`,
  `Deno.listen`), `Deno.serve`, FFI, KV, and so on;
- `crypto` / WebCrypto, `WebSocket`, `localStorage`, workers.

Ask for an API if you need it; most can be re-enabled.

## JSON imports

Use the standard syntax:

```ts
import data from "./data.json" with { type: "json" };
```

The old `assert { type: "json" }` still works in `.ts` files, but it is a
`SyntaxError` in plain `.js` files. Switch to `with`.

## Network

`fetch` can reach any host. Macros are not a security boundary: only run
macros you trust.
