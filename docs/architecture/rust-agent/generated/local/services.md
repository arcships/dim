# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p57["branch"]
    p58["BranchScope"]
    p60["BranchOptionalParts"]
    p59["BranchParts"]
    p78["WaitBranch"]
    p77["TreeTimeline"]
    p76["ToolExecutor"]
    p75["ToolBinding"]
    p74["SkillsBranch"]
    p73["SessionBroadcast"]
    p72["PromptSourcesBranch"]
    p71["Prompt"]
    p70["Permission"]
    p69["ModelProvider"]
    p68["Loop"]
    p67["InputQueue"]
    p66["HooksBranch"]
    p65["FileReads"]
    p64["ContextPreparation"]
    p63["BranchLine"]
    p62["BackgroundExec"]
    p61["Approval"]
  end
  subgraph process["process"]
    p2["rutis-loader"]
    p28["SkillsCatalog"]
    p5["HostPlatform"]
    p4["FileStorage"]
  end
  subgraph session["session"]
    p32["session"]
    p55["SkillContentInstances"]
    p56["SkillContent"]
    p33["SessionScope"]
    p35["SessionOptionalParts"]
    p34["SessionParts"]
    p54["WorkspaceTools"]
    p53["Wait"]
    p52["ToolRegistry"]
    p51["TimelineTree"]
    p50["Skills"]
    p49["ShellInput"]
    p48["SessionMethods"]
    p47["SessionBroadcastEnd"]
    p46["Recall"]
    p45["Questions"]
    p44["PromptSuggestion"]
    p43["PromptSources"]
    p42["PromptRegistry"]
    p41["PromptGuide"]
    p40["PrepareModelHost"]
    p39["InputMethods"]
    p38["HooksSession"]
    p37["BranchTool"]
    p36["Attachments"]
  end
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p78
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p77
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p77
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p77
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p77
  p61 -->|"dim_agent_capabilities::approval::Approval@main"| p76
  p75 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p76
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p76
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p76
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p76
  p71 -->|"dim_agent_capabilities::prompt::Prompt@main"| p75
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p75
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p75
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p75
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p75
  p58 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p75
  p50 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p74
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p74
  p71 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p74
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p73
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p73
  p42 -->|"dim_agent_capabilities::host::PromptHost@session"| p72
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p72
  p71 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p72
  p71 -->|"dyn dim_agent_runtime::contracts::ConfigureControl@main"| p72
  p64 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p71
  p42 -->|"dim_agent_capabilities::host::PromptHost@session"| p71
  p42 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p71
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p71
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p71
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p71
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p71
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p71
  p33 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p71
  p58 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p71
  p33 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p70
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p70
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p70
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p70
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p70
  p64 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p69
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p69
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p69
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p69
  p33 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p68
  p33 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p68
  p33 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p68
  p40 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p68
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p68
  p75 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p68
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p68
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p68
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p68
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p67
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p67
  p61 -->|"dim_agent_capabilities::approval::Approval@main"| p66
  p38 -->|"dim_agent_capabilities::hooks::runner::HooksRuntime@session"| p66
  p70 -->|"dim_agent_capabilities::permission::Permission@main"| p66
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p66
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p66
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p66
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p66
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p66
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p65
  p33 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p64
  p33 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p64
  p33 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p64
  p33 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p64
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p64
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p64
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p64
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p64
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p64
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p63
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p63
  p58 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p63
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p63
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p63
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p62
  p33 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p62
  p58 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p62
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p62
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p62
  p70 -->|"dim_agent_capabilities::permission::Permission@main"| p61
  p58 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p61
  p58 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p61
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p28
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p5
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p54
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p54
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p54
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p54
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p54
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p53
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p53
  p54 -->|"dim_agent_capabilities::host::ToolHost@session"| p52
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p52
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p52
  p33 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p52
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p51
  p33 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p51
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p51
  p33 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p51
  p39 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p50
  p42 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p50
  p33 -->|"dim_agent_capabilities::skills::SkillsConfig@session"| p50
  p28 -->|"dim_agent_capabilities::skills::catalog::SkillsCatalog"| p50
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p50
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p50
  p33 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p50
  p33 -->|"dim_agent_runtime::scope::SessionLoader@session"| p50
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p49
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p49
  p41 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p49
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p49
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p49
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p49
  p33 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p49
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p47
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p46
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p46
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p45
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p45
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p45
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p44
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p44
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p44
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p44
  p33 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p44
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p43
  p42 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p43
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p43
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p42
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p40
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p39
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p39
  p33 -->|"dim_agent_capabilities::hooks::sources::HookSourcesHost@session"| p38
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p38
  p39 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p38
  p33 -->|"dim_agent_runtime::config::SessionConfig@session"| p38
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p38
  p51 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p37
  p52 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p37
  p48 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p36
  p33 -->|"dim_agent_runtime::config::SessionIdentity@session"| p36
```
