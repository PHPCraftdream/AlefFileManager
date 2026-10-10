// SPDX-License-Identifier: MIT OR Apache-2.0
//! Permission-checked, document-owned tray resources; no native backend lives here.
use alef_core::{
    ids::{ResourceId, SessionId},
    registry::{
        command::Reply,
        context::CallContext,
        dispatch::Registry,
        host::Host,
        window::{
            menu::MenuItem,
            tray::{self, TrayCall},
            UiCall,
        },
    },
    security::{consent::Decision, permissions::Permission},
    session::Resource,
    AlefError, ErrorCode,
};
use image::{ImageFormat, ImageReader, Limits};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io::Cursor,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as SyncMutex, Weak,
    },
};
use tokio::sync::Mutex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    #[serde(default)]
    tooltip: String,
    #[serde(default)]
    menu: Vec<MenuItem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TooltipArgs {
    id: String,
    tooltip: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuArgs {
    id: String,
    menu: Vec<MenuItem>,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}
fn not_found() -> AlefError {
    AlefError::new(ErrorCode::NotFound, "tray not found")
}
fn internal() -> AlefError {
    AlefError::new(
        ErrorCode::Internal,
        "invalid tray reply or failed tray transaction",
    )
}
fn handle(owner: SessionId, id: ResourceId) -> String {
    format!("s{}:r{}", owner.0, id.0)
}
fn resource_id(text: &str, owner: SessionId) -> Result<ResourceId, AlefError> {
    text.split_once(":r")
        .and_then(|(_, id)| id.parse().ok())
        .map(ResourceId)
        .filter(|id| text == handle(owner, *id))
        .ok_or_else(not_found)
}

/// Validate on all platforms and even for substituted calls. Set limits before decoding,
/// including the worst-case 16-bit RGBA allocation; never use clipboard's larger limits.
fn icon(ctx: &CallContext) -> Result<Vec<u8>, AlefError> {
    let png = ctx
        .body()
        .ok_or_else(|| invalid("tray icon needs a binary PNG body"))?;
    if png.is_empty() || png.len() > tray::ICON_BYTES_LIMIT {
        return Err(invalid("tray PNG must be 1..256 KiB"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(tray::ICON_SIDE_LIMIT);
    limits.max_image_height = Some(tray::ICON_SIDE_LIMIT);
    limits.max_alloc = Some(tray::ICON_DECODED_LIMIT);
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| invalid("invalid or oversized tray PNG"))?;
    if decoded.width() == 0
        || decoded.height() == 0
        || decoded.width() > tray::ICON_SIDE_LIMIT
        || decoded.height() > tray::ICON_SIDE_LIMIT
    {
        return Err(invalid("tray PNG dimensions must be 1..256 on each side"));
    }
    Ok(png.to_vec())
}
fn no_body(ctx: &CallContext) -> Result<(), AlefError> {
    if ctx.body().is_some() {
        return Err(invalid("this tray command does not accept a binary body"));
    }
    Ok(())
}

#[derive(Default)]
struct State {
    live: HashSet<String>,
}
type States = Arc<SyncMutex<HashMap<SessionId, Weak<Mutex<State>>>>>;
fn state(states: &States, owner: SessionId) -> Arc<Mutex<State>> {
    let mut states = states.lock().expect("tray states mutex poisoned");
    states.retain(|_, state| state.strong_count() > 0);
    let entry = states.entry(owner).or_default();
    if let Some(state) = entry.upgrade() {
        return state;
    }
    let state = Arc::new(Mutex::new(State::default()));
    *entry = Arc::downgrade(&state);
    state
}

#[derive(Clone)]
struct Tray {
    host: Arc<dyn Host>,
    window: u64,
    owner: SessionId,
    token: String,
    native: bool,
    state: Arc<Mutex<State>>,
}
impl Tray {
    async fn native_destroy(&self) -> Result<(), AlefError> {
        if self.native {
            let reply = self
                .host
                .ui(
                    self.window,
                    UiCall::Tray(TrayCall::Destroy {
                        owner: self.owner,
                        token: self.token.clone(),
                    }),
                )
                .await?;
            if !reply.is_null() {
                return Err(internal());
            }
        }
        Ok(())
    }
    async fn destroy(&self) -> Result<(), AlefError> {
        let mut state = self.state.lock().await;
        if state.live.contains(&self.token) {
            self.native_destroy().await?;
            state.live.remove(&self.token);
        }
        Ok(())
    }
}
impl Resource for Tray {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let _ = self.destroy().await;
        })
    }
}

