use crate::{
    adapters::*,
    agent_loop::LoopPlugin,
    approval::ApprovalPlugin,
    context_preparation::ContextPreparationPlugin,
    history::HistoryRenderer,
    host::*,
    input_queue::InputQueuePlugin,
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules, PermissionPlugin},
    prompt::{InitialPromptConfiguration, PromptPlugin},
    session_broadcast::{SessionBroadcastEndPlugin, SessionBroadcastPlugin},
    timeline_tree::{TimelineTreePlugin, TreeTimelinePlugin},
    tool_binding::ToolBindingPlugin,
    tool_executor::ToolExecutorPlugin,
    tool_registry::ToolRegistryPlugin,
};
use dim_agent_runtime::{AgentBuilder, Feature, Names};
use rutis::Error;
use std::sync::Arc;

/// The same timeline factories apply to main and every future branch.
pub fn install(builder: &mut AgentBuilder) {
    install_with_prompt(builder, Arc::new(EmptyPromptHost), None);
}

pub fn install_with_prompt(
    builder: &mut AgentBuilder,
    host: Arc<dyn LoadPrompt>,
    initial: Option<dim_agent_runtime::ConfigureUpdate>,
) {
    install_profile(builder, host, initial, ModelProviderPlugin::new);
}

/// Install the same execution loop with process-owned network model services.
pub fn install_with_process_model(builder: &mut AgentBuilder) {
    install_profile(
        builder,
        Arc::new(EmptyPromptHost),
        None,
        ModelProviderPlugin::process,
    );
}

fn install_profile(
    builder: &mut AgentBuilder,
    host: Arc<dyn LoadPrompt>,
    initial: Option<dim_agent_runtime::ConfigureUpdate>,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
) {
    builder
        .session_value(Arc::new(PromptHost(host)))
        .session_value(Arc::new(InitialPromptConfiguration::fixed(initial)))
        .session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))));
    install_with_prompt_capability(builder, model);
}

/// Installs the same capabilities without binding `PromptHost` or
/// `InitialPromptConfiguration`: a Session capability such as PromptSources
/// provides both. The host binds `CommandRulesHost`.
pub fn install_with_prompt_capability(
    builder: &mut AgentBuilder,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
) {
    install_with_prompt_capability_and_branch(builder, model, true);
}

/// Base variant without the branch tool; used by the minimal execution
/// assembly where the branch tool is not part of the contract.
pub fn install_with_prompt_capability_no_branch(
    builder: &mut AgentBuilder,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
) {
    install_with_prompt_capability_and_branch(builder, model, false);
}

fn install_with_prompt_capability_and_branch(
    builder: &mut AgentBuilder,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
    branch_tool: bool,
) {
    crate::register_names(builder);
    builder
        .require_feature(
            Feature::new("SessionBroadcast")
                .session("SessionBroadcastEnd", |names| {
                    Ok(Box::new(SessionBroadcastEndPlugin::new(names)?))
                })
                .branch("SessionBroadcast", |names| {
                    Ok(Box::new(SessionBroadcastPlugin::new(names)?))
                }),
        )
        .require_feature(
            Feature::new("TimelineTree")
                .session("TimelineTree", |names| {
                    Ok(Box::new(TimelineTreePlugin::new(names)?))
                })
                .branch("TreeTimeline", |names| {
                    Ok(Box::new(TreeTimelinePlugin::new(names)?))
                }),
        )
        .require_feature(
            Feature::new("SessionMethods")
                .session("SessionMethods", crate::methods::SessionMethodsPlugin::part),
        )
        .require_feature(
            Feature::new("InputQueue")
                .session("InputMethods", |names| {
                    Ok(Box::new(crate::input_queue::InputMethodsPlugin::new(
                        names,
                    )?))
                })
                .branch("InputQueue", |names| {
                    Ok(Box::new(InputQueuePlugin::new(names)?))
                }),
        )
        .require_feature(
            Feature::new("ToolRegistry").session("ToolRegistry", |names| {
                Ok(Box::new(ToolRegistryPlugin::new(names)?))
            }),
        )
        .require_feature(
            Feature::new("ContextPreparation").branch("ContextPreparation", |names| {
                Ok(Box::new(ContextPreparationPlugin::new(names)?))
            }),
        )
        .require_feature(
            Feature::new("Prompt")
                .branch("Prompt", |names| Ok(Box::new(PromptPlugin::new(names)?))),
        )
        .require_feature(
            Feature::new("Loop").branch("Loop", |names| Ok(Box::new(LoopPlugin::new(names)?))),
        )
        .require_feature(
            Feature::new("ModelProvider")
                .branch("ModelProvider", move |names| Ok(Box::new(model(names)?))),
        )
        .require_feature(
            Feature::new("Permission")
                .branch("Permission", |names| {
                    Ok(Box::new(PermissionPlugin::new(names)?))
                })
                .branch("Approval", |names| {
                    Ok(Box::new(ApprovalPlugin::new(names)?))
                }),
        )
        .require_feature(
            Feature::new("ToolExecution")
                .branch("ToolBinding", |names| {
                    Ok(Box::new(ToolBindingPlugin::new(names)?))
                })
                .branch("ToolExecutor", |names| {
                    Ok(Box::new(ToolExecutorPlugin::new(names)?))
                }),
        );
    if branch_tool {
        builder.require_feature(
            Feature::new("BranchTool")
                .session("BranchTool", |names| {
                    Ok(Box::new(crate::timeline_tree::BranchToolPlugin::new(
                        names,
                    )?))
                })
                .branch("BranchLine", |names| {
                    Ok(Box::new(crate::timeline_tree::BranchLinePlugin::new(
                        names,
                    )?))
                }),
        );
    }
}

