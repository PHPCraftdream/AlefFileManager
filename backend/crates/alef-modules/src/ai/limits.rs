// SPDX-License-Identifier: MIT OR Apache-2.0
//! The deadlines and the sizes of the `ai` module (docs/stages/m9-ai.md §6). The user may lower
//! them; the runtime never raises them.

/// `ai.roles` answers within this.
pub const ROLE_LIST_TIME: std::time::Duration = std::time::Duration::from_secs(10);
/// The chat fails when the provider has not produced its first event within this.
pub const FIRST_EVENT_TIME: std::time::Duration = std::time::Duration::from_secs(30);
/// After the first event, the chat fails when the provider stays silent this long.
pub const STREAM_IDLE_TIME: std::time::Duration = std::time::Duration::from_secs(60);
/// The whole chat, tool handlers included, fails at this.
pub const CHAT_TIME: std::time::Duration = std::time::Duration::from_secs(300);
/// How long one tool handler may take on its own (enforced by the provider's wait on the result;
/// the chat deadlines bound the handler in practice).
pub const HANDLER_TIME: std::time::Duration = std::time::Duration::from_secs(30);
/// The whole serialized `ai.chat` arguments, at the most.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Named tools per chat, at the most; also the number of live registrations per session.
pub const MAX_TOOLS: usize = 32;
/// One tool schema, argument or result, serialized, at the most.
pub const MAX_TOOL_DATA_BYTES: usize = 64 * 1024;
/// One text part of a chat, serialized, at the most.
pub const MAX_TEXT_PART_BYTES: usize = 256 * 1024;
/// Parallel chats across every application, at the most.
pub const MAX_CHATS: usize = 16;
/// Parallel chats per application, at the most.
pub const MAX_CHATS_PER_APPLICATION: usize = 2;
/// Pending tool calls per application, at the most.
pub const MAX_PENDING_CALLS: usize = 8;

/// One message of the history, at the most.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const BOUND: Duration = Duration::from_secs(600);
    const ONE_MIB: usize = 1024 * 1024;

    #[test]
    fn every_deadline_is_nonzero_and_at_most_ten_minutes() {
        for duration in [
            ROLE_LIST_TIME,
            FIRST_EVENT_TIME,
            STREAM_IDLE_TIME,
            CHAT_TIME,
            HANDLER_TIME,
        ] {
            assert!(!duration.is_zero());
            assert!(duration <= BOUND, "{duration:?}");
        }
    }

    #[test]
    fn every_size_is_nonzero_and_at_most_one_mebibyte() {
        for size in [
            MAX_MESSAGE_BYTES,
            MAX_TOOLS,
            MAX_TOOL_DATA_BYTES,
            MAX_TEXT_PART_BYTES,
            MAX_CONTENT_BYTES,
        ] {
            assert!(size > 0);
            assert!(size <= ONE_MIB, "{size}");
        }
    }

    #[test]
    fn the_handler_deadline_is_shorter_than_the_whole_chat() {
        assert!(HANDLER_TIME < CHAT_TIME);
    }

    #[test]
    fn the_first_event_may_not_take_longer_than_a_stream_idle_gap() {
        assert!(FIRST_EVENT_TIME <= STREAM_IDLE_TIME);
    }
}
