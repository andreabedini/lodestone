//! Tests that run real JS/TS through [`MacroExecutor::spawn`].
//!
//! They are the safety net for the Deno stack upgrade (see
//! `docs/deno-upgrade-scoping.md`). Every check is made through behaviour that
//! JS can see: the value an op or glue function returns, the events a macro
//! emits, and the exit status of the macro. None of them calls an op from Rust,
//! so they should keep passing unchanged after the op layer is rewritten.
//!
//! Test macros import the glue in this repository by `file://` URL, so they
//! run the local copy and need no network.
//!
//! A macro reports failure by throwing. The error message ends up in
//! `ExitStatus::Error`, and the Rust side asserts on the exit status.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;

use crate::event_broadcaster::EventBroadcaster;
use crate::events::{
    CausedBy, Event, EventInner, InstanceEvent, InstanceEventInner, MacroEvent, MacroEventInner,
    ProgressionEventInner,
};
use crate::implementations::generic::GenericInstance;
use crate::macro_executor::{
    DefaultExtensionGenerator, ExtensionGenerator, MacroExecutor, MacroPID, SpawnResult,
};
use crate::prelude::{GameInstance, VERSION};
use crate::traits::t_configurable::GameType;
use crate::traits::t_macro::ExitStatus;
use crate::traits::t_server::State;
use crate::types::{DotLodestoneConfig, InstanceUuid};

/// Upper bound for one macro run. Real runs take well under a second.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Small assertion helpers prepended to every test macro.
const JS_ASSERT: &str = r#"
function assert(cond, msg) {
    if (!cond) throw new Error("assertion failed: " + msg);
}
function assertEq(actual, expected, msg) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) throw new Error(`${msg}: expected ${e}, got ${a}`);
}
async function assertRejects(promise, needle, msg) {
    let error = null;
    try {
        await promise;
    } catch (e) {
        error = e;
    }
    if (error === null) throw new Error(`${msg}: expected a rejection`);
    const text = String(error && error.message !== undefined ? error.message : error);
    if (!text.includes(needle))
        throw new Error(`${msg}: expected the error to contain ${JSON.stringify(needle)}, got ${JSON.stringify(text)}`);
}
"#;

