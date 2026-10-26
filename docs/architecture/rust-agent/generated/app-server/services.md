# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p52["Attachments"]
    p51["Questions"]
    p50["PrepareModelHost"]
    p49["ContextCompaction"]
    p48["CommandRules"]
    p47["PromptSources"]
    p46["ShellInput"]
    p45["Wait"]
    p44["WorkspaceTools"]
    p43["BranchTool"]
    p42["ToolExecution"]
    p41["Permission"]
    p40["ModelProvider"]
    p39["Loop"]
    p38["Prompt"]
    p37["ContextPreparation"]
    p36["ToolRegistry"]
    p35["InputQueue"]
    p34["SessionMethods"]
    p33["TimelineTree"]
    p32["SessionBroadcast"]
    p31["PartRegistry"]
    p30["Transport"]
    p29["Rpc"]
    p28["Connection"]
    p27["UiHost"]
    p26["Collaboration"]
    p25["Agents"]
    p24["CollaborationDirectory"]
    p23["AgentDirectory"]
    p22["PluginManager"]
    p21["CollaborationModelPolicy"]
    p20["PermissionRules"]
    p19["SkillsCatalog"]
    p18["HostPlatform"]
    p17["Mcp"]
    p16["McpPublication"]
    p15["SettingsMethods"]
    p14["Settings"]
    p13["Provider"]
    p12["GrokOAuth"]
    p11["OpenAICodexOAuth"]
    p10["ManagedProviders"]
    p9["PluginFiles"]
    p8["ManagementMethods"]
    p7["SessionStore"]
    p6["SqliteStorage"]
    p5["Aimux"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p52
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p51
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p50
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p49
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p48
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p47
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p46
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p45
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p44
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p43
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p42
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p41
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p40
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p39
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p38
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p37
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p36
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p35
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p34
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p33
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p32
  p28 -->|"dim_app_server::connection::ConnectionHub"| p30
  p3 -->|"dim_app_server::control::ServerControl"| p30
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p30
  p25 -->|"dim_app_server::agents::AgentIndex"| p29
  p26 -->|"dim_app_server::collaboration::Collaboration"| p29
  p28 -->|"dim_app_server::connection::ConnectionHub"| p29
  p3 -->|"dim_app_server::control::ServerControl"| p29
  p8 -->|"dim_app_server::methods::ManagementMethods"| p29
  p2 -->|"rutis_loader::loader::Loader"| p29
  p3 -->|"dim_app_server::control::ServerControl"| p28
  p27 -->|"dim_app_server::ui_host::UiHost"| p28
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p26
  p25 -->|"dim_app_server::agents::AgentIndex"| p26
  p24 -->|"dim_app_server::collaboration::CollaborationDirectory"| p26
  p14 -->|"dim_app_server::settings::SettingsService"| p26
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p25
  p23 -->|"dim_app_server::agents::AgentDirectory"| p25
  p14 -->|"dim_app_server::settings::SettingsService"| p25
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p25
  p7 -->|"dim_app_server::storage::timeline::SessionStore"| p25
  p2 -->|"rutis_loader::loader::Loader"| p25
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p22
  p8 -->|"dim_app_server::methods::ManagementMethods"| p22
  p2 -->|"rutis_loader::loader::Loader"| p22
  p8 -->|"dim_app_server::methods::ManagementMethods"| p21
  p14 -->|"dim_app_server::settings::SettingsService"| p21
  p8 -->|"dim_app_server::methods::ManagementMethods"| p20
  p14 -->|"dim_app_server::settings::SettingsService"| p20
  p18 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p19
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p19
  p8 -->|"dim_app_server::methods::ManagementMethods"| p19
  p14 -->|"dim_app_server::settings::SettingsService"| p19
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p18
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p17
  p31 -->|"dim_agent_runtime::parts::PartRegistry"| p17
  p16 -->|"dim_app_server::mcp::publication::Publication"| p17
  p8 -->|"dim_app_server::methods::ManagementMethods"| p17
  p14 -->|"dim_app_server::settings::SettingsService"| p17
  p14 -->|"dim_app_server::settings::SettingsService"| p16
  p8 -->|"dim_app_server::methods::ManagementMethods"| p15
  p14 -->|"dim_app_server::settings::SettingsService"| p15
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p14
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p14
  p5 -->|"dim_agent_capabilities::host::ModelHost"| p13
  p11 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p13
  p12 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p13
  p10 -->|"dim_app_server::managed_provider::ManagedProviders"| p13
  p8 -->|"dim_app_server::methods::ManagementMethods"| p13
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p13
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p12
  p8 -->|"dim_app_server::methods::ManagementMethods"| p12
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p11
  p8 -->|"dim_app_server::methods::ManagementMethods"| p11
  p8 -->|"dim_app_server::methods::ManagementMethods"| p10
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p9
  p6 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p7
```
