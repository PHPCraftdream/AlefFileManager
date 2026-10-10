// SPDX-License-Identifier: MIT OR Apache-2.0
//! Document-owned menus; the host owns native presentation and owner-scoped release.
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as SyncMutex, Weak},
};

use alef_core::{
    ids::SessionId,
    registry::{
        command::Reply,
        context::CallContext,
        dispatch::Registry,
        host::Host,
        window::{
            menu::{MenuCall, MenuItem, MenuKind},
            UiCall,
        },
    },
    session::Resource,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use tokio::sync::Mutex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationArgs {
    items: Vec<MenuItem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowArgs {
    label: Option<String>,
    items: Vec<MenuItem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PopupArgs {
    items: Vec<MenuItem>,
    x: Option<f64>,
    y: Option<f64>,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}
fn internal() -> AlefError {
    AlefError::new(ErrorCode::Internal, "invalid menu reply from UI host")
}

#[cfg(any(windows, target_os = "macos"))]
fn well_formed(text: &str) -> bool {
    text.parse::<muda::accelerator::Accelerator>().is_ok()
}
#[cfg(not(any(windows, target_os = "macos")))]
fn well_formed(text: &str) -> bool {
    text.parse::<global_hotkey::hotkey::HotKey>().is_ok()
}

fn accelerators(items: &[MenuItem]) -> Result<(), AlefError> {
    for item in items {
        if item
            .accelerator
            .as_deref()
            .is_some_and(|text| !well_formed(text))
        {
            return Err(invalid("invalid menu accelerator"));
        }
        if let Some(children) = &item.items {
            accelerators(children)?;
        }
    }
    Ok(())
}

fn actionable(items: &[MenuItem], id: &str) -> bool {
    items.iter().any(|item| {
        item.enabled != Some(false)
            && ((matches!(item.effective_kind(), MenuKind::Normal | MenuKind::Check)
                && item.id.as_deref() == Some(id))
                || item
                    .items
                    .as_ref()
                    .is_some_and(|children| actionable(children, id)))
    })
}

#[derive(Default)]
struct State {
    inserted: bool,
}
#[derive(Clone)]
struct Menu {
    host: Arc<dyn Host>,
    window: u64,
    owner: SessionId,
    state: Arc<Mutex<State>>,
}
impl Menu {
    async fn release(&self) -> Result<(), AlefError> {
        self.host
            .ui(
                self.window,
                UiCall::Menu(MenuCall::Release { owner: self.owner }),
            )
            .await?;
        Ok(())
    }
}
impl Resource for Menu {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            // Waits for a call in flight; a late one fails to insert its resource and is rolled back.
            let _state = self.state.lock().await;
            // Teardown has no error channel; the host's owner-scoped release is idempotent.
            let _ = self.release().await;
        })
    }
}

type States = Arc<SyncMutex<HashMap<SessionId, Weak<Mutex<State>>>>>;

async fn execute(
    ctx: CallContext,
    host: Arc<dyn Host>,
    states: States,
    call: MenuCall,
) -> Result<Reply, AlefError> {
    if ctx.body().is_some() {
        return Err(invalid("menu commands do not accept a binary body"));
    }
    call.check().map_err(|message| invalid(&message))?;
    let items = match &call {
        MenuCall::SetApplication { items, .. }
        | MenuCall::SetWindow { items, .. }
        | MenuCall::Popup { items, .. } => items,
        MenuCall::Release { .. } => unreachable!(),
    };
    accelerators(items)?;
    let state = {
        let mut states = states.lock().expect("menu states mutex poisoned");
        states.retain(|_, state| state.strong_count() > 0);
        let entry = states.entry(ctx.session.id()).or_default();
        match entry.upgrade() {
            Some(state) => state,
            None => {
                let state = Arc::new(Mutex::new(State::default()));
                *entry = Arc::downgrade(&state);
                state
            }
        }
    };
    let menu = Menu {
        host,
        window: ctx.session.window(),
        owner: ctx.session.id(),
        state,
    };
    let mut state = menu.state.lock().await;
    let reply = menu
        .host
        .ui(menu.window, UiCall::Menu(call.clone()))
        .await?;
    // Insert once per document, including popups. Serialization with teardown prevents a
    // release racing a replacement, while the original owner prevents cross-document release.
    if !state.inserted {
        if let Err(error) = ctx.resources().insert(Box::new(menu.clone())) {
            menu.release().await?;
            return Err(error);
        }
        state.inserted = true;
    }
    let valid = match &call {
        MenuCall::Popup { .. } => {
            reply.is_null() || reply.as_str().is_some_and(|id| actionable(items, id))
        }
        _ => reply.is_null(),
    };
    if !valid {
        return Err(internal());
    }
    Ok(Reply::Json(reply))
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    let states: States = Arc::new(SyncMutex::new(HashMap::new()));
    let application_host = host.clone();
    let application_states = states.clone();
    registry
        .command::<ApplicationArgs>("menu.setApplicationMenu")?
        .handler(move |ctx, args| {
            let call = MenuCall::SetApplication {
                owner: ctx.session.id(),
                items: args.items,
            };
            let transaction = tokio::spawn(execute(
                ctx,
                application_host.clone(),
                application_states.clone(),
                call,
            ));
            async move { transaction.await.map_err(|_| internal())? }
        })?;
    let window_host = host.clone();
    let window_states = states.clone();
    registry
        .command::<WindowArgs>("menu.setWindowMenu")?
        .handler(move |ctx, args| {
            let call = MenuCall::SetWindow {
                owner: ctx.session.id(),
                label: args.label,
                items: args.items,
            };
            let transaction = tokio::spawn(execute(
                ctx,
                window_host.clone(),
                window_states.clone(),
                call,
            ));
            async move { transaction.await.map_err(|_| internal())? }
        })?;
    registry
        .command::<PopupArgs>("menu.popup")?
        .handler(move |ctx, args| {
            let call = MenuCall::Popup {
                owner: ctx.session.id(),
                label: None,
                items: args.items,
                x: args.x,
                y: args.y,
            };
            // Like shortcut registration, caller cancellation must not abandon insertion/rollback.
            let transaction = tokio::spawn(execute(ctx, host.clone(), states.clone(), call));
            async move { transaction.await.map_err(|_| internal())? }
        })
}
