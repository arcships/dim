//! The process host ports whose owner is a Settings source or a model
//! catalogue: the switches, the installed runtimes, PromptSuggestion's
//! document and the three model ports. Each face is served under its own name
//! for any language, and the Rust binding calls the same operations: these
//! tests pin that both answer the same
//! ([service faces §4.8](../../docs/design/service-faces.md#48-宿主端口executionsettingshost-等六个)).
use dim_agent_capabilities::execution_settings::{
    ExecutionSettings, ExecutionSettingsError, ExecutionSettingsHost,
};
use dim_agent_capabilities::host::{
    HostError, ModalityConnection, ModalityConnectionHost, ModelConnection, ModelPortError,
    ModelProtocol, ModelSelectionHost, ModelSelectionRequest, SelectableModelsHost,
};
use dim_agent_capabilities::prompt_suggestion::{
    PromptSuggestionSettings, PromptSuggestionSettingsError, PromptSuggestionSettingsHost,
};
use dim_agent_capabilities::toolchain::{
    InstalledToolchain, ToolchainSourceHost, ToolchainSourceReply, ToolchainView,
};
use dim_agent_types::{ModalityModelSnapshot, ModelId, ModelSelection};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{Error, HostDispatch, Value as RpcValue, host_key};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;

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

/// What the face `name` answers `method`, read as its reply type.
async fn wire<T: DeserializeOwned>(root: &Ctx, name: &str, method: &str) -> T {
    let reply = call(&face(root, name), method, vec![])
        .await
        .unwrap_or_else(|error| panic!("{name}.{method}: {error}"));
    serde_json::from_value(reply).unwrap_or_else(|error| panic!("{name}.{method}: {error}"))
}

#[tokio::test]
async fn the_execution_switches_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let switches = ExecutionSettings {
        sandbox: true,
        network: false,
    };
    root.provide(ExecutionSettingsHost::fixed(switches))
        .unwrap();
    let binding = root.get::<ExecutionSettingsHost>().unwrap();
    assert_eq!(
        binding.read().await.unwrap(),
        wire::<ExecutionSettings>(&root, "executionSettingsHost", "read").await
    );
    assert_eq!(
        wire::<ExecutionSettings>(&root, "executionSettingsHost", "read").await,
        switches
    );
}

#[tokio::test]
async fn the_installed_runtimes_answer_the_same_through_the_binding_and_the_face() {
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
    root.provide(ToolchainSourceHost::fixed(installed)).unwrap();
    let binding = root.get::<ToolchainSourceHost>().unwrap();
    let listed = wire::<ToolchainSourceReply>(&root, "toolchainSourceHost", "list").await;
    assert_eq!(binding.list().unwrap(), listed);
    assert_eq!(
        listed,
        ToolchainSourceReply {
            toolchains: vec![
                ToolchainView {
                    bin: "/home/runtimes/node-v22/bin".into(),
                    root: "/home/runtimes/node-v22".into(),
                },
                ToolchainView {
                    bin: "/home/runtimes/python/bin".into(),
                    root: "/home/runtimes/python".into(),
                },
            ],
        }
    );
}

#[tokio::test]
async fn the_prompt_suggestion_document_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let document = PromptSuggestionSettings {
        enabled: true,
        max_chars: 120,
    };
    root.provide(PromptSuggestionSettingsHost::new(Arc::new(move || {
        Box::pin(async move { Ok(document) })
    })))
    .unwrap();
    let binding = root.get::<PromptSuggestionSettingsHost>().unwrap();
    let read =
        wire::<PromptSuggestionSettings>(&root, "promptSuggestionSettingsHost", "read").await;
    assert_eq!(binding.read().await.unwrap(), read);
    assert_eq!(read, document, "the document keeps its Settings spelling");
}

#[tokio::test]
async fn the_selectable_models_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let declared = || ModalityModelSnapshot {
        target: ModelId {
            provider_id: "provider".into(),
            model_id: "media".into(),
        },
        operations: vec!["image.generate".into()],
    };
    root.provide(SelectableModelsHost::new(
        Arc::new(|| Box::pin(async { Ok(Vec::new()) })),
        Arc::new(move || Box::pin(async move { Ok(vec![declared()]) })),
    ))
    .unwrap();
    let binding = root.get::<SelectableModelsHost>().unwrap();
    assert_eq!(
        binding.selectable_models().await.unwrap().models,
        wire::<dim_agent_capabilities::host::SelectableModelsReply>(
            &root,
            "selectableModelsHost",
            "selectableModels"
        )
        .await
        .models
    );
    let modality = wire::<dim_agent_capabilities::host::ModalityModelsReply>(
        &root,
        "selectableModelsHost",
        "selectableForModality",
    )
    .await;
    assert_eq!(binding.selectable_for_modality().await.unwrap(), modality);
    assert_eq!(modality.models, vec![declared()]);
}

