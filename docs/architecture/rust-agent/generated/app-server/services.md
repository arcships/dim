# App Server 进程组合

由 `serve` 以产品配置（`--product-home`）真实装配生成，只含进程层，不含 Agent 实例。重新生成：`cargo run -p xtask -- graph`。

```mermaid
%% Generated from live rutis diagnostics; do not edit.
flowchart LR
  subgraph process["process"]
    p1["root"]
    p2["rutis-loader"]
    p27["Transport"]
    p26["Rpc"]
    p25["Connection"]
    p24["Collaboration"]
    p23["Agents"]
    p22["CollaborationDirectory"]
    p21["AgentDirectory"]
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
    p8["SessionStore"]
    p7["SqliteStorage"]
    p6["Aimux"]
    p5["FileWriteCoordinator"]
    p4["FileStorage"]
    p3["ServerControl"]
  end
  p25 -->|"dim_app_server::connection::ConnectionHub"| p27
  p3 -->|"dim_app_server::control::ServerControl"| p27
  p7 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p27
  p23 -->|"dim_app_server::agents::AgentIndex"| p26
  p24 -->|"dim_app_server::collaboration::Collaboration"| p26
  p25 -->|"dim_app_server::connection::ConnectionHub"| p26
  p3 -->|"dim_app_server::control::ServerControl"| p26
  p2 -->|"rutis_loader::loader::Loader"| p26
  p3 -->|"dim_app_server::control::ServerControl"| p25
  p23 -->|"dim_app_server::agents::AgentIndex"| p24
  p22 -->|"dim_app_server::collaboration::CollaborationDirectory"| p24
  p21 -->|"dim_app_server::agents::AgentDirectory"| p23
  p14 -->|"dim_app_server::settings::SettingsService"| p23
  p7 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p23
  p8 -->|"dim_app_server::storage::timeline::SessionStore"| p23
  p2 -->|"rutis_loader::loader::Loader"| p23
  p14 -->|"dim_app_server::settings::SettingsService"| p20
  p18 -->|"dim_agent_capabilities::host_platform::HostPlatform"| p19
  p14 -->|"dim_app_server::settings::SettingsService"| p19
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p18
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p17
  p16 -->|"dim_app_server::mcp::publication::Publication"| p17
  p14 -->|"dim_app_server::settings::SettingsService"| p17
  p14 -->|"dim_app_server::settings::SettingsService"| p16
  p14 -->|"dim_app_server::settings::SettingsService"| p15
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p14
  p7 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p14
  p6 -->|"dim_agent_capabilities::host::ModelHost"| p13
  p11 -->|"dim_app_server::codex_oauth::OpenAICodexOAuthService"| p13
  p12 -->|"dim_app_server::grok_oauth::GrokOAuthService"| p13
  p10 -->|"dim_app_server::managed_provider::ManagedProviders"| p13
  p7 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p13
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p12
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p11
  p4 -->|"dim_agent_capabilities::file_storage::FileStorage"| p9
  p7 -->|"dim_app_server::storage::sqlite::SqliteStorage"| p8
```
