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
pub mod images;
pub mod input_queue;
pub mod methods;
pub mod model_provider;
mod output;
pub mod permission;
pub mod prompt;
pub mod prompt_guide;
pub mod prompt_registry;
pub mod prompt_sources;
pub mod prompt_state;
pub mod recall;
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

use dim_agent_runtime::PublicService;
/// Registers the names of this crate's Session and branch services (plugin
/// contract §4). Every installer below applies it; registering twice does
/// nothing.
pub fn register_names(builder: &mut dim_agent_runtime::AgentBuilder) {
    register_names_in(builder.catalog_mut());
}

/// The runtime's catalog with this crate's names, for mounting parts outside
/// an assembly ([`dim_agent_runtime::names::process_plugin`]).
pub fn catalog() -> dim_agent_runtime::names::ServiceCatalog {
    let mut catalog = dim_agent_runtime::names::catalog();
    register_names_in(&mut catalog);
    catalog
}

fn register_names_in(catalog: &mut dim_agent_runtime::names::ServiceCatalog) {
    use dim_agent_runtime::{BRANCH_GROUP, SESSION_GROUP, names::register};
    let session = Some(SESSION_GROUP);
    register::<timeline_tree::TimelineTree>(catalog, session);
    register::<timeline_tree::TreeCommands>(catalog, session);
    register::<tool_registry::ToolRegistry>(catalog, session);
    register::<tool_registry::ToolSourcesView>(catalog, session);
    register::<history::HistoryRenderer>(catalog, session);
    register::<prompt::InitialPromptConfiguration>(catalog, session);
    register::<prompt_guide::GuideEntries>(catalog, session);
    register::<prompt_registry::PromptRegistry>(catalog, session);
    register::<skills::SessionSkills>(catalog, session);
    register::<host::PrepareModelHost>(catalog, session);
    register::<host::PrepareContextHost>(catalog, session);
    register::<host::ContextModelHost>(catalog, session);
    register::<host::CompactionStrategyHost>(catalog, session);
    register::<host::PromptHost>(catalog, session);
    register::<host::ToolHost>(catalog, session);
    register::<permission::CommandRulesHost>(catalog, session);
    register::<methods::SessionMethods>(catalog, session);
    let branch = Some(BRANCH_GROUP);
    register::<input_queue::InputQueue>(catalog, branch);
    register::<agent_loop::AgentLoop>(catalog, branch);
    register::<prompt::Prompt>(catalog, branch);
    register::<context_preparation::ContextPreparation>(catalog, branch);
    register::<context_preparation::ConfigurationPublisher>(catalog, branch);
    register::<model_provider::ModelProvider>(catalog, branch);
    register::<tool_executor::ToolExecutor>(catalog, branch);
    register::<tool_binding::ToolBinding>(catalog, branch);
    register::<permission::Permission>(catalog, branch);
    register::<approval::Approval>(catalog, branch);
    register::<skills::SkillsBranch>(catalog, branch);
    register::<workspace_tools::background::BackgroundExec>(catalog, branch);
    register::<shell_session::ShellSession>(catalog, branch);
    register::<dyn exec_stop::ExecStop>(catalog, branch);
    register::<wait::Wait>(catalog, branch);
    register::<workspace_tools::file_reads::FileReads>(catalog, branch);
    register::<timeline_tree::BranchLine>(catalog, branch);
    register::<host_platform::HostPlatform>(catalog, None);
    register::<file_storage::FileStorage>(catalog, None);
    register::<host::ModelHost>(catalog, None);
    register::<host::ProviderConnectionResolver>(catalog, None);
    register::<host::ModelSelectionHost>(catalog, None);
    register::<host::SessionFilesHost>(catalog, None);
    register::<skill_settings::SkillSettingsHost>(catalog, None);
    register::<skills::SkillsCatalog>(catalog, None);
}
impl PublicService for approval::Approval {}
impl PublicService for input_queue::InputQueue {}
impl PublicService for permission::Permission {}
impl PublicService for workspace_tools::background::BackgroundExec {}
impl PublicService for shell_session::ShellSession {}
impl PublicService for dyn exec_stop::ExecStop {}
impl PublicService for workspace_tools::file_reads::FileReads {}
impl PublicService for timeline_tree::BranchLine {}
impl PublicService for wait::Wait {}
impl PublicService for skills::SkillsBranch {}
impl PublicService for prompt::Prompt {}
impl PublicService for tool_registry::ToolSourcesView {}
impl PublicService for methods::SessionMethods {}
impl PublicService for agent_loop::AgentLoop {}
impl PublicService for context_preparation::ContextPreparation {}
