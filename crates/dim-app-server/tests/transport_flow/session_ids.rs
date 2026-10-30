//! Session ids on the wire (methods contract A.1): an empty id is invalid
//! input where a Session method resolves its Session, while a non-empty id
//! that names no Session stays `session_closed`.
use super::*;
use dim_agent_capabilities::{
    model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use std::os::unix::fs::DirBuilderExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_session_id_is_invalid_params_and_an_unknown_one_stays_closed() {
    let home = std::path::PathBuf::from("/tmp").join(format!("dsi-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&native)
        .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: native.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    // A built-in, a contributed method with more params, and one that never
    // acquires the Session: all resolve it through the same authorization.
    for (method, params) in [
        ("session.mcp", json!({"sessionId":""})),
        (
            "session.context.usage",
            json!({"sessionId":"","branchId":"main"}),
        ),
        ("session.close", json!({"sessionId":""})),
    ] {
        let reply = client.rpc(method, params).await;
        assert_eq!(reply["error"]["code"], json!(-32602), "{reply}");
        assert_eq!(
            reply["error"]["data"]["appError"], "invalid_params",
            "{reply}"
        );
        assert_eq!(
            reply["error"]["message"], "sessionId must not be empty",
            "{reply}"
        );
    }
    for (method, params) in [
        ("session.mcp", json!({"sessionId":"missing"})),
        (
            "session.context.usage",
            json!({"sessionId":"missing","branchId":"main"}),
        ),
    ] {
        let reply = client.rpc(method, params).await;
        assert_eq!(
            reply["error"]["data"]["appError"], "session_closed",
            "{reply}"
        );
    }
}
