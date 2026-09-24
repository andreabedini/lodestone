//! Runs macros: TypeScript/JavaScript on an embedded V8 (`deno_core`).
//!
//! Each macro gets its own OS thread, with its own current-thread tokio
//! runtime and its own `JsRuntime`. The runtime has the web APIs from
//! deno_web/deno_fetch, a small `Deno` namespace (fs scoped to the macro's
//! root directory), and Lodestone's ops; see `macro_executor/bootstrap.js`.
//!
//! Ops that touch the app state or an instance run on Lodestone's shared
//! runtime (see [`crate::deno_ops::run_on_shared`]).

use std::{
    fmt::{Debug, Display},
    iter::zip,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use color_eyre::eyre::{eyre, Context};
use dashmap::DashMap;
use deno_core::{JsRuntime, ModuleSpecifier, PollEventLoopOptions, RuntimeOptions};
use futures_util::Future;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, error, warn};
use ts_rs::TS;

use crate::{
    error::{Error, ErrorKind},
    event_broadcaster::EventBroadcaster,
    events::{CausedBy, EventInner, MacroEvent, MacroEventInner},
    prelude::VERSION,
    traits::t_configurable::manifest::{ConfigurableValue, ConfigurableValueType, SettingManifest},
    traits::t_macro::ExitStatus,
    types::InstanceUuid,
    util::fs,
};

mod extension;
mod loader;
mod permissions;

use loader::TypescriptModuleLoader;

/// Extra extensions for a macro, on top of the standard `lodestone` one.
///
/// Their ops are exposed to the macro like Lodestone's own. Generic ("atom")
/// instances use this for the procedure bridge.
pub trait ExtensionGenerator: Send + Sync {
    /// Called on the macro's thread (an `Extension` is not `Send`).
    fn generate(&self) -> Vec<deno_core::Extension>;
}

/// No extra extensions.
pub struct DefaultExtensionGenerator;

impl ExtensionGenerator for DefaultExtensionGenerator {
    fn generate(&self) -> Vec<deno_core::Extension> {
        Vec::new()
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Hash, TS)]
#[serde(transparent)]
#[ts(export)]
pub struct MacroPID(pub usize); // todo remove pub

impl From<MacroPID> for usize {
    fn from(uid: MacroPID) -> Self {
        uid.0
    }
}

impl From<&MacroPID> for usize {
    fn from(uid: &MacroPID) -> Self {
        uid.0
    }
}

impl AsRef<usize> for MacroPID {
    fn as_ref(&self) -> &usize {
        &self.0
    }
}

impl Display for MacroPID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MacroPID({})", self.0)
    }
}

/// Handle to a running macro, used to abort it from another thread.
#[derive(Debug)]
struct MacroHandle {
    isolate_handle: deno_core::v8::IsolateHandle,
    /// Set before the isolate is terminated, so that the macro thread can tell
    /// an abort apart from an error thrown by the macro.
    aborted: Arc<AtomicBool>,
    /// Wakes a macro whose event loop is idle (e.g. awaiting `next_event()`),
    /// which `terminate_execution` alone cannot interrupt.
    wake: Arc<Notify>,
}

impl MacroHandle {
    fn abort(&self) {
        // The flag must be set before the macro thread can observe the
        // termination.
        self.aborted.store(true, Ordering::SeqCst);
        // interrupts running JS (busy loops) ...
        self.isolate_handle.terminate_execution();
        // ... and wakes an idle event loop
        self.wake.notify_one();
    }
}

#[derive(Clone, Debug)]
pub struct MacroExecutor {
    macro_process_table: Arc<DashMap<MacroPID, MacroHandle>>,
    exit_status_table: Arc<DashMap<MacroPID, ExitStatus>>,
    #[allow(dead_code)]
    channel_table:
        Arc<DashMap<MacroPID, (mpsc::UnboundedSender<Value>, mpsc::UnboundedSender<Value>)>>,
    event_broadcaster: EventBroadcaster,
    next_process_id: Arc<AtomicUsize>,
    /// Lodestone's shared runtime, on which ops that touch instances run.
    rt: tokio::runtime::Handle,
}

pub struct SpawnResult {
    pub macro_pid: MacroPID,
    pub detach_future: Pin<Box<dyn Future<Output = ()> + Send>>,
    pub exit_future: Pin<Box<dyn Future<Output = Result<ExitStatus, Error>> + Send>>,
}

/// Everything the macro thread needs.
struct MacroThread {
    pid: MacroPID,
    main_module: ModuleSpecifier,
    args: Vec<String>,
    instance_uuid: Option<InstanceUuid>,
    pre_injection_code: Option<String>,
    fs_root: Option<PathBuf>,
    extension_generator: Box<dyn ExtensionGenerator>,
    event_broadcaster: EventBroadcaster,
    process_table: Arc<DashMap<MacroPID, MacroHandle>>,
    shared_runtime: tokio::runtime::Handle,
}

