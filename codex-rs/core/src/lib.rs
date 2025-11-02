//! Root of the `codex-core` library.

// Prevent accidental direct writes to stdout/stderr in library code. All
// user-visible output must go through the appropriate abstraction (e.g.,
// the TUI or the tracing stack).
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;
mod agent_task;
pub mod agent_task_factory;
mod apply_patch;
mod events;
pub use events::{AgentEvent, SubagentCompletionResult};
pub mod main_agent;
mod session;
mod state;
pub use state::AgentStateManager;
pub mod sub_agent;
mod submission_loop;
mod turn_context;
mod types;
pub use types::AgentState;

pub mod auth;
pub mod bash;
mod chat_completions;
mod client;
mod client_common;
pub mod codex;
mod codex_conversation;
mod compact;
pub mod token_data;
pub use codex_conversation::CodexConversation;
mod codex_delegate;
mod command_safety;
pub mod config;
// pub mod config_edit;  // Module not found in main branch
// pub mod config_profile;  // Module not found in main branch
// pub mod config_types;  // Module not found in main branch
pub mod context_store;
pub mod config_loader;
mod conversation_history;
pub mod custom_prompts;
mod environment_context;
pub mod error;

pub mod exec;
pub mod exec_env;
pub mod features;
mod flags;
pub mod git_info;
pub mod landlock;
pub mod mcp;
mod mcp_connection_manager;
mod mcp_tool_call;
mod message_history;
mod model_provider_info;
pub mod multi_agent_coordinator;
pub mod parse_command;
pub mod performance;
mod response_processing;
pub mod sandboxing;
mod truncate;
mod unified_exec;
mod user_instructions;
pub use model_provider_info::BUILT_IN_OSS_MODEL_PROVIDER_ID;
pub use model_provider_info::ModelProviderInfo;
pub use model_provider_info::WireApi;
pub use model_provider_info::built_in_model_providers;
pub use model_provider_info::create_oss_provider_with_base_url;
mod conversation_manager;
mod event_mapping;
pub mod review_format;
pub use codex_protocol::protocol::InitialHistory;
pub use conversation_manager::ConversationManager;
pub use conversation_manager::NewConversation;
// Re-export common auth types for workspace consumers
pub use auth::AuthManager;
pub use auth::CodexAuth;
pub mod default_client;

pub mod function_call_router;
pub mod llm_subagent_executor;
pub mod mock_subagent_executor;
pub mod model_family;
mod openai_model_info;
mod openai_tools;
pub mod project_doc;
mod rollout;
pub(crate) mod safety;
pub mod seatbelt;
pub mod shell;
pub mod spawn;
pub mod subagent_completion_tracker;
pub mod subagent_manager;
pub mod subagent_system_messages;
pub mod terminal;
// mod tool_apply_patch;  // Module not found in main branch
pub mod tool_config;
pub mod tool_registry;
mod tools;
pub mod turn_diff_tracker;
pub mod unified_error_handler;
pub mod unified_error_types;
pub mod unified_function_executor;
pub mod unified_function_handler;
pub use rollout::ARCHIVED_SESSIONS_SUBDIR;
pub use rollout::INTERACTIVE_SESSION_SOURCES;
pub use rollout::RolloutRecorder;
pub use rollout::SESSIONS_SUBDIR;
pub use rollout::SessionMeta;
pub use rollout::find_conversation_path_by_id_str;
pub use rollout::list::ConversationItem;
pub use rollout::list::ConversationsPage;
pub use rollout::list::Cursor;
pub use rollout::list::read_head_for_summary;
mod function_tool;
mod tasks;
mod user_notification;
pub mod util;



pub use apply_patch::CODEX_APPLY_PATCH_ARG1;
pub use command_safety::is_safe_command;
pub use safety::get_platform_sandbox;
// Re-export the protocol types from the standalone `codex-protocol` crate so existing
// `codex_core::protocol::...` references continue to work across the workspace.
pub use codex_protocol::protocol;
// Re-export protocol config enums to ensure call sites can use the same types
// as those in the protocol crate when constructing protocol messages.
pub use codex_protocol::config_types as protocol_config_types;

pub use client::ModelClient;
pub use client_common::Prompt;
pub use client_common::REVIEW_PROMPT;
pub use client_common::ResponseEvent;
pub use client_common::ResponseStream;
pub use codex::compact::content_items_to_text;
pub use codex_protocol::models::ContentItem;
pub use codex_protocol::models::LocalShellAction;
pub use codex_protocol::models::LocalShellExecAction;
pub use codex_protocol::models::LocalShellStatus;
pub use codex_protocol::models::ResponseItem;
pub use compact::is_session_prefix_message;
pub use event_mapping::parse_turn_item;
pub mod otel_init;
