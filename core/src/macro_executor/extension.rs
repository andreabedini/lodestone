//! The `lodestone` extension: Lodestone's ops, and the ESM entry point
//! (`bootstrap.js`) that builds the global scope macros see.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use deno_core::{op2, Extension, ExtensionFileSource, ExtensionFileSourceCode, OpState};
use serde::Serialize;

use crate::deno_ops::{events, instance_control, prelude, SharedRuntime};
use crate::event_broadcaster::EventBroadcaster;

include!(concat!(env!("OUT_DIR"), "/extension_sources.rs"));

/// Replace every extension source that deno_core would read from disk with the
/// copy embedded by `build.rs`.
///
/// `extension!` records JS files by their absolute path at build time, and a
/// runtime without a V8 snapshot reads them from there: the binary would only
/// work next to the source tree and cargo registry it was built from.
pub fn embed_sources(ext: &mut Extension) -> anyhow::Result<()> {
    let name = ext.name;
    for files in [
        &mut ext.js_files,
        &mut ext.esm_files,
        &mut ext.lazy_loaded_esm_files,
        &mut ext.lazy_loaded_js_files,
    ] {
        let embedded = files
            .iter()
            .map(|file| {
                #[allow(deprecated, reason = "the variant deno_core uses for extension! files")]
                let ExtensionFileSourceCode::LoadedFromFsDuringSnapshot(path) = file.code
                else {
                    return Ok(file.clone());
                };
                let key = Path::new(path)
                    .components()
                    .filter(|c| !matches!(c, Component::CurDir))
                    .collect::<PathBuf>();
                let code = embedded_extension_source(&key.to_string_lossy()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} (extension {name}) is not embedded in this build; add its crate to \
                         DENO_EXTENSION_CRATES in core/build.rs",
                        file.specifier
                    )
                })?;
                Ok(ExtensionFileSource::new(file.specifier, code))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        *files = Cow::Owned(embedded);
    }
    Ok(())
}

/// Values that `bootstrap.js` reads once, through `lodestone_bootstrap_info`.
struct BootstrapState {
    args: Vec<String>,
    root: Option<PathBuf>,
    /// Ops exposed to macros through `Deno[Deno.internal].core.ops`.
    ops: Vec<&'static str>,
}

#[derive(Serialize)]
struct BootstrapInfo {
    ops: Vec<&'static str>,
    args: Vec<String>,
    /// The macro's fs root, which `Deno.cwd()` returns. `None` without fs
    /// access.
    root: Option<String>,
    os: &'static str,
    arch: &'static str,
}

/// Used by bootstrap.js only; not exposed to macros.
#[op2]
#[serde]
fn lodestone_bootstrap_info(state: &mut OpState) -> BootstrapInfo {
    let b = state.borrow::<BootstrapState>();
    BootstrapInfo {
        ops: b.ops.clone(),
        args: b.args.clone(),
        root: b.root.as_ref().map(|r| r.to_string_lossy().into_owned()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

deno_core::extension!(
    lodestone,
    deps = [deno_webidl, deno_web, deno_net, deno_io, deno_fs, deno_fetch],
    ops = [
        lodestone_bootstrap_info,
        prelude::get_lodestone_version,
        events::next_event,
        events::emit_console_out,
        events::emit_detach,
        events::emit_state_change,
        events::next_instance_event,
        events::next_instance_state_change,
        events::next_instance_output,
        events::next_instance_player_message,
        events::next_instance_system_message,
        events::next_instance_player_change,
        events::emit_progression_event_start,
        events::emit_progression_event_update,
        events::emit_progression_event_end,
        instance_control::instance_exists,
        instance_control::all_instances,
        instance_control::get_instance_state,
        instance_control::get_instance_path,
        instance_control::get_instance_name,
        instance_control::get_instance_player_count,
        instance_control::get_instance_max_players,
        instance_control::get_instance_player_list,
        instance_control::get_instance_game,
        instance_control::get_instance_game_version,
        instance_control::get_instance_description,
        instance_control::get_instance_port,
        instance_control::set_instance_name,
        instance_control::set_instance_description,
        instance_control::set_instance_port,
        instance_control::set_instance_auto_start,
        instance_control::start_instance,
        instance_control::stop_instance,
        instance_control::restart_instance,
        instance_control::monitor_instance,
        instance_control::send_command,
        instance_control::kill_instance,
        instance_control::is_rcon_available,
        instance_control::try_send_rcon_command,
        instance_control::send_rcon_command,
        instance_control::wait_till_rcon_available,
    ],
    esm_entry_point = "ext:lodestone/bootstrap.js",
    esm = [dir "src/macro_executor", "bootstrap.js"],
    options = {
        event_broadcaster: EventBroadcaster,
        shared_runtime: tokio::runtime::Handle,
        permissions: deno_permissions::PermissionsContainer,
    },
    state = |state, options| {
        state.put(options.event_broadcaster);
        state.put(SharedRuntime(options.shared_runtime));
        // deno_fs and deno_fetch look this up for every op.
        state.put(options.permissions);
    },
);

/// The `lodestone` extension, plus the state `bootstrap.js` needs.
///
/// `extra` are the extensions from the macro's
/// [`ExtensionGenerator`](super::ExtensionGenerator); their ops are exposed to
/// the macro too.
pub fn lodestone_extension(
    event_broadcaster: EventBroadcaster,
    shared_runtime: tokio::runtime::Handle,
    permissions: deno_permissions::PermissionsContainer,
    args: Vec<String>,
    root: Option<PathBuf>,
    extra: &[Extension],
) -> Extension {
    let mut ext = lodestone::init(event_broadcaster, shared_runtime, permissions);
    let ops: Vec<&'static str> = ext
        .ops
        .iter()
        .chain(extra.iter().flat_map(|e| e.ops.iter()))
        .map(|decl| decl.name)
        .filter(|name| *name != "lodestone_bootstrap_info")
        .collect();
    let inner = ext.op_state_fn.take();
    ext.op_state_fn = Some(Box::new(move |state: &mut OpState| {
        if let Some(f) = inner {
            f(state);
        }
        state.put(BootstrapState { args, root, ops });
    }));
    ext
}
