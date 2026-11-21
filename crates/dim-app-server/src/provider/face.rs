use super::auxiliary::AuxiliaryModel;
use dim_agent_capabilities::{
    host::ModelPortError,
    port_faces::{request, serving},
};
use dim_agent_types::{ModelConfiguration, ModelId, ModelPreferences};
use rutis::host::CtxHost;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareStartup {
    target: crate::protocol::ModelSelection,
    cwd: String,
    preferences: ModelPreferences,
    input: super::StartupInput,
    current: Option<ModelConfiguration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetainStartup {
    session_id: String,
    generation: String,
    model: dim_agent_types::ModelSelection,
    admission_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseStartup {
    admission_id: String,
}

impl HostDispatch for AuxiliaryModel {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            json!({"configuration":"async","applyPreferences":"async","prepareStartup":"async","retainStartup":"async","checkStartup":"async","releaseStartup":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let owner = self.owner.clone();
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        match method {
            "prepareStartup" => {
                let p: PrepareStartup = request(NAME, args)?;
                let caller = rutis_bridge::session::caller_row()
                    .ok_or_else(|| Error::value("prepareStartup needs its caller"))?;
                let caller = caller.ctx().clone();
                let cancel = caller.cancellation_token().child_token();
                serving(NAME, async move {
                    let _guard = cancel.clone().drop_guard();
                    let selection = owner
                        .prepare_startup(
                            p.target,
                            Path::new(&p.cwd),
                            p.preferences,
                            p.input,
                            p.current.as_ref(),
                            &cancel,
                        )
                        .await
                        .map_err(ModelPortError::from)?;
                    selection.bind(&caller).map_err(ModelPortError::from)
                })
            }
            "retainStartup" => {
                let p: RetainStartup = request(NAME, args)?;
                serving(NAME, async move {
                    let source = p
                        .model
                        .metadata
                        .as_ref()
                        .and_then(|m| m.startup.as_ref())
                        .and_then(|s| s.connection.as_ref());
                    if let Some(id) = &p.admission_id
                        && source.map(|s| &s.id) != Some(id)
                    {
                        return Err(ModelPortError::InvalidRequest {
                            message: "Startup admission does not match model".into(),
                        });
                    }
                    owner
                        .retain_startup(&p.session_id, &p.generation, &p.model)
                        .map_err(ModelPortError::from)?;
                    if let Some(id) = p.admission_id {
                        owner
                            .release_startup(&id)
                            .await
                            .map_err(ModelPortError::from)?;
                    }
                    Ok(())
                })
            }
            "checkStartup" => {
                let source = request(NAME, args)?;
                serving(NAME, async move {
                    owner.check_startup(&source).map_err(ModelPortError::from)
                })
            }
            "releaseStartup" => {
                let p: ReleaseStartup = request(NAME, args)?;
                serving(NAME, async move {
                    owner
                        .release_startup(&p.admission_id)
                        .await
                        .map_err(ModelPortError::from)
                })
            }
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
            _ => Err(Error::value(format!("{NAME} has no method {method}"))),
        }
    }
}
