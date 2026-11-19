//! The `provider` settings source: the User-Agent dim puts on its model
//! requests, when the user replaces the built-in one.
//!
//! Provider owns its process configuration
//! (`docs/design/appserver/provider-management.md` §2), so this source belongs
//! here. It holds no Provider record: a record's own `headers` stay the
//! record's, and a User-Agent there still wins for that Provider alone.
use crate::settings::{
    Settings, SettingsChanged, SettingsScope, SourceDeclaration, settings_changes,
};
use dim_agent_aimux::ModelIdentity;
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The source name, as `settings.update` names it.
const SOURCE: &str = "provider";
/// The document file under the settings root.
const USER_FILE: &str = "provider.json";

fn source_declaration() -> SourceDeclaration {
    SourceDeclaration {
        source: SOURCE.into(),
        user_file: USER_FILE.into(),
        project_file: None,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "userAgent": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 512,
                    "pattern": "^[ -~]+$"
                }
            }
        }),
        user_only: vec!["/userAgent".into()],
    }
}

/// The document the source stores. `userAgent` replaces dim's whole User-Agent
/// string; `DIM_USER_AGENT` wins over it when that is set.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    #[serde(default)]
    user_agent: Option<String>,
}

pub struct ProviderSettingsPlugin {
    settings: Key<Settings>,
    /// The identity of this App Server's Aimux part: the state this row
    /// writes, and the one its model requests read.
    identity: ModelIdentity,
    /// Serializes the read and the publish, so an older document cannot be
    /// published after a newer one.
    order: Arc<Mutex<()>>,
}

impl ProviderSettingsPlugin {
    pub fn new(names: &Names<'_>, identity: ModelIdentity) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            identity,
            order: Arc::new(Mutex::new(())),
        })
    }
}

impl DynPlugin for ProviderSettingsPlugin {
    fn name(&self) -> &str {
        "ProviderSettings"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let identity = self.identity.clone();
            settings.register(ctx, source_declaration())?;
            if let Err(error) = apply(&settings, &identity, &self.order).await {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(error)));
            }
            if let Err(error) = ctx.effect(|| {
                let identity = identity.clone();
                async move {
                    identity.set_configured(None);
                    Ok(())
                }
            }) {
                identity.set_configured(None);
                return Err(error);
            }
            let settings = settings.clone();
            let identity = identity.clone();
            let order = self.order.clone();
            ctx.listen::<SettingsChanged>()
                .name(settings_changes(SOURCE))
                .on(move |_, _| {
                    let settings = settings.clone();
                    let identity = identity.clone();
                    let order = order.clone();
                    async move {
                        if let Err(error) = apply(&settings, &identity, &order).await {
                            tracing::warn!(%error, "the model request User-Agent was not updated");
                        }
                        Ok(None)
                    }
                })?;
            Ok(Effect::Done)
        })
    }
}

/// Publishes the document's value on this App Server's identity.
///
/// A missing document publishes no configured override, so dim keeps its
/// built-in User-Agent. An unreadable or invalid one fails the row's mount:
/// reading the source propagates the error, as the model-log source's does.
async fn apply(
    settings: &Settings,
    identity: &ModelIdentity,
    order: &Mutex<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _order = order.lock().await;
    let cancel = CancellationToken::new();
    let snapshot = settings.read(SOURCE, SettingsScope::User, &cancel).await?;
    let document: Document = serde_json::from_value(snapshot.document)?;
    identity.set_configured(document.user_agent);
    Ok(())
}
