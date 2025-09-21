use std::path::{Path, PathBuf};

use crate::{
    client::ModelClient,
    config_types::ShellEnvironmentPolicy,
    protocol::SandboxPolicy,
    tool_config::UnifiedToolConfig,
};

/// The context needed for a single turn of the conversation.
#[derive(Debug)]
pub struct TurnContext {
    pub(crate) client: ModelClient,
    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    pub(crate) cwd: PathBuf,
    pub(crate) base_instructions: Option<String>,
    pub(crate) user_instructions: Option<String>,
    pub(crate) approval_policy: crate::protocol::AskForApproval,
    pub(crate) sandbox_policy: SandboxPolicy,
    pub(crate) shell_environment_policy: ShellEnvironmentPolicy,
    pub(crate) unified_tool_config: UnifiedToolConfig,
}

impl TurnContext {
    pub(crate) fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }
}