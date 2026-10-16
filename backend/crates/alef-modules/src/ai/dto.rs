// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data shapes of the `ai` module (docs/stages/m9-ai.md §2), exported for the packages.
use serde::{Deserialize, Serialize};

/// A Rush role the application may ask for (`ai.roles`), as the provider describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "modules.ts")]
pub struct Role {
    pub id: String,
    pub title: String,
    pub description: String,
    /// What the role can do (`text`, `tool_calls`, `usage`, `cancel`).
    pub capabilities: Vec<String>,
}

/// One message of the history the application sends; only `user` and `assistant` are allowed.
/// (A tagged enum buffers its content, so `deny_unknown_fields` says nothing here.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "role", rename_all = "lowercase")]
#[ts(export, export_to = "modules.ts")]
pub enum ChatMessage {
    User { content: String },
    Assistant { content: String },
}

/// One part of a chat as it streams to the page (`ai.chat` iterator).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "modules.ts")]
pub enum ChatPart {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        cache_creation_tokens: Option<u64>,
        estimated: bool,
        complete: bool,
    },
    End {
        #[serde(default = "completed")]
        reason: String,
    },
}

/// The only reason a chat ends with while the stub backend stands in.
fn completed() -> String {
    "completed".to_owned()
}

/// What the page asks of `ai.chat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "modules.ts")]
pub struct ChatOptions {
    pub role: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub builtin: Vec<String>,
}
