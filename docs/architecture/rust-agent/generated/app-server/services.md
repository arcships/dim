# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p74["Attachments"]
    p73["Report"]
    p72["goal"]
    p71["eventhub"]
    p75["eventhub-ready"]
    p70["Scheduler"]
    p69["Questions"]
    p68["PromptSuggestion"]
    p67["Modality"]
    p66["PrepareModelHost"]
    p65["ContextCompaction"]
    p64["CommandRules"]
    p63["PromptSources"]
    p62["ShellInput"]
    p61["Recall"]
    p60["Wait"]
    p59["WorkspaceToolsOwner"]
    p58["BranchTool"]
    p57["ToolExecution"]
    p56["Permission"]
    p55["ModelProvider"]
    p54["Loop"]
    p53["Prompt"]
    p52["ContextPreparation"]
    p51["ToolRegistry"]
    p50["InputQueue"]
    p49["SessionMethods"]
    p48["TimelineTree"]
    p47["SessionBroadcast"]
    p46["session-title"]
    p45["PartRegistry"]
    p44["Transport"]
    p43["Rpc"]
    p42["Connection"]
    p41["UiHost"]
    p40["Collaboration"]
    p39["Agents"]
    p38["CollaborationDirectory"]
    p37["AgentDirectory"]
    p36["PluginManager"]
    p35["UiExtensions"]
    p34["CollaborationModelPolicy"]
    p33["HooksSettings"]
    p32["PermissionRules"]
    p31["SkillsCatalog"]
    p30["ExecutionBackend"]
    p29["ExecShellSettings"]
    p28["HostPlatform"]
    p27["PromptSettings"]
    p26["PromptSuggestionSettings"]
    p25["ToolchainSource"]
    p24["ExecutionSettings"]
    p23["Mcp"]
    p22["McpPublication"]
    p21["ModelLog"]
    p20["web"]
    p19["SettingsMethods"]
    p18["Settings"]
    p17["Provider"]
    p16["GrokOAuth"]
    p15["OpenAICodexOAuth"]
    p14["ManagedProviderMethods"]
    p13["PluginFiles"]
    p12["ManagedProviders"]
    p11["CallChannel"]
    p10["WorkspaceFiles"]
    p9["ManagementMethods"]
    p8["SessionStore"]
    p7["StorageMethods"]
    p6["SqliteStorage"]
    p5["Aimux"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p74
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p73
  p9 -->|"dim_app_server::methods::ManagementMethods"| p73
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p72
  p18 -->|"dim_app_server::settings::SettingsService"| p72
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p71
  p9 -->|"dim_app_server::methods::ManagementMethods"| p71
  p13 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p71
  p37 -->|"dyn rutis_bridge::session::services::HostDispatch#sessions"| p71
  p71 -->|"dim_event_hub::bridge::EventHub"| p75
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p70
  p9 -->|"dim_app_server::methods::ManagementMethods"| p70
  p13 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p70
  p18 -->|"dim_app_server::settings::SettingsService"| p70
  p37 -->|"dyn rutis_bridge::session::services::HostDispatch#sessions"| p70
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p69
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p68
  p17 -->|"dim_agent_capabilities::host::ModelSelectionHost"| p67
  p17 -->|"dim_agent_capabilities::host::SelectableModelsHost"| p67
  p9 -->|"dim_app_server::methods::ManagementMethods"| p67
  p13 -->|"dim_app_server::plugin_files::PluginFileTransactions"| p67
  p18 -->|"dim_app_server::settings::SettingsService"| p67
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p66
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p65
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p64
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p63
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p62
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p61
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p60
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p59
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p58
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p57
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p56
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p55
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p54
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p53
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p46
  p18 -->|"dim_app_server::settings::SettingsService"| p46
  p42 -->|"dim_app_server::connection::ConnectionHub"| p44
  p3 -->|"dim_app_server::control::ServerControl"| p44
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p44
  p11 -->|"dim_agent_capabilities::methods::Callers"| p43
  p39 -->|"dim_app_server::agents::AgentIndex"| p43
  p40 -->|"dim_app_server::collaboration::Collaboration"| p43
  p42 -->|"dim_app_server::connection::ConnectionHub"| p43
  p3 -->|"dim_app_server::control::ServerControl"| p43
  p9 -->|"dim_app_server::methods::ManagementMethods"| p43
  p2 -->|"rutis_loader::loader::Loader"| p43
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p42
  p3 -->|"dim_app_server::control::ServerControl"| p42
  p41 -->|"dim_app_server::ui_host::UiHost"| p42
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p40
  p39 -->|"dim_app_server::agents::AgentIndex"| p40
  p38 -->|"dim_app_server::collaboration::CollaborationDirectory"| p40
  p18 -->|"dim_app_server::settings::SettingsService"| p40
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p39
  p37 -->|"dim_app_server::agents::AgentDirectory"| p39
  p18 -->|"dim_app_server::settings::SettingsService"| p39
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p39
  p8 -->|"dim_app_server::storage::timeline::SessionStore"| p39
  p2 -->|"rutis_loader::loader::Loader"| p39
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p36
  p9 -->|"dim_app_server::methods::ManagementMethods"| p36
  p2 -->|"rutis_loader::loader::Loader"| p36
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p35
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p35
  p9 -->|"dim_app_server::methods::ManagementMethods"| p35
  p9 -->|"dim_app_server::methods::ManagementMethods"| p34
  p18 -->|"dim_app_server::settings::SettingsService"| p34
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p33
  p9 -->|"dim_app_server::methods::ManagementMethods"| p33
  p18 -->|"dim_app_server::settings::SettingsService"| p33
  p9 -->|"dim_app_server::methods::ManagementMethods"| p32
  p18 -->|"dim_app_server::settings::SettingsService"| p32
  p28 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p31
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p31
  p9 -->|"dim_app_server::methods::ManagementMethods"| p31
  p18 -->|"dim_app_server::settings::SettingsService"| p31
  p28 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p30
  p9 -->|"dim_app_server::methods::ManagementMethods"| p30
  p28 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p29
  p9 -->|"dim_app_server::methods::ManagementMethods"| p29
  p24 -->|"dim_agent_capabilities::execution_settings::ExecutionSettingsHost"| p28
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p28
  p25 -->|"dim_agent_capabilities::toolchain::ToolchainSourceHost"| p28
  p18 -->|"dim_app_server::settings::SettingsService"| p27
  p18 -->|"dim_app_server::settings::SettingsService"| p26
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p24
  p18 -->|"dim_app_server::settings::SettingsService"| p24
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p23
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p23
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p23
  p22 -->|"dim_app_server::mcp::publication::Publication"| p23
  p9 -->|"dim_app_server::methods::ManagementMethods"| p23
  p18 -->|"dim_app_server::settings::SettingsService"| p23
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p22
  p18 -->|"dim_app_server::settings::SettingsService"| p22
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p21
  p18 -->|"dim_app_server::settings::SettingsService"| p21
  p45 -->|"dim_agent_runtime::parts::PartRegistry"| p20
  p18 -->|"dim_app_server::settings::SettingsService"| p20
  p9 -->|"dim_app_server::methods::ManagementMethods"| p19
  p18 -->|"dim_app_server::settings::SettingsService"| p19
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p18
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p18
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p17
  p39 -->|"dim_app_server::agents::AgentIndex"| p17
  p15 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p17
  p16 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p17
  p12 -->|"dim_app_server::managed_provider::ManagedProviders"| p17
  p9 -->|"dim_app_server::methods::ManagementMethods"| p17
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p17
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p16
  p9 -->|"dim_app_server::methods::ManagementMethods"| p16
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p15
  p9 -->|"dim_app_server::methods::ManagementMethods"| p15
  p12 -->|"dim_app_server::managed_provider::ManagedProviders"| p14
  p9 -->|"dim_app_server::methods::ManagementMethods"| p14
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p13
  p59 -->|"dim_agent_capabilities::workspace_tools::mentions::MentionIndex"| p10
  p9 -->|"dim_app_server::methods::ManagementMethods"| p10
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p8
  p9 -->|"dim_app_server::methods::ManagementMethods"| p7
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p7
```
