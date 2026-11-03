//! The `prompt-suggestion` Settings source.
//!
//! The source is a declared Settings document (`prompt-suggestion.json`, user
//! scope only) with the feature's switch and its length limit. The App Server
//! owns the source; PromptSuggestion's Session part reads the host port at
//! each decision ([PromptSuggestion](../../docs/design/plugins/PromptSuggestion.md)).
use crate::settings::{SettingsScope, SettingsService, SourceDeclaration};
use dim_agent_capabilities::prompt_suggestion::{
    PromptSuggestionSettings, PromptSuggestionSettingsError, PromptSuggestionSettingsHost,
    PromptSuggestionSettingsSource, SOURCE,
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Storage {
    settings: Arc<SettingsService>,
}

impl PromptSuggestionSettingsSource for Storage {
    fn read(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<PromptSuggestionSettings, PromptSuggestionSettingsError>> {
        Box::pin(async move {
            let snapshot = self
                .settings
                .read_declared(SOURCE, SettingsScope::User, &cancel)
                .await
                .map_err(|error| PromptSuggestionSettingsError::Failed(error.to_string()))?;
            serde_json::from_value(snapshot.document)
                .map_err(|error| PromptSuggestionSettingsError::Failed(error.to_string()))
        })
    }
}

pub struct PromptSuggestionSettingsPlugin {
    settings: Key<SettingsService>,
}

impl PromptSuggestionSettingsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
        })
    }
}

impl DynPlugin for PromptSuggestionSettingsPlugin {
    fn name(&self) -> &str {
        "PromptSuggestionSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            settings.register_declared(
                ctx,
                SourceDeclaration {
                    source: SOURCE.into(),
                    user_file: "prompt-suggestion.json".into(),
                    project_file: None,
                    schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "enabled": { "type": "boolean", "default": false },
                            "maxChars": {
                                "type": "integer",
                                "minimum": 10,
                                "maximum": 200,
                                "default": 80
                            }
                        }
                    }),
                    user_only: Vec::new(),
                },
            )?;
            let host = PromptSuggestionSettingsHost(Arc::new(Storage { settings }));
            ctx.provide_as::<PromptSuggestionSettingsHost>(Arc::new(host))?;
            Ok(Effect::Done)
        })
    }
}
