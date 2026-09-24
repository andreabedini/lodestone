use async_trait::async_trait;
use color_eyre::eyre::eyre;
use indexmap::IndexMap;

use crate::error::{Error, ErrorKind};
use crate::events::CausedBy;
use crate::macro_executor::ExtensionGenerator;
use crate::traits::t_configurable::manifest::SettingLocalCache;
use crate::traits::t_macro::{HistoryEntry, MacroEntry, TMacro, TaskEntry};

use super::bridge::procedure_call::{lodestone_procedure_bridge, ProcedureBridge};
use super::GenericInstance;

/// Gives a generic instance's core macro the procedure bridge ops
/// (`next_procedure`, `emit_result`, `proc_bridge_ready`).
pub struct ProcedureBridgeExtension {
    bridge: ProcedureBridge,
}

impl ProcedureBridgeExtension {
    pub fn new(bridge: ProcedureBridge) -> Self {
        Self { bridge }
    }
}

impl ExtensionGenerator for ProcedureBridgeExtension {
    fn generate(&self) -> Vec<deno_core::Extension> {
        vec![lodestone_procedure_bridge::init(self.bridge.clone())]
    }
}

#[async_trait]
impl TMacro for GenericInstance {
    async fn get_macro_list(&self) -> Result<Vec<MacroEntry>, Error> {
        Ok(Vec::new())
    }
    async fn get_task_list(&self) -> Result<Vec<TaskEntry>, Error> {
        Ok(Vec::new())
    }
    async fn get_history_list(&self) -> Result<Vec<HistoryEntry>, Error> {
        Ok(Vec::new())
    }
    async fn delete_macro(&self, _name: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn create_macro(&self, _name: &str, _content: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn run_macro(
        &self,
        _name: &str,
        _args: Vec<String>,
        _configs: Option<IndexMap<String, SettingLocalCache>>,
        _caused_by: CausedBy,
    ) -> Result<TaskEntry, Error> {
        Err(Error {
            kind: ErrorKind::UnsupportedOperation,
            source: eyre!("Generic macro is not supported"),
        })
    }
}
