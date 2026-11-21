# 本地执行运行组合

由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p61["branch"]
    p62["BranchScope"]
    p64["BranchOptionalParts"]
    p63["BranchParts"]
    p83["WaitBranch"]
    p82["TreeTimeline"]
    p81["ToolExecutor"]
    p80["ToolBinding"]
    p79["SkillsBranch"]
    p78["SessionBroadcast"]
    p77["PromptSourcesBranch"]
    p76["Prompt"]
    p75["Permission"]
    p74["ModelProvider"]
    p73["Loop"]
    p72["InputQueue"]
    p71["HooksBranch"]
    p70["FileReads"]
    p69["ContextUsage"]
    p68["ContextPreparation"]
    p67["BranchLine"]
    p66["BackgroundExec"]
    p65["Approval"]
  end
  subgraph process["process"]
    p2["rutis-loader"]
    p29["SkillsCatalog"]
    p20["WorkspaceToolsOwner"]
    p5["HostPlatform"]
    p4["FileStorage"]
  end
  subgraph session["session"]
    p33["session"]
    p59["SkillContentInstances"]
    p60["SkillContent"]
    p34["SessionScope"]
    p36["SessionOptionalParts"]
    p35["SessionParts"]
    p58["WorkspaceTools"]
    p57["Wait"]
    p56["ToolRegistry"]
    p55["ToolMethods"]
    p54["TimelineTree"]
    p53["Skills"]
    p52["ShellInput"]
    p51["SessionMethods"]
    p50["SessionBroadcastEnd"]
    p49["Recall"]
    p48["Questions"]
    p47["PromptSuggestion"]
    p46["PromptSources"]
    p45["PromptRegistry"]
    p44["PromptGuide"]
    p43["PrepareModelHost"]
    p42["LoopMethods"]
    p41["InputMethods"]
    p40["HooksSession"]
    p39["HistoryRenderer"]
    p38["BranchTool"]
    p37["Attachments"]
  end
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p83
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p83
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p82
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p82
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p82
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p82
  p65 -->|"dim_agent_capabilities::approval::Approval@main"| p81
  p80 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p81
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p81
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p81
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p81
  p76 -->|"dim_agent_capabilities::prompt::Prompt@main"| p80
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p80
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p80
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p80
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p80
  p62 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p80
  p76 -->|"dim_agent_capabilities::prompt::Prompt@main"| p79
  p53 -->|"dim_agent_capabilities::skills::session::SessionSkills@session"| p79
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p79
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p78
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p78
  p45 -->|"dim_agent_capabilities::host::PromptHost@session"| p77
  p76 -->|"dim_agent_capabilities::prompt::Prompt@main"| p77
  p46 -->|"dim_agent_capabilities::prompt_sources::SessionLanguage@session"| p77
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p77
  p76 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::prompt_state::PromptSnapshot>@main"| p77
  p68 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p76
  p45 -->|"dim_agent_capabilities::host::PromptHost@session"| p76
  p45 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p76
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p76
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p76
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p76
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p76
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p76
  p34 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p76
  p62 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p76
  p34 -->|"dim_agent_capabilities::permission::CommandRules@session"| p75
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p75
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p75
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p75
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p75
  p68 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p74
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p74
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p74
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p74
  p68 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p73
  p39 -->|"dim_agent_capabilities::history::face::HistoryFace@session"| p73
  p34 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p73
  p34 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p73
  p43 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p73
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p73
  p80 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p73
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p73
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p73
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p73
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p72
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p72
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p72
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p72
  p65 -->|"dim_agent_capabilities::approval::Approval@main"| p71
  p40 -->|"dim_agent_capabilities::hooks::runner::HooksRuntime@session"| p71
  p75 -->|"dim_agent_capabilities::permission::Permission@main"| p71
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p71
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p71
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p71
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p71
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p71
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p70
  p34 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p69
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p69
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p69
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p69
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p69
  p69 -->|"dim_agent_capabilities::context_usage::ContextUsage@main"| p68
  p39 -->|"dim_agent_capabilities::history::face::HistoryFace@session"| p68
  p34 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p68
  p34 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p68
  p34 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p68
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p68
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p68
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p68
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p68
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p68
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p67
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p67
  p62 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p67
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p67
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p67
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p67
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p67
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p66
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p66
  p62 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p66
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p66
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p66
  p75 -->|"dim_agent_capabilities::permission::Permission@main"| p65
  p62 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p65
  p62 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p65
  p75 -->|"dim_agent_runtime::state::StateCell<dim_agent_capabilities::permission::PermissionState>@main"| p65
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p29
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p5
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p58
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p58
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p58
  p20 -->|"dim_agent_capabilities::workspace_tools::mentions::MentionIndex"| p58
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p58
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p58
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p57
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p57
  p58 -->|"dim_agent_capabilities::host::ToolHost@session"| p56
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p56
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p56
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p56
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p55
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p55
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p54
  p34 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p54
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p54
  p34 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p54
  p41 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p53
  p45 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p53
  p34 -->|"dim_agent_capabilities::skills::SkillsConfig@session"| p53
  p29 -->|"dim_agent_capabilities::skills::catalog::SkillsCatalog#dim/skills/catalog-owner"| p53
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p53
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p53
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p53
  p34 -->|"dim_agent_runtime::scope::SessionLoader@session"| p53
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p52
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p52
  p44 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p52
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p52
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p52
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p52
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p52
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p50
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p49
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p49
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p48
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p48
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p48
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p48
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p47
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p47
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p47
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p47
  p34 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p47
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p46
  p45 -->|"dim_agent_capabilities::prompt_registry::PromptRegistry@session"| p46
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p46
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p45
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p43
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p42
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p42
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p41
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p41
  p34 -->|"dim_agent_capabilities::hooks::sources::HookSourcesHost@session"| p40
  p5 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p40
  p41 -->|"dim_agent_capabilities::input_queue::SubmitChecks@session"| p40
  p34 -->|"dim_agent_runtime::config::SessionConfig@session"| p40
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p40
  p34 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p39
  p54 -->|"dim_agent_capabilities::timeline_tree::TimelineTree@session"| p39
  p54 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p38
  p56 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p38
  p51 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p37
  p34 -->|"dim_agent_runtime::config::SessionIdentity@session"| p37
```