/// Build the `JsRuntime` of a macro.
///
/// `fs_root` is the only directory the macro can read and write through the
/// `Deno.*` fs API; `None` means no fs access.
pub(crate) fn build_runtime(
    event_broadcaster: EventBroadcaster,
    shared_runtime: tokio::runtime::Handle,
    args: Vec<String>,
    fs_root: Option<&Path>,
    extra: Vec<deno_core::Extension>,
) -> anyhow::Result<JsRuntime> {
    let (permissions, root) = permissions::macro_permissions(fs_root)?;
    let lodestone = extension::lodestone_extension(
        event_broadcaster,
        shared_runtime,
        permissions,
        args,
        root,
        &extra,
    );
    // Dependencies must come before their dependents (checked in debug builds).
    let mut extensions = vec![
        deno_webidl::deno_webidl::init(),
        deno_web::deno_web::init(
            Arc::new(deno_web::BlobStore::default()),
            None,
            false,
            deno_web::InMemoryBroadcastChannel::default(),
        ),
        // deno_fetch's JS loads ext:deno_net/02_tls.js, so deno_net is needed
        // although deno_fetch does not declare it.
        deno_net::deno_net::init(None, None),
        // deno_io replaces deno_core's op_print with one that writes to
        // resources 1 and 2, so stdio must be registered or console.log
        // throws. Console output goes to Lodestone's stdout/stderr, as it
        // did with deno_runtime.
        deno_io::deno_io::init(Some(deno_io::Stdio::default())),
        deno_fs::deno_fs::init(deno_fs::sync::MaybeArc::new(deno_fs::RealFs)),
        deno_fetch::deno_fetch::init(deno_fetch::Options {
            user_agent: format!("Lodestone/{}", VERSION.with(|v| v.to_string())),
            ..Default::default()
        }),
        lodestone,
    ];
    extensions.extend(extra);
    for ext in &mut extensions {
        extension::embed_sources(ext)?;
    }
    Ok(JsRuntime::try_new(RuntimeOptions {
        module_loader: Some(Rc::new(TypescriptModuleLoader::default())),
        extensions,
        ..Default::default()
    })?)
}

impl MacroThread {
    /// Body of the macro thread. Reports exactly one `Stopped` event.
    fn run(self) {
        let pid = self.pid;
        let instance_uuid = self.instance_uuid.clone();
        let event_broadcaster = self.event_broadcaster.clone();
        let stopped_sent = Arc::new(AtomicBool::new(false));
        // Async ops `tokio::spawn` `!Send` futures (through deno_unsync),
        // which is only sound on a current-thread runtime. So every macro has
        // its own, and anything that has to outlive the macro runs on the
        // shared runtime instead.
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("Failed to build the macro's tokio runtime")?;
            rt.block_on(self.run_macro(stopped_sent.clone()));
            Ok::<_, color_eyre::Report>(())
        }));
        debug!("MacroExecutor thread exited");
        if stopped_sent.load(Ordering::SeqCst) {
            return;
        }
        let error_msg = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(e)) => format!("{e:#}"),
            Err(_) => "Macro executor thread unexpectedly panicked".to_string(),
        };
        error!("Macro {pid} failed: {error_msg}");
        event_broadcaster.send(
            MacroEvent {
                macro_pid: pid,
                macro_event_inner: MacroEventInner::Stopped {
                    exit_status: ExitStatus::Error {
                        time: chrono::Utc::now().timestamp(),
                        error_msg,
                    },
                },
                instance_uuid,
            }
            .into(),
        );
    }

    async fn run_macro(self, stopped_sent: Arc<AtomicBool>) {
        let MacroThread {
            pid,
            main_module,
            args,
            instance_uuid,
            pre_injection_code,
            fs_root,
            extension_generator,
            event_broadcaster,
            process_table,
            shared_runtime,
        } = self;
        let send_stopped = |exit_status: ExitStatus| {
            stopped_sent.store(true, Ordering::SeqCst);
            event_broadcaster.send(
                MacroEvent {
                    macro_pid: pid,
                    macro_event_inner: MacroEventInner::Stopped { exit_status },
                    instance_uuid: instance_uuid.clone(),
                }
                .into(),
            );
        };

        let setup = || -> anyhow::Result<JsRuntime> {
            let mut js = build_runtime(
                event_broadcaster.clone(),
                shared_runtime,
                args,
                fs_root.as_deref(),
                extension_generator.generate(),
            )?;
            // `null` (not the string "null") when there is no instance
            let instance_uuid_js = match &instance_uuid {
                Some(uuid) => serde_json::to_string(uuid.as_ref())?,
                None => "null".to_string(),
            };
            js.execute_script(
                "deps_inject",
                format!(
                    "const __macro_pid = {}; const __instance_uuid = {};",
                    pid.0, instance_uuid_js
                ),
            )?;
            if let Some(config_code) = pre_injection_code {
                js.execute_script("config_inject", config_code)?;
            }
            Ok(js)
        };
        let mut js = match setup() {
            Ok(js) => js,
            Err(e) => {
                error!("Failed to set up macro {pid}: {e:#}");
                send_stopped(ExitStatus::Error {
                    error_msg: format!("Failed to set up the macro runtime: {e:#}"),
                    time: chrono::Utc::now().timestamp(),
                });
                return;
            }
        };

        let aborted = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        process_table.insert(
            pid,
            MacroHandle {
                isolate_handle: js.v8_isolate().thread_safe_handle(),
                aborted: aborted.clone(),
                wake: wake.clone(),
            },
        );

        event_broadcaster.send(
            MacroEvent {
                macro_pid: pid,
                macro_event_inner: MacroEventInner::Started,
                instance_uuid: instance_uuid.clone(),
            }
            .into(),
        );

        let run = async {
            let id = js.load_main_es_module(&main_module).await?;
            let evaluation = js.mod_evaluate(id);
            js.run_event_loop(PollEventLoopOptions::default()).await?;
            evaluation.await
        };
        let result = tokio::select! {
            result = run => result.map_err(|e| e.to_string()),
            _ = wake.notified() => Err("Macro execution was terminated".to_string()),
        };

        let exit_status = match result {
            Ok(()) => {
                debug!("Macro event loop exited");
                ExitStatus::Success {
                    time: chrono::Utc::now().timestamp(),
                }
            }
            // An error after an abort is the termination itself, not a
            // failure of the macro.
            Err(_) if aborted.load(Ordering::SeqCst) => {
                warn!("User terminated macro execution");
                ExitStatus::Killed {
                    time: chrono::Utc::now().timestamp(),
                }
            }
            Err(error_msg) => {
                error!("Error executing main module {main_module}: {error_msg}");
                ExitStatus::Error {
                    error_msg,
                    time: chrono::Utc::now().timestamp(),
                }
            }
        };
        send_stopped(exit_status);
    }
}

