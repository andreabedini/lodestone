//! Instance-control ops.
//!
//! Every op that awaits instance code runs its body on the shared runtime
//! through [`run_on_shared`], because instance methods spawn tasks and own IO
//! objects that must not be tied to the macro's runtime. `instance_exists` and
//! `all_instances` only read the instance map, so they stay synchronous.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use anyhow::{bail, Context};
use deno_core::{op2, OpState};

use super::{run_on_shared, MacroOpError};
use crate::{
    events::CausedBy,
    macro_executor::MacroPID,
    prelude::{app_state, GameInstance},
    traits::{
        t_configurable::{Game, TConfigurable},
        t_player::{Player, TPlayerManagement},
        t_server::{MonitorReport, State, TServer},
    },
    types::InstanceUuid,
};

/// Clone the instance out of the map, so that no `DashMap` guard is held
/// across an `.await` (that would block writers to the same shard, e.g.
/// instance creation or deletion, for as long as the op runs).
///
/// `GameInstance` is a handle: its state lives behind `Arc`s, so the clone is
/// cheap and shares state with the entry in the map.
fn get_instance(instance_uuid: &InstanceUuid) -> Result<GameInstance, anyhow::Error> {
    app_state()
        .instances
        .get(instance_uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| anyhow::anyhow!("Instance not found"))
}

#[op2]
pub fn instance_exists(#[serde] instance_uuid: InstanceUuid) -> bool {
    app_state().instances.contains_key(&instance_uuid)
}

#[op2]
#[serde]
pub fn all_instances() -> Vec<InstanceUuid> {
    app_state()
        .instances
        .iter()
        .map(|entry| entry.key().clone())
        .collect()
}

#[op2]
pub async fn start_instance(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[serde] task_pid: MacroPID,
    block: bool,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        instance
            .start(
                CausedBy::Macro {
                    macro_pid: task_pid,
                },
                block,
            )
            .await
            .context("Failed to start instance")
    })
    .await
}

#[op2]
pub async fn stop_instance(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[serde] task_pid: MacroPID,
    block: bool,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        instance
            .stop(
                CausedBy::Macro {
                    macro_pid: task_pid,
                },
                block,
            )
            .await
            .context("Failed to stop instance")
    })
    .await
}

#[op2]
pub async fn restart_instance(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[serde] task_pid: MacroPID,
    block: bool,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        instance
            .restart(
                CausedBy::Macro {
                    macro_pid: task_pid,
                },
                block,
            )
            .await
            .context("Failed to restart instance")
    })
    .await
}

#[op2]
pub async fn kill_instance(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[serde] task_pid: MacroPID,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        instance
            .kill(CausedBy::Macro {
                macro_pid: task_pid,
            })
            .await
            .context("Failed to kill instance")
    })
    .await
}

#[op2]
#[serde]
pub async fn get_instance_state(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<State, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;

        Ok(instance.state().await)
    })
    .await
}

#[op2]
pub async fn send_command(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[string] command: String,
    #[serde] task_pid: MacroPID,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        instance
            .send_command(
                &command,
                CausedBy::Macro {
                    macro_pid: task_pid,
                },
            )
            .await
            .context("Failed to send command")
    })
    .await
}

#[op2]
#[serde]
pub async fn monitor_instance(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<MonitorReport, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.monitor().await)
    })
    .await
}

#[op2]
pub async fn get_instance_player_count(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<u32, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.get_player_count().await?)
    })
    .await
}

#[op2]
pub async fn get_instance_max_players(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<u32, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.get_max_player_count().await?)
    })
    .await
}

#[op2]
#[serde]
pub async fn get_instance_player_list(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<HashSet<Player>, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.get_player_list().await?)
    })
    .await
}

#[op2]
#[string]
pub async fn get_instance_name(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<String, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.name().await)
    })
    .await
}

#[op2]
#[serde]
pub async fn get_instance_game(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<Game, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.game_type().await)
    })
    .await
}

#[op2]
#[string]
pub async fn get_instance_game_version(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<String, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.version().await)
    })
    .await
}

#[op2]
#[string]
pub async fn get_instance_description(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<String, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.description().await)
    })
    .await
}

#[op2]
pub async fn get_instance_port(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<u32, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.port().await)
    })
    .await
}

#[op2]
#[string]
pub async fn get_instance_path(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<String, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        Ok(instance.path().await.to_string_lossy().to_string())
    })
    .await
}

#[op2]
pub async fn set_instance_name(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[string] name: String,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;

        instance
            .set_name(name)
            .await
            .context("Failed to set instance name")
    })
    .await
}

#[op2]
pub async fn set_instance_description(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[string] description: String,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;

        instance
            .set_description(description)
            .await
            .context("Failed to set instance description")
    })
    .await
}

#[op2]
pub async fn set_instance_port(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    port: u32,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;

        instance
            .set_port(port)
            .await
            .context("Failed to set instance port")
    })
    .await
}

#[op2]
pub async fn set_instance_auto_start(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    auto_start: bool,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;

        instance
            .set_auto_start(auto_start)
            .await
            .context("Failed to set instance auto start")
    })
    .await
}

#[op2]
pub async fn is_rcon_available(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<bool, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        match &instance {
            GameInstance::MinecraftInstance(v) => Ok(v.get_rcon().lock().await.is_some()),
            GameInstance::GenericInstance(_) => {
                bail!("RCON not available for atom instances")
            }
        }
    })
    .await
}

#[op2]
#[string]
pub async fn try_send_rcon_command(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[string] command: String,
) -> Result<Option<String>, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        match &instance {
            GameInstance::MinecraftInstance(v) => Ok(v.send_rcon(&command).await.ok()),
            GameInstance::GenericInstance(_) => {
                bail!("RCON not available for atom instances")
            }
        }
    })
    .await
}

#[op2]
#[string]
pub async fn send_rcon_command(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
    #[string] command: String,
) -> Result<String, MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        match &instance {
            GameInstance::MinecraftInstance(v) => {
                let rcon = v.get_rcon();
                loop {
                    if let Some(rcon) = rcon.lock().await.as_mut() {
                        return Ok(rcon.cmd(&command).await?);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            GameInstance::GenericInstance(_) => {
                bail!("RCON not available for atom instances")
            }
        }
    })
    .await
}

#[op2]
pub async fn wait_till_rcon_available(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> Result<(), MacroOpError> {
    run_on_shared(&state, async move {
        let instance = get_instance(&instance_uuid)?;
        match &instance {
            GameInstance::MinecraftInstance(v) => {
                let rcon = v.get_rcon();
                loop {
                    if rcon.lock().await.is_some() {
                        break Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            GameInstance::GenericInstance(_) => {
                bail!("RCON not available for atom instances")
            }
        }
    })
    .await
}
