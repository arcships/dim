//! The `compaction` Settings source (#2235 PR 1b).
//!
//! The source is a declared Settings document (`compaction.json`, user scope
//! only) carrying the default compaction window and the per-model overrides.
//! The App Server owns the source; the model stacks read it through the port
//! this row publishes into the process's cell on every preparation
//! ([ContextCompaction](../../../docs/design/plugins/ContextCompaction.md)).
//! The document shape, the selection order, the soft cap and the shaping
//! floor are shared with the client in
//! [`dim_agent_types::compaction_settings`]; the row adds the write
//! boundary's schema and the wiring.

use crate::settings::{Settings, SettingsScope, SourceDeclaration};
use dim_agent_capabilities::compaction_settings::{
    CompactionSettingsCell, CompactionSettingsError, CompactionSettingsHost,
};
use dim_agent_runtime::Names;
use dim_agent_types::compaction_settings::CompactionSettings;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;

/// The source's name in `settings.sources` (#2235 PR 1b).
pub const COMPACTION_SOURCE: &str = "compaction";

/// The write boundary of the persisted compaction document: values are
/// `auto`, a positive token count (an integer, or a string with an optional
/// `k`/`m` suffix) or a positive percentage of at most 100.
fn compaction_value() -> serde_json::Value {
    json!({
        "oneOf": [
            {"type": "integer", "minimum": 1, "maximum": 9007199254740991u64},
            {"type": "string", "pattern": "^(?:auto|(?:100(?:\\.0+)?|[1-9][0-9]?(?:\\.[0-9]+)?|0\\.[0-9]*[1-9][0-9]*)%|[1-9][0-9]{0,14}|[1-9][0-9]{0,11}[kK]|[1-9][0-9]{0,8}[mM])$"}
        ]
    })
}

pub struct CompactionSettingsPlugin {
    settings: Key<Settings>,
    cell: CompactionSettingsCell,
}

impl CompactionSettingsPlugin {
    pub fn new(names: &Names<'_>, cell: CompactionSettingsCell) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            cell,
        })
    }
}

impl DynPlugin for CompactionSettingsPlugin {
    fn name(&self) -> &str {
        "CompactionSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            settings.register(
                ctx,
                SourceDeclaration {
                    source: COMPACTION_SOURCE.into(),
                    user_file: "compaction.json".into(),
                    project_file: None,
                    schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "default": {
                                "default": "auto",
                                "oneOf": compaction_value()["oneOf"].clone()
                            },
                            "models": {
                                "type": "object",
                                "default": {},
                                "additionalProperties": compaction_value()
                            }
                        }
                    }),
                    user_only: Vec::new(),
                },
            )?;
            self.cell
                .set(CompactionSettingsHost(Arc::new(move |cancel| {
                    let settings = settings.clone();
                    Box::pin(async move {
                        let snapshot = settings
                            .read(COMPACTION_SOURCE, SettingsScope::User, &cancel)
                            .await
                            .map_err(|error| match error {
                                crate::settings::SettingsError::Cancelled => {
                                    CompactionSettingsError::Cancelled
                                }
                                _ => CompactionSettingsError::Storage,
                            })?;
                        serde_json::from_value::<CompactionSettings>(snapshot.document)
                            .map_err(|_| CompactionSettingsError::Storage)
                    })
                })));
            let cell = self.cell.clone();
            Ok(Effect::Disposer(Box::new(move || {
                cell.clear();
                Ok(())
            })))
        })
    }
}
