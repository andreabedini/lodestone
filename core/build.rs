//! Generates the table of TS/JS glue files that are embedded in the binary.
//!
//! Macros import this glue by `https://raw.githubusercontent.com/...` URL. The
//! module loader serves those URLs from the embedded copies (see
//! `src/embedded_glue.rs`), so the glue always matches this build and is never
//! fetched from GitHub.

use std::path::{Path, PathBuf};
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

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
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