impl MacroExecutor {
    pub fn new(event_broadcaster: EventBroadcaster, rt: tokio::runtime::Handle) -> MacroExecutor {
        let process_table = Arc::new(DashMap::new());
        let process_id = Arc::new(AtomicUsize::new(0));
        let exit_status_table = Arc::new(DashMap::new());

        // spawn a task to listen for exit events and update the exit status table
        tokio::task::spawn({
            let exit_status_table = exit_status_table.clone();
            let mut rx = event_broadcaster.subscribe();
            async move {
                loop {
                    if let Ok(event) = rx.recv().await {
                        if let Some(MacroEvent {
                            macro_pid,
                            macro_event_inner: MacroEventInner::Stopped { exit_status },
                            ..
                        }) = event.try_macro_event()
                        {
                            exit_status_table.insert(*macro_pid, exit_status.clone());
                        }
                    }
                }
            }
        });

        MacroExecutor {
            macro_process_table: process_table,
            event_broadcaster,
            channel_table: Arc::new(DashMap::new()),
            exit_status_table,
            next_process_id: process_id,
            rt,
        }
    }

    /// Run the macro at `path_to_main_module` on a new thread.
    ///
    /// `fs_root` is the directory the macro can read and write through the
    /// `Deno.*` fs API (the instance directory); `Deno.cwd()` returns it and
    /// relative paths resolve against it. `None` means no fs access.
    ///
    /// Returns once the macro has started. `exit_future` resolves when it
    /// stops, `detach_future` when it asks to run in the background.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        &self,
        path_to_main_module: PathBuf,
        args: Vec<String>,
        _caused_by: CausedBy,
        extension_generator: Box<dyn ExtensionGenerator>,
        pre_injection_code: Option<String>,
        instance_uuid: Option<InstanceUuid>,
        fs_root: Option<PathBuf>,
    ) -> Result<SpawnResult, Error> {
        let pid = MacroPID(self.next_process_id.fetch_add(1, Ordering::SeqCst));
        let exit_future = Box::pin({
            let __self = self.clone();
            async move { __self.wait_with_timeout(pid).await }
        });
        let detach_future = Box::pin({
            let __self = self.clone();
            async move {
                __self.wait_for_detach(pid).await;
            }
        });
        let main_module = deno_core::resolve_path(
            &path_to_main_module.to_string_lossy(),
            &std::env::current_dir().context("Failed to get current directory")?,
        )
        .context("Failed to resolve the path of the main module")?;

        // subscribe before the thread starts, so that no event is missed
        let mut rx = self.event_broadcaster.subscribe();

        let thread = MacroThread {
            pid,
            main_module,
            args,
            instance_uuid,
            pre_injection_code,
            fs_root,
            extension_generator,
            event_broadcaster: self.event_broadcaster.clone(),
            process_table: self.macro_process_table.clone(),
            shared_runtime: self.rt.clone(),
        };
        std::thread::Builder::new()
            .name(format!("macro-{}", pid.0))
            .spawn(move || thread.run())
            .context("Failed to spawn macro thread")?;

        // wait until the macro has started, or failed to
        let started = async move {
            loop {
                let event = match rx.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break Err(eyre!("Failed to receive macro started event")),
                };
                if let EventInner::MacroEvent(MacroEvent {
                    macro_pid,
                    macro_event_inner,
                    ..
                }) = event.event_inner
                {
                    if macro_pid != pid {
                        continue;
                    }
                    match macro_event_inner {
                        MacroEventInner::Started => break Ok(()),
                        MacroEventInner::Stopped { exit_status } => {
                            break Err(eyre!("Macro stopped before it started: {exit_status:?}"))
                        }
                        _ => {}
                    }
                }
            }
        };

        tokio::time::timeout(Duration::from_secs(10), started)
            .await
            .context("Failed to spawn macro")??;
        Ok(SpawnResult {
            macro_pid: pid,
            detach_future,
            exit_future,
        })
    }

    /// abort a macro execution
    pub fn abort_macro(&self, pid: MacroPID) -> Result<(), Error> {
        self.macro_process_table
            .get(&pid)
            .ok_or_else(|| Error {
                kind: ErrorKind::NotFound,
                source: eyre!("Macro with pid {} not found", pid),
            })?
            .abort();
        Ok(())
    }

    pub async fn wait_for_detach(&self, target_macro_pid: MacroPID) {
        let mut rx = self.event_broadcaster.subscribe();
        loop {
            let event = rx.recv().await.unwrap();
            if let EventInner::MacroEvent(MacroEvent {
                macro_pid,
                macro_event_inner,
                ..
            }) = event.event_inner
            {
                if target_macro_pid == macro_pid {
                    if let MacroEventInner::Detach = macro_event_inner {
                        return;
                    }
                }
            }
        }
    }

    /// wait for a macro to finish
    async fn wait_with_timeout(&self, taget_macro_pid: MacroPID) -> Result<ExitStatus, Error> {
        let mut rx = self.event_broadcaster.subscribe();
        loop {
            let event = rx.recv().await.unwrap();
            if let EventInner::MacroEvent(MacroEvent {
                macro_pid,
                macro_event_inner,
                ..
            }) = event.event_inner
            {
                if taget_macro_pid == macro_pid {
                    if let MacroEventInner::Stopped { exit_status } = macro_event_inner {
                        break Ok(exit_status);
                    }
                }
            }
        }
    }

    pub async fn get_macro_status(&self, pid: MacroPID) -> Option<ExitStatus> {
        self.exit_status_table.get(&pid).map(|v| v.clone())
    }

    pub async fn get_config_manifest(
        path: &PathBuf,
    ) -> Result<IndexMap<String, SettingManifest>, Error> {
        match extract_config_code(&fs::read_to_string(path).await?) {
            Ok(optional_code) => match optional_code {
                Some((var_name, definition)) => get_config_from_code(&var_name, &definition),
                None => Ok(IndexMap::<String, SettingManifest>::new()),
            },
            Err(e) => Err(e),
        }
    }

    pub fn shutdown_all(&self) {
        for element in self.macro_process_table.iter() {
            element.value().abort();
        }
    }
}

