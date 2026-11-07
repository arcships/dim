# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p69["Attachments"]
    p68["goal"]
    p67["Questions"]
    p66["PromptSuggestion"]
    p65["Modality"]
    p64["PrepareModelHost"]
    p63["ContextCompaction"]
    p62["CommandRules"]
    p61["PromptSources"]
    p60["ShellInput"]
    p59["Recall"]
    p58["Wait"]
    p57["WorkspaceTools"]
    p56["BranchTool"]
    p55["ToolExecution"]
    p54["Permission"]
    p53["ModelProvider"]
    p52["Loop"]
    p51["Prompt"]
    p50["ContextPreparation"]
    p49["ToolRegistry"]
    p48["InputQueue"]
    p47["SessionMethods"]
    p46["TimelineTree"]
    p45["SessionBroadcast"]
    p44["session-title"]
    p43["PartRegistry"]
    p42["Transport"]
    p41["Rpc"]
    p40["Connection"]
    p39["UiHost"]
    p38["Collaboration"]
    p37["Agents"]
    p36["CollaborationDirectory"]
    p35["AgentDirectory"]
    p34["PluginManager"]
    p33["UiExtensions"]
    p32["CollaborationModelPolicy"]
    p31["HooksSettings"]
    p30["PermissionRules"]
    p29["SkillsCatalog"]
    p28["ExecutionBackend"]
    p27["ExecShellSettings"]
    p26["HostPlatform"]
    p25["PromptSuggestionSettings"]
    p24["ComponentSource"]
    p23["ToolchainSource"]
    p22["ExecutionSettings"]
    p21["Mcp"]
    p20["McpPublication"]
    p19["ModelLog"]
    p18["web"]
    p17["SettingsMethods"]
    p16["Settings"]
    p15["Provider"]
    p14["GrokOAuth"]
    p13["OpenAICodexOAuth"]
    p12["ManagedProviderMethods"]
    p11["PluginFiles"]
    p10["ManagedProviders"]
    p9["CallChannel"]
    p8["ManagementMethods"]
    p7["SessionStore"]
    p6["SqliteStorage"]
    p5["Aimux"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p69
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p68
  p16 -->|"dim_app_server::settings::SettingsService"| p68
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p67
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p66
  p15 -->|"dim_agent_capabilities::host::ModelSelectionHost"| p65
  p15 -->|"dim_agent_capabilities::host::SelectableModelsHost"| p65
  p8 -->|"dim_app_server::methods::ManagementMethods"| p65
  p16 -->|"dim_app_server::settings::SettingsService"| p65
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p64
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p63
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p62
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p61
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p60
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p59
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p58
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p57
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p56
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p55
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p54
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p53
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p46
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p45
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p44
  p16 -->|"dim_app_server::settings::SettingsService"| p44
  p40 -->|"dim_app_server::connection::ConnectionHub"| p42
  p3 -->|"dim_app_server::control::ServerControl"| p42
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p42
  p9 -->|"dim_agent_capabilities::methods::Callers"| p41
  p37 -->|"dim_app_server::agents::AgentIndex"| p41
  p38 -->|"dim_app_server::collaboration::Collaboration"| p41
  p40 -->|"dim_app_server::connection::ConnectionHub"| p41
  p3 -->|"dim_app_server::control::ServerControl"| p41
  p8 -->|"dim_app_server::methods::ManagementMethods"| p41
  p2 -->|"rutis_loader::loader::Loader"| p41
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p40
  p3 -->|"dim_app_server::control::ServerControl"| p40
  p39 -->|"dim_app_server::ui_host::UiHost"| p40
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p38
  p37 -->|"dim_app_server::agents::AgentIndex"| p38
  p36 -->|"dim_app_server::collaboration::CollaborationDirectory"| p38
  p16 -->|"dim_app_server::settings::SettingsService"| p38
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p37
  p35 -->|"dim_app_server::agents::AgentDirectory"| p37
  p16 -->|"dim_app_server::settings::SettingsService"| p37
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p37
  p7 -->|"dim_app_server::storage::timeline::SessionStore"| p37
  p2 -->|"rutis_loader::loader::Loader"| p37
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p34
  p8 -->|"dim_app_server::methods::ManagementMethods"| p34
  p2 -->|"rutis_loader::loader::Loader"| p34
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p33
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p33
  p8 -->|"dim_app_server::methods::ManagementMethods"| p33
  p8 -->|"dim_app_server::methods::ManagementMethods"| p32
  p16 -->|"dim_app_server::settings::SettingsService"| p32
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p31
  p8 -->|"dim_app_server::methods::ManagementMethods"| p31
  p16 -->|"dim_app_server::settings::SettingsService"| p31
  p8 -->|"dim_app_server::methods::ManagementMethods"| p30
  p16 -->|"dim_app_server::settings::SettingsService"| p30
  p26 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p29
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p29
  p8 -->|"dim_app_server::methods::ManagementMethods"| p29
  p16 -->|"dim_app_server::settings::SettingsService"| p29
  p26 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p28
  p8 -->|"dim_app_server::methods::ManagementMethods"| p28
  p26 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p27
  p8 -->|"dim_app_server::methods::ManagementMethods"| p27
  p22 -->|"dim_agent_capabilities::execution_settings::ExecutionSettingsHost"| p26
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p26
  p23 -->|"dim_agent_capabilities::toolchain::ToolchainSourceHost"| p26
  p16 -->|"dim_app_server::settings::SettingsService"| p25
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p22
  p16 -->|"dim_app_server::settings::SettingsService"| p22
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p21
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p21
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p21
  p20 -->|"dim_app_server::mcp::publication::Publication"| p21
  p8 -->|"dim_app_server::methods::ManagementMethods"| p21
  p16 -->|"dim_app_server::settings::SettingsService"| p21
  p1 -->|"dim_agent_capabilities::host::CredentialVariablesHost"| p20
  p16 -->|"dim_app_server::settings::SettingsService"| p20
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p19
  p16 -->|"dim_app_server::settings::SettingsService"| p19
  p43 -->|"dim_agent_runtime::parts::PartRegistry"| p18
  p16 -->|"dim_app_server::settings::SettingsService"| p18
  p8 -->|"dim_app_server::methods::ManagementMethods"| p17
  p16 -->|"dim_app_server::settings::SettingsService"| p17
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p16
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p16
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p15
  p37 -->|"dim_app_server::agents::AgentIndex"| p15
  p13 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p15
  p14 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p15
  p10 -->|"dim_app_server::managed_provider::ManagedProviders"| p15
  p8 -->|"dim_app_server::methods::ManagementMethods"| p15
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p15
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p14
  p8 -->|"dim_app_server::methods::ManagementMethods"| p14
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p13
  p8 -->|"dim_app_server::methods::ManagementMethods"| p13
  p10 -->|"dim_app_server::managed_provider::ManagedProviders"| p12
  p8 -->|"dim_app_server::methods::ManagementMethods"| p12
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p11
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p7
```