/// `file://` URL of a file under `core/`, for importing the local glue.
fn core_url(relative: &str) -> String {
    url::Url::from_file_path(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .expect("CARGO_MANIFEST_DIR is absolute")
        .to_string()
}

fn prelude_ts() -> String {
    core_url("src/deno_ops/prelude/prelude.ts")
}

fn events_ts() -> String {
    core_url("src/deno_ops/events/events.ts")
}

fn instance_control_ts() -> String {
    core_url("src/deno_ops/instance_control/instance_control.ts")
}

fn temp_dir() -> PathBuf {
    tempfile::TempDir::new()
        .expect("failed to create temp dir")
        .into_path()
}

/// Options for [`Harness::run`].
#[derive(Default)]
struct RunOptions {
    args: Vec<String>,
    instance_uuid: Option<InstanceUuid>,
    pre_injection_code: Option<String>,
    /// Events to send to the broadcaster until the macro exits, one every 10ms
    /// in rotation. Used to answer `next_*` ops, which only see events sent
    /// after they subscribe; sending one at a time lets each op see each kind.
    feed: Vec<Event>,
    /// Abort the macro once it emits a detach event.
    abort_on_detach: bool,
    /// The directory the macro can access through the `Deno.*` fs API.
    fs_root: Option<PathBuf>,
}

struct RunResult {
    pid: MacroPID,
    exit_status: ExitStatus,
    /// Every event seen from spawn until the macro stopped.
    events: Vec<Event>,
}

impl RunResult {
    #[track_caller]
    fn assert_success(&self) {
        assert!(
            matches!(self.exit_status, ExitStatus::Success { .. }),
            "macro {} did not succeed: {:?}",
            self.pid,
            self.exit_status
        );
    }

    fn error_msg(&self) -> String {
        match &self.exit_status {
            ExitStatus::Error { error_msg, .. } => error_msg.clone(),
            other => panic!("expected an error exit, got {other:?}"),
        }
    }
}

struct Harness {
    event_broadcaster: EventBroadcaster,
    executor: MacroExecutor,
}

impl Harness {
    fn new() -> Self {
        let _ = tracing_subscriber::fmt::try_init();
        let (event_broadcaster, _rx) = EventBroadcaster::new(1024);
        let executor =
            MacroExecutor::new(event_broadcaster.clone(), tokio::runtime::Handle::current());
        Self {
            event_broadcaster,
            executor,
        }
    }

    /// Write `source` (with the assertion helpers prepended) to `main.ts` in
    /// `dir` and run it.
    async fn run_in(&self, dir: &Path, source: &str, options: RunOptions) -> RunResult {
        let path = dir.join("main.ts");
        std::fs::write(&path, format!("{JS_ASSERT}\n{source}")).unwrap();
        self.run_file(path, Box::new(DefaultExtensionGenerator), options)
            .await
    }

    async fn run(&self, source: &str, options: RunOptions) -> RunResult {
        self.run_in(&temp_dir(), source, options).await
    }

    /// Run the macro at `path` and wait for its first `Stopped` event.
    ///
    /// The first `Stopped` event is the one that `SpawnResult::exit_future`
    /// returns.
    async fn run_file(
        &self,
        path: PathBuf,
        generator: Box<dyn ExtensionGenerator>,
        options: RunOptions,
    ) -> RunResult {
        // subscribe before spawning so that no event is missed
        let mut rx = self.event_broadcaster.subscribe();
        let SpawnResult { macro_pid: pid, .. } = self
            .executor
            .spawn(
                path,
                options.args,
                CausedBy::Unknown,
                generator,
                options.pre_injection_code,
                options.instance_uuid,
                options.fs_root,
            )
            .await
            .expect("failed to spawn macro");

        let mut events = Vec::new();
        let mut feed_tick = tokio::time::interval(Duration::from_millis(10));
        let mut fed = 0;
        let collect = async {
            loop {
                tokio::select! {
                    event = rx.recv() => {
                        let event = match event {
                            Ok(event) => event,
                            Err(RecvError::Lagged(_)) => continue,
                            Err(RecvError::Closed) => panic!("event broadcaster closed"),
                        };
                        if let Some(MacroEvent { macro_pid, macro_event_inner, .. }) =
                            event.try_macro_event()
                        {
                            if *macro_pid == pid {
                                match macro_event_inner {
                                    MacroEventInner::Stopped { exit_status } => {
                                        let exit_status = exit_status.clone();
                                        events.push(event);
                                        return exit_status;
                                    }
                                    MacroEventInner::Detach if options.abort_on_detach => {
                                        self.executor.abort_macro(pid).unwrap();
                                    }
                                    _ => {}
                                }
                            }
                        }
                        events.push(event);
                    }
                    _ = feed_tick.tick(), if !options.feed.is_empty() => {
                        self.event_broadcaster
                            .send(options.feed[fed % options.feed.len()].clone());
                        fed += 1;
                    }
                }
            }
        };
        let exit_status = tokio::time::timeout(RUN_TIMEOUT, collect)
            .await
            .unwrap_or_else(|_| {
                let _ = self.executor.abort_macro(pid);
                panic!("macro {pid} did not stop within {RUN_TIMEOUT:?}")
            });
        RunResult {
            pid,
            exit_status,
            events,
        }
    }
}

// ---------------------------------------------------------------------------
// prelude ops

#[tokio::test]
async fn prelude_version_pid_and_no_instance() {
    let harness = Harness::new();
    let source = format!(
        r#"
        import {{ lodestoneVersion, getCurrentTaskPid, getCurrentInstanceUUID }} from "{prelude}";
        import {{ emitDetach }} from "{events}";
        assertEq(lodestoneVersion(), "{version}", "lodestone version");
        assertEq(typeof getCurrentTaskPid(), "number", "task pid type");
        assertEq(getCurrentInstanceUUID(), null, "instance uuid without an instance");
        // report our pid back to the host
        emitDetach(getCurrentTaskPid());
        "#,
        prelude = prelude_ts(),
        events = events_ts(),
        version = VERSION.with(|v| v.to_string()),
    );
    let result = harness.run(&source, RunOptions::default()).await;
    result.assert_success();
    let detach_pids: Vec<MacroPID> = result
        .events
        .iter()
        .filter_map(|e| match e.try_macro_event() {
            Some(MacroEvent {
                macro_pid,
                macro_event_inner: MacroEventInner::Detach,
                ..
            }) => Some(*macro_pid),
            _ => None,
        })
        .collect();
    assert_eq!(detach_pids, vec![result.pid]);
}

#[tokio::test]
async fn prelude_instance_uuid_injection() {
    let harness = Harness::new();
    let uuid = InstanceUuid::default();
    let source = format!(
        r#"
        import {{ getCurrentInstanceUUID }} from "{prelude}";
        assertEq(getCurrentInstanceUUID(), "{uuid}", "instance uuid");
        "#,
        prelude = prelude_ts(),
    );
    let result = harness
        .run(
            &source,
            RunOptions {
                instance_uuid: Some(uuid.clone()),
                ..Default::default()
            },
        )
        .await;
    result.assert_success();
    // the lifecycle events carry the instance uuid too
    let stopped = result.events.last().unwrap().try_macro_event().unwrap();
    assert_eq!(stopped.instance_uuid, Some(uuid));
}

#[tokio::test]
async fn pre_injection_code_is_visible() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"assertEq(__injected_config.answer, 42, "injected value");"#,
            RunOptions {
                pre_injection_code: Some("const __injected_config = { answer: 42 };".to_string()),
                ..Default::default()
            },
        )
        .await;
    result.assert_success();
}

#[tokio::test]
async fn deno_args_console_and_timers() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"
            assertEq(Deno.args, ["first", "with space", ""], "Deno.args");
            console.log("console.log works", { nested: [1, 2] });
            console.error("console.error works");
            const start = Date.now();
            await new Promise((resolve) => setTimeout(resolve, 30));
            assert(Date.now() - start >= 25, "setTimeout waited");
            let ticks = 0;
            await new Promise((resolve) => {
                const id = setInterval(() => {
                    ticks += 1;
                    if (ticks === 3) {
                        clearInterval(id);
                        resolve(undefined);
                    }
                }, 5);
            });
            assertEq(ticks, 3, "setInterval ticks");
            // a cleared timer must not keep the macro alive
            clearTimeout(setTimeout(() => { throw new Error("cleared timer fired"); }, 10));
            "#,
            RunOptions {
                args: vec!["first".into(), "with space".into(), "".into()],
                ..Default::default()
            },
        )
        .await;
    result.assert_success();
}

#[tokio::test]
async fn uncaught_error_is_reported() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"throw new Error("boom from macro");"#,
            RunOptions::default(),
        )
        .await;
    assert!(
        result.error_msg().contains("boom from macro"),
        "{:?}",
        result.exit_status
    );
}

// ---------------------------------------------------------------------------
// event ops