///
/// extract the class definition and the name of the declared config instance
/// from the typescript code
///
/// *returns (instance_name, class_definition)*
///
fn extract_config_code(code: &str) -> Result<Option<(String, String)>, Error> {
    let config_indices: Vec<_> = code.match_indices("LodestoneConfig").collect();
    if config_indices.is_empty() {
        return Ok(None);
    }
    if config_indices.len() < 2 {
        return Err(Error::ts_syntax_error(
            "Class definition or config declaration is missing",
        ));
    }

    // first occurrence of LodeStoneConfig must be the class declaration
    let config_code = &code[(config_indices[0].0)..];
    // end_index is for extracting the class definition
    let end_index = {
        let mut open_count = 0;
        let mut close_count = 0;
        let mut i = 0;
        // match for brackets to determine where the class definition ends
        for &char_item in config_code.to_string().as_bytes().iter() {
            if char_item == b'{' {
                open_count += 1;
            }
            if char_item == b'}' {
                close_count += 1;
            }
            i += 1;

            if open_count == close_count && open_count > 0 {
                break;
            }
        }

        if i == 0 || open_count != close_count {
            return Err(Error::ts_syntax_error("config"));
        }

        i
    };

    // second occurrence of LodeStoneConfig must be the config variable declaration
    // idea: slice from the end of class definition to 'let/const/var (name): LodestoneConfig'
    let config_var_code = {
        let second_occur_index =
            config_indices[1].0 - config_indices[0].0 + "LodestoneConfig".len();
        &config_code[end_index..second_occur_index]
    };

    // parse whether the keyword 'var', 'let', or 'const' is used
    let decl_keyword = {
        let mut config_var_tokens: Vec<_> = config_var_code.split(' ').collect();
        config_var_tokens.reverse();

        let keywords = ["let", "const", "var"];
        let keyword_found = config_var_tokens.iter().find(|&kw| keywords.contains(kw));
        match keyword_found {
            Some(&kw) => kw,
            None => {
                return Err(Error::ts_syntax_error(
                    "Class definition detected but cannot find config declaration",
                ))
            }
        }
    };

    let config_var_code = config_var_code.replace(' ', "");
    let config_var_name = {
        // now check for the last occurrence of the keyword to find the starting index of
        // 'let/const/var (name): LodestoneConfig'
        let decl_keyword_index = match config_var_code
            .match_indices(decl_keyword)
            .collect::<Vec<_>>()
            .last()
        {
            Some(val) => val.0,
            None => {
                return Err(Error::ts_syntax_error(
                    "Class definition detected but cannot find config declaration",
                ));
            }
        };

        // slice from the keyword to the end to isolate 'let/const/var (name): LodestoneConfig'
        let decl_var_statement = &config_var_code[decl_keyword_index..];
        // since spaces are removed, ':' is the separator between name and 'LodestoneConfig'
        let var_name_end_index = match decl_var_statement.find(':') {
            Some(index) => index,
            None => {
                return Err(Error::ts_syntax_error(
                    "Class definition detected but cannot find config declaration",
                ));
            }
        };

        // the name is in between the keyword and ':'
        &decl_var_statement[decl_keyword.len()..var_name_end_index]
    };

    // last sanity check: class definition must start with a '{'
    match config_code.find('{') {
        Some(start_index) => Ok(Some((
            config_var_name.to_string(),
            // we no longer need the declaration, so slice after the '{'
            config_code[start_index..end_index].to_string(),
        ))),
        None => Err(Error::ts_syntax_error("config")),
    }
}

