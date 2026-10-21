# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p27["branch"]
    p44["SkillsBranch"]
    p43["PromptSourcesBranch"]
    p42["WaitBranch"]
    p41["BackgroundExec"]
    p40["FileReads"]
    p39["ToolExecutor"]
    p38["ToolBinding"]
    p37["Approval"]
    p36["Permission"]
    p35["ModelProvider"]
    p34["Loop"]
    p33["InputQueue"]
    p32["Prompt"]
    p31["ContextPreparation"]
    p30["TreeTimeline"]
    p29["SessionBroadcast"]
    p28["BranchScope"]
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
    p25["SkillContentInstances"]
    p26["SkillContent"]
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
  p23 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p44
  p28 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p44
  p32 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p44
  p20 -->|"dim_agent_capabilities::host::PromptHost@session"| p43
  p28 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p43
  p32 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p43
  p32 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p43
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p42
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p42
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p41
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p41
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p41
  p37 -->|"dim_agent_capabilities::approval::Approval@main"| p39
  p38 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p39
  p28 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p39
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p39
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p39
  p28 -->|"dim_agent_runtime::diagnostics::Diagnostics@main"| p39
  p32 -->|"dim_agent_capabilities::prompt::Prompt@main"| p38
  p16 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p38
  p28 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p38
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p38
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p38
  p28 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p38
  p36 -->|"dim_agent_capabilities::permission::Permission@main"| p37
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p37
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p37
  p10 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p36
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p36
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p36
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p36
  p31 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p35
  p10 -->|"dim_agent_capabilities::host::ModelHost@session"| p35
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p35
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p35
  p28 -->|"dim_agent_runtime::diagnostics::Diagnostics@main"| p35
  p10 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p34
  p10 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p34
  p10 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p34
  p12 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p34
  p38 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p34
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p34
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p34
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p33
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p33
  p31 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p32
  p20 -->|"dim_agent_capabilities::host::PromptHost@session"| p32
  p20 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p32
  p28 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p32
  p28 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p32
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p32
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p32
  p28 -->|"dim_agent_runtime::diagnostics::Diagnostics@main"| p32
  p10 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p32
  p28 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p32
  p10 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p31
  p10 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p31
  p10 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p31
  p10 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p31
  p28 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p31
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p31
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p31
  p28 -->|"dim_agent_runtime::diagnostics::Diagnostics@main"| p31
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p30
  p28 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p30
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p30
  p28 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p30
  p28 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p29
  p28 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p29
  p3 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p4
  p2 -->|"dim_agent_capabilities::file_storage::FileStorage"| p3
  p22 -->|"dim_agent_capabilities::skills::content::Contributions@session"| p26
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
  p10 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p19
  p10 -->|"dim_agent_runtime::diagnostics::Diagnostics@session"| p19
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
  p10 -->|"dim_agent_runtime::diagnostics::Diagnostics@session"| p16
  p14 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p15
  p10 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p14
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p14
  p10 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p14
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p13
  p16 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p12
  p10 -->|"dim_agent_runtime::config::SessionConfig@session"| p12
  p10 -->|"dim_agent_runtime::config::SessionIdentity@session"| p11
```
