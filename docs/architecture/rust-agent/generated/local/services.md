# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p55["branch"]
    p56["BranchScope"]
    p58["BranchOptionalParts"]
    p57["BranchParts"]
    p76["WaitBranch"]
    p75["TreeTimeline"]
    p74["ToolExecutor"]
    p73["ToolBinding"]
    p72["SkillsBranch"]
    p71["SessionBroadcast"]
    p70["PromptSourcesBranch"]
    p69["Prompt"]
    p68["Permission"]
    p67["ModelProvider"]
    p66["Loop"]
    p65["InputQueue"]
    p64["HooksBranch"]
    p63["FileReads"]
    p62["ContextPreparation"]
    p61["BranchLine"]
    p60["BackgroundExec"]
    p59["Approval"]
  end
  subgraph process["process"]
    p2["rutis-loader"]
    p27["SkillsCatalog"]
    p5["HostPlatform"]
    p4["FileStorage"]
  end
  subgraph session["session"]
    p31["session"]
    p53["SkillContentInstances"]
    p54["SkillContent"]
    p32["SessionScope"]
    p34["SessionOptionalParts"]
    p33["SessionParts"]
    p52["WorkspaceTools"]
    p51["Wait"]
    p50["ToolRegistry"]
    p49["TimelineTree"]
    p48["Skills"]
    p47["ShellInput"]
    p46["SessionMethods"]
    p45["SessionBroadcastEnd"]
    p44["Recall"]
    p43["Questions"]
    p42["PromptSources"]
    p41["PromptRegistry"]
    p40["PromptGuide"]
    p39["PrepareModelHost"]
    p38["InputMethods"]
    p37["HooksSession"]
    p36["BranchTool"]
    p35["Attachments"]
  end
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p76
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p75
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p75
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p75
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p75
  p59 -->|"dim_agent_capabilities::approval::Approval@main"| p74
  p73 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p74
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p74
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p74
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p74
  p69 -->|"dim_agent_capabilities::prompt::Prompt@main"| p73
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p73
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p73
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p73
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p73
  p56 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p73
  p48 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p72
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p72
  p69 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p72
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p71
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p71
  p41 -->|"dim_agent_capabilities::host::PromptHost@session"| p70
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p70
  p69 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p70
  p69 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p70
  p62 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p69
  p41 -->|"dim_agent_capabilities::host::PromptHost@session"| p69
  p41 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p69
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p69
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p69
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p69
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p69
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p69
  p32 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p69
  p56 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p69
  p32 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p68
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p68
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p68
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p68
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p68
  p62 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p67
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p67
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p67
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p67
  p32 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p66
  p32 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p66
  p32 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p66
  p39 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p66
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p66
  p73 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p66
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p66
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p66
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p66
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p65
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p65
  p59 -->|"dim_agent_capabilities::approval::Approval@main"| p64
  p37 -->|"dim_agent_capabilities::hooks::runner::HooksRuntime@session"| p64
  p68 -->|"dim_agent_capabilities::permission::Permission@main"| p64
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p64
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p64
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p64
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p64
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p63
  p32 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p62
  p32 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p62
  p32 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p62
  p32 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p62
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p62
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p62
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p62
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p62
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p62
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p61
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p61
  p56 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p61
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p61
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p61
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p60
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p60
  p56 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p60
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p60
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p60
  p68 -->|"dim_agent_capabilities::permission::Permission@main"| p59
  p56 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p59
  p56 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p59
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p27
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p5
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p52
  p46 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p52
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p52
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p52
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p52
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p51
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p51
  p52 -->|"dim_agent_capabilities::host::ToolHost@session"| p50
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p50
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p50
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p50
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p49
  p32 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p49
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p49
  p32 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p49
  p41 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p48
  p32 -->|"dim_agent_capabilities::skills::SkillsConfig@session"| p48
  p27 -->|"dim_agent_capabilities::skills::catalog::SkillsCatalog"| p48
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p48
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p48
  p32 -->|"dim_agent_runtime::scope::SessionLoader@session"| p48
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p47
  p46 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p47
  p40 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p47
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p47
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p47
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p47
  p32 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p47
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p45
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p44
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p44
  p46 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p43
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p43
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p43
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p42
  p41 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p42
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p42
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p41
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p39
  p46 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p38
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p38
  p32 -->|"dim_agent_capabilities::hooks::sources::HookSourcesHost@session"| p37
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p37
  p38 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p37
  p32 -->|"dim_agent_runtime::config::SessionConfig@session"| p37
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p37
  p49 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p36
  p50 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p36
  p46 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p35
  p32 -->|"dim_agent_runtime::config::SessionIdentity@session"| p35
```
