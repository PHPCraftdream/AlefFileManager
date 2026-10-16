// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded filesystem AI builtins. Canonicalization and authorization happen in the
//! blocking job immediately before the operation; canonical-path races remain a residual risk.
use std::{
    io::Read,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use super::provider::ToolDef;
use alef_core::{
    registry::context::CancelHandle,
    security::{consent::Decision, permissions::PermissionSet},
    session::BlockingExecutor,
    AlefError, ErrorCode,
};
use serde_json::{json, Value};

const MAX_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 256;

fn executor() -> &'static BlockingExecutor {
    static EXECUTOR: OnceLock<BlockingExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| BlockingExecutor::new(NonZeroUsize::new(8).expect("eight workers")))
}
fn denied() -> AlefError {
    AlefError::new(
        ErrorCode::PermissionDenied,
        "AI builtin path is not allowed",
    )
}
fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// Return allowed definitions only. Filesystem scopes are represented as path argument schemas,
/// never disclosed to the provider.
pub fn offered(permissions: &PermissionSet, names: &[String]) -> Result<Vec<ToolDef>, AlefError> {
    let mut out = Vec::new();
    for name in names {
        if out.iter().any(|tool: &ToolDef| tool.name == *name) {
            return Err(invalid("a builtin is named twice"));
        }
        let declared = permissions
            .ai_builtins()
            .iter()
            .find(|builtin| builtin.name == *name)
            .ok_or_else(denied)?;
        match name.as_str() {
            "fs.read" | "fs.write" | "fs.list" => {
                let mut allowed = Vec::new();
                for scope in &declared.scopes {
                    match permissions.check_ai_builtin(name, Some(scope))? {
                        Decision::Allow => allowed.push(scope.clone()),
                        Decision::Substitute | Decision::Deny => {},
                    }
                }
                if allowed.is_empty() { continue; }
                let description = format!("{} within an authorized filesystem scope", name);
                out.push(ToolDef { name: name.clone(), description, schema: json!({
                    "type":"object", "properties":{"path":{"type":"string"},"content":{"type":"string"}}, "required":["path"], "additionalProperties":false
                }) });
            }
            "exec" | "bash" => match permissions.check_ai_builtin(name, None)? {
                Decision::Allow => out.push(ToolDef { name: name.clone(), description: "Execution builtin (not available in this runtime)".to_owned(), schema: json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}) }),
                Decision::Substitute => {}, Decision::Deny => return Err(denied()),
            },
            _ => return Err(denied()),
        }
    }
    Ok(out)
}

/// Execute one offered builtin. Filesystem work, including authorization, is one bounded blocking job.
pub async fn execute(
    permissions: Arc<PermissionSet>,
    name: String,
    arguments: Value,
    cancel: CancelHandle,
) -> Result<Value, AlefError> {
    if cancel.is_cancelled() {
        return Err(AlefError::new(ErrorCode::Closed, "chat is closed"));
    }
    if name == "exec" || name == "bash" {
        match permissions.check_ai_builtin(&name, None)? {
            Decision::Allow | Decision::Substitute => {
                return Err(AlefError::new(
                    ErrorCode::NotAvailable,
                    "execution builtin is unavailable",
                ));
            }
            Decision::Deny => return Err(denied()),
        }
    }
    if serde_json::to_vec(&arguments).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return Err(invalid("builtin arguments exceed 64 KiB"));
    }
    let scope = arguments
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("builtin path must be a string"))?
        .to_owned();
    if std::path::Path::new(&scope).is_relative() {
        return Err(invalid("builtin path must be absolute"));
    }
    let path = scope.clone();
    let cancelled = cancel.clone();
    executor()
        .submit(move || {
            if cancelled.is_cancelled() {
                return Err(AlefError::new(ErrorCode::Closed, "chat is closed"));
            }
            let authorization = permissions.authorize_ai_builtin_path(&name, &path)?;
            match authorization.decision {
                Decision::Allow => {}
                Decision::Deny => return Err(denied()),
                Decision::Substitute => {
                    return Err(AlefError::new(
                        ErrorCode::NotAvailable,
                        "substituted builtin is unavailable",
                    ));
                }
            }
            if cancelled.is_cancelled() {
                return Err(AlefError::new(ErrorCode::Closed, "chat is closed"));
            }
            match name.as_str() {
                "fs.read" => {
                    let file = std::fs::File::open(&authorization.path).map_err(|_| {
                        AlefError::new(
                            ErrorCode::Internal,
                            "authorized filesystem operation failed",
                        )
                    })?;
                    let mut bytes = Vec::with_capacity(MAX_BYTES);
                    file.take((MAX_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(|_| {
                            AlefError::new(
                                ErrorCode::Internal,
                                "authorized filesystem operation failed",
                            )
                        })?;
                    if bytes.len() > MAX_BYTES {
                        return Err(invalid("file exceeds 64 KiB"));
                    }
                    let text =
                        String::from_utf8(bytes).map_err(|_| invalid("file is not UTF-8"))?;
                    Ok(json!({"content":text}))
                }
                "fs.write" => {
                    let content = arguments
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("write content must be a string"))?;
                    if content.len() > MAX_BYTES {
                        return Err(invalid("content exceeds 64 KiB"));
                    }
                    std::fs::write(&authorization.path, content).map_err(|_| {
                        AlefError::new(
                            ErrorCode::Internal,
                            "authorized filesystem operation failed",
                        )
                    })?;
                    Ok(json!({"written":content.len()}))
                }
                "fs.list" => {
                    let mut names = Vec::new();
                    for entry in std::fs::read_dir(PathBuf::from(authorization.path))
                        .map_err(|e| AlefError::new(ErrorCode::Internal, e.to_string()))?
                    {
                        if names.len() == MAX_ENTRIES {
                            break;
                        }
                        let entry = entry.map_err(|_| {
                            AlefError::new(
                                ErrorCode::Internal,
                                "authorized filesystem operation failed",
                            )
                        })?;
                        names.push(entry.file_name().to_string_lossy().into_owned());
                    }
                    Ok(json!({"entries":names}))
                }
                _ => Err(denied()),
            }
        })?
        .result()
        .await?
}