async fn create(
    ctx: CallContext,
    args: CreateArgs,
    host: Arc<dyn Host>,
    states: States,
    counter: Arc<AtomicU64>,
) -> Result<Reply, AlefError> {
    let icon = icon(&ctx)?;
    tray::check_tooltip(&args.tooltip).map_err(|e| invalid(&e))?;
    tray::check_menu(&args.menu).map_err(|e| invalid(&e))?;
    let owner = ctx.session.id();
    let state = state(&states, owner);
    let mut locked = state.lock().await;
    if locked.live.len() >= tray::SESSION_LIMIT {
        return Err(AlefError::new(
            ErrorCode::Busy,
            "tray limit of eight per document reached",
        ));
    }
    let number = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .map_err(|_| internal())?;
    let token = format!("tray:{number}");
    let resource = Tray {
        host,
        window: ctx.session.window(),
        owner,
        token: token.clone(),
        native: ctx.decision() != Decision::Substitute,
        state: state.clone(),
    };
    if resource.native {
        let reply = resource
            .host
            .ui(
                resource.window,
                UiCall::Tray(TrayCall::Create {
                    owner,
                    token: token.clone(),
                    icon,
                    tooltip: args.tooltip,
                    menu: args.menu,
                }),
            )
            .await?;
        if !reply.is_null() {
            resource.native_destroy().await?;
            return Err(internal());
        }
    }
    let id = match ctx.resources().insert(Box::new(resource.clone())) {
        Ok(id) => id,
        Err(error) => {
            resource.native_destroy().await?;
            return Err(error);
        }
    };
    locked.live.insert(token.clone());
    Ok(Reply::Json(
        json!({"id": handle(owner, id), "owner": owner.0, "token": token}),
    ))
}

fn get(ctx: &CallContext, id: &str) -> Result<(ResourceId, Tray), AlefError> {
    let id = resource_id(id, ctx.session.id())?;
    Ok((id, ctx.resources().with_as::<Tray, _>(id, Clone::clone)?))
}
async fn mutate(
    ctx: CallContext,
    id: String,
    call: impl FnOnce(&Tray) -> TrayCall,
) -> Result<Reply, AlefError> {
    let (_, resource) = get(&ctx, &id)?;
    let locked = resource.state.lock().await;
    if !locked.live.contains(&resource.token) {
        return Err(not_found());
    }
    let call = call(&resource);
    call.check().map_err(|e| invalid(&e))?;
    if resource.native && ctx.decision() != Decision::Substitute {
        let reply = resource
            .host
            .ui(resource.window, UiCall::Tray(call))
            .await?;
        if !reply.is_null() {
            return Err(internal());
        }
    }
    Ok(Reply::Json(Value::Null))
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    let states: States = Arc::new(SyncMutex::new(HashMap::new()));
    let counter = Arc::new(AtomicU64::new(1));
    registry
        .command::<CreateArgs>("tray.create")?
        .permission(Permission::Tray, |_| None)
        .substitutes()
        .handler(move |ctx, args| {
            // Detached transaction always finishes insertion or rollback after transport cancellation.
            let task = tokio::spawn(create(
                ctx,
                args,
                host.clone(),
                states.clone(),
                counter.clone(),
            ));
            async move { task.await.map_err(|_| internal())? }
        })?;
    registry
        .command::<IdArgs>("tray.setIcon")?
        .permission(Permission::Tray, |_| None)
        .substitutes()
        .handler(|ctx, args| async move {
            let icon = icon(&ctx)?;
            mutate(ctx, args.id, |r| TrayCall::SetIcon {
                owner: r.owner,
                token: r.token.clone(),
                icon,
            })
            .await
        })?;
    registry
        .command::<TooltipArgs>("tray.setTooltip")?
        .permission(Permission::Tray, |_| None)
        .substitutes()
        .handler(|ctx, args| async move {
            no_body(&ctx)?;
            tray::check_tooltip(&args.tooltip).map_err(|e| invalid(&e))?;
            mutate(ctx, args.id, |r| TrayCall::SetTooltip {
                owner: r.owner,
                token: r.token.clone(),
                tooltip: args.tooltip,
            })
            .await
        })?;
    registry
        .command::<MenuArgs>("tray.setMenu")?
        .permission(Permission::Tray, |_| None)
        .substitutes()
        .handler(|ctx, args| async move {
            no_body(&ctx)?;
            tray::check_menu(&args.menu).map_err(|e| invalid(&e))?;
            mutate(ctx, args.id, |r| TrayCall::SetMenu {
                owner: r.owner,
                token: r.token.clone(),
                menu: args.menu,
            })
            .await
        })?;
    registry
        .command::<IdArgs>("tray.destroy")?
        .handler(|ctx, args| {
            let task = tokio::spawn(async move {
                no_body(&ctx)?;
                let (id, resource) = get(&ctx, &args.id)?;
                resource.destroy().await?;
                ctx.resources().take(id)?.close().await;
                Ok(Reply::Json(Value::Null))
            });
            async move { task.await.map_err(|_| internal())? }
        })
}
