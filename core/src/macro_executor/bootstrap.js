// ESM entry point of the `lodestone` extension (ext:lodestone/bootstrap.js).
//
// Runs once per macro, before any macro code. It builds the global scope
// macros see:
//   - web APIs from deno_web / deno_fetch (console, timers, URL, fetch, ...);
//   - a small `Deno` namespace (see DENO_API below);
//   - the compat shim `Deno[Deno.internal].core.{ops,opAsync}` that the
//     Lodestone glue and lodestone-macro-lib use to call Lodestone's ops.
//
// deno_web / deno_fetch / deno_fs ship their JS as lazily loaded scripts:
// nothing runs until `core.loadExtScript(...)` is called.
import { core, internals, primordials } from "ext:core/mod.js";

const {
  ObjectFreeze,
  ObjectDefineProperty,
  ObjectPrototypeIsPrototypeOf,
  SafeArrayIterator,
  Symbol,
  SymbolHasInstance,
  Error,
} = primordials;

// ---------------------------------------------------------------- web APIs --
core.loadExtScript("ext:deno_webidl/00_webidl.js");
const consoleMod = core.loadExtScript("ext:deno_web/01_console.js");
const url = core.loadExtScript("ext:deno_web/00_url.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const event = core.loadExtScript("ext:deno_web/02_event.js");
const abortSignal = core.loadExtScript("ext:deno_web/03_abort_signal.js");
const messagePort = core.loadExtScript("ext:deno_web/13_message_port.js");
const loadTimers = () => core.loadExtScript("ext:deno_web/02_timers.js");
const loadEncoding = () => core.loadExtScript("ext:deno_web/08_text_encoding.js");
const loadBase64 = () => core.loadExtScript("ext:deno_web/05_base64.js");
const loadStreams = () => core.loadExtScript("ext:deno_web/06_streams.js");
const loadFile = () => core.loadExtScript("ext:deno_web/09_file.js");
const performanceMod = core.loadExtScript("ext:deno_web/15_performance.js");
performanceMod.setTimeOrigin();
// 26_fetch.js loads ext:deno_telemetry/*.ts unless these internals exist.
// We don't ship deno_telemetry (OpenTelemetry deps, and TS that would need an
// extension transpiler), so install a "tracing disabled" stub instead.
const DID_NOT_ENTER = Symbol("DID_NOT_ENTER");
const noop = () => {};
internals.__telemetry = {
  TRACING_ENABLED: false,
  PROPAGATORS: [],
  DID_NOT_ENTER,
  builtinTracer: () => {
    throw new Error("telemetry disabled");
  },
  ContextManager: { active: noop },
  enterSpan: () => DID_NOT_ENTER,
  exitSpan: noop,
};
internals.__telemetryUtil = {
  updateSpanFromClientResponse: noop,
  updateSpanFromError: noop,
  updateSpanFromRequest: noop,
};
// fetch pulls in streams (~200 KB of JS); load it on first use.
const loadHeaders = () => core.loadExtScript("ext:deno_fetch/20_headers.js");
const loadRequest = () => core.loadExtScript("ext:deno_fetch/23_request.js");
const loadResponse = () => core.loadExtScript("ext:deno_fetch/23_response.js");
const loadFetch = () => core.loadExtScript("ext:deno_fetch/26_fetch.js");

core.setReportExceptionCallback(event.reportException);

// Like deno_runtime: log/info/debug go to stdout, warn/error to stderr.
const console = new consoleMod.Console((msg, level) =>
  core.print(msg, level > 1)
);

core.defineGlobalProperties(globalThis, {
  console: core.propNonEnumerable(console),
  URL: core.propNonEnumerable(url.URL),
  URLSearchParams: core.propNonEnumerable(url.URLSearchParams),
  DOMException: core.propNonEnumerable(DOMException),
  Event: core.propNonEnumerable(event.Event),
  EventTarget: core.propNonEnumerable(event.EventTarget),
  AbortController: core.propNonEnumerable(abortSignal.AbortController),
  AbortSignal: core.propNonEnumerable(abortSignal.AbortSignal),
  structuredClone: core.propWritable(messagePort.structuredClone),
  performance: core.propWritable(performanceMod.performance),
  Blob: core.propNonEnumerableLazyLoaded((m) => m.Blob, loadFile),
  File: core.propNonEnumerableLazyLoaded((m) => m.File, loadFile),
  ReadableStream: core.propNonEnumerableLazyLoaded((m) => m.ReadableStream, loadStreams),
  WritableStream: core.propNonEnumerableLazyLoaded((m) => m.WritableStream, loadStreams),
  TransformStream: core.propNonEnumerableLazyLoaded((m) => m.TransformStream, loadStreams),
  TextEncoder: core.propNonEnumerableLazyLoaded((m) => m.TextEncoder, loadEncoding),
  TextDecoder: core.propNonEnumerableLazyLoaded((m) => m.TextDecoder, loadEncoding),
  atob: core.propWritableLazyLoaded((m) => m.atob, loadBase64),
  btoa: core.propWritableLazyLoaded((m) => m.btoa, loadBase64),
  setTimeout: core.propWritableLazyLoaded((m) => m.setTimeout, loadTimers),
  setInterval: core.propWritableLazyLoaded((m) => m.setInterval, loadTimers),
  clearTimeout: core.propWritableLazyLoaded((m) => m.clearTimeout, loadTimers),
  clearInterval: core.propWritableLazyLoaded((m) => m.clearInterval, loadTimers),
  Headers: core.propNonEnumerableLazyLoaded((m) => m.Headers, loadHeaders),
  Request: core.propNonEnumerableLazyLoaded((m) => m.Request, loadRequest),
  Response: core.propNonEnumerableLazyLoaded((m) => m.Response, loadResponse),
  fetch: core.propWritable(function fetch(...args) {
    return loadFetch().fetch(...new SafeArrayIterator(args));
  }),
  // queueMicrotask is already installed by deno_core.
});

// ------------------------------------------------------------ Deno.errors --
// The same classes as deno_runtime's 01_errors.js. Registering them makes
// errors thrown by deno_fs / deno_fetch ops (class "NotFound",
// "AlreadyExists", ...) instances of these classes.
function makeErrorClass(name) {
  const C = class extends Error {
    constructor(msg, opts) {
      super(msg, opts);
      this.name = name;
    }
  };
  ObjectDefineProperty(C, "name", { value: name });
  return C;
}
const errorNames = [
  "NotFound", "ConnectionRefused", "ConnectionReset", "ConnectionAborted",
  "NotConnected", "AddrInUse", "AddrNotAvailable", "BrokenPipe",
  "PermissionDenied", "AlreadyExists", "InvalidData", "TimedOut",
  "WouldBlock", "WriteZero", "UnexpectedEof", "Http", "Busy", "NotSupported",
  "FilesystemLoop", "IsADirectory", "NetworkUnreachable", "NotADirectory",
];
const errors = { __proto__: null };
for (const name of new SafeArrayIterator(errorNames)) {
  errors[name] = makeErrorClass(name);
  core.registerErrorClass(name, errors[name]);
}
// Permission failures are class "NotCapable" (Deno 2). Older macros catch
// `Deno.errors.PermissionDenied`, so `instanceof PermissionDenied` accepts
// NotCapable too.
const { NotCapable, BadResource, Interrupted } = core;
const PermissionDenied = errors.PermissionDenied;
ObjectDefineProperty(PermissionDenied, SymbolHasInstance, {
  value: (v) =>
    ObjectPrototypeIsPrototypeOf(PermissionDenied.prototype, v) ||
    ObjectPrototypeIsPrototypeOf(NotCapable.prototype, v),
});
errors.NotCapable = NotCapable;
errors.BadResource = BadResource;
errors.Interrupted = Interrupted;
for (const [name, kind] of new SafeArrayIterator([
  ["DOMExceptionOperationError", "OperationError"],
  ["DOMExceptionNotSupportedError", "NotSupportedError"],
  ["DOMExceptionNetworkError", "NetworkError"],
  ["DOMExceptionAbortError", "AbortError"],
  ["DOMExceptionInvalidCharacterError", "InvalidCharacterError"],
  ["DOMExceptionDataError", "DataError"],
  ["DOMExceptionInvalidStateError", "InvalidStateError"],
  ["DOMExceptionSyntaxError", "SyntaxError"],
])) {
  core.registerErrorBuilder(name, (msg) => new DOMException(msg, kind));
}

// ---------------------------------------------------------- compat shim --
const info = core.ops.lodestone_bootstrap_info();

// Only Lodestone's own ops (the names come from the Rust side). core.ops also
// holds every deno_* op, which macros must not reach directly.
const lodestoneOps = { __proto__: null };
for (const name of new SafeArrayIterator(info.ops)) {
  lodestoneOps[name] = core.ops[name];
}
ObjectFreeze(lodestoneOps);

const internalSymbol = Symbol("Deno.internal");
const internal = ObjectFreeze({
  core: ObjectFreeze({
    ops: lodestoneOps,
    // Old glue: `await core.opAsync("next_event", ...)`. Since deno_core
    // 0.243 an async op returns a promise when called, so this is an alias.
    opAsync: (name, ...args) => lodestoneOps[name](...new SafeArrayIterator(args)),
  }),
});

// ------------------------------------------------------------- DENO_API --
// Everything macros get on `globalThis.Deno`. To re-enable an API, add it
// here (and register the extension that implements it in
// macro_executor.rs). Deliberately absent: Deno.run / Command / env / exit /
// core, and the rest of deno_runtime's namespace; the extensions behind
// run/Command/env/exit are not even loaded.
const fs = core.loadExtScript("ext:deno_fs/30_fs.js");
const fsApi = [
  "readTextFile", "readTextFileSync", "writeTextFile", "writeTextFileSync",
  "readFile", "readFileSync", "writeFile", "writeFileSync",
  "readDir", "readDirSync", "stat", "statSync", "lstat", "lstatSync",
  "mkdir", "mkdirSync", "remove", "removeSync", "rename", "renameSync",
  "copyFile", "copyFileSync", "realPath", "realPathSync",
  "readLink", "readLinkSync", "symlink", "symlinkSync",
  "utime", "utimeSync", "truncate", "truncateSync",
  "makeTempDir", "makeTempDirSync", "makeTempFile", "makeTempFileSync",
  "open", "openSync", "create", "createSync",
];
const denoNs = {
  args: ObjectFreeze([...new SafeArrayIterator(info.args)]),
  build: ObjectFreeze({ os: info.os, arch: info.arch }),
  errors: ObjectFreeze(errors),
  // The fs API resolves relative paths against the macro's root (the
  // instance directory), so that is the cwd (std/path uses Deno.cwd()).
  cwd: () => {
    if (info.root === null) {
      throw new NotCapable("This macro has no filesystem access");
    }
    return info.root;
  },
  inspect: consoleMod.inspect,
  internal: internalSymbol,
  [internalSymbol]: internal,
};
for (const name of new SafeArrayIterator(fsApi)) denoNs[name] = fs[name];
// ------------------------------------------------------------------------

// Replace the `{ core }` object deno_core put on globalThis.Deno: macros must
// not see Deno.core (the full op table, the resource table, ...).
ObjectDefineProperty(globalThis, "Deno", {
  value: ObjectFreeze(denoNs),
  writable: false,
  enumerable: false,
  configurable: false,
});
// Hide the bootstrap object. core.loadExtScript keeps working: deno_core
// re-installs a captured copy while it lazily loads a script.
delete globalThis.__bootstrap;
