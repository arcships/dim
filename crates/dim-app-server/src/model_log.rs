//! The user's switch for recording model wire exchanges
//! (`docs/design/appserver/model-log.md`). The recording itself is aimux's
//! RFC-0023 format, written under the home root.
use crate::settings::{
    Settings, SettingsChanged, SettingsScope, SourceDeclaration, settings_changes,
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "model-log";

fn source_declaration() -> SourceDeclaration {
    SourceDeclaration {
        source: SOURCE.into(),
        user_file: "model-log.json".into(),
        project_file: None,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "enabled": { "type": "boolean", "default": false }
            }
        }),
        user_only: Vec::new(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    enabled: bool,
}

pub struct ModelLogPlugin {
    settings: Key<Settings>,
    directory: PathBuf,
    /// Serializes the read and the recorder swap, so an older document
    /// cannot be applied after a newer one.
    order: Arc<Mutex<()>>,
}

impl ModelLogPlugin {
    pub fn new(names: &Names<'_>, directory: PathBuf) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            directory,
            order: Arc::new(Mutex::new(())),
        })
    }
}

impl DynPlugin for ModelLogPlugin {
    fn name(&self) -> &str {
        "ModelLog"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            settings.register(ctx, source_declaration())?;
            if let Err(error) = apply(&settings, &self.directory, &self.order).await {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(error)));
            }
            if let Err(error) = ctx.effect(|| async {
                let _ = dim_agent_aimux::recording::set_request_recording(None);
                Ok(())
            }) {
                let _ = dim_agent_aimux::recording::set_request_recording(None);
                return Err(error);
            }
            let settings = settings.clone();
            let order = self.order.clone();
            let directory = self.directory.clone();
            ctx.listen::<SettingsChanged>()
                .name(settings_changes(SOURCE))
                .on(move |_, _| {
                    let settings = settings.clone();
                    let directory = directory.clone();
                    let order = order.clone();
                    async move {
                        if let Err(error) = apply(&settings, &directory, &order).await {
                            tracing::warn!(%error, "model request recording was not updated");
                        }
                        Ok(None)
                    }
                })?;
            Ok(Effect::Done)
        })
    }
}

/// Reads the source and starts or stops the recorder. Off when the document
/// is missing or disabled; the home root holds `model-log/recordings.jsonl`.
async fn apply(
    settings: &Settings,
    directory: &Path,
    order: &Mutex<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _order = order.lock().await;
    let cancel = CancellationToken::new();
    let snapshot = settings.read(SOURCE, SettingsScope::User, &cancel).await?;
    let document: Document = serde_json::from_value(snapshot.document)?;
    dim_agent_aimux::recording::set_request_recording(document.enabled.then_some(directory))?;
    Ok(())
}