#[tokio::test]
async fn emit_ops_reach_the_event_broadcaster() {
    let harness = Harness::new();
    let uuid = InstanceUuid::default();
    let source = format!(
        r#"
        import * as Events from "{events}";
        const core = Deno[Deno.internal].core;
        Events.emitStateChange("Running", "test instance", "{uuid}");
        // emitConsoleOut looks the instance name up first, so call the op directly
        core.ops.emit_console_out("{uuid}", "test instance", "a console line");
        const id = Events.emitProgressionEventStart("test progression", 10, null);
        assertEq(typeof id, "string", "progression event id");
        Events.emitProgressiontEventUpdate(id, "halfway", 5);
        Events.emitProgressionEventEnd(id, true, "id=" + id, null);
        "#,
        events = events_ts(),
    );
    let result = harness.run(&source, RunOptions::default()).await;
    result.assert_success();

    let instance_events: Vec<&InstanceEvent> = result
        .events
        .iter()
        .filter_map(|e| match &e.event_inner {
            EventInner::InstanceEvent(inner) if inner.instance_uuid == uuid => Some(inner),
            _ => None,
        })
        .collect();
    assert_eq!(instance_events.len(), 2, "{instance_events:?}");
    assert_eq!(instance_events[0].instance_name, "test instance");
    assert_eq!(
        instance_events[0].instance_event_inner,
        InstanceEventInner::StateTransition { to: State::Running }
    );
    assert_eq!(
        instance_events[1].instance_event_inner,
        InstanceEventInner::InstanceOutput {
            message: "a console line".to_string()
        }
    );

    let progression: Vec<_> = result
        .events
        .iter()
        .filter_map(|e| match &e.event_inner {
            EventInner::ProgressionEvent(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(progression.len(), 3, "{progression:?}");
    let event_id = progression[0].event_id();
    assert!(progression.iter().all(|p| p.event_id() == event_id));
    assert!(matches!(
        progression[0].progression_event_inner(),
        ProgressionEventInner::ProgressionStart {
            progression_name,
            total: Some(t),
            inner: None,
        } if progression_name == "test progression" && *t == 10.0
    ));
    assert!(matches!(
        progression[1].progression_event_inner(),
        ProgressionEventInner::ProgressionUpdate {
            progress_message,
            progress,
        } if progress_message == "halfway" && *progress == 5.0
    ));
    // the id returned to JS is the id of the event
    let expected_message = format!(
        "id={}",
        serde_json::to_value(event_id).unwrap().as_str().unwrap()
    );
    assert!(matches!(
        progression[2].progression_event_inner(),
        ProgressionEventInner::ProgressionEnd {
            success: true,
            message,
            inner: None,
        } if message.as_deref() == Some(expected_message.as_str())
    ));
}

#[tokio::test]
async fn next_ops_round_trip() {
    let harness = Harness::new();
    let uuid = InstanceUuid::default();
    let source = format!(
        r#"
        import * as Events from "{events}";
        const core = Deno[Deno.internal].core;
        const uuid = "{uuid}";
        assertEq(await Events.nextInstanceConsoleOut(uuid), "hello from the host", "console out");
        assertEq(await Events.nextInstanceStateChange(uuid), "Running", "state change");
        assertEq(await Events.nextPlayerMessage(uuid), {{ player: "steve", message: "hi" }}, "player message");
        assertEq(await Events.nextInstanceSystemMessage(uuid), "system says", "system message");
        const instanceEvent = await Events.nextInstanceEvent(uuid);
        assertEq(instanceEvent.instance_uuid, uuid, "instance event uuid");
        assertEq(instanceEvent.instance_name, "fed instance", "instance event name");
        const change = await core.opAsync("next_instance_player_change", uuid);
        assertEq(change.player_list, [], "player change list");
        const event = await Events.nextEvent();
        assert(event.event_inner !== undefined && typeof event.snowflake === "string", "next event shape: " + JSON.stringify(event));
        "#,
        events = events_ts(),
    );
    let name = "fed instance".to_string();
    let feed = vec![
        Event::new_instance_output(uuid.clone(), name.clone(), "hello from the host".into()),
        Event::new_instance_state_transition(uuid.clone(), name.clone(), State::Running),
        Event::new_player_message(uuid.clone(), name.clone(), "steve".into(), "hi".into()),
        Event::new_system_message(uuid.clone(), name.clone(), "system says".into()),
        Event {
            event_inner: EventInner::InstanceEvent(InstanceEvent {
                instance_uuid: uuid.clone(),
                instance_name: name.clone(),
                instance_event_inner: InstanceEventInner::PlayerChange {
                    player_list: Default::default(),
                    players_joined: Default::default(),
                    players_left: Default::default(),
                },
            }),
            ..Event::new_instance_output(uuid.clone(), name, String::new())
        },
    ];
    let result = harness
        .run(
            &source,
            RunOptions {
                feed,
                ..Default::default()
            },
        )
        .await;
    result.assert_success();
}

// ---------------------------------------------------------------------------
// instance-control ops

#[tokio::test]
async fn instance_control_without_instance() {
    let app_state = crate::init_test_app_state().await;
    let harness = Harness::new();
    let missing = InstanceUuid::default();
    assert!(!app_state.instances.contains_key(&missing));
    let source = format!(
        r#"
        import * as IC from "{instance_control}";
        const missing = "{missing}";
        assertEq(IC.instanceExists(missing), false, "instanceExists");
        const all = IC.allInstanceUuids();
        assert(Array.isArray(all), "allInstanceUuids returns an array");
        assert(!all.includes(missing), "allInstanceUuids does not list a missing instance");
        await assertRejects(IC.getInstanceName(missing), "Instance not found", "getInstanceName");
        await assertRejects(IC.getInstanceState(missing), "Instance not found", "getInstanceState");
        await assertRejects(IC.startInstance(false, missing), "Instance not found", "startInstance");
        await assertRejects(IC.setInstancePort(1234, missing), "Instance not found", "setInstancePort");
        await assertRejects(IC.isRconAvailable(missing), "Instance not found", "isRconAvailable");
        "#,
        instance_control = instance_control_ts(),
    );
    harness
        .run(&source, RunOptions::default())
        .await
        .assert_success();
}

/// JS side of a fake generic ("atom") instance.
///
/// It speaks the procedure bridge through the raw ops (`proc_bridge_ready`,
/// `next_procedure`, `emit_result`) rather than through `procedure_bridge.ts`,
/// because `procedure_bridge.ts` imports `lodestone-macro-lib` from GitHub.
///
/// `GetVersion` returns the fake's internal state as JSON, so that a macro can
/// check what the instance received.
fn fake_atom_source() -> String {
    format!(
        r#"
        import {{ emitDetach }} from "{events}";
        import {{ getCurrentTaskPid, getCurrentInstanceUUID }} from "{prelude}";
        const core = Deno[Deno.internal].core;
        const {{ ops }} = core;

        const st = {{
            name: "generic test",
            description: "a fake atom",
            port: 25565,
            autoStart: false,
            state: "Stopped",
            restored: false,
            doubleReadyRejected: false,
            lastCommand: null,
            calls: [],
            cwd: Deno.cwd(),
        }};

        ops.proc_bridge_ready();
        try {{
            ops.proc_bridge_ready();
        }} catch (_) {{
            st.doubleReadyRejected = true;
        }}
        // let the host start waiting for the detach event
        await new Promise((resolve) => setTimeout(resolve, 50));
        emitDetach(getCurrentTaskPid());

        while (true) {{
            const procedure = await core.opAsync("next_procedure");
            const inner = procedure.inner;
            let ret = "Void";
            let error = null;
            switch (inner.type) {{
                case "RestoreInstance":
                    st.restored = inner.dot_lodestone_config.uuid === getCurrentInstanceUUID();
                    break;
                case "DestructInstance":
                    break;
                case "GetName": ret = {{ String: st.name }}; break;
                case "SetName": st.name = inner.new_name; break;
                case "GetDescription": ret = {{ String: st.description }}; break;
                case "SetDescription": st.description = inner.new_description; break;
                case "GetVersion": ret = {{ String: JSON.stringify(st) }}; break;
                case "GetGame":
                    ret = {{ Game: {{ type: "Generic", game_name: "Generic", game_display_name: "Fake Game" }} }};
                    break;
                case "GetPort": ret = {{ Num: st.port }}; break;
                case "SetPort":
                    if (inner.new_port === 0) {{
                        error = {{ kind: "BadRequest", source: "port 0 is not allowed" }};
                    }} else {{
                        st.port = inner.new_port;
                    }}
                    break;
                case "GetAutoStart": ret = {{ Bool: st.autoStart }}; break;
                case "SetAutoStart": st.autoStart = inner.new_auto_start; break;
                case "GetState": ret = {{ State: st.state }}; break;
                case "StartInstance":
                case "StopInstance":
                case "RestartInstance":
                case "KillInstance":
                    st.calls.push(inner);
                    st.state = inner.type === "StopInstance" || inner.type === "KillInstance" ? "Stopped" : "Running";
                    break;
                case "SendCommand": st.lastCommand = inner; break;
                case "Monitor":
                    ret = {{ Monitor: {{ memory_usage: null, disk_usage: null, cpu_usage: 1.5, start_time: null }} }};
                    break;
                case "GetPlayerCount": ret = {{ Num: 1 }}; break;
                case "GetMaxPlayerCount": ret = {{ Num: 20 }}; break;
                case "GetPlayerList": ret = {{ Player: [{{ id: "p1", name: "steve" }}] }}; break;
                default:
                    error = {{ kind: "UnsupportedOperation", source: "fake atom: " + inner.type }};
            }}
            ops.emit_result({{
                id: procedure.id,
                success: error === null,
                inner: error === null ? ret : null,
                error,
            }});
        }}
        "#,
        events = events_ts(),
        prelude = prelude_ts(),
    )
}

/// Restore a generic instance backed by [`fake_atom_source`] and add it to the
/// global app state.
async fn spawn_fake_generic_instance(
    executor: &MacroExecutor,
    event_broadcaster: &EventBroadcaster,
) -> (InstanceUuid, PathBuf, GenericInstance) {
    let app_state = crate::init_test_app_state().await;
    let path = temp_dir();
    std::fs::write(path.join("run.ts"), fake_atom_source()).unwrap();
    let uuid = InstanceUuid::default();
    let instance = tokio::time::timeout(
        RUN_TIMEOUT,
        GenericInstance::restore(
            path.clone(),
            DotLodestoneConfig::new(uuid.clone(), GameType::Generic),
            event_broadcaster.clone(),
            executor.clone(),
        ),
    )
    .await
    .expect("restoring the fake generic instance timed out")
    .expect("failed to restore the fake generic instance");
    app_state.instances.insert(
        uuid.clone(),
        GameInstance::GenericInstance(instance.clone()),
    );
    (uuid, path, instance)
}

#[tokio::test]
async fn instance_control_with_generic_instance() {
    let harness = Harness::new();
    let (uuid, path, _instance) =
        spawn_fake_generic_instance(&harness.executor, &harness.event_broadcaster).await;

    let source = format!(
        r#"
        import * as IC from "{instance_control}";
        import {{ getCurrentTaskPid }} from "{prelude}";
        const uuid = "{uuid}";
        const me = {{ type: "Macro", macro_pid: getCurrentTaskPid() }};
        const fake = async () => JSON.parse(await IC.getInstanceGameVersion(uuid));

        assertEq(IC.instanceExists(uuid), true, "instanceExists");
        assert(IC.allInstanceUuids().includes(uuid), "allInstanceUuids lists the instance");
        assertEq((await fake()).restored, true, "RestoreInstance reached the atom");
        assertEq((await fake()).doubleReadyRejected, true, "second proc_bridge_ready throws");

        assertEq(await IC.getInstanceName(uuid), "generic test", "name");
        await IC.setInstanceName("renamed", uuid);
        assertEq(await IC.getInstanceName(uuid), "renamed", "name after set");
        assertEq(await IC.getInstanceDescription(uuid), "a fake atom", "description");
        await IC.setInstanceDescription("new description", uuid);
        assertEq(await IC.getInstanceDescription(uuid), "new description", "description after set");
        assertEq(await IC.getInstancePort(uuid), 25565, "port");
        await IC.setInstancePort(25570, uuid);
        assertEq(await IC.getInstancePort(uuid), 25570, "port after set");
        await assertRejects(IC.setInstancePort(0, uuid), "Failed to set instance port", "error from the atom");
        await IC.setInstanceAutoStart(true, uuid);
        assertEq((await fake()).autoStart, true, "auto start after set");
        assertEq(await IC.getInstanceGame(uuid), {{ type: "Generic", game_name: "Generic", game_display_name: "Fake Game" }}, "game");
        assertEq(await IC.getInstancePath(uuid), {path}, "path");

        assertEq(await IC.getInstanceState(uuid), "Stopped", "state");
        await IC.startInstance(true, uuid);
        assertEq(await IC.getInstanceState(uuid), "Running", "state after start");
        await IC.restartInstance(false, uuid);
        await IC.stopInstance(false, uuid);
        assertEq(await IC.getInstanceState(uuid), "Stopped", "state after stop");
        await IC.killInstance(uuid);
        assertEq((await fake()).calls, [
            {{ type: "StartInstance", caused_by: me, block: true }},
            {{ type: "RestartInstance", caused_by: me, block: false }},
            {{ type: "StopInstance", caused_by: me, block: false }},
            {{ type: "KillInstance", caused_by: me }},
        ], "server calls carry the task pid");

        await IC.sendCommand("say hi", uuid);
        assertEq((await fake()).lastCommand, {{ type: "SendCommand", command: "say hi", caused_by: me }}, "sendCommand");

        assertEq((await IC.monitorInstance(uuid)).cpu_usage, 1.5, "monitor");
        assertEq(await IC.getInstancePlayerCount(uuid), 1, "player count");
        assertEq(await IC.getInstanceMaxPlayers(uuid), 20, "max players");
        assertEq(await IC.getInstancePlayerList(uuid), [{{ type: "GenericPlayer", id: "p1", name: "steve" }}], "player list");

        await assertRejects(IC.isRconAvailable(uuid), "RCON not available", "isRconAvailable");
        await assertRejects(IC.trySendRconCommand("list", uuid), "RCON not available", "trySendRconCommand");
        await assertRejects(IC.sendRconCommand("list", uuid), "RCON not available", "sendRconCommand");
        await assertRejects(IC.waitTillRconAvailable(uuid), "RCON not available", "waitTillRconAvailable");
        "#,
        instance_control = instance_control_ts(),
        prelude = prelude_ts(),
        path = serde_json::to_string(&path.to_string_lossy()).unwrap(),
    );
    let result = harness.run(&source, RunOptions::default()).await;

    crate::init_test_app_state().await.instances.remove(&uuid);
    result.assert_success();
}

// ---------------------------------------------------------------------------
// module loader

#[tokio::test]
async fn module_loader_local_files() {
    let harness = Harness::new();
    let dir = temp_dir();
    std::fs::write(
        dir.join("lib.ts"),
        r#"
        import { triple } from "./plain.js";
        export enum Color { Red, Green, Blue }
        export interface Greeting { text: string }
        export function greet(name: string): Greeting {
            return { text: `hello ${name}` };
        }
        export const nine: number = triple(3);
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.join("plain.js"),
        "export function triple(x) { return x * 3; }\n",
    )
    .unwrap();
    std::fs::write(dir.join("data.json"), r#"{ "answer": 42, "list": [1, 2] }"#).unwrap();
    std::fs::create_dir(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("nested").join("up.ts"),
        "export { nine } from \"../lib.ts\";\n",
    )
    .unwrap();

    let source = format!(
        r#"
        import {{ Color, greet, nine, type Greeting }} from "./lib.ts";
        import {{ nine as nineAgain }} from "./nested/up.ts";
        import {{ triple }} from "./plain.js";
        // deno_ast 0.27 only parses `assert`; newer V8/deno_ast want `with`.
        import data from "./data.json" assert {{ type: "json" }};
        import {{ lodestoneVersion }} from "{prelude}";

        const greeting: Greeting = greet("world");
        assertEq(greeting.text, "hello world", "TS import");
        assertEq(Color.Blue, 2, "TS enum");
        assertEq(nine, 9, "TS importing JS");
        assertEq(nineAgain, 9, "relative import from a subdirectory");
        assertEq(triple(2), 6, "JS import");
        assertEq(data, {{ answer: 42, list: [1, 2] }}, "JSON import");
        assertEq(typeof lodestoneVersion(), "string", "glue import by file URL");

        // top-level await
        const awaited: number = await new Promise<number>((resolve) => setTimeout(() => resolve(7), 1));
        assertEq(awaited, 7, "top-level await");

        // dynamic import
        const lib = await import("./lib.ts");
        assertEq(lib.greet("again").text, "hello again", "dynamic import");

        // more TS-only syntax
        class Box<T> {{
            constructor(private readonly value: T) {{}}
            get(): T {{ return this.value; }}
        }}
        const boxed = new Box<string>("boxed") as Box<string>;
        assertEq(boxed.get()!, "boxed", "generics, parameter properties, as and !");
        "#,
        prelude = prelude_ts(),
    );
    harness
        .run_in(&dir, &source, RunOptions::default())
        .await
        .assert_success();
}

#[tokio::test]
async fn module_loader_missing_file_is_an_error() {
    let harness = Harness::new();
    let result = harness
        .run(r#"import "./does_not_exist.ts";"#, RunOptions::default())
        .await;
    let error_msg = result.error_msg();
    assert!(!error_msg.is_empty());
}

// ---------------------------------------------------------------------------
// abort

#[tokio::test]
async fn abort_busy_loop_during_module_evaluation() {
    let harness = Harness::new();
    let source = format!(
        r#"
        import {{ emitDetach }} from "{events}";
        import {{ getCurrentTaskPid }} from "{prelude}";
        emitDetach(getCurrentTaskPid());
        while (true) {{}}
        "#,
        events = events_ts(),
        prelude = prelude_ts(),
    );
    let result = harness
        .run(
            &source,
            RunOptions {
                abort_on_detach: true,
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result.exit_status, ExitStatus::Killed { .. }),
        "{:?}",
        result.exit_status
    );
}

#[tokio::test]
async fn abort_macro_in_event_loop() {
    let harness = Harness::new();
    // the module finishes evaluating; the loop keeps the event loop alive
    let source = format!(
        r#"
        import {{ emitDetach }} from "{events}";
        import {{ getCurrentTaskPid }} from "{prelude}";
        (async () => {{
            while (true) {{
                await new Promise((resolve) => setTimeout(resolve, 5));
            }}
        }})();
        setTimeout(() => emitDetach(getCurrentTaskPid()), 20);
        "#,
        events = events_ts(),
        prelude = prelude_ts(),
    );
    let result = harness
        .run(
            &source,
            RunOptions {
                abort_on_detach: true,
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result.exit_status, ExitStatus::Killed { .. }),
        "{:?}",
        result.exit_status
    );
}

#[tokio::test]
async fn abort_unknown_pid_is_not_found() {
    let harness = Harness::new();
    let err = harness
        .executor
        .abort_macro(MacroPID(usize::MAX))
        .unwrap_err();
    assert!(
        matches!(err.kind, crate::error::ErrorKind::NotFound),
        "{err:?}"
    );
}

#[tokio::test]
async fn error_that_looks_like_termination_is_an_error() {
    // A macro can throw an error whose text matches a termination. It must
    // still be reported as an error, not as killed.
    let harness = Harness::new();
    let result = harness
        .run(
            r#"
            const e = new Error("execution terminated");
            e.stack = "Error: execution terminated";
            throw e;
            "#,
            RunOptions::default(),
        )
        .await;
    assert!(
        matches!(result.exit_status, ExitStatus::Error { .. }),
        "{:?}",
        result.exit_status
    );
}

#[tokio::test]
async fn macro_status_is_the_reported_exit_status() {
    // The executor thread must not report a second, spurious exit status
    // after the macro has stopped.
    let harness = Harness::new();
    let result = harness
        .run("console.log('done');", RunOptions::default())
        .await;
    result.assert_success();
    let status = tokio::time::timeout(RUN_TIMEOUT, async {
        loop {
            // give a spurious second event time to arrive
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Some(status) = harness.executor.get_macro_status(result.pid).await {
                break status;
            }
        }
    })
    .await
    .expect("macro status was never recorded");
    assert!(matches!(status, ExitStatus::Success { .. }), "{status:?}");
}

// ---------------------------------------------------------------------------
// embedded glue

/// URL of a glue file under `core/` on upstream's `dev` branch, which the
/// loader serves from the embedded copy.
fn upstream_glue_url(relative: &str) -> String {
    format!(
        "{}{relative}",
        crate::embedded_glue::EMBEDDED_GLUE_URL_PREFIXES[0]
    )
}

/// Runs without network access to raw.githubusercontent.com: the glue must
/// come from the binary.
#[tokio::test]
async fn embedded_glue_is_served_without_network() {
    let harness = Harness::new();
    let source = format!(
        r#"
        import {{ lodestoneVersion, getCurrentTaskPid }} from "{prelude}";
        import * as Events from "{events}";
        import * as IC from "{instance_control}";
        import {{ isTServer }} from "{utils}";
        assertEq(lodestoneVersion(), "{version}", "lodestone version");
        // instance_control.ts re-exports getCurrentTaskPid from ../prelude/prelude.ts
        assertEq(IC.getCurrentTaskPid(), getCurrentTaskPid(), "relative import resolved");
        assertEq(isTServer({{ type: "GetState" }}), true, "generic glue");
        Events.emitDetach(getCurrentTaskPid());
        "#,
        prelude = upstream_glue_url("src/deno_ops/prelude/prelude.ts"),
        events = upstream_glue_url("src/deno_ops/events/events.ts"),
        instance_control = upstream_glue_url("src/deno_ops/instance_control/instance_control.ts"),
        utils = upstream_glue_url("src/implementations/generic/js/main/libs/utils.ts"),
        version = VERSION.with(|v| v.to_string()),
    );
    let result = harness.run(&source, RunOptions::default()).await;
    result.assert_success();
    assert!(result.events.iter().any(|e| matches!(
        e.try_macro_event(),
        Some(MacroEvent {
            macro_event_inner: MacroEventInner::Detach,
            ..
        })
    )));
}

#[tokio::test]
async fn unknown_embedded_glue_path_is_an_error() {
    let harness = Harness::new();
    let source = format!(
        r#"import "{}";"#,
        upstream_glue_url("src/deno_ops/does_not_exist.ts")
    );
    let result = harness.run(&source, RunOptions::default()).await;
    let error_msg = result.error_msg();
    assert!(
        error_msg.contains("not part of the Lodestone glue embedded in this build"),
        "{error_msg}"
    );
}

/// `lodestone-macro-lib` stays remote, but its imports of the Lodestone glue
/// are served from the embedded copies. Needs network access to
/// raw.githubusercontent.com, so it is ignored by default.
#[tokio::test]
#[ignore]
async fn macro_lib_uses_embedded_glue() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"
            import { lodestoneVersion } from "https://raw.githubusercontent.com/Lodestone-Team/lodestone-macro-lib/main/prelude.ts";
            import { ProgressionHandler } from "https://raw.githubusercontent.com/Lodestone-Team/lodestone-macro-lib/main/events.ts";
            assertEq(typeof lodestoneVersion(), "string", "lodestoneVersion");
            const handler = ProgressionHandler.create("from macro-lib", 10);
            handler.complete(true, "done");
            "#,
            RunOptions::default(),
        )
        .await;
    result.assert_success();
}

// ---------------------------------------------------------------------------
// `Deno` namespace, fs permissions, fetch

/// An instance directory with a small world, and a sibling directory the
/// macro must not reach (directly, via `..`, or via the `escape-link`
/// symlink inside the instance directory).
fn fs_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::TempDir::new().unwrap();
    let root = base.path().join("instance");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(root.join("world/region")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("world/level.dat"), "level data").unwrap();
    std::fs::write(root.join("world/region/r.0.0.mca"), "region data").unwrap();
    std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape-link")).unwrap();
    let root = root.canonicalize().unwrap();
    let outside = outside.canonicalize().unwrap();
    (base, root, outside)
}

/// The fs API works inside the root, with relative paths resolved against it,
/// and the std `fs/copy.ts` (what the auto-backup example uses) works.
/// Needs network access to deno.land.
#[tokio::test]
async fn fs_inside_the_instance_directory() {
    let harness = Harness::new();
    let (_base, root, _outside) = fs_fixture();
    let source = format!(
        r#"
        import {{ copy }} from "https://deno.land/std@0.191.0/fs/copy.ts";
        assertEq(Deno.cwd(), {root}, "Deno.cwd() is the instance directory");
        assertEq(await Deno.readTextFile("world/level.dat"), "level data", "relative read");
        assertEq(Deno.readTextFileSync({root} + "/world/level.dat"), "level data", "absolute read");
        await Deno.writeTextFile("notes.txt", "hi");
        assertEq(Deno.statSync("notes.txt").size, 2, "write + stat");
        await Deno.mkdir("a/b", {{ recursive: true }});
        await Deno.remove("a", {{ recursive: true }});
        let e = null;
        try {{ await Deno.readTextFile("does-not-exist"); }} catch (err) {{ e = err; }}
        assert(e instanceof Deno.errors.NotFound, "missing file is NotFound: " + e);
        e = null;
        try {{ await Deno.mkdir("world"); }} catch (err) {{ e = err; }}
        assert(e instanceof Deno.errors.AlreadyExists, "existing dir is AlreadyExists: " + e);

        await copy("world", "backups/world");
        const names = [];
        for await (const entry of Deno.readDir("backups/world")) names.push(entry.name);
        assertEq(names.sort(), ["level.dat", "region"], "copied tree");
        "#,
        root = serde_json::to_string(&root.to_string_lossy()).unwrap(),
    );
    harness
        .run_in(
            &root,
            &source,
            RunOptions {
                fs_root: Some(root.clone()),
                ..Default::default()
            },
        )
        .await
        .assert_success();
    assert_eq!(
        std::fs::read_to_string(root.join("backups/world/region/r.0.0.mca")).unwrap(),
        "region data"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "hi"
    );
}

/// Every way out of the instance directory is denied with an error that is
/// `instanceof Deno.errors.PermissionDenied`.
#[tokio::test]
async fn fs_outside_the_instance_directory_is_denied() {
    let harness = Harness::new();
    let (_base, root, outside) = fs_fixture();
    let source = format!(
        r#"
        const root = {root};
        const outside = {outside};
        async function denied(what, f) {{
            let e = null;
            try {{
                await f();
            }} catch (err) {{
                e = err;
            }}
            assert(e !== null, what + ": not denied");
            assert(e instanceof Deno.errors.PermissionDenied, what + ": wrong error " + e);
            assert(e instanceof Deno.errors.NotCapable, what + ": not NotCapable " + e);
        }}
        await denied("absolute read", () => Deno.readTextFile(outside + "/secret.txt"));
        await denied("absolute sync read", () => Deno.readTextFileSync(outside + "/secret.txt"));
        await denied("absolute write", () => Deno.writeTextFile(outside + "/pwned.txt", "x"));
        await denied("relative ../", () => Deno.readTextFile("../outside/secret.txt"));
        await denied("absolute root/..", () => Deno.readTextFile(root + "/world/../../outside/secret.txt"));
        await denied("symlink read", () => Deno.readTextFile("escape-link/secret.txt"));
        await denied("symlink write", () => Deno.writeTextFile("escape-link/pwned.txt", "x"));
        await denied("readDir /", async () => {{ for await (const _ of Deno.readDir("/")) {{}} }});
        await denied("/etc/hostname", () => Deno.readTextFile("/etc/hostname"));
        await denied("copyFile out", () => Deno.copyFile("world/level.dat", outside + "/stolen"));
        await denied("rename out", () => Deno.rename("world/level.dat", "../outside/stolen"));
        "#,
        root = serde_json::to_string(&root.to_string_lossy()).unwrap(),
        outside = serde_json::to_string(&outside.to_string_lossy()).unwrap(),
    );
    harness
        .run_in(
            &root,
            &source,
            RunOptions {
                fs_root: Some(root.clone()),
                ..Default::default()
            },
        )
        .await
        .assert_success();
    let mut left: Vec<_> = std::fs::read_dir(&outside)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(left, vec![std::ffi::OsString::from("secret.txt")]);
    assert!(root.join("world/level.dat").exists());
}

/// A macro spawned without an fs root has no fs access at all.
#[tokio::test]
async fn fs_without_a_root_is_denied() {
    let harness = Harness::new();
    let dir = temp_dir();
    std::fs::write(dir.join("data.txt"), "data").unwrap();
    let source = format!(
        r#"
        for (const [what, f] of [
            ["read", () => Deno.readTextFileSync({file})],
            ["write", () => Deno.writeTextFileSync({file}, "x")],
            ["cwd", () => Deno.cwd()],
        ]) {{
            let e = null;
            try {{ f(); }} catch (err) {{ e = err; }}
            assert(e instanceof Deno.errors.PermissionDenied, what + ": " + e);
        }}
        "#,
        file = serde_json::to_string(&dir.join("data.txt").to_string_lossy()).unwrap(),
    );
    harness
        .run_in(&dir, &source, RunOptions::default())
        .await
        .assert_success();
    assert_eq!(
        std::fs::read_to_string(dir.join("data.txt")).unwrap(),
        "data"
    );
}

/// What `Deno` exposes, and what it must not.
#[tokio::test]
async fn deno_namespace_is_restricted() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"
            for (const name of ["run", "Command", "env", "exit", "core", "spawn", "spawnSync", "kill", "dlopen", "serve", "listen", "connect"]) {
                assert(!(name in Deno), "Deno." + name + " must not exist");
            }
            assert(globalThis.__bootstrap === undefined, "__bootstrap is hidden");
            assert(Object.isFrozen(Deno), "Deno is frozen");
            assertEq(typeof Deno.build.os, "string", "Deno.build");
            assertEq(typeof Deno.readTextFile, "function", "fs API");
            assertEq(typeof Deno.inspect, "function", "Deno.inspect");
            assert(typeof Deno.errors.PermissionDenied === "function", "Deno.errors");
            // the compat table holds Lodestone's ops only
            const ops = Object.keys(Deno[Deno.internal].core.ops);
            assert(ops.includes("next_event") && ops.includes("start_instance"), "lodestone ops present");
            assert(!ops.includes("lodestone_bootstrap_info"), "bootstrap op hidden");
            assert(ops.every((name) => !name.startsWith("op_")), "no deno ops: " + ops.filter((n) => n.startsWith("op_")));
            for (const g of ["fetch", "Request", "Response", "Headers", "URL", "TextEncoder", "AbortController", "structuredClone", "atob", "performance", "Blob", "File", "ReadableStream", "WritableStream", "TransformStream"]) {
                assert(g in globalThis, g + " is a global");
            }
            const t = performance.now();
            assert(typeof t === "number" && t >= 0 && t < 60000, "performance.now() is relative to the macro start: " + t);
            assertEq(await new Blob(["ab", "c"]).text(), "abc", "Blob");
            const reader = new ReadableStream({ start(c) { c.enqueue(1); c.close(); } }).getReader();
            assertEq((await reader.read()).value, 1, "ReadableStream");
            // the internal modules that hold the full op table are not importable
            for (const specifier of ["ext:core/mod.js", "ext:core/ops", "ext:lodestone/bootstrap.js", "ext:deno_fs/30_fs.js"]) {
                let imported = null;
                try {
                    imported = await import(specifier);
                } catch (_) {
                    // expected
                }
                assert(imported === null, specifier + " is importable");
            }
            "#,
            RunOptions::default(),
        )
        .await;
    result.assert_success();
}

