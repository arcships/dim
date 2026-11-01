# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p56["branch"]
    p59["BranchOptionalParts"]
    p58["BranchParts"]
    p77["WaitBranch"]
    p76["TreeTimeline"]
    p75["ToolExecutor"]
    p74["ToolBinding"]
    p73["SkillsBranch"]
    p72["SessionBroadcast"]
    p71["PromptSourcesBranch"]
    p70["Prompt"]
    p69["Permission"]
    p68["ModelProvider"]
    p67["Loop"]
    p66["InputQueue"]
    p65["HooksBranch"]
    p64["FileReads"]
    p63["ContextPreparation"]
    p62["BranchLine"]
    p61["BackgroundExec"]
    p60["Approval"]
    p57["BranchScope"]
  end
  subgraph process["process"]
    p2["rutis-loader"]
    p27["SkillsCatalog"]
    p5["HostPlatform"]
    p4["FileStorage"]
  end
  subgraph session["session"]
    p31["session"]
    p54["SkillContentInstances"]
    p55["SkillContent"]
    p35["SessionOptionalParts"]
    p34["SessionParts"]
    p53["WorkspaceTools"]
    p52["Wait"]
    p51["ToolRegistry"]
    p50["TimelineTree"]
    p49["Skills"]
    p48["ShellInput"]
    p47["SessionMethods"]
    p46["SessionBroadcastEnd"]
    p45["Recall"]
    p44["Questions"]
    p43["PromptSources"]
    p42["PromptRegistry"]
    p41["PromptGuide"]
    p40["PrepareModelHost"]
    p39["InputMethods"]
    p38["HooksSession"]
    p37["BranchTool"]
    p36["Attachments"]
    p33["SessionExtension"]
    p32["SessionScope"]
  end
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p77
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p76
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p76
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p76
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p76
  p60 -->|"dim_agent_capabilities::approval::Approval@main"| p75
  p74 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p75
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p75
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p75
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p75
  p70 -->|"dim_agent_capabilities::prompt::Prompt@main"| p74
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p74
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p74
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p74
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p74
  p57 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p74
  p49 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p73
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p73
  p70 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p73
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p72
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p72
  p42 -->|"dim_agent_capabilities::host::PromptHost@session"| p71
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p71
  p70 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p71
  p70 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p71
  p63 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p70
  p42 -->|"dim_agent_capabilities::host::PromptHost@session"| p70
  p42 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p70
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p70
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p70
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p70
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p70
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p70
  p32 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p70
  p57 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p70
  p32 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p69
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p69
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p69
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p69
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p69
  p63 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p68
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p68
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p68
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p68
  p32 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p67
  p32 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p67
  p32 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p67
  p40 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p67
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p67
  p74 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p67
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p67
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p67
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p67
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p66
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p66
  p60 -->|"dim_agent_capabilities::approval::Approval@main"| p65
  p38 -->|"dim_agent_capabilities::hooks::runner::HooksRuntime@session"| p65
  p69 -->|"dim_agent_capabilities::permission::Permission@main"| p65
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p65
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p65
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p65
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p65
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p64
  p32 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p63
  p32 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p63
  p32 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p63
  p32 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p63
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p63
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p63
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p63
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p63
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p63
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p62
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p62
  p57 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p62
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p62
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p62
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p61
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p61
  p57 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p61
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p61
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p61
  p69 -->|"dim_agent_capabilities::permission::Permission@main"| p60
  p57 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p60
  p57 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p60
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p27
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p5
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p53
  p47 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p53
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p53
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p53
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p53
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p52
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p52
  p53 -->|"dim_agent_capabilities::host::ToolHost@session"| p51
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p51
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p51
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p51
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p50
  p32 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p50
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p50
  p32 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p50
  p42 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p49
  p32 -->|"dim_agent_capabilities::skills::SkillsConfig@session"| p49
  p27 -->|"dim_agent_capabilities::skills::catalog::SkillsCatalog"| p49
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p49
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p49
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p49
  p32 -->|"dim_agent_runtime::scope::SessionLoader@session"| p49
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p48
  p47 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p48
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p48
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p46
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p45
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p45
  p47 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p44
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p44
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p44
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p43
  p42 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p43
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p43
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p42
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p40
  p47 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p39
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p39
  p32 -->|"dim_agent_capabilities::hooks::sources::HookSourcesHost@session"| p38
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p38
  p39 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p38
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p38
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p38
  p50 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p37
  p51 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p37
  p47 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p36
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p36
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p33
```
