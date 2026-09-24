//! Permissions for the `Deno.*` fs API (deno_fs) and `fetch` (deno_fetch).
//!
//! deno_fs and deno_fetch look up one `PermissionsContainer` in `OpState` and
//! call `check_open(..)` / `check_net_url(..)` on it for every op. A macro gets
//! read and write access to its root directory (the instance directory) and
//! nothing else; network access is unrestricted; there are no prompts.
//!
//! Stock Deno compares the *lexically normalised* path against the allow
//! list and only canonicalises afterwards, so a symlink inside the root that
//! points outside it escapes. [`ScopedParser`] closes that: every queried path
//! is (1) resolved against the root if relative and normalised (`..`
//! removed), then (2) canonicalised (symlinks resolved, for the part that
//! exists). If the canonical path leaves the root, the *canonical* path is
//! handed to the permission check, which then fails with the usual
//! `NotCapable` error. Otherwise the normalised path is used, so that
//! no-follow ops (lstat, readLink, removing a symlink) keep their semantics.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use deno_permissions::{
    AllowRunDescriptorParseResult, DenyRunDescriptor, EnvDescriptor, EnvDescriptorParseError,
    FfiDescriptor, ImportDescriptor, NetDescriptor, NetDescriptorParseError, PathQueryDescriptor,
    PathResolveError, PermissionDescriptorParser, Permissions, PermissionsContainer,
    PermissionsOptions, ReadDescriptor, RunDescriptorParseError, RunQueryDescriptor,
    RuntimePermissionDescriptorParser, SpecialFilePathQueryDescriptor, SysDescriptor,
    SysDescriptorParseError, WriteDescriptor,
};

type Inner = RuntimePermissionDescriptorParser<sys_traits::impls::RealSys>;

#[derive(Debug)]
struct ScopedParser {
    inner: Inner,
    /// Canonical root. Relative paths are resolved against it.
    root: PathBuf,
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Canonicalise the longest existing ancestor, then re-append the rest.
fn canonicalize_maybe_not_exists(p: &Path) -> std::io::Result<PathBuf> {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut c) => {
                for part in rest.iter().rev() {
                    c.push(part);
                }
                return Ok(c);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match (existing.file_name(), existing.parent()) {
                    (Some(name), Some(parent)) => {
                        rest.push(name.to_os_string());
                        existing = parent.to_path_buf();
                    }
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

impl ScopedParser {
    fn scoped(&self, path: &Path) -> Result<PathBuf, PathResolveError> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let abs = normalize(&abs);
        let canon = canonicalize_maybe_not_exists(&abs).map_err(PathResolveError::Canonicalize)?;
        Ok(if canon.starts_with(&self.root) {
            abs
        } else {
            canon
        })
    }
}

impl PermissionDescriptorParser for ScopedParser {
    fn parse_path_query<'a>(
        &self,
        path: Cow<'a, Path>,
    ) -> Result<PathQueryDescriptor<'a>, PathResolveError> {
        if path.as_os_str().is_empty() {
            return Err(PathResolveError::EmptyPath);
        }
        let scoped = self.scoped(&path)?;
        let requested = path.to_string_lossy().into_owned();
        Ok(PathQueryDescriptor::new_known_absolute(Cow::Owned(scoped)).with_requested(requested))
    }

    // Everything else delegates to Deno's stock parser.
    fn parse_read_descriptor(&self, t: &str) -> Result<ReadDescriptor, PathResolveError> {
        self.inner.parse_read_descriptor(t)
    }
    fn parse_write_descriptor(&self, t: &str) -> Result<WriteDescriptor, PathResolveError> {
        self.inner.parse_write_descriptor(t)
    }
    fn parse_net_descriptor(&self, t: &str) -> Result<NetDescriptor, NetDescriptorParseError> {
        self.inner.parse_net_descriptor(t)
    }
    fn parse_import_descriptor(
        &self,
        t: &str,
    ) -> Result<ImportDescriptor, NetDescriptorParseError> {
        self.inner.parse_import_descriptor(t)
    }
    fn parse_env_descriptor(&self, t: &str) -> Result<EnvDescriptor, EnvDescriptorParseError> {
        self.inner.parse_env_descriptor(t)
    }
    fn parse_sys_descriptor(&self, t: &str) -> Result<SysDescriptor, SysDescriptorParseError> {
        self.inner.parse_sys_descriptor(t)
    }
    fn parse_allow_run_descriptor(
        &self,
        t: &str,
    ) -> Result<AllowRunDescriptorParseResult, RunDescriptorParseError> {
        self.inner.parse_allow_run_descriptor(t)
    }
    fn parse_deny_run_descriptor(&self, t: &str) -> Result<DenyRunDescriptor, PathResolveError> {
        self.inner.parse_deny_run_descriptor(t)
    }
    fn parse_ffi_descriptor(&self, t: &str) -> Result<FfiDescriptor, PathResolveError> {
        self.inner.parse_ffi_descriptor(t)
    }
    fn parse_special_file_descriptor<'a>(
        &self,
        path: PathQueryDescriptor<'a>,
    ) -> Result<SpecialFilePathQueryDescriptor<'a>, PathResolveError> {
        self.inner.parse_special_file_descriptor(path)
    }
    fn parse_net_query(&self, t: &str) -> Result<NetDescriptor, NetDescriptorParseError> {
        self.inner.parse_net_query(t)
    }
    fn parse_run_query<'a>(
        &self,
        requested: &'a str,
    ) -> Result<RunQueryDescriptor<'a>, RunDescriptorParseError> {
        self.inner.parse_run_query(requested)
    }
}

/// The permissions of a macro, and its canonical fs root.
///
/// With a root, the macro can read and write inside it and nothing else. With
/// no root, every fs access is denied. Network access is always allowed.
pub fn macro_permissions(
    fs_root: Option<&Path>,
) -> anyhow::Result<(PermissionsContainer, Option<PathBuf>)> {
    use anyhow::Context;
    let root = fs_root
        .map(|root| {
            std::fs::canonicalize(root)
                .with_context(|| format!("Failed to resolve the macro root {}", root.display()))
        })
        .transpose()?;
    let parser = Arc::new(ScopedParser {
        inner: RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
        // Without a root nothing is allowed, so the base for relative paths
        // does not matter.
        root: root.clone().unwrap_or_else(|| PathBuf::from("/")),
    });
    // `None` denies everything; `Some(vec![])` would allow everything.
    let allow_fs = root
        .as_ref()
        .map(|root| vec![root.to_string_lossy().into_owned()]);
    let perms = Permissions::from_options(
        parser.as_ref(),
        &PermissionsOptions {
            allow_read: allow_fs.clone(),
            allow_write: allow_fs,
            allow_net: Some(vec![]), // allow all
            prompt: false,
            ..Default::default()
        },
    )?;
    Ok((PermissionsContainer::new(parser, perms), root))
}
