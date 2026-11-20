use super::auxiliary::AuxiliaryModel;
use dim_agent_capabilities::{
    host::{ModalityModelsReply, ModelPortError, ModelSelectionRequest, SelectableModelsReply},
    port_faces::{Empty, request, serving},
};
use dim_agent_types::{ModelConfiguration, ModelId, ModelPreferences};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

const NAME: &str = "providerService";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configuration {
    explicit: Option<ModelId>,
    cwd: Option<String>,
    preferences: ModelPreferences,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPreferences {
    current: ModelConfiguration,
    update: crate::protocol::ModelPreferenceUpdate,
}

impl HostDispatch for AuxiliaryModel {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            json!({"resolve":"async","selectableModels":"async","selectableForModality":"async",
            "configuration":"async","applyPreferences":"async","complete":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method == "complete" {
            return dim_agent_capabilities::host::AuxiliaryModel(Arc::new(self.clone()))
                .invoke(method, args);
        }
        let owner = self.owner.clone();
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        match method {
            "configuration" => {
                let p: Configuration = request(NAME, args)?;
                serving(NAME, async move {
                    let _guard = guard;
                    owner
                        .configuration(
                            p.explicit,
                            p.cwd.as_deref().map(Path::new),
                            p.preferences,
                            &cancel,
                        )
                        .await
                        .map_err(ModelPortError::from)
                })
            }
            "applyPreferences" => {
                let p: ApplyPreferences = request(NAME, args)?;
                serving(NAME, async move {
                    let _guard = guard;
                    owner
                        .apply_preferences(&p.current, &p.update, &cancel)
                        .await
                        .map_err(ModelPortError::from)
                })
            }
            "resolve" => {
                let p: ModelSelectionRequest = request(NAME, args)?;
                serving(NAME, async move {
                    let _guard = guard;
                    owner
                        .configuration_at_defaults(
                            p.explicit,
                            p.cwd.as_deref().map(Path::new),
                            &cancel,
                        )
                        .await
                        .map_err(ModelPortError::from)
                })
            }
            "selectableModels" => {
                let Empty {} = request(NAME, args)?;
                serving(NAME, async move {
                    let _guard = guard;
                    owner
                        .selectable_models(&cancel)
                        .await
                        .map(|models| SelectableModelsReply { models })
                        .map_err(ModelPortError::from)
                })
            }
            "selectableForModality" => {
                let Empty {} = request(NAME, args)?;
                serving(NAME, async move {
                    let _guard = guard;
                    owner
                        .selectable_modality_models(&cancel)
                        .await
                        .map(|models| ModalityModelsReply { models })
                        .map_err(ModelPortError::from)
                })
            }
            _ => Err(Error::Value(format!("{NAME} has no method {method}"))),
        }
    }
}
