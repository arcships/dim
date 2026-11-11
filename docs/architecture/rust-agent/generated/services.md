# 默认运行组合

由真实 rutis 装配生成。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p34["branch"]
    p35["BranchScope"]
    p37["BranchOptionalParts"]
    p36["BranchParts"]
    p50["TreeTimeline"]
    p49["ToolExecutor"]
    p48["ToolBinding"]
    p47["SessionBroadcast"]
    p46["Prompt"]
    p45["Permission"]
    p44["ModelProvider"]
    p43["Loop"]
    p42["InputQueue"]
    p41["ContextUsage"]
    p40["ContextPreparation"]
    p39["BranchLine"]
    p38["Approval"]
  end
  subgraph session["session"]
    p20["session"]
    p21["SessionScope"]
    p23["SessionOptionalParts"]
    p22["SessionParts"]
    p33["ToolRegistry"]
    p32["ToolMethods"]
    p31["TimelineTree"]
    p30["SessionMethods"]
    p29["SessionBroadcastEnd"]
    p28["PromptGuide"]
    p27["PrepareModelHost"]
    p26["LoopMethods"]
    p25["InputMethods"]
    p24["BranchTool"]
  end
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p50
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p50
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p50
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p50
  p38 -->|"dim_agent_capabilities::approval::Approval@main"| p49
  p48 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p49
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p49
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p49
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p49
  p46 -->|"dim_agent_capabilities::prompt::Prompt@main"| p48
  p33 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p48
  p35 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p48
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p48
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p48
  p35 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p48
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p47
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p47
  p40 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p46
  p21 -->|"dim_agent_capabilities::host::PromptHost@session"| p46
  p21 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p46
  p28 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p46
  p35 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p46
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p46
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p46
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p46
  p21 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p46
  p35 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p46
  p21 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p45
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p45
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p45
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p45
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p45
  p40 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p44
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p44
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p44
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p44
  p21 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p43
  p21 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p43
  p21 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p43
  p27 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p43
  p28 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p43
  p48 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p43
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p43
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p43
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p43
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p42
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p42
  p21 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p41
  p33 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p41
  p35 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p41
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p41
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p41
  p41 -->|"dim_agent_capabilities::context_usage::ContextUsage@main"| p40
  p21 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p40
  p21 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p40
  p21 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p40
  p21 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p40
  p28 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p40
  p35 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p40
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p40
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p40
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p40
  p28 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p39
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p39
  p35 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p39
  p21 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p39
  p35 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p39
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p39
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p39
  p45 -->|"dim_agent_capabilities::permission::Permission@main"| p38
  p35 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p38
  p35 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p38
  p21 -->|"dim_agent_capabilities::host::ToolHost@session"| p33
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p33
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p33
  p21 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p33
  p30 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p32
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p32
  p28 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p31
  p21 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p31
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p31
  p21 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p31
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p29
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p27
  p30 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p26
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p26
  p30 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p25
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p25
  p31 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p24
  p33 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p24
```
