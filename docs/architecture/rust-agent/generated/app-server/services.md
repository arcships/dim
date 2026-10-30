# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p56["Attachments"]
    p55["Questions"]
    p54["PrepareModelHost"]
    p53["ContextCompaction"]
    p52["CommandRules"]
    p51["PromptSources"]
    p50["ShellInput"]
    p49["Recall"]
    p48["Wait"]
    p47["WorkspaceTools"]
    p46["BranchTool"]
    p45["ToolExecution"]
    p44["Permission"]
    p43["ModelProvider"]
    p42["Loop"]
    p41["Prompt"]
    p40["ContextPreparation"]
    p39["ToolRegistry"]
    p38["InputQueue"]
    p37["SessionMethods"]
    p36["TimelineTree"]
    p35["SessionBroadcast"]
    p34["PartRegistry"]
    p33["Transport"]
    p32["Rpc"]
    p31["Connection"]
    p30["UiHost"]
    p29["Collaboration"]
    p28["Agents"]
    p27["CollaborationDirectory"]
    p26["AgentDirectory"]
    p25["PluginManager"]
    p24["CollaborationModelPolicy"]
    p23["PermissionRules"]
    p22["SkillsCatalog"]
    p21["IsolationPolicy"]
    p20["HostPlatform"]
    p19["Mcp"]
    p18["McpPublication"]
    p17["web"]
    p16["SettingsMethods"]
    p15["Settings"]
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
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p56
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p55
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p54
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p53
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p46
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p45
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p44
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p43
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p42
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p41
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p40
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p39
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p38
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p37
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p36
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p35
  p31 -->|"dim_app_server::connection::ConnectionHub"| p33
  p3 -->|"dim_app_server::control::ServerControl"| p33
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p33
  p28 -->|"dim_app_server::agents::AgentIndex"| p32
  p29 -->|"dim_app_server::collaboration::Collaboration"| p32
  p31 -->|"dim_app_server::connection::ConnectionHub"| p32
  p3 -->|"dim_app_server::control::ServerControl"| p32
  p8 -->|"dim_app_server::methods::ManagementMethods"| p32
  p2 -->|"rutis_loader::loader::Loader"| p32
  p3 -->|"dim_app_server::control::ServerControl"| p31
  p30 -->|"dim_app_server::ui_host::UiHost"| p31
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p29
  p28 -->|"dim_app_server::agents::AgentIndex"| p29
  p27 -->|"dim_app_server::collaboration::CollaborationDirectory"| p29
  p15 -->|"dim_app_server::settings::SettingsService"| p29
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p28
  p26 -->|"dim_app_server::agents::AgentDirectory"| p28
  p15 -->|"dim_app_server::settings::SettingsService"| p28
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p28
  p7 -->|"dim_app_server::storage::timeline::SessionStore"| p28
  p2 -->|"rutis_loader::loader::Loader"| p28
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p25
  p8 -->|"dim_app_server::methods::ManagementMethods"| p25
  p2 -->|"rutis_loader::loader::Loader"| p25
  p8 -->|"dim_app_server::methods::ManagementMethods"| p24
  p15 -->|"dim_app_server::settings::SettingsService"| p24
  p8 -->|"dim_app_server::methods::ManagementMethods"| p23
  p15 -->|"dim_app_server::settings::SettingsService"| p23
  p20 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p22
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p22
  p8 -->|"dim_app_server::methods::ManagementMethods"| p22
  p15 -->|"dim_app_server::settings::SettingsService"| p22
  p20 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p21
  p8 -->|"dim_app_server::methods::ManagementMethods"| p21
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p20
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p19
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p19
  p18 -->|"dim_app_server::mcp::publication::Publication"| p19
  p8 -->|"dim_app_server::methods::ManagementMethods"| p19
  p15 -->|"dim_app_server::settings::SettingsService"| p19
  p15 -->|"dim_app_server::settings::SettingsService"| p18
  p34 -->|"dim_agent_runtime::parts::PartRegistry"| p17
  p15 -->|"dim_app_server::settings::SettingsService"| p17
  p8 -->|"dim_app_server::methods::ManagementMethods"| p16
  p15 -->|"dim_app_server::settings::SettingsService"| p16
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p15
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p15
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p14
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
