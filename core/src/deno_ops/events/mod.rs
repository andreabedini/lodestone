//! Event ops. They only use the [`EventBroadcaster`] (a broadcast channel,
//! usable from any runtime), so they run on the macro's own runtime.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context;
use deno_core::{op2, OpState};

use super::MacroOpError;
use crate::{
    event_broadcaster::{EventBroadcaster, PlayerChange, PlayerMessage},
    events::{
        CausedBy, Event, InstanceEvent, ProgressionEndValue, ProgressionEventID,
        ProgressionStartValue,
    },
    macro_executor::MacroPID,
    traits::t_server::State,
    types::InstanceUuid,
};

#[op2]
#[serde]
pub async fn next_event(state: Rc<RefCell<OpState>>) -> Result<Event, MacroOpError> {
    let rx = state.borrow().borrow::<EventBroadcaster>().clone();
    let event = rx
        .subscribe()
        .recv()
        .await
        .context("Failed to receive event")?;
    Ok(event)
}

#[op2]
#[serde]
pub async fn next_instance_event(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> InstanceEvent {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster.next_instance_event(&instance_uuid).await
}

#[op2]
#[serde]
pub async fn next_instance_state_change(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> State {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster
        .next_instance_state_change(&instance_uuid)
        .await
}

#[op2]
#[string]
pub async fn next_instance_output(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> String {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster.next_instance_output(&instance_uuid).await
}

#[op2]
#[serde]
pub async fn next_instance_player_message(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> PlayerMessage {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster
        .next_instance_player_message(&instance_uuid)
        .await
}

#[op2]
#[string]
pub async fn next_instance_system_message(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> String {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster
        .next_instance_system_message(&instance_uuid)
        .await
}

#[op2]
#[serde]
pub async fn next_instance_player_change(
    state: Rc<RefCell<OpState>>,
    #[serde] instance_uuid: InstanceUuid,
) -> PlayerChange {
    let event_broadcaster = state.borrow().borrow::<EventBroadcaster>().clone();
    event_broadcaster
        .next_instance_player_change(&instance_uuid)
        .await
}

#[op2]
pub fn emit_detach(state: &mut OpState, #[serde] macro_pid: MacroPID) {
    let tx = state.borrow::<EventBroadcaster>();
    tx.send(Event::new_macro_detach_event(macro_pid));
}

#[op2]
pub fn emit_console_out(
    state: &mut OpState,
    #[serde] instance_uuid: InstanceUuid,
    #[string] instance_name: String,
    #[string] line: String,
) {
    let tx = state.borrow::<EventBroadcaster>();
    tx.send(Event::new_instance_output(
        instance_uuid,
        instance_name,
        line,
    ));
}

#[op2]
pub fn emit_state_change(
    state: &mut OpState,
    #[serde] instance_uuid: InstanceUuid,
    #[string] instance_name: String,
    #[serde] new_state: State,
) {
    let tx = state.borrow::<EventBroadcaster>();
    tx.send(Event::new_instance_state_transition(
        instance_uuid,
        instance_name,
        new_state,
    ))
}

#[op2]
#[serde]
pub fn emit_progression_event_start(
    state: &mut OpState,
    #[string] progression_name: String,
    total: Option<f64>,
    #[serde] inner: Option<ProgressionStartValue>,
) -> ProgressionEventID {
    let tx = state.borrow::<EventBroadcaster>();
    let (event, id) =
        Event::new_progression_event_start(progression_name, total, inner, CausedBy::System);
    tx.send(event);
    id
}

#[op2]
pub fn emit_progression_event_update(
    state: &mut OpState,
    #[serde] event_id: ProgressionEventID,
    #[string] progress_msg: String,
    progress: f64,
) {
    let tx = state.borrow::<EventBroadcaster>();
    tx.send(Event::new_progression_event_update(
        &event_id,
        progress_msg,
        progress,
    ));
}

#[op2]
pub fn emit_progression_event_end(
    state: &mut OpState,
    #[serde] event_id: ProgressionEventID,
    success: bool,
    #[string] message: Option<String>,
    #[serde] inner: Option<ProgressionEndValue>,
) {
    let tx = state.borrow::<EventBroadcaster>();
    tx.send(Event::new_progression_event_end(
        event_id, success, message, inner,
    ));
}