/// Product profile of the minimal execution assembly: only `execute` as the
/// local tool, with Skills and prompt sources. No workspace tools, no wait,
/// no branch tool. The host binds `CommandRulesHost`. See
/// `docs/design/appserver/minimal-execution.md`.
pub fn install_minimal_execution(
    builder: &mut AgentBuilder,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
) {
    install_with_prompt_capability_no_branch(builder, model);
    builder.require_feature(
        Feature::new("ShellSession").branch("ShellSession", |names| {
            Ok(Box::new(crate::shell_session::ShellSessionPlugin::session(
                names,
            )?))
        }),
    );
    crate::shell_tools::install(builder);
    crate::prompt_sources::install(builder);
    crate::skills::install(builder);
}

/// Product profile of the local execution and context capabilities. The
/// composition root also registers the process plugins `HostPlatformPlugin`
/// and `SkillsCatalogPlugin`, plus the product
/// `SkillSettingsHost`, and binds the Session `CommandRulesHost`. A new
/// Session adopts `SessionConfig.model` with its prompt sources.
pub fn install_local_execution(
    builder: &mut AgentBuilder,
    model: fn(&Names<'_>) -> Result<ModelProviderPlugin, Error>,
    workspace: crate::workspace_tools::WorkspaceToolsConfig,
) {
    install_with_prompt_capability(builder, model);
    crate::workspace_tools::install(builder, workspace);
    crate::wait::install(builder);
    crate::shell_input::install(builder);
    crate::prompt_sources::install(builder);
    crate::skills::install(builder);
}

/// Installs `driver` as the Session `ToolHost`.
pub fn tool_host(builder: &mut AgentBuilder, driver: Arc<dyn ToolDriver>) {
    crate::register_names(builder);
    builder.session_value(Arc::new(ToolHost(driver)));
}

/// Use this with a custom `ToolHost` binding, e.g. [`tool_host`].
pub fn mock_model_hosts(builder: &mut AgentBuilder) {
    mock_model_hosts_with_history(builder, HistoryRenderer::default());
}

/// Configure a pure history renderer without installing a duplicate binding.
pub fn mock_model_hosts_with_history(builder: &mut AgentBuilder, history: HistoryRenderer) {
    mock_model_hosts_with_model(builder, history, Arc::new(EchoModel));
}

pub fn mock_model_hosts_with_model(
    builder: &mut AgentBuilder,
    history: HistoryRenderer,
    model: Arc<dyn ModelDriver>,
) {
    crate::register_names(builder);
    builder
        .session_value(Arc::new(history))
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
}
pub fn mock_hosts(builder: &mut AgentBuilder) {
    mock_hosts_with_model(builder, HistoryRenderer::default(), Arc::new(EchoModel));
}

/// [`mock_hosts`] with `model` answering requests in place of [`EchoModel`].
pub fn mock_hosts_with_model(
    builder: &mut AgentBuilder,
    history: HistoryRenderer,
    model: Arc<dyn ModelDriver>,
) {
    mock_model_hosts_with_model(builder, history, model);
    tool_host(builder, Arc::new(MockTools));
}
