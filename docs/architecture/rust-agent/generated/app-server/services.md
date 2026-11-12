# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p75["Attachments"]
    p74["Report"]
    p73["goal"]
    p72["eventhub"]
    p76["eventhub-ready"]
    p71["Scheduler"]
    p70["Questions"]
    p69["PromptSuggestion"]
    p68["Modality"]
    p67["PrepareModelHost"]
    p66["ContextCompaction"]
    p65["CommandRules"]
    p64["PromptSources"]
    p63["ShellInput"]
    p62["Recall"]
    p61["Wait"]
    p60["WorkspaceToolsOwner"]
    p59["BranchTool"]
    p58["ToolExecution"]
    p57["Permission"]
    p56["ModelProvider"]
    p55["Loop"]
    p54["Prompt"]
    p53["ContextPreparation"]
    p52["ToolRegistry"]
    p51["InputQueue"]
    p50["SessionMethods"]
    p49["TimelineTree"]
    p48["SessionBroadcast"]
    p47["session-title"]
    p46["PartRegistry"]
    p45["Transport"]
    p44["Rpc"]
    p43["Connection"]
    p42["UiHost"]
    p41["Collaboration"]
    p40["Agents"]
    p39["CollaborationDirectory"]
    p38["AgentDirectory"]
    p37["PluginManager"]
    p36["UiExtensions"]
    p35["CollaborationModelPolicy"]
    p34["HooksSettings"]
    p33["PermissionRules"]
    p32["SkillsCatalog"]
    p31["ExecutionBackend"]
    p30["ExecShellSettings"]
    p29["HostPlatform"]
    p28["PromptSettings"]
    p27["PromptSuggestionSettings"]
    p26["ToolchainSource"]
    p25["ExecutionSettings"]
    p24["Mcp"]
    p23["McpPublication"]
    p22["ModelLog"]
    p21["web"]
    p20["SettingsMethods"]
    p19["Settings"]
    p18["Provider"]
    p17["GrokOAuth"]
    p16["OpenAICodexOAuth"]
    p15["ManagedProviderMethods"]
    p14["PluginFiles"]
    p13["ManagedProviders"]
    p12["CallChannel"]
    p11["WorkspaceFiles"]
    p10["ManagementMethods"]
    p9["ManagedWorktrees"]
    p8["SessionStore"]
    p7["StorageMethods"]
    p6["SqliteStorage"]
    p5["Aimux"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p75
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p74
  p10 -->|"dim_app_server::methods::ManagementMethods"| p74
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p73
  p19 -->|"dim_app_server::settings::SettingsService"| p73
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p72
  p10 -->|"dim_app_server::methods::ManagementMethods"| p72
  p14 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p72
  p38 -->|"dyn rutis_bridge::session::services::HostDispatch#sessions"| p72
  p72 -->|"dim_event_hub::bridge::EventHub"| p76
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p71
  p10 -->|"dim_app_server::methods::ManagementMethods"| p71
  p14 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p71
  p19 -->|"dim_app_server::settings::SettingsService"| p71
  p38 -->|"dyn rutis_bridge::session::services::HostDispatch#sessions"| p71
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p70
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p69
  p18 -->|"dim_agent_capabilities::host::ModelSelectionHost"| p68
  p18 -->|"dim_agent_capabilities::host::SelectableModelsHost"| p68
  p10 -->|"dim_app_server::methods::ManagementMethods"| p68
  p14 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p68
  p19 -->|"dim_app_server::settings::SettingsService"| p68
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p67
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p66
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p65
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p64
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p63
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p62
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p61
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p60
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p59
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p58
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p57
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p56
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p55
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p54
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p53
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p19 -->|"dim_app_server::settings::SettingsService"| p47
  p43 -->|"dim_app_server::connection::ConnectionHub"| p45
  p3 -->|"dim_app_server::control::ServerControl"| p45
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p45
  p12 -->|"dim_agent_capabilities::methods::Callers"| p44
  p40 -->|"dim_app_server::agents::AgentIndex"| p44
  p41 -->|"dim_app_server::collaboration::Collaboration"| p44
  p43 -->|"dim_app_server::connection::ConnectionHub"| p44
  p3 -->|"dim_app_server::control::ServerControl"| p44
  p10 -->|"dim_app_server::methods::ManagementMethods"| p44
  p2 -->|"rutis_loader::loader::Loader"| p44
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p43
  p3 -->|"dim_app_server::control::ServerControl"| p43
  p42 -->|"dim_app_server::ui_host::UiHost"| p43
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p41
  p40 -->|"dim_app_server::agents::AgentIndex"| p41
  p39 -->|"dim_app_server::collaboration::CollaborationDirectory"| p41
  p19 -->|"dim_app_server::settings::SettingsService"| p41
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p40
  p38 -->|"dim_app_server::agents::AgentDirectory"| p40
  p19 -->|"dim_app_server::settings::SettingsService"| p40
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p40
  p8 -->|"dim_app_server::storage::timeline::SessionStore"| p40
  p2 -->|"rutis_loader::loader::Loader"| p40
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p37
  p10 -->|"dim_app_server::methods::ManagementMethods"| p37
  p2 -->|"rutis_loader::loader::Loader"| p37
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p36
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p36
  p10 -->|"dim_app_server::methods::ManagementMethods"| p36
  p10 -->|"dim_app_server::methods::ManagementMethods"| p35
  p19 -->|"dim_app_server::settings::SettingsService"| p35
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p34
  p10 -->|"dim_app_server::methods::ManagementMethods"| p34
  p19 -->|"dim_app_server::settings::SettingsService"| p34
  p10 -->|"dim_app_server::methods::ManagementMethods"| p33
  p19 -->|"dim_app_server::settings::SettingsService"| p33
  p29 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p32
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p32
  p10 -->|"dim_app_server::methods::ManagementMethods"| p32
  p19 -->|"dim_app_server::settings::SettingsService"| p32
  p29 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p31
  p10 -->|"dim_app_server::methods::ManagementMethods"| p31
  p29 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p30
  p10 -->|"dim_app_server::methods::ManagementMethods"| p30
  p25 -->|"dim_agent_capabilities::execution_settings::ExecutionSettingsHost"| p29
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p29
  p26 -->|"dim_agent_capabilities::toolchain::ToolchainSourceHost"| p29
  p19 -->|"dim_app_server::settings::SettingsService"| p28
  p19 -->|"dim_app_server::settings::SettingsService"| p27
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p25
  p19 -->|"dim_app_server::settings::SettingsService"| p25
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p24
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p24
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p24
  p23 -->|"dim_app_server::mcp::publication::Publication"| p24
  p10 -->|"dim_app_server::methods::ManagementMethods"| p24
  p19 -->|"dim_app_server::settings::SettingsService"| p24
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p23
  p19 -->|"dim_app_server::settings::SettingsService"| p23
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p22
  p19 -->|"dim_app_server::settings::SettingsService"| p22
  p46 -->|"dim_agent_runtime::parts::PartRegistry"| p21
  p19 -->|"dim_app_server::settings::SettingsService"| p21
  p10 -->|"dim_app_server::methods::ManagementMethods"| p20
  p19 -->|"dim_app_server::settings::SettingsService"| p20
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p19
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p19
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p18
  p40 -->|"dim_app_server::agents::AgentIndex"| p18
  p16 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p18
  p17 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p18
  p13 -->|"dim_app_server::managed_provider::ManagedProviders"| p18
  p10 -->|"dim_app_server::methods::ManagementMethods"| p18
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p18
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p17
  p10 -->|"dim_app_server::methods::ManagementMethods"| p17
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p16
  p10 -->|"dim_app_server::methods::ManagementMethods"| p16
  p13 -->|"dim_app_server::managed_provider::ManagedProviders"| p15
  p10 -->|"dim_app_server::methods::ManagementMethods"| p15
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p14
  p60 -->|"dim_agent_capabilities::workspace_tools::mentions::MentionIndex"| p11
  p10 -->|"dim_app_server::methods::ManagementMethods"| p11
  p10 -->|"dim_app_server::methods::ManagementMethods"| p9
  p8 -->|"dim_app_server::storage::timeline::SessionStore"| p9
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p8
  p10 -->|"dim_app_server::methods::ManagementMethods"| p7
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p7
```
