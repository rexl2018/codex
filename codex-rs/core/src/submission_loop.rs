use std::sync::Arc;

use async_channel::Receiver;
use tracing::{debug, error, info, warn};

use crate::{
    agent_task::AgentTask,
    config::Config,
    config_edit::{persist_non_null_overrides, CONFIG_KEY_EFFORT, CONFIG_KEY_MODEL},
    context_store::{ContextQuery, IContextRepository},
    environment_context::EnvironmentContext,
    model_family::find_family_for_model,
    openai_model_info::get_model_info,
    protocol::{
        AgentMessageEvent, BackgroundEventEvent, ContextQueryResultEvent, ContextSummary,
        ConversationPathResponseEvent, ErrorEvent, Event, EventMsg, GetContextsResultEvent,
        GetHistoryEntryResponseEvent, InputItem, ListCustomPromptsResponseEvent,
        LoadContextsFromFileResultEvent, McpListToolsResponseEvent, Op, ReviewDecision,
        SaveContextsToFileResultEvent, Submission,
    },
    session::{MutexExt, Session},
    subagent_manager::{ISubagentManager, SubagentTaskSpec},
    tool_config::UnifiedToolConfig,
    turn_context::TurnContext,
};
use codex_protocol::{
    custom_prompts::CustomPrompt,
    models::ResponseItem,
    protocol::{ContextItem, InitialHistory, SubagentStartedEvent},
};

/// Refactored submission loop that uses the new agent architecture
pub(crate) async fn refactored_submission_loop(
    sess: Arc<Session>,
    turn_context: TurnContext,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // For now, delegate to the original submission loop
    // This can be enhanced later with proper agent routing
    original_submission_loop(sess, turn_context, config, rx_sub).await;
}

pub(crate) async fn submission_loop(
    sess: Arc<Session>,
    turn_context: TurnContext,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // Use the refactored submission loop
    refactored_submission_loop(sess, turn_context, config, rx_sub).await;
}

