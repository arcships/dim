#[path = "fixtures/mcp_host.rs"]
mod mcp_host;

#[cfg(unix)]
mod attachments_flow;
mod hooks_settings_flow;
mod legacy_port_faces;
mod legacy_session_faces;
mod managed_oauth_flow;
mod managed_provider_flow;
mod mcp_flow;
mod mcp_http_flow;
#[cfg(unix)]
mod mcp_oauth_flow;
mod mcp_session_flow;
mod model_catalog_flow;
#[cfg(windows)]
mod named_pipe_flow;
#[cfg(any(unix, windows))]
mod plugin_files_flow;
mod prompt_settings_flow;
#[path = "provider_flow.rs"]
mod provider_flow;
#[cfg(unix)]
mod read_image_flow;
mod session_files_flow;
#[cfg(unix)]
mod session_language_flow;
mod settings_flow;
#[path = "storage_flow.rs"]
mod storage_flow;
mod storage_session_flow;
#[cfg(unix)]
mod transport_limits;
