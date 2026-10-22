# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p28["branch"]
    p45["SkillsBranch"]
    p44["PromptSourcesBranch"]
    p43["WaitBranch"]
    p42["BackgroundExec"]
    p41["FileReads"]
    p40["ToolExecutor"]
    p39["ToolBinding"]
    p38["Approval"]
    p37["Permission"]
    p36["ModelProvider"]
    p35["Loop"]
    p34["InputQueue"]
    p33["Prompt"]
    p32["ContextPreparation"]
    p31["TreeTimeline"]
    p30["SessionBroadcast"]
    p29["BranchScope"]
  end
  subgraph process["process"]
    p6["rutis-loader"]
    p5["FileWriteCoordinator"]
    p4["SkillsCatalog"]
    p3["HostPlatform"]
    p2["FileStorage"]
  end
  subgraph session["session"]
    p9["session"]
    p26["SkillContentInstances"]
    p27["SkillContent"]
    p25["Questions"]
    p24["Attachments"]
    p23["Skills"]
    p22["SkillContributions"]
    p21["PromptSources"]
    p20["PromptRegistry"]
    p19["ShellInput"]
    p18["Wait"]
    p17["WorkspaceTools"]
    p16["ToolRegistry"]
    p15["InputMethods"]
    p14["TimelineTree"]
    p13["SessionBroadcastEnd"]
    p12["PrepareModelHost"]
    p11["SessionExtension"]
    p10["SessionScope"]
  end
  p23 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p45
  p29 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p45
  p33 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p45
  p20 -->|"dim_agent_capabilities::host::PromptHost@session"| p44
  p29 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p44
  p33 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p44
  p33 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p44
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p43
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p43
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p42
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p42
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p42
  p38 -->|"dim_agent_capabilities::approval::Approval@main"| p40
  p39 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p40
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p40
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p40
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p40
  p33 -->|"dim_agent_capabilities::prompt::Prompt@main"| p39
  p16 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p39
  p29 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p39
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p39
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p39
  p29 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p39
  p37 -->|"dim_agent_capabilities::permission::Permission@main"| p38
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p38
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p38
  p10 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p37
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p37
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p37
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p37
  p32 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p36
  p10 -->|"dim_agent_capabilities::host::ModelHost@session"| p36
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p36
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p36
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p36
  p10 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p35
  p10 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p35
  p10 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p35
  p12 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p35
  p39 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p35
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p35
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p35
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p34
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p34
  p32 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p33
  p20 -->|"dim_agent_capabilities::host::PromptHost@session"| p33
  p20 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p33
  p29 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p33
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p33
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p33
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p33
  p10 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p33
  p29 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p33
  p10 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p32
  p10 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p32
  p10 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p32
  p10 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p32
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p32
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p32
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p32
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p31
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p31
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p31
  p29 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p31
  p29 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p30
  p29 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p30
  p3 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p4
  p2 -->|"dim_agent_capabilities::file_storage::FileStorage"| p3
  p22 -->|"dim_agent_capabilities::skills::content::Contributions@session"| p27
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p25
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p24
  p4 -->|"dim_agent_capabilities::skills::catalog::SkillsCatalog"| p23
  p22 -->|"dim_agent_capabilities::skills::content::Contributions@session"| p23
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p23
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p23
  p10 -->|"dim_agent_runtime::scope::SessionLoader@session"| p23
  p3 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p21
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p21
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/base@session"| p20
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/environment/context@session"| p20
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/environment/date@session"| p20
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/instructions/project@session"| p20
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/instructions/user@session"| p20
  p21 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/session@session"| p20
  p23 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/skills/instructions@session"| p20
  p23 -->|"dim_agent_capabilities::prompt_registry::PromptSource#dim/skills/list@session"| p20
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p20
  p3 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p19
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p19
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p19
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p19
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p19
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p18
  p3 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p17
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p17
  p5 -->|"dim_agent_capabilities::workspace_tools::files::FileWriteCoordinator"| p17
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p17
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p17
  p17 -->|"dim_agent_capabilities::host::ToolHost@session"| p16
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p16
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p16
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p16
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p15
  p10 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p14
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p14
  p10 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p14
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p13
  p16 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p12
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p12
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p11
```