/// `fetch` works (decision B). Needs network access to example.com.
#[tokio::test]
async fn fetch_works() {
    let harness = Harness::new();
    let result = harness
        .run(
            r#"
            const res = await fetch("https://example.com/");
            assertEq(res.status, 200, "status");
            assert((await res.text()).includes("Example Domain"), "body");
            "#,
            RunOptions::default(),
        )
        .await;
    result.assert_success();
}

/// JSON modules with the standard `with` syntax, from TS and from plain JS.
#[tokio::test]
async fn json_import_attributes() {
    let harness = Harness::new();
    let dir = temp_dir();
    std::fs::write(dir.join("data.json"), r#"{ "answer": 42 }"#).unwrap();
    std::fs::write(
        dir.join("plain.js"),
        "import data from \"./data.json\" with { type: \"json\" };\nexport const answer = data.answer;\n",
    )
    .unwrap();
    harness
        .run_in(
            &dir,
            r#"
            import data from "./data.json" with { type: "json" };
            import { answer } from "./plain.js";
            assertEq(data.answer, 42, "TS import with `with`");
            assertEq(answer, 42, "JS import with `with`");
            "#,
            RunOptions::default(),
        )
        .await
        .assert_success();
}

/// The deprecated `assert` syntax is rejected in plain JS by V8 (TS files are
/// transpiled, which rewrites it to `with`).
#[tokio::test]
async fn json_import_assert_in_plain_js_is_a_syntax_error() {
    let harness = Harness::new();
    let dir = temp_dir();
    std::fs::write(dir.join("data.json"), r#"{ "answer": 42 }"#).unwrap();
    std::fs::write(
        dir.join("old.js"),
        "import data from \"./data.json\" assert { type: \"json\" };\nexport default data;\n",
    )
    .unwrap();
    let result = harness
        .run_in(&dir, r#"import "./old.js";"#, RunOptions::default())
        .await;
    let error_msg = result.error_msg();
    assert!(error_msg.contains("SyntaxError"), "{error_msg}");
}

/// A generic instance's code gets its instance directory as its fs root.
#[tokio::test]
async fn generic_instance_fs_root_is_the_instance_directory() {
    let harness = Harness::new();
    let (uuid, path, _instance) =
        spawn_fake_generic_instance(&harness.executor, &harness.event_broadcaster).await;
    let source = format!(
        r#"
        import * as IC from "{instance_control}";
        assertEq(JSON.parse(await IC.getInstanceGameVersion("{uuid}")).cwd, {path}, "atom's Deno.cwd()");
        "#,
        instance_control = instance_control_ts(),
        path = serde_json::to_string(&path.canonicalize().unwrap().to_string_lossy()).unwrap(),
    );
    let result = harness.run(&source, RunOptions::default()).await;
    crate::init_test_app_state().await.instances.remove(&uuid);
    result.assert_success();
}
