// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session-owned cookie jar commands.
use super::Jar;
use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::resources::Resource,
    AlefError,
};
use serde::Deserialize;
use serde_json::json;
use std::{future::Future, pin::Pin};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Open {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    jar: u64,
    url: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Clear {
    jar: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Close {
    jar: u64,
}
pub(in crate::net::http) struct Owned(pub(in crate::net::http) Jar);
impl Resource for Owned {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move { self.0.close() })
    }
}
fn scoped(
    ctx: &alef_core::registry::context::CallContext,
    text: &str,
) -> Result<url::Url, AlefError> {
    let u = super::super::spec::spec(text, None, &[], false)?.url;
    match ctx
        .permissions
        .check(Permission::NetHttp, Some(u.as_str()), &ctx.grants())?
    {
        Decision::Allow => Ok(u),
        _ => Err(AlefError::new(
            alef_core::ErrorCode::PermissionDenied,
            "address is outside the allowed HTTP scope",
        )),
    }
}
pub fn register(r: &mut Registry) -> Result<(), AlefError> {
    r.command::<Open>("http.cookieJar")?
        .handler(|ctx, _| async move {
            let id = ctx.resources().insert(Box::new(Owned(Jar::new())))?;
            Ok(Reply::Json(json!({"jar":id.0})))
        })?;
    r.command::<Args>("http.cookieJar.list")?
        .handler(|ctx, a| async move {
            let u = scoped(&ctx, &a.url)?;
            let jar = ctx
                .resources()
                .with_as::<Owned, _>(ResourceId(a.jar), |x| x.0.clone())?;
            let vals = jar.list(&u)?;
            Ok(Reply::Json(json!(vals
                .into_iter()
                .map(|(name, value)| json!({"name":name,"value":value}))
                .collect::<Vec<_>>())))
        })?;
    r.command::<Clear>("http.cookieJar.clear")?
        .handler(|ctx, a| async move {
            let jar = ctx
                .resources()
                .with_as::<Owned, _>(ResourceId(a.jar), |x| x.0.clone())?;
            jar.clear()?;
            Ok(Reply::Json(json!({})))
        })?;
    r.command::<Close>("http.cookieJar.close")?
        .handler(|ctx, a| async move {
            let id = ResourceId(a.jar);
            ctx.resources().with_as::<Owned, _>(id, |_| ())?;
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(json!({})))
        })?;
    Ok(())
}