fn get_config_from_code(
    config_var_name: &str,
    config_definition: &str,
) -> Result<IndexMap<String, SettingManifest>, Error> {
    // remove the open and close brackets
    let str_length = config_definition.len();
    let config_params_str = &config_definition[1..str_length - 1].to_string();

    let config_params_str: Vec<_> = config_params_str.split('\n').collect();

    // parse config code into a collection of description and definition
    let mut comment_lines: Vec<String> = vec![];
    let mut code_lines: Vec<String> = vec![];
    let mut comments: Vec<String> = vec![];
    let mut codes: Vec<String> = vec![];
    let mut comment_block_count = 0;
    for line in config_params_str {
        let line = line.replace('\r', "");
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        // comments within a comment block
        if comment_block_count > 0 {
            // closing the comment block
            if line.starts_with("*/") {
                comment_block_count -= 1;
                continue;
            }

            // comments within a comment block
            let comment_str = {
                if let Some(line) = line.strip_prefix('*') {
                    line.trim()
                } else {
                    line
                }
            };
            // do not push empty comment at the beginning of the comment block
            if !comment_str.is_empty() || !comment_lines.is_empty() {
                comment_lines.push(comment_str.to_string());
            }
            continue;
        }

        // single line comment & opening of a comment block
        if line.starts_with("//") {
            if let Some(comment_str) = cleanup_comment_line(line, "//") {
                comment_lines.push(comment_str.to_string());
            }
        } else if line.starts_with("/**") {
            if let Some(comment_str) = cleanup_comment_line(line, "/**") {
                comment_lines.push(comment_str.to_string());
            }
            comment_block_count += 1;
        } else if line.starts_with("/*") {
            if let Some(comment_str) = cleanup_comment_line(line, "/*") {
                comment_lines.push(comment_str.to_string());
            }
            comment_block_count += 1;
        } else {
            // if non of those are satisfied, it must be a line of actual code instead of a comment
            let code_line = line.replace([' ', '\t'], "").trim().to_string();
            code_lines.push(code_line.clone());
            if code_line.ends_with(';') {
                comments.push(comment_lines.join(" "));
                comment_lines.clear();
                codes.push(code_lines.join("").strip_suffix(';').unwrap().to_string());
                code_lines.clear();
            }
        }
    }

    let mut configs: IndexMap<String, SettingManifest> = IndexMap::new();
    for (definition, desc) in zip(codes, comments) {
        let (var_name, config) = parse_config_single(&definition, &desc, config_var_name)?;
        configs.insert(var_name, config);
    }

    Ok(configs)
}

fn cleanup_comment_line(comment_line: &str, comment_prefix: &str) -> Option<String> {
    let result_str = comment_line.strip_prefix(comment_prefix).unwrap().trim();
    if result_str.is_empty() {
        None
    } else {
        Some(result_str.to_string())
    }
}