#[tokio::test]
async fn the_model_selection_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(ModelSelectionHost::new(Arc::new(|explicit, _| {
        Box::pin(async move {
            let id = explicit.unwrap_or(ModelId {
                provider_id: "provider".into(),
                model_id: "default".into(),
            });
            Ok(ModelSelection {
                provider_id: id.provider_id,
                model_id: id.model_id,
                capability_snapshot: None,
                config: None,
                metadata: None,
            })
        })
    })))
    .unwrap();
    let binding = root.get::<ModelSelectionHost>().unwrap();
    let request = ModelSelectionRequest::at(
        Some(ModelId {
            provider_id: "provider".into(),
            model_id: "chosen".into(),
        }),
        Some(std::path::Path::new("/work")),
    )
    .unwrap();
    let reply = call(
        &face(&root, "modelSelectionHost"),
        "resolve",
        vec![serde_json::to_value(&request).unwrap().into()],
    )
    .await
    .unwrap();
    let selected: ModelSelection = serde_json::from_value(reply).unwrap();
    assert_eq!(binding.resolve(request).await.unwrap(), selected);
    assert_eq!(selected.model_id, "chosen");
}

#[tokio::test]
async fn the_modality_connection_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(ModalityConnectionHost::new(Arc::new(|target| {
        Box::pin(async move {
            Ok(ModalityConnection {
                connection: ModelConnection::Local,
                protocol: ModelProtocol::Anthropic,
                model: target.target.model_id,
                realtime: None,
            })
        })
    })))
    .unwrap();
    let binding = root.get::<ModalityConnectionHost>().unwrap();
    let target = dim_agent_capabilities::host::ModalityTarget {
        target: ModelId {
            provider_id: "provider".into(),
            model_id: "media".into(),
        },
        operation: "image.generate".into(),
    };
    let reply = call(
        &face(&root, "modalityConnectionHost"),
        "resolveModalityConnection",
        vec![serde_json::to_value(&target).unwrap().into()],
    )
    .await
    .unwrap();
    let connection: ModalityConnection = serde_json::from_value(reply).unwrap();
    assert_eq!(
        binding.resolve_modality_connection(target).await.unwrap(),
        connection
    );
    assert_eq!(connection.model, "media");
}

#[tokio::test]
async fn a_host_failure_keeps_the_code_its_caller_acts_on() {
    let root = Ctx::root().unwrap();
    root.provide(SelectableModelsHost::new(
        Arc::new(|| Box::pin(async { Err(HostError::Cancelled("the owner closed".into())) })),
        Arc::new(|| Box::pin(async { Err(HostError::failed("storage")) })),
    ))
    .unwrap();
    let cancelled = call(
        &face(&root, "selectableModelsHost"),
        "selectableModels",
        vec![],
    )
    .await
    .expect_err("the owner fails");
    assert_eq!(cancelled.code(), Some("cancelled"));
    let failed = call(
        &face(&root, "selectableModelsHost"),
        "selectableForModality",
        vec![],
    )
    .await
    .expect_err("the owner fails");
    assert_eq!(failed.code(), Some("failed"));
}

#[test]
fn the_port_errors_cross_as_code_and_message() {
    let cancelled = serde_json::to_value(ModelPortError::Cancelled {
        message: "stop".into(),
    })
    .unwrap();
    assert_eq!(cancelled, json!({"code": "cancelled", "message": "stop"}));
    let closed = serde_json::to_value(ModelPortError::OwnerClosed {
        message: "reload".into(),
    })
    .unwrap();
    assert_eq!(closed, json!({"code": "owner_closed", "message": "reload"}));
    let failed = serde_json::to_value(ExecutionSettingsError::Failed {
        message: "no document".into(),
    })
    .unwrap();
    assert_eq!(failed, json!({"code": "failed", "message": "no document"}));
    let failed = serde_json::to_value(PromptSuggestionSettingsError::Failed {
        message: "no document".into(),
    })
    .unwrap();
    assert_eq!(failed, json!({"code": "failed", "message": "no document"}));
}
