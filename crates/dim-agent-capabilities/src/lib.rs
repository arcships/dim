pub mod adapters;
pub mod agent_loop;
pub mod approval;
pub(crate) mod branch_input;
pub mod context_block;
pub mod context_methods;
pub mod context_model;
mod context_policy;
pub mod context_preparation;
pub mod defaults;
pub mod exec_stop;
pub mod file_storage;
pub mod history;
pub mod host;
pub mod host_platform;
pub mod input_queue;
pub mod methods;
pub mod model_provider;
mod output;
pub mod permission;
pub mod prompt;
pub mod prompt_registry;
pub mod prompt_sources;
pub mod prompt_state;
pub mod session_broadcast;
pub mod settings;
pub mod shell_input;
pub mod shell_session;
pub mod shell_tools;
pub mod skill_settings;
pub mod skills;
pub mod steer;
pub mod summary_strategy;
pub mod timeline_tree;
pub mod tool_binding;
pub mod tool_executor;
pub mod tool_registry;
pub mod wait;
pub mod workspace_tools;

use dim_agent_runtime::{BranchService, PublicService, SessionService};
impl SessionService for timeline_tree::TimelineTree {}
impl SessionService for timeline_tree::TreeCommands {}
impl SessionService for tool_registry::ToolRegistry {}
impl SessionService for history::HistoryRenderer {}
impl SessionService for prompt::InitialPromptConfiguration {}
impl SessionService for prompt_registry::PromptSource {}
impl SessionService for skills::SessionSkills {}
impl SessionService for host::PrepareModelHost {}
impl SessionService for host::PrepareContextHost {}
impl SessionService for host::ContextModelHost {}
impl SessionService for host::CompactionStrategyHost {}
impl SessionService for host::PromptHost {}
impl SessionService for permission::CommandRulesHost {}
impl SessionService for host::ModelHost {}
impl SessionService for host::ToolHost {}
impl SessionService for methods::SessionMethods {}
impl BranchService for input_queue::InputQueue {}
impl BranchService for agent_loop::AgentLoop {}
impl BranchService for prompt::Prompt {}
impl BranchService for context_preparation::ContextPreparation {}
impl BranchService for context_preparation::ConfigurationPublisher {}
impl BranchService for model_provider::ModelProvider {}
impl BranchService for tool_executor::ToolExecutor {}
impl BranchService for tool_binding::ToolBinding {}
impl BranchService for permission::Permission {}
impl BranchService for approval::Approval {}
impl BranchService for skills::SkillsBranch {}
impl BranchService for workspace_tools::background::BackgroundExec {}
impl BranchService for shell_session::ShellSession {}
impl BranchService for dyn exec_stop::ExecStop {}
impl BranchService for wait::Wait {}

// Business ports safe to obtain from a Session handle: RPC calls them by
// request branch, tool handlers by call branch.
impl PublicService for approval::Approval {}
impl PublicService for input_queue::InputQueue {}
impl PublicService for permission::Permission {}
impl PublicService for workspace_tools::background::BackgroundExec {}
impl PublicService for shell_session::ShellSession {}
impl PublicService for dyn exec_stop::ExecStop {}
impl PublicService for workspace_tools::file_reads::FileReads {}
impl PublicService for wait::Wait {}
impl PublicService for skills::SkillsBranch {}
impl PublicService for prompt::Prompt {}
impl PublicService for tool_registry::ToolSourcesView {}
impl PublicService for methods::SessionMethods {}
impl SessionService for tool_registry::ToolSourcesView {}
impl PublicService for agent_loop::AgentLoop {}
impl PublicService for context_preparation::ContextPreparation {}