pub(crate) async fn original_submission_loop(
    sess: Arc<Session>,
    turn_context: TurnContext,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // Wrap once to avoid cloning TurnContext for each task.
    let mut turn_context = Arc::new(turn_context);
    // To break out of this loop, send Op::Shutdown.
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        match sub.op {
            Op::Interrupt => {
                sess.interrupt_task();
            }
            Op::OverrideTurnContext {
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
            } => {
                // Recalculate the persistent turn context with provided overrides.
                let prev = Arc::clone(&turn_context);
                let provider = prev.client.get_provider();

                // Effective model + family
                let (effective_model, effective_family) = if let Some(ref m) = model {
                    let fam =
                        find_family_for_model(m).unwrap_or_else(|| config.model_family.clone());
                    (m.clone(), fam)
                } else {
                    (prev.client.get_model(), prev.client.get_model_family())
                };

                // Effective reasoning settings
                let effective_effort = effort.unwrap_or(prev.client.get_reasoning_effort());
                let effective_summary = summary.unwrap_or(prev.client.get_reasoning_summary());

                let auth_manager = prev.client.get_auth_manager();

                // Build updated config for the client
                let mut updated_config = (*config).clone();
                updated_config.model = effective_model.clone();
                updated_config.model_family = effective_family.clone();
                if let Some(model_info) = get_model_info(&effective_family) {
                    updated_config.model_context_window = Some(model_info.context_window);
                }

                let client = crate::client::ModelClient::new(
                    Arc::new(updated_config),
                    auth_manager,
                    provider,
                    effective_effort,
                    effective_summary,
                    sess.conversation_id,
                );

                let new_approval_policy = approval_policy.unwrap_or(prev.approval_policy);
                let new_sandbox_policy = sandbox_policy
                    .clone()
                    .unwrap_or(prev.sandbox_policy.clone());
                let new_cwd = cwd.clone().unwrap_or_else(|| prev.cwd.clone());

                let new_turn_context = TurnContext {
                    client,
                    unified_tool_config: UnifiedToolConfig::default(),
                    user_instructions: prev.user_instructions.clone(),
                    base_instructions: prev.base_instructions.clone(),
                    approval_policy: new_approval_policy,
                    sandbox_policy: new_sandbox_policy.clone(),
                    shell_environment_policy: prev.shell_environment_policy.clone(),
                    cwd: new_cwd.clone(),
                };

                // Install the new persistent context for subsequent tasks/turns.
                turn_context = Arc::new(new_turn_context);

                // Optionally persist changes to model / effort
                let effort_str = effort.and_then(|_| effective_effort.map(|e| e.to_string()));

                if let Err(e) = persist_non_null_overrides(
                    &config.codex_home,
                    config.active_profile.as_deref(),
                    &[
                        (&[CONFIG_KEY_MODEL], model.as_deref()),
                        (&[CONFIG_KEY_EFFORT], effort_str.as_deref()),
                    ],
                )
                .await
                {
                    warn!("failed to persist overrides: {e:#}");
                }

                // Note: Environment context recording would be handled here
                // but requires access to private methods
            }
            Op::UserInput { items } => {
                // attempt to inject input into current task
                if let Err(items) = sess.inject_input(items) {
                    // no current task, spawn a new one
                    tracing::debug!("Creating agent task for UserInput: sub_id={}", sub.id);
                    tracing::info!("Creating agent task for UserInput: sub_id={}", sub.id);

                    // Set the flag to indicate a new user task has been created
                    if let Some(components) = &sess.multi_agent_components {
                        components
                            .new_user_task_created
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }

                    let task =
                        AgentTask::spawn(sess.clone(), Arc::clone(&turn_context), sub.id, items);
                    sess.set_task(task);
                }
            }
            Op::UserTurn {
                items,
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
            } => {
                // attempt to inject input into current task
                if let Err(items) = sess.inject_input(items) {
                    // Derive a fresh TurnContext for this turn using the provided overrides.
                    let provider = turn_context.client.get_provider();
                    let auth_manager = turn_context.client.get_auth_manager();

                    // Derive a model family for the requested model; fall back to the session's.
                    let model_family = find_family_for_model(&model)
                        .unwrap_or_else(|| config.model_family.clone());

                    // Create a per‑turn Config clone with the requested model/family.
                    let mut per_turn_config = (*config).clone();
                    per_turn_config.model = model.clone();
                    per_turn_config.model_family = model_family.clone();
                    if let Some(model_info) = get_model_info(&model_family) {
                        per_turn_config.model_context_window = Some(model_info.context_window);
                    }

                    // Build a new client with per‑turn reasoning settings.
                    // Reuse the same provider and session id; auth defaults to env/API key.
                    let client = crate::client::ModelClient::new(
                        Arc::new(per_turn_config),
                        auth_manager,
                        provider,
                        effort,
                        summary,
                        sess.conversation_id,
                    );

                    let fresh_turn_context = TurnContext {
                        client,
                        unified_tool_config: UnifiedToolConfig::default(),
                        user_instructions: turn_context.user_instructions.clone(),
                        base_instructions: turn_context.base_instructions.clone(),
                        approval_policy,
                        sandbox_policy,
                        shell_environment_policy: turn_context.shell_environment_policy.clone(),
                        cwd,
                    };
                    // TODO: record the new environment context in the conversation history
                    // no current task, spawn a new one with the per‑turn context
                    tracing::debug!("Creating agent task for UserTurn: sub_id={}", sub.id);
                    tracing::info!("Creating agent task for UserTurn: sub_id={}", sub.id);
                    let task =
                        AgentTask::spawn(sess.clone(), Arc::new(fresh_turn_context), sub.id, items);
                    sess.set_task(task);
                }
            }
            Op::ExecApproval { id, decision } => match decision {
                ReviewDecision::Abort => {
                    sess.interrupt_task();
                }
                other => sess.notify_approval(&id, other),
            },
            Op::PatchApproval { id, decision } => match decision {
                ReviewDecision::Abort => {
                    sess.interrupt_task();
                }
                other => sess.notify_approval(&id, other),
            },
            Op::AddToHistory { text } => {
                let id = sess.conversation_id;
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::message_history::append_entry(&text, &id, &config).await
                    {
                        warn!("failed to append to message history: {e}");
                    }
                });
            }

            Op::GetHistoryEntryRequest { offset, log_id } => {
                let config = config.clone();
                let sess_clone = sess.clone();
                let sub_id = sub.id.clone();

                tokio::spawn(async move {
                    // Run lookup in blocking thread because it does file IO + locking.
                    let entry_opt = tokio::task::spawn_blocking(move || {
                        crate::message_history::lookup(log_id, offset, &config)
                    })
                    .await
                    .unwrap_or(None);

                    let event = Event {
                        id: sub_id,
                        msg: EventMsg::GetHistoryEntryResponse(
                            GetHistoryEntryResponseEvent {
                                offset,
                                log_id,
                                entry: entry_opt.map(|e| {
                                    codex_protocol::message_history::HistoryEntry {
                                        conversation_id: e.session_id,
                                        ts: e.ts,
                                        text: e.text,
                                    }
                                }),
                            },
                        ),
                    };

                    sess_clone.send_event(event).await;
                });
            }
            Op::ListMcpTools => {
                let sub_id = sub.id.clone();

                // This is a cheap lookup from the connection manager's cache.
                let tools = sess.mcp_connection_manager.list_all_tools();
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::McpListToolsResponse(
                        McpListToolsResponseEvent { tools },
                    ),
                };
                sess.send_event(event).await;
            }
            Op::ListCustomPrompts => {
                let sub_id = sub.id.clone();

                let custom_prompts: Vec<CustomPrompt> =
                    if let Some(dir) = crate::custom_prompts::default_prompts_dir() {
                        crate::custom_prompts::discover_prompts_in(&dir).await
                    } else {
                        Vec::new()
                    };

                let event = Event {
                    id: sub_id,
                    msg: EventMsg::ListCustomPromptsResponse(ListCustomPromptsResponseEvent {
                        custom_prompts,
                    }),
                };
                sess.send_event(event).await;
            }
            Op::Compact => {
                // Note: Compact functionality would be handled here
                // but requires access to compact module
                warn!("Compact operation not yet implemented in refactored submission loop");
            }
            Op::Shutdown => {
                info!("Shutting down Codex instance");

                // Gracefully flush and shutdown rollout recorder on session end so tests
                // that inspect the rollout file do not race with the background writer.
                let recorder_opt = sess.rollout.lock_unchecked().take();
                if let Some(rec) = recorder_opt
                    && let Err(e) = rec.shutdown().await
                {
                    warn!("failed to shutdown rollout recorder: {e}");
                    let event = Event {
                        id: sub.id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Failed to shutdown rollout recorder".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }

                let event = Event {
                    id: sub.id.clone(),
                    msg: EventMsg::ShutdownComplete,
                };
                sess.send_event(event).await;
                break;
            }
            Op::GetPath => {
                let sub_id = sub.id.clone();
                // Flush rollout writes before returning the path so readers observe a consistent file.
                let (path, rec_opt) = {
                    let guard = sess.rollout.lock_unchecked();
                    match guard.as_ref() {
                        Some(rec) => (rec.get_rollout_path(), Some(rec.clone())),
                        None => {
                            error!("rollout recorder not found");
                            continue;
                        }
                    }
                };
                if let Some(rec) = rec_opt
                    && let Err(e) = rec.flush().await
                {
                    warn!("failed to flush rollout recorder before GetHistory: {e}");
                }
                let event = Event {
                    id: sub_id.clone(),
                    msg: EventMsg::ConversationPath(ConversationPathResponseEvent {
                        conversation_id: sess.conversation_id,
                        path,
                    }),
                };
                sess.send_event(event).await;
            }
            // Subagent operations
            Op::CreateSubagentTask {
                agent_type,
                title,
                description,
                context_refs,
                bootstrap_paths,
            } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // 构建注入会话历史：包含最近的用户和助手消息
                    let max_injected: usize = 50; // TODO: 配置化
                    let history = sess.state.lock_unchecked().history.contents();
                    let mut injected: Vec<codex_protocol::models::ResponseItem> = history
                        .iter()
                        .filter_map(|item| match item {
                            codex_protocol::models::ResponseItem::Message { role, content, .. } => {
                                if role == "user" || role == "assistant" {
                                    // 为助手消息增加 [MainAgent] 前缀
                                    let mut new_content: Vec<codex_protocol::models::ContentItem> = Vec::new();
                                    for c in content {
                                        match c {
                                            codex_protocol::models::ContentItem::OutputText { text } => {
                                                let t = if role == "assistant" {
                                                    format!("[MainAgent] {}", text)
                                                } else {
                                                    text.clone()
                                                };
                                                new_content.push(codex_protocol::models::ContentItem::OutputText { text: t });
                                            }
                                            codex_protocol::models::ContentItem::InputText { text } => {
                                                let t = if role == "assistant" {
                                                    format!("[MainAgent] {}", text)
                                                } else {
                                                    text.clone()
                                                };
                                                new_content.push(codex_protocol::models::ContentItem::InputText { text: t });
                                            }
                                            other => new_content.push(other.clone()),
                                        }
                                    }
                                    Some(codex_protocol::models::ResponseItem::Message {
                                        id: None,
                                        role: role.clone(),
                                        content: new_content,
                                    })
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        })
                        .collect();
                    // 保留最近 max_injected 条
                    if injected.len() > max_injected {
                        injected = injected.split_off(injected.len().saturating_sub(max_injected));
                    }

                    let spec = SubagentTaskSpec {
                        agent_type: agent_type.clone(),
                        title: title.clone(),
                        description,
                        context_refs,
                        bootstrap_paths,
                        max_turns: None,
                        timeout_ms: None,
                        network_access: None, // TODO: 根据上下文传入网络访问策略
                        injected_conversation: if injected.is_empty() { None } else { Some(injected) },
                    };

                    match components.subagent_manager.create_task(spec).await {
                        Ok(task_id) => {
                            // Auto-launch the task
                            if let Err(e) =
                                components.subagent_manager.launch_subagent(&task_id).await
                            {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::Error(ErrorEvent {
                                        message: format!("Failed to launch subagent: {}", e),
                                    }),
                                };
                                sess.send_event(event).await;
                            } else {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::SubagentStarted(SubagentStartedEvent {
                                        agent_type,
                                        task_id,
                                        title: title.clone(),
                                    }),
                                };
                                sess.send_event(event).await;
                            }
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to create subagent task: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                }
            }
            Op::QueryContextStore { query } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Check if this is a single context query (like /ci get <id>) before moving query
                    let is_single_context_query = query
                        .ids
                        .as_ref()
                        .map(|ids| ids.len() == 1)
                        .unwrap_or(false);

                    // Convert protocol ContextQuery to context_store ContextQuery
                    let context_query = ContextQuery {
                        ids: query.ids,
                        tags: query.tags,
                        created_by: query.created_by,
                        limit: query.limit,
                    };
                    match components.context_repo.query_contexts(&context_query).await {
                        Ok(contexts) => {
                            let total_count = contexts.len();

                            // Check if this is a single context query (like /ci get <id>)
                            // If so, we'll send the full content via a background event
                            if contexts.len() == 1 && is_single_context_query {
                                let ctx = &contexts[0];
                                let full_content_message = format!(
                                    "📋 Context Item: **{}**\n\n**Summary:** {}\n**Size:** {} bytes\n**Created by:** {}\n**Created at:** {}\n\n**Full Content:**\n{}",
                                    ctx.id,
                                    ctx.summary,
                                    ctx.size_bytes(),
                                    ctx.created_by,
                                    ctx.created_at
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    ctx.content
                                );

                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                                        message: full_content_message,
                                    }),
                                };
                                sess.send_event(event).await;
                            } else {
                                // For multiple contexts or list queries, return summaries as before
                                let summaries: Vec<ContextSummary> = contexts
                                    .into_iter()
                                    .map(|ctx| {
                                        let size_bytes = ctx.size_bytes();
                                        // Compute namespace (root project directory name)
                                        let ns_base = crate::git_info::resolve_root_git_project_for_trust(&config.cwd)
                                            .unwrap_or_else(|| config.cwd.clone());
                                        let namespace = ns_base
                                            .file_name()
                                            .map(|s| s.to_string_lossy().to_string())
                                            .unwrap_or_else(|| ns_base.to_string_lossy().to_string());

                                        ContextSummary {
                                            id: ctx.id,
                                            summary: ctx.summary,
                                            created_by: ctx.created_by,
                                            created_at: ctx
                                                .created_at
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs()
                                                .to_string(),
                                            size_bytes,
                                            namespace,
                                        }
                                    })
                                    .collect();

                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::ContextQueryResult(ContextQueryResultEvent {
                                        query_id: uuid::Uuid::new_v4().to_string(),
                                        contexts: summaries,
                                        total_count,
                                    }),
                                };
                                sess.send_event(event).await;
                            }
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Context query failed: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::GetContexts { ids } => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.context_repo.get_contexts(&ids).await {
                        Ok(contexts) => {
                            let total_count = contexts.len();
                            // Convert full Context objects to ContextItem for the response
                            // Compute namespace (root project directory name)
                             let ns_base = crate::git_info::resolve_root_git_project_for_trust(&config.cwd)
                                 .unwrap_or_else(|| config.cwd.clone());
                             let namespace = ns_base
                                 .file_name()
                                 .map(|s| s.to_string_lossy().to_string())
                                 .unwrap_or_else(|| ns_base.to_string_lossy().to_string());

                             let context_items: Vec<ContextItem> = contexts
                                  .into_iter()
                                  .map(|ctx| ContextItem {
                             id: ctx.id.clone(),
                             summary: ctx.summary.clone(),
                             content: ctx.content.clone(),
                             created_by: ctx.created_by.clone(),
                             created_at: ctx
                                 .created_at
                                 .duration_since(std::time::UNIX_EPOCH)
                                 .unwrap_or_default()
                                 .as_secs()
                                 .to_string(),
                             size_bytes: ctx.size_bytes(),
                             namespace: namespace.clone(),
                         })
                                  .collect();

                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::GetContextsResult(GetContextsResultEvent {
                                    contexts: context_items,
                                    total_count,
                                }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Get contexts failed: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::SaveContextsToFile { file_path, ids } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Get contexts to save
                    let contexts_to_save = if let Some(ids) = ids {
                        // Save specific contexts
                        match components.context_repo.get_contexts(&ids).await {
                            Ok(contexts) => contexts,
                            Err(e) => {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::SaveContextsToFileResult(
                                        SaveContextsToFileResultEvent {
                                            file_path: file_path.clone(),
                                            success: false,
                                            message: format!("Failed to get contexts: {}", e),
                                            contexts_saved: 0,
                                        },
                                    ),
                                };
                                sess.send_event(event).await;
                                continue;
                            }
                        }
                    } else {
                        // Save all contexts
                        match components
                            .context_repo
                            .query_contexts(&ContextQuery {
                                ids: None,
                                tags: None,
                                created_by: None,
                                limit: None,
                            })
                            .await
                        {
                            Ok(contexts) => contexts,
                            Err(e) => {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::SaveContextsToFileResult(
                                        SaveContextsToFileResultEvent {
                                            file_path: file_path.clone(),
                                            success: false,
                                            message: format!("Failed to query contexts: {}", e),
                                            contexts_saved: 0,
                                        },
                                    ),
                                };
                                sess.send_event(event).await;
                                continue;
                            }
                        }
                    };

                    // Convert to ContextItems with metadata for serialization
                    // Compute namespace (root project directory name)
                    let ns_base = crate::git_info::resolve_root_git_project_for_trust(&config.cwd)
                        .unwrap_or_else(|| config.cwd.clone());
                    let namespace = ns_base
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| ns_base.to_string_lossy().to_string());

                    let context_items: Vec<ContextItem> = contexts_to_save
                        .iter()
                        .map(|ctx| ContextItem {
                            id: ctx.id.clone(),
                            summary: ctx.summary.clone(),
                            content: ctx.content.clone(),
                            created_by: ctx.created_by.clone(),
                            created_at: ctx
                                .created_at
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                .to_string(),
                            size_bytes: ctx.size_bytes(),
                            namespace: namespace.clone(),
                        })
                        .collect();

                    // Save to file
                    let result = tokio::task::spawn_blocking({
                        let file_path = file_path.clone();
                        let context_items = context_items.clone();
                        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                            let json_content = serde_json::to_string_pretty(&context_items)?;
                            std::fs::write(&file_path, json_content)?;
                            Ok(())
                        }
                    })
                    .await;

                    let event = match result {
                        Ok(Ok(())) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(
                                SaveContextsToFileResultEvent {
                                    file_path: file_path.clone(),
                                    success: true,
                                    message: format!(
                                        "Successfully saved {} context items to '{}'",
                                        context_items.len(),
                                        file_path
                                    ),
                                    contexts_saved: context_items.len(),
                                },
                            ),
                        },
                        Ok(Err(e)) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(
                                SaveContextsToFileResultEvent {
                                    file_path: file_path.clone(),
                                    success: false,
                                    message: format!("Failed to save file: {}", e),
                                    contexts_saved: 0,
                                },
                            ),
                        },
                        Err(e) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(
                                SaveContextsToFileResultEvent {
                                    file_path: file_path.clone(),
                                    success: false,
                                    message: format!("Task execution failed: {}", e),
                                    contexts_saved: 0,
                                },
                            ),
                        },
                    };
                    sess.send_event(event).await;
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                            file_path: file_path.clone(),
                            success: false,
                            message: "Multi-agent functionality not enabled".to_string(),
                            contexts_saved: 0,
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::LoadContextsFromFile { file_path } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Load from file
                    let result = tokio::task::spawn_blocking({
                        let file_path = file_path.clone();
                        move || -> Result<Vec<ContextItem>, Box<dyn std::error::Error + Send + Sync>> {
                            let file_content = std::fs::read_to_string(&file_path)?;
                            let context_items: Vec<ContextItem> = serde_json::from_str(&file_content)?;
                            Ok(context_items)
                        }
                    }).await;

                    let event = match result {
                        Ok(Ok(context_items)) => {
                            // Store each context item
                            let mut stored_count = 0;
                            let mut errors = Vec::new();

                            for item in &context_items {
                                let context = crate::context_store::Context::new(
                                    item.id.clone(),
                                    item.summary.clone(),
                                    item.content.clone(),
                                    item.created_by.clone(),
                                    None,
                                );

                                match components.context_repo.store_context(context).await {
                                    Ok(()) => stored_count += 1,
                                    Err(e) => errors.push(format!(
                                        "Failed to store context '{}': {}",
                                        item.id, e
                                    )),
                                }
                            }

                            let success = errors.is_empty();
                            let message = if success {
                                format!(
                                    "Successfully loaded {} context items from '{}'",
                                    stored_count, file_path
                                )
                            } else {
                                format!(
                                    "Loaded {} out of {} context items. Errors: {}",
                                    stored_count,
                                    context_items.len(),
                                    errors.join("; ")
                                )
                            };

                            Event {
                                id: sub.id,
                                msg: EventMsg::LoadContextsFromFileResult(
                                    LoadContextsFromFileResultEvent {
                                        file_path: file_path.clone(),
                                        success,
                                        message,
                                        contexts_loaded: stored_count,
                                    },
                                ),
                            }
                        }
                        Ok(Err(e)) => Event {
                            id: sub.id,
                            msg: EventMsg::LoadContextsFromFileResult(
                                LoadContextsFromFileResultEvent {
                                    file_path: file_path.clone(),
                                    success: false,
                                    message: format!("Failed to load file: {}", e),
                                    contexts_loaded: 0,
                                },
                            ),
                        },
                        Err(e) => Event {
                            id: sub.id,
                            msg: EventMsg::LoadContextsFromFileResult(
                                LoadContextsFromFileResultEvent {
                                    file_path: file_path.clone(),
                                    success: false,
                                    message: format!("Task execution failed: {}", e),
                                    contexts_loaded: 0,
                                },
                            ),
                        },
                    };
                    sess.send_event(event).await;
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::LoadContextsFromFileResult(
                            LoadContextsFromFileResultEvent {
                                file_path: file_path.clone(),
                                success: false,
                                message: "Multi-agent functionality not enabled".to_string(),
                                contexts_loaded: 0,
                            },
                        ),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::GetSubagentStatus { task_id } => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.subagent_manager.get_task_status(&task_id).await {
                        Ok(status) => {
                            // Convert status to a simple message for now
                            let status_msg = format!("Task {} status: {:?}", task_id, status);
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::AgentMessage(AgentMessageEvent {
                                    message: status_msg,
                                }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to get task status: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::ListActiveSubagents => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.subagent_manager.get_active_tasks().await {
                        Ok(tasks) => {
                            let task_list = tasks
                                .iter()
                                .map(|t| {
                                    format!("- {} ({:?}): {}", t.task_id, t.agent_type, t.title)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            let message = if task_list.is_empty() {
                                "No active subagent tasks".to_string()
                            } else {
                                format!("Active subagent tasks:\n{}", task_list)
                            };

                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::AgentMessage(AgentMessageEvent { message }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to list active tasks: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            _ => {
                // Ignore unknown ops; enum is non_exhaustive to allow extensions.
            }
        }
    }
    debug!("Agent loop exited");
}