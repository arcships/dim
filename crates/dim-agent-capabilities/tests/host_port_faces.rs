//! The process host ports whose owner is a Settings source: the switches, the
//! installed runtimes and PromptSuggestion's document. Each is read by its
//! Rust consumer and, under its own name, by a part of any language
//! ([service faces §2.1](../../docs/design/service-faces.md#21-标记)).
use dim_agent_capabilities::execution_settings::{
    ExecutionSettings, ExecutionSettingsError, ExecutionSettingsHost, ExecutionSettingsSource,
};
use dim_agent_capabilities::host::{HostError, HostFuture, SelectableModels, SelectableModelsHost};
use dim_agent_capabilities::prompt_suggestion::{
    PromptSuggestionSettings, PromptSuggestionSettingsError, PromptSuggestionSettingsHost,
    PromptSuggestionSettingsSource,
};
use dim_agent_capabilities::toolchain::{InstalledToolchain, ToolchainSourceHost, ToolchainView};
use dim_agent_types::{ModalityModelSnapshot, ModelCapabilitySnapshot};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost};
use rutis_bridge::session::{Error, HostDispatch, Value as RpcValue, host_key};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The face `name` is provided under, as a caller of another language reads
/// it.
fn face(root: &Ctx, name: &str) -> Arc<dyn HostDispatch> {
    root.get_at::<dyn HostDispatch>(&host_key(name))
        .unwrap_or_else(|| panic!("{name} is a data face"))
}

/// One call of `port`'s face, waiting for its result.
async fn call(
    port: &Arc<dyn HostDispatch>,
    method: &str,
    args: Vec<RpcValue>,
) -> Result<Value, Error> {
    match port.invoke(method, RpcValue::List(args))? {
        RpcValue::Reference(reply) if reply.is_future() => Ok(reply.wait_async().await?.json()?),
        value => Ok(value.json()?),
    }
}

/// An execution source whose read fails, as a Settings document that cannot
/// be read does.
struct Unreadable;

impl ExecutionSettingsSource for Unreadable {
    fn read(
        &self,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<ExecutionSettings, ExecutionSettingsError>> {
        Box::pin(async move { Err(ExecutionSettingsError::Failed("no document".into())) })
    }
}

/// A selectable-models owner whose read ends with the failure of a closed
/// owner.
struct Closed;

impl SelectableModels for Closed {
    fn selectable(&self, _: CancellationToken) -> HostFuture<'_, Vec<ModelCapabilitySnapshot>> {
        Box::pin(async move { Err(HostError::Cancelled("the owner closed".into())) })
    }
    fn selectable_for_modality(
        &self,
        _: CancellationToken,
    ) -> HostFuture<'_, Vec<ModalityModelSnapshot>> {
        Box::pin(async move { Err(HostError::Cancelled("the owner closed".into())) })
    }
}

struct FixedSuggestions(PromptSuggestionSettings);

impl PromptSuggestionSettingsSource for FixedSuggestions {
    fn read(
        &self,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<PromptSuggestionSettings, PromptSuggestionSettingsError>> {
        Box::pin(async move { Ok(self.0) })
    }
}

#[tokio::test]
async fn the_execution_switches_cross_as_data() {
    let root = Ctx::root().unwrap();
    let switches = ExecutionSettings {
        sandbox: true,
        network: false,
    };
    dim_agent_capabilities::execution_settings::provide(
        &root,
        &ExecutionSettingsHost::fixed(switches),
    )
    .unwrap();
    assert!(
        root.get::<ExecutionSettingsHost>().is_some(),
        "the Rust consumer still reads the port at its own key"
    );
    let port = face(&root, "executionSettingsHost");
    assert_eq!(port.methods().unwrap(), json!({"read": "async"}));
    assert_eq!(
        call(&port, "read", vec![]).await.unwrap(),
        json!({"sandbox": true, "network": false})
    );
    // A cancellation that is not a function is the caller's mistake.
    assert_eq!(
        call(&port, "read", vec![json!({"cancel": true}).into()])
            .await
            .unwrap_err()
            .code(),
        Some("invalid_request")
    );
    assert!(call(&port, "write", vec![]).await.is_err());

    let failing = Ctx::root().unwrap();
    dim_agent_capabilities::execution_settings::provide(
        &failing,
        &ExecutionSettingsHost(Arc::new(Unreadable)),
    )
    .unwrap();
    assert_eq!(
        call(&face(&failing, "executionSettingsHost"), "read", vec![])
            .await
            .unwrap_err()
            .code(),
        Some("failed"),
        "the owner's failure keeps its own code, not the caller's"
    );
}

#[tokio::test]
async fn the_installed_runtimes_cross_as_data() {
    let root = Ctx::root().unwrap();
    let installed = vec![
        InstalledToolchain {
            bin: "/home/runtimes/node-v22/bin".into(),
            root: "/home/runtimes/node-v22".into(),
        },
        InstalledToolchain {
            bin: "/home/runtimes/python/bin".into(),
            root: "/home/runtimes/python".into(),
        },
    ];
    dim_agent_capabilities::toolchain::provide(&root, &ToolchainSourceHost::fixed(installed))
        .unwrap();
    assert!(root.get::<ToolchainSourceHost>().is_some());
    let port = face(&root, "toolchainSourceHost");
    assert_eq!(port.methods().unwrap(), json!({"list": "sync"}));
    let reply = call(&port, "list", vec![]).await.unwrap();
    let toolchains: Vec<ToolchainView> =
        serde_json::from_value(reply["toolchains"].clone()).unwrap();
    assert_eq!(
        toolchains,
        [
            ToolchainView {
                bin: "/home/runtimes/node-v22/bin".into(),
                root: "/home/runtimes/node-v22".into(),
            },
            ToolchainView {
                bin: "/home/runtimes/python/bin".into(),
                root: "/home/runtimes/python".into(),
            },
        ]
    );
    assert_eq!(
        call(&port, "list", vec![json!({}).into()])
            .await
            .unwrap_err()
            .code(),
        Some("invalid_request")
    );
}

#[tokio::test]
async fn the_prompt_suggestion_document_crosses_as_data() {
    let root = Ctx::root().unwrap();
    let document = PromptSuggestionSettings {
        enabled: true,
        max_chars: 120,
    };
    dim_agent_capabilities::prompt_suggestion::provide_settings(
        &root,
        &PromptSuggestionSettingsHost(Arc::new(FixedSuggestions(document))),
    )
    .unwrap();
    assert!(root.get::<PromptSuggestionSettingsHost>().is_some());
    let port = face(&root, "promptSuggestionSettingsHost");
    assert_eq!(port.methods().unwrap(), json!({"read": "async"}));
    assert_eq!(
        call(&port, "read", vec![]).await.unwrap(),
        json!({"enabled": true, "maxChars": 120}),
        "the document crosses in the spelling its Settings source has"
    );
}

#[tokio::test]
async fn a_host_failure_keeps_its_kind_as_the_code() {
    let root = Ctx::root().unwrap();
    dim_agent_capabilities::port_faces::provide_selectable_models(
        &root,
        &SelectableModelsHost(Arc::new(Closed)),
    )
    .unwrap();
    let port = face(&root, "selectableModelsHost");
    assert_eq!(
        port.methods().unwrap(),
        json!({"selectableModels": "async", "selectableForModality": "async"})
    );
    for method in ["selectableModels", "selectableForModality"] {
        assert_eq!(
            call(&port, method, vec![]).await.unwrap_err().code(),
            Some("cancelled"),
            "a host failure crosses with the kind its caller acts on"
        );
    }
}
