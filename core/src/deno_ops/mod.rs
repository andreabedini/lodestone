//! Lodestone's ops: the Rust functions that macro JS calls through
//! `Deno[Deno.internal].core.ops` (see `macro_executor/bootstrap.js`).
//!
//! The ops are registered by the `lodestone` extension in
//! `macro_executor/extension.rs`.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use deno_core::OpState;

pub mod events;
pub mod instance_control;
pub mod prelude;

/// The error type of every Lodestone op.
///
/// Op bodies keep using `anyhow` (`?`, `.context()`, `bail!`): the catch-all
/// variant converts it, and JS sees a plain `Error` whose message is the whole
/// context chain ("outer: inner").
#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum MacroOpError {
    #[class(generic)]
    #[error("{0:#}")]
    Other(#[from] anyhow::Error),
}

/// Handle to Lodestone's shared tokio runtime, kept in each macro's `OpState`.
///
/// A macro runs on its own thread with its own current-thread runtime, which
/// goes away when the macro exits. Op bodies that touch the app state or an
/// instance must run on the shared runtime instead (see [`run_on_shared`]).
#[derive(Clone)]
pub struct SharedRuntime(pub tokio::runtime::Handle);

/// Run `fut` on the shared runtime and wait for it from the macro's runtime.
///
/// Instance methods spawn tasks of their own (a server's supervision tasks,
/// for example). Run on the macro's runtime, those tasks would die with the
/// macro. The IO objects an instance owns (RCON connections, child process
/// pipes) are also registered with the shared runtime.
///
/// If the macro is killed while waiting, `fut` keeps running to completion
/// on the shared runtime.
pub async fn run_on_shared<T, F>(state: &Rc<RefCell<OpState>>, fut: F) -> Result<T, MacroOpError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, anyhow::Error>> + Send + 'static,
{
    let handle = state.borrow().borrow::<SharedRuntime>().0.clone();
    handle
        .spawn(fut)
        .await
        .map_err(|e| anyhow::Error::new(e).context("Op task on the shared runtime failed"))?
        .map_err(MacroOpError::from)
}
