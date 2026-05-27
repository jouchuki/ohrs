//! Tool trait and implementations for OpenHarness.
//!
//! Provides 43+ tools: bash, file I/O, search, web, MCP, tasks, etc.

pub mod pathsafe;
pub mod registry;
pub mod traits;

// Tool implementations
pub mod agent;
pub mod ask_user_question;
pub mod bash;
pub mod brief;
pub mod config_tool;
pub mod cron_create;
pub mod cron_delete;
pub mod cron_list;
pub mod enter_plan_mode;
pub mod enter_worktree;
pub mod exit_plan_mode;
pub mod exit_worktree;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod glob_tool;
pub mod grep;
pub mod hook_manage;
pub mod list_mcp_resources;
pub mod lsp;
pub mod mcp_auth;
pub mod mcp_tool;
pub mod notebook_edit;
pub mod read_mcp_resource;
pub mod remote_trigger;
pub mod send_message;
pub mod skill;
pub mod sleep;
pub mod task_create;
pub mod task_get;
pub mod task_list;
pub mod task_output;
pub mod task_stop;
pub mod task_update;
pub mod team_create;
pub mod team_delete;
pub mod teammate_message;
pub mod todo_write;
pub mod tool_search;
pub mod web_fetch;
pub mod web_search;

pub use registry::ToolRegistry;
pub use traits::Tool;

/// Serializes tests that mutate process-global environment variables.
///
/// Unit tests across this crate compile into a single test binary and run in
/// parallel by default. Several tests (cron create/list/delete, teammate
/// messages, etc.) mutate the shared `OPENHARNESSRS_DATA_DIR` env var, so they
/// must acquire this lock before touching the environment to avoid clobbering
/// each other.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Create the default tool registry with all built-in tools.
pub fn create_default_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(Box::new(bash::BashTool));
    registry.register(Box::new(file_read::FileReadTool));
    registry.register(Box::new(file_write::FileWriteTool));
    registry.register(Box::new(file_edit::FileEditTool));
    registry.register(Box::new(glob_tool::GlobTool));
    registry.register(Box::new(grep::GrepTool));
    registry.register(Box::new(web_fetch::WebFetchTool));
    registry.register(Box::new(web_search::WebSearchTool));
    registry.register(Box::new(agent::AgentTool));
    registry.register(Box::new(ask_user_question::AskUserQuestionTool));
    registry.register(Box::new(send_message::SendMessageTool));
    registry.register(Box::new(teammate_message::TeammateMessageTool));
    registry.register(Box::new(skill::SkillTool::new()));
    registry.register(Box::new(tool_search::ToolSearchTool));
    registry.register(Box::new(sleep::SleepTool));
    registry.register(Box::new(notebook_edit::NotebookEditTool));
    registry.register(Box::new(todo_write::TodoWriteTool));
    registry.register(Box::new(config_tool::ConfigTool));
    registry.register(Box::new(brief::BriefTool));
    registry.register(Box::new(enter_plan_mode::EnterPlanModeTool));
    registry.register(Box::new(exit_plan_mode::ExitPlanModeTool));
    registry.register(Box::new(enter_worktree::EnterWorktreeTool));
    registry.register(Box::new(exit_worktree::ExitWorktreeTool));
    registry.register(Box::new(cron_create::CronCreateTool));
    registry.register(Box::new(cron_list::CronListTool));
    registry.register(Box::new(cron_delete::CronDeleteTool));
    registry.register(Box::new(remote_trigger::RemoteTriggerTool));
    registry.register(Box::new(task_create::TaskCreateTool));
    registry.register(Box::new(task_get::TaskGetTool));
    registry.register(Box::new(task_list::TaskListTool));
    registry.register(Box::new(task_update::TaskUpdateTool));
    registry.register(Box::new(task_stop::TaskStopTool));
    registry.register(Box::new(task_output::TaskOutputTool));
    registry.register(Box::new(team_create::TeamCreateTool));
    registry.register(Box::new(team_delete::TeamDeleteTool));
    registry.register(Box::new(mcp_tool::McpTool));
    registry.register(Box::new(mcp_auth::McpAuthTool));
    registry.register(Box::new(list_mcp_resources::ListMcpResourcesTool));
    registry.register(Box::new(read_mcp_resource::ReadMcpResourceTool));
    registry.register(Box::new(lsp::LspTool));
    registry.register(Box::new(hook_manage::HookManageTool));

    registry
}