fn parse_config_single(
    single_config_definition: &str,
    config_description: &str,
    setting_id_prefix: &str,
) -> Result<(String, SettingManifest), Error> {
    let entry = single_config_definition.trim().to_string();

    // compute indices to isolate class field names and types
    let (name_end_index, type_start_index) = match entry.find('?') {
        Some(index) => (index, index + 2),
        None => match entry.find(':') {
            Some(index) => (index, index + 1),
            None => {
                return Err(Error::ts_syntax_error("config"));
            }
        },
    };
    let var_name = &entry[..name_end_index];
    // if the field name is 2 char from the type, it must be optional ('?:')
    let is_optional = name_end_index + 2 == type_start_index;

    let default_value_index = match entry.find('=') {
        Some(index) => index,
        None => entry.len(),
    };
    if type_start_index >= default_value_index {
        return Err(Error::ts_syntax_error("config"));
    }

    let var_type = &entry[type_start_index..default_value_index];
    let config_type = get_config_value_type(var_type)?;
    let has_default = default_value_index != entry.len();

    // TODO: remove this. We will handle this in validation instead
    // TODO: we actually can't remove this - a required settings manifest must have a value
    if !is_optional && !has_default {
        return Err(Error {
            kind: ErrorKind::NotFound,
            source: eyre!("{var_name} is not optional and thus must have a default value"),
        });
    }

    let default_val = if has_default {
        // default value as string
        let val_str = entry[default_value_index + 1..].to_string();
        let val_str_len = val_str.len();
        Some(match config_type {
            ConfigurableValueType::String { .. } => {
                ConfigurableValue::String(val_str[1..val_str_len - 1].to_string())
            }
            ConfigurableValueType::Boolean => {
                let value = match val_str.parse::<bool>() {
                    Ok(val) => val,
                    Err(_) => {
                        return Err(Error {
                            kind: ErrorKind::Internal,
                            source: eyre!("Cannot parse \"{val_str}\" to a bool"),
                        });
                    }
                };
                ConfigurableValue::Boolean(value)
            }
            ConfigurableValueType::Float { .. } => {
                let value = match val_str.parse::<f32>() {
                    Ok(val) => val,
                    Err(_) => {
                        return Err(Error {
                            kind: ErrorKind::Internal,
                            source: eyre!("Cannot parse \"{val_str}\" to a number"),
                        });
                    }
                };
                ConfigurableValue::Float(value)
            }
            ConfigurableValueType::Enum { .. } => {
                let value = ConfigurableValue::Enum(val_str[1..val_str_len - 1].to_string());
                config_type.type_check(&value)?;
                value
            }
            _ => panic!("TS config parsing error: invalid type not caught by the type parser"),
        })
    } else {
        None
    };

    let mut settings_id = setting_id_prefix.to_string();
    settings_id.push('|');
    settings_id.push_str(var_name);
    Ok((
        var_name.to_string(),
        SettingManifest::new_value_with_type(
            settings_id,
            var_name.to_string(),
            config_description.to_string(),
            default_val.clone(),
            config_type,
            default_val,
            false,
            true,
        ),
    ))
}

fn get_config_value_type(type_str: &str) -> Result<ConfigurableValueType, Error> {
    let result = match type_str {
        "string" => ConfigurableValueType::String { regex: None },
        "boolean" => ConfigurableValueType::Boolean,
        "number" => ConfigurableValueType::Float {
            max: None,
            min: None,
        },
        _ => {
            // try to parse it into an enum
            let enum_options: Vec<_> = type_str.split('|').collect();
            let mut options: Vec<String> = Vec::new();

            for option in enum_options {
                // verify the enum options are strings
                let first_quote_index = {
                    if let Some(i) = option.find('\'') {
                        i
                    } else if let Some(i) = option.find('"') {
                        i
                    } else {
                        return Err(Error {
                            kind: ErrorKind::Internal,
                            source: eyre!("cannot parse type \"{}\"", type_str),
                        });
                    }
                };

                if first_quote_index == 0 {
                    let str_len = option.len();
                    options.push(option[1..str_len - 1].to_string());
                } else {
                    return Err(Error {
                        kind: ErrorKind::Internal,
                        source: eyre!("cannot parse type \"{}\"", type_str),
                    });
                }
            }

            ConfigurableValueType::Enum { options }
        }
    };
    Ok(result)
}

#[cfg(test)]
mod tests {

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use deno_core::{op2, OpState};

    use super::{DefaultExtensionGenerator, ExtensionGenerator};

    use crate::deno_ops::{run_on_shared, MacroOpError};
    use crate::event_broadcaster::EventBroadcaster;
    use crate::events::CausedBy;
    use crate::macro_executor::{
        extract_config_code, get_config_from_code, parse_config_single, SpawnResult,
    };
    use crate::traits::t_configurable::manifest::ConfigurableValue;
    use crate::traits::t_macro::ExitStatus;

    #[op2]
    #[string]
    fn hello_world() -> String {
        "Hello World".to_string()
    }

    #[op2]
    #[string]
    async fn async_hello_world() -> String {
        "async Hello World".to_string()
    }

    deno_core::extension!(test_hello_ops, ops = [hello_world, async_hello_world]);

    struct BasicExtensionGenerator;

    impl ExtensionGenerator for BasicExtensionGenerator {
        fn generate(&self) -> Vec<deno_core::Extension> {
            vec![test_hello_ops::init()]
        }
    }

