# 默认运行组合

由真实 rutis 装配生成。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph main["main"]
    p32["branch"]
    p35["BranchOptionalParts"]
    p34["BranchParts"]
    p47["TreeTimeline"]
    p46["ToolExecutor"]
    p45["ToolBinding"]
    p44["SessionBroadcast"]
    p43["Prompt"]
    p42["Permission"]
    p41["ModelProvider"]
    p40["Loop"]
    p39["InputQueue"]
    p38["ContextPreparation"]
    p37["BranchLine"]
    p36["Approval"]
    p33["BranchScope"]
  end
  subgraph session["session"]
    p20["session"]
    p23["SessionOptionalParts"]
    p22["SessionParts"]
    p31["ToolRegistry"]
    p30["TimelineTree"]
    p29["SessionMethods"]
    p28["SessionBroadcastEnd"]
    p27["PromptGuide"]
    p26["PrepareModelHost"]
    p25["InputMethods"]
    p24["BranchTool"]
    p21["SessionScope"]
  end
  p30 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p47
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p47
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p47
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p47
  p36 -->|"dim_agent_capabilities::approval::Approval@main"| p46
  p45 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p46
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p46
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p46
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p46
  p43 -->|"dim_agent_capabilities::prompt::Prompt@main"| p45
  p31 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p45
  p33 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p45
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p45
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p45
  p33 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p45
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p44
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p44
  p38 -->|"dim_agent_capabilities::context_preparation::ConfigurationPublisher@main"| p43
  p21 -->|"dim_agent_capabilities::host::PromptHost@session"| p43
  p21 -->|"dim_agent_capabilities::prompt::InitialPromptConfiguration@session"| p43
  p27 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p43
  p33 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p43
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p43
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p43
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p43
  p21 -->|"dim_agent_runtime::lifecycle::FaultReporter@session"| p43
  p33 -->|"dim_agent_runtime::timeline_context::BranchOpening@main"| p43
  p21 -->|"dim_agent_capabilities::permission::CommandRulesHost@session"| p42
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p42
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p42
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p42
  p38 -->|"dim_agent_capabilities::context_preparation::ContextPreparation@main"| p41
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p41
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p41
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p41
  p21 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p40
  p21 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p40
  p21 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p40
  p26 -->|"dim_agent_capabilities::host::PrepareModelHost@session"| p40
  p27 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p40
  p45 -->|"dim_agent_capabilities::tool_binding::ToolBinding@main"| p40
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p40
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p40
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p40
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p39
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p39
  p21 -->|"dim_agent_capabilities::history::HistoryRenderer@session"| p38
  p21 -->|"dim_agent_capabilities::host::CompactionStrategyHost@session"| p38
  p21 -->|"dim_agent_capabilities::host::ContextModelHost@session"| p38
  p21 -->|"dim_agent_capabilities::host::PrepareContextHost@session"| p38
  p27 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p38
  p33 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p38
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p38
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p38
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p38
  p27 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p37
  p30 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p37
  p33 -->|"dim_agent_runtime::config::BranchLifecycle@main"| p37
  p33 -->|"dim_agent_runtime::config::TimelineIdentity@main"| p37
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p37
  p42 -->|"dim_agent_capabilities::permission::Permission@main"| p36
  p33 -->|"dim_agent_runtime::core::dispatcher::Dispatcher@main"| p36
  p33 -->|"dim_agent_runtime::core::timeline::Timeline@main"| p36
  p21 -->|"dim_agent_capabilities::host::ToolHost@session"| p31
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p31
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p31
  p21 -->|"dim_agent_runtime::config::SessionLifecycle@session"| p31
  p27 -->|"dim_agent_capabilities::prompt_guide::GuideEntries@session"| p30
  p21 -->|"dim_agent_runtime::assembly::TimelineFactory@session"| p30
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p30
  p21 -->|"dyn dim_agent_runtime::timeline_store::TimelineStore@session"| p30
  p21 -->|"dim_agent_runtime::config::SessionIdentity@session"| p28
  p21 -->|"dim_agent_runtime::config::SessionConfig@session"| p26
  p29 -->|"dim_agent_capabilities::methods::SessionMethods@session"| p25
  p30 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p25
  p30 -->|"dim_agent_capabilities::timeline_tree::TreeCommands@session"| p24
  p31 -->|"dim_agent_capabilities::tool_registry::ToolRegistry@session"| p24
```
