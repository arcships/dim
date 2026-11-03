# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p64["Attachments"]
    p63["Questions"]
    p62["PromptSuggestion"]
    p61["PrepareModelHost"]
    p60["ContextCompaction"]
    p59["CommandRules"]
    p58["PromptSources"]
    p57["ShellInput"]
    p56["Recall"]
    p55["Wait"]
    p54["WorkspaceTools"]
    p53["BranchTool"]
    p52["ToolExecution"]
    p51["Permission"]
    p50["ModelProvider"]
    p49["Loop"]
    p48["Prompt"]
    p47["ContextPreparation"]
    p46["ToolRegistry"]
    p45["InputQueue"]
    p44["SessionMethods"]
    p43["TimelineTree"]
    p42["SessionBroadcast"]
    p41["session-title"]
    p40["PartRegistry"]
    p39["Transport"]
    p38["Rpc"]
    p37["Connection"]
    p36["UiHost"]
    p35["Collaboration"]
    p34["Agents"]
    p33["CollaborationDirectory"]
    p32["AgentDirectory"]
    p31["PluginManager"]
    p30["CollaborationModelPolicy"]
    p29["HooksSettings"]
    p28["PermissionRules"]
    p27["SkillsCatalog"]
    p26["ExecutionBackend"]
    p25["ExecShellSettings"]
    p24["HostPlatform"]
    p23["PromptSuggestionSettings"]
    p22["ExecutionSettings"]
    p21["Mcp"]
    p20["McpPublication"]
    p19["ModelLog"]
    p18["web"]
    p17["SettingsMethods"]
    p16["Settings"]
    p15["Modality"]
    p14["Provider"]
    p13["GrokOAuth"]
    p12["OpenAICodexOAuth"]
    p11["ManagedProviderMethods"]
    p10["PluginFiles"]
    p9["ManagedProviders"]
    p8["ManagementMethods"]
    p7["SessionStore"]
    p6["SqliteStorage"]
    p5["Aimux"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p64
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p63
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p62
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p61
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p60
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p59
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p58
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p57
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p56
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p55
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p54
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p53
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p46
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p45
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p44
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p43
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p42
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p41
  p16 -->|"dim_app_server::settings::SettingsService"| p41
  p37 -->|"dim_app_server::connection::ConnectionHub"| p39
  p3 -->|"dim_app_server::control::ServerControl"| p39
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p39
  p34 -->|"dim_app_server::agents::AgentIndex"| p38
  p35 -->|"dim_app_server::collaboration::Collaboration"| p38
  p37 -->|"dim_app_server::connection::ConnectionHub"| p38
  p3 -->|"dim_app_server::control::ServerControl"| p38
  p8 -->|"dim_app_server::methods::ManagementMethods"| p38
  p2 -->|"rutis_loader::loader::Loader"| p38
  p3 -->|"dim_app_server::control::ServerControl"| p37
  p36 -->|"dim_app_server::ui_host::UiHost"| p37
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p35
  p34 -->|"dim_app_server::agents::AgentIndex"| p35
  p33 -->|"dim_app_server::collaboration::CollaborationDirectory"| p35
  p16 -->|"dim_app_server::settings::SettingsService"| p35
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p34
  p32 -->|"dim_app_server::agents::AgentDirectory"| p34
  p16 -->|"dim_app_server::settings::SettingsService"| p34
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p34
  p7 -->|"dim_app_server::storage::timeline::SessionStore"| p34
  p2 -->|"rutis_loader::loader::Loader"| p34
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p31
  p8 -->|"dim_app_server::methods::ManagementMethods"| p31
  p2 -->|"rutis_loader::loader::Loader"| p31
  p8 -->|"dim_app_server::methods::ManagementMethods"| p30
  p16 -->|"dim_app_server::settings::SettingsService"| p30
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p29
  p8 -->|"dim_app_server::methods::ManagementMethods"| p29
  p16 -->|"dim_app_server::settings::SettingsService"| p29
  p8 -->|"dim_app_server::methods::ManagementMethods"| p28
  p16 -->|"dim_app_server::settings::SettingsService"| p28
  p24 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p27
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p27
  p8 -->|"dim_app_server::methods::ManagementMethods"| p27
  p16 -->|"dim_app_server::settings::SettingsService"| p27
  p24 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p26
  p8 -->|"dim_app_server::methods::ManagementMethods"| p26
  p24 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p25
  p8 -->|"dim_app_server::methods::ManagementMethods"| p25
  p22 -->|"dim_agent_capabilities::execution_settings::ExecutionSettingsHost"| p24
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p24
  p16 -->|"dim_app_server::settings::SettingsService"| p23
  p16 -->|"dim_app_server::settings::SettingsService"| p22
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p21
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p21
  p20 -->|"dim_app_server::mcp::publication::Publication"| p21
  p8 -->|"dim_app_server::methods::ManagementMethods"| p21
  p16 -->|"dim_app_server::settings::SettingsService"| p21
  p16 -->|"dim_app_server::settings::SettingsService"| p20
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p19
  p16 -->|"dim_app_server::settings::SettingsService"| p19
  p40 -->|"dim_agent_runtime::parts::PartRegistry"| p18
  p16 -->|"dim_app_server::settings::SettingsService"| p18
  p8 -->|"dim_app_server::methods::ManagementMethods"| p17
  p16 -->|"dim_app_server::settings::SettingsService"| p17
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p16
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p16
  p14 -->|"dim_agent_capabilities::host::ModelSelectionHost"| p15
  p14 -->|"dim_agent_capabilities::host::SelectableModelsHost"| p15
  p8 -->|"dim_app_server::methods::ManagementMethods"| p15
  p16 -->|"dim_app_server::settings::SettingsService"| p15
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p14
  p34 -->|"dim_app_server::agents::AgentIndex"| p14
  p12 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p14
  p13 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p14
  p9 -->|"dim_app_server::managed_provider::ManagedProviders"| p14
  p8 -->|"dim_app_server::methods::ManagementMethods"| p14
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p14
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p13
  p8 -->|"dim_app_server::methods::ManagementMethods"| p13
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p12
  p8 -->|"dim_app_server::methods::ManagementMethods"| p12
  p9 -->|"dim_app_server::managed_provider::ManagedProviders"| p11
  p8 -->|"dim_app_server::methods::ManagementMethods"| p11
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p10
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p7
```