    #[tokio::test]
    async fn basic_execution() {
        // init tracing
        let _ = tracing_subscriber::fmt::try_init();
        let (event_broadcaster, _rx) = EventBroadcaster::new(10);
        // construct a macro executor
        let executor =
            super::MacroExecutor::new(event_broadcaster, tokio::runtime::Handle::current());

        // create a temp directory
        let temp_dir = tempdir::TempDir::new("macro_test").unwrap().into_path();

        // create a macro file

        let path_to_macro = temp_dir.join("test.ts");

        std::fs::write(
            &path_to_macro,
            r#"
            const core = Deno[Deno.internal].core;
            const { ops } = core;
            console.log(ops.hello_world())
            console.log(await core.opAsync("async_hello_world"))
            "#,
        )
        .unwrap();

        let SpawnResult { exit_future, .. } = executor
            .spawn(
                path_to_macro,
                Vec::new(),
                CausedBy::Unknown,
                Box::new(BasicExtensionGenerator),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let exit_status = exit_future.await.unwrap();
        assert!(
            matches!(exit_status, ExitStatus::Success { .. }),
            "{exit_status:?}"
        );
    }

    #[tokio::test]
    async fn test_http_url() {
        let _ = tracing_subscriber::fmt::try_init();

        let (event_broadcaster, _rx) = EventBroadcaster::new(10);
        // construct a macro executor
        let executor =
            super::MacroExecutor::new(event_broadcaster, tokio::runtime::Handle::current());

        // create a temp directory
        let temp_dir = tempdir::TempDir::new("macro_test").unwrap().into_path();

        // create a macro file

        let path_to_macro = temp_dir.join("test.ts");

        std::fs::write(
            &path_to_macro,
            r#"
            import { readLines } from "https://deno.land/std@0.104.0/io/mod.ts";
            console.log(readLines);
            "#,
        )
        .unwrap();

        let SpawnResult { exit_future, .. } = executor
            .spawn(
                path_to_macro,
                Vec::new(),
                CausedBy::Unknown,
                Box::new(BasicExtensionGenerator),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        exit_future.await.unwrap();
    }

    /// Every Lodestone op name must be unique among all the ops in the
    /// runtime: deno_core only checks for duplicates in debug builds (by
    /// panicking), and a release build would silently shadow one of them.
    #[tokio::test]
    async fn lodestone_op_names_do_not_collide() {
        let (event_broadcaster, _rx) = EventBroadcaster::new(10);
        let shared = tokio::runtime::Handle::current();
        // Lodestone's ops, including the procedure bridge's
        let ours: Vec<&'static str> = super::extension::lodestone::init(
            event_broadcaster.clone(),
            shared.clone(),
            super::permissions::macro_permissions(None).unwrap().0,
        )
        .ops
        .iter()
        .chain(
            crate::implementations::generic::procedure_bridge_extension_for_tests()
                .ops
                .iter(),
        )
        .map(|decl| decl.name)
        .collect();
        // 41 in `lodestone` (with lodestone_bootstrap_info), 3 in the bridge
        assert_eq!(ours.len(), 44, "{ours:?}");

        let js = super::build_runtime(
            event_broadcaster,
            shared,
            Vec::new(),
            None,
            vec![crate::implementations::generic::procedure_bridge_extension_for_tests()],
        )
        .expect("failed to build the runtime (a duplicate op name panics in debug builds)");
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for name in js.op_names() {
            *counts.entry(name).or_default() += 1;
        }
        let collisions: Vec<_> = ours
            .iter()
            .filter(|name| counts.get(*name).copied().unwrap_or(0) != 1)
            .map(|name| (*name, counts.get(name).copied().unwrap_or(0)))
            .collect();
        assert!(
            collisions.is_empty(),
            "op names not registered exactly once: {collisions:?}"
        );
    }

    /// Set by the task that [`spawn_task_on_shared_runtime`] spawns.
    static SHARED_TASK_DONE: AtomicBool = AtomicBool::new(false);

    /// Does what an instance-control op does when it calls into instance
    /// code: run through [`run_on_shared`], where the instance code
    /// `tokio::spawn`s a long-lived task (like a server's supervision task).
    #[op2]
    #[string]
    async fn spawn_task_on_shared_runtime(
        state: Rc<RefCell<OpState>>,
    ) -> Result<String, MacroOpError> {
        run_on_shared(&state, async {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                SHARED_TASK_DONE.store(true, Ordering::SeqCst);
            });
            Ok(format!(
                "{:?} on {}",
                tokio::runtime::Handle::current().runtime_flavor(),
                std::thread::current().name().unwrap_or("<unnamed>")
            ))
        })
        .await
    }

    deno_core::extension!(test_shared_ops, ops = [spawn_task_on_shared_runtime]);

    struct SharedOpsGenerator;

    impl ExtensionGenerator for SharedOpsGenerator {
        fn generate(&self) -> Vec<deno_core::Extension> {
            vec![test_shared_ops::init()]
        }
    }

    /// Op bodies run through `run_on_shared` execute on the shared runtime,
    /// not on the macro's thread, and tasks they spawn outlive the macro.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ops_run_on_the_shared_runtime() {
        let _ = tracing_subscriber::fmt::try_init();
        let (event_broadcaster, _rx) = EventBroadcaster::new(10);
        let executor =
            super::MacroExecutor::new(event_broadcaster, tokio::runtime::Handle::current());
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("main.js");
        std::fs::write(
            &path,
            r#"
            const where = await Deno[Deno.internal].core.opAsync("spawn_task_on_shared_runtime");
            if (!where.startsWith("MultiThread on ") || where.includes("macro-")) {
                throw new Error("op ran on the wrong runtime: " + where);
            }
            "#,
        )
        .unwrap();
        let SpawnResult { exit_future, .. } = executor
            .spawn(
                path,
                Vec::new(),
                CausedBy::Unknown,
                Box::new(SharedOpsGenerator),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let exit_status = exit_future.await.unwrap();
        assert!(
            matches!(exit_status, ExitStatus::Success { .. }),
            "{exit_status:?}"
        );
        // the macro, and its runtime, are gone; the spawned task is not done yet
        assert!(!SHARED_TASK_DONE.load(Ordering::SeqCst));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !SHARED_TASK_DONE.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the task spawned by the op did not outlive the macro");
    }

    /// A macro awaiting an op that never resolves (an idle event loop) can be
    /// killed too, not only one running JS.
    #[tokio::test]
    async fn abort_idle_macro() {
        let _ = tracing_subscriber::fmt::try_init();
        let (event_broadcaster, _rx) = EventBroadcaster::new(10);
        let executor =
            super::MacroExecutor::new(event_broadcaster, tokio::runtime::Handle::current());
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("main.js");
        std::fs::write(
            &path,
            r#"await Deno[Deno.internal].core.opAsync("next_event");"#,
        )
        .unwrap();
        let SpawnResult {
            macro_pid,
            exit_future,
            ..
        } = executor
            .spawn(
                path,
                Vec::new(),
                CausedBy::Unknown,
                Box::new(DefaultExtensionGenerator),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        executor.abort_macro(macro_pid).unwrap();
        let exit_status = tokio::time::timeout(Duration::from_secs(10), exit_future)
            .await
            .expect("the idle macro was not killed")
            .unwrap();
        assert!(
            matches!(exit_status, ExitStatus::Killed { .. }),
            "{exit_status:?}"
        );
    }

    #[test]
    fn test_macro_config_extraction() {
        // should return None if no there is no config definition
        let result = extract_config_code(
            r#"
            console.log("hello world");
            const message = "hello macro";
            console.debug(message);
            "#,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);

        // should return an error if the instance declaration is missing
        let result = extract_config_code(
            r#"
            class LodestoneConfig {
                id: string = 'defaultId';
            }
            "#,
        );
        assert!(result.is_err());

        // should return an error if the class definition is missing
        let result = extract_config_code(
            r#"
            declare let config: LodestoneConfig;
            console.debug(config);
            "#,
        );
        assert!(result.is_err());

        // should extract the correct instance name and class definition
        let result = extract_config_code(
            r#"
            class LodestoneConfig {
                id: string = 'defaultId';
            }
            declare let config: LodestoneConfig;
            console.debug(config);
            "#,
        );
        assert!(result.is_ok());
        let (name, code) = result.unwrap().unwrap();
        assert_eq!(
            &code,
            r#"{
                id: string = 'defaultId';
            }"#
        );
        assert_eq!(&name, "config");
    }

    #[test]
    fn test_macro_config_single_parsing() {
        // should return an error if a non-option variable does not have default value
        let result = parse_config_single("id:string", "", "");
        assert!(result.is_err());

        // should return an error if the value and type does not match
        let result = parse_config_single("id:number='defaultId'", "", "prefix");
        assert!(result.is_err());

        // should properly parse the optional variable
        let result = parse_config_single("id?:string", "", "prefix");
        let (_, config) = result.unwrap();
        assert!(config.get_value().is_none());
        assert_eq!(config.get_identifier(), "prefix|id");

        // should properly parse the non-optional variable
        let result = parse_config_single("id:string='defaultId'", "", "prefix");
        let (_, config) = result.unwrap();
        let value = config.get_value().unwrap();
        match value {
            ConfigurableValue::String(val) => assert_eq!(val, "defaultId"),
            _ => panic!("incorrect value"),
        }
        assert_eq!(config.get_identifier(), "prefix|id");
    }

    #[test]
    fn test_macro_config_multi_parsing() {
        let result = get_config_from_code(
            "config",
            r#"{
                id: string = 'defaultId';
                interval?: number;
            }"#,
        )
        .unwrap();
        let identifiers = ["config|id", "config|interval"];
        let configs: Vec<_> = result.iter().collect();
        for (_, settings) in configs {
            assert_ne!(
                identifiers
                    .iter()
                    .find(|&val| val == settings.get_identifier()),
                None
            );
        }
    }
}
