//! Generates two tables of files embedded in the binary:
//!
//! 1. The TS/JS glue that macros import. Macros import it by
//!    `https://raw.githubusercontent.com/...` URL; the module loader serves
//!    those URLs from the embedded copies (see `src/embedded_glue.rs`), so the
//!    glue always matches this build and is never fetched from GitHub.
//! 2. The JS of the Deno extensions the macro runtime uses (deno_web,
//!    deno_fetch, ..., and our own `bootstrap.js`). deno_core's `extension!`
//!    only records these files by absolute path and, without a V8 snapshot,
//!    reads them from disk whenever a runtime is created: a binary would only
//!    work on the machine that built it. `macro_executor` swaps each path for
//!    the copy embedded here (see `src/macro_executor/extension.rs`).

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::{env, fs};

/// Directories, relative to `core/`, whose files macros can import.
const GLUE_DIRS: &[&str] = &[
    "src/deno_ops",
    "deno_bindings",
    "src/implementations/generic/js",
];

/// Extensions of the files that are embedded.
const GLUE_EXTENSIONS: &[&str] = &["ts", "js", "json"];

fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|entry| entry.expect("failed to read directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, files);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| GLUE_EXTENSIONS.contains(&ext))
        {
            // always use `/`, since the keys are matched against URL paths
            let relative = path
                .strip_prefix(root)
                .expect("glue file is under the manifest dir")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push(relative);
        }
    }
}

/// Crates whose extension JS the macro runtime loads.
const DENO_EXTENSION_CRATES: &[&str] = &[
    "deno_webidl",
    "deno_web",
    "deno_net",
    "deno_io",
    "deno_fs",
    "deno_fetch",
];

/// Our own extension JS, relative to `core/`.
const OWN_EXTENSION_FILES: &[&str] = &["src/macro_executor/bootstrap.js"];

/// `path` with `.` and repeated separators removed; the key the runtime looks
/// paths up by (deno_core builds them with `concat!`).
fn normalize(path: &Path) -> String {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// Source directory of each of `names`, from `cargo metadata`.
fn crate_dirs(manifest_dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest_dir.join("Cargo.toml"))
        .output()
        .expect("failed to run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata printed invalid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata has no packages");
    names
        .iter()
        .map(|name| {
            let mut found: Vec<PathBuf> = packages
                .iter()
                .filter(|p| p["name"] == *name)
                .filter_map(|p| p["manifest_path"].as_str())
                .map(|m| Path::new(m).parent().unwrap().to_path_buf())
                .collect();
            assert!(
                found.len() == 1,
                "expected one {name} package, found {found:?}"
            );
            found.remove(0)
        })
        .collect()
}

/// Every `.js`/`.ts` file under `dir` that deno_core can embed (7-bit ASCII).
fn extension_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|entry| entry.expect("failed to read directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            extension_sources(&path, files);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "js" || ext == "ts")
            && fs::read(&path).is_ok_and(|bytes| bytes.is_ascii())
        {
            files.push(path);
        }
    }
}

fn write_extension_sources(manifest_dir: &Path) {
    let mut files: Vec<PathBuf> = OWN_EXTENSION_FILES
        .iter()
        .map(|f| manifest_dir.join(f))
        .collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    // a Deno crate upgrade changes the lock file
    println!("cargo:rerun-if-changed=../Cargo.lock");
    for dir in crate_dirs(manifest_dir, DENO_EXTENSION_CRATES) {
        extension_sources(&dir, &mut files);
    }
    let mut out = String::from(
        "/// The embedded copy of the extension source at `path` (normalized).
         pub fn embedded_extension_source(path: &str) -> Option<::deno_core::FastStaticString> {
             Some(match path {
",
    );
    for file in &files {
        let absolute = file.to_string_lossy();
        out.push_str(&format!(
            "        {:?} => ::deno_core::ascii_str_include!({:?}),
",
            normalize(file),
            absolute
        ));
    }
    out.push_str(
        "        _ => return None,
    })
}
",
    );
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("extension_sources.rs");
    fs::write(&out_path, out)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    write_extension_sources(&manifest_dir);
    let mut files = Vec::new();
    for dir in GLUE_DIRS {
        // a directory is scanned recursively for changes
        println!("cargo:rerun-if-changed={dir}");
        collect(&manifest_dir, &manifest_dir.join(dir), &mut files);
    }
    println!("cargo:rerun-if-changed=build.rs");
    // sorted, so that lookups can binary search
    files.sort();

    let mut out = String::from(
        "/// `(path under core/, contents)` of every embedded glue file, sorted by path.\n\
         pub static EMBEDDED_GLUE: &[(&str, &str)] = &[\n",
    );
    for relative in &files {
        let absolute = manifest_dir.join(relative);
        out.push_str(&format!(
            "    ({relative:?}, include_str!({:?})),\n",
            absolute.to_string_lossy()
        ));
    }
    out.push_str("];\n");
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded_glue.rs");
    fs::write(&out_path, out)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));
}
