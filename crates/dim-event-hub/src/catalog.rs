//! The catalog of declared sources and their types (design §2.1). It is
//! process memory, and each declaration lives exactly as long as the row
//! that declared it: it is withdrawn by an effect of the caller's context.
use crate::{error::EventHubError, model::DeclareRequest};
use dim_agent_types::InputMode;
use rutis::Ctx;
use rutis::host::CtxHost;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// One declared type as `eventhub.sources` reports it (design A.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredType {
    pub description: String,
    pub mode: InputMode,
}

/// One source's declaration, as publishers and subscribers read it.
#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    /// The fiber that declared it; another row's source is not this row's.
    pub(crate) owner: rutis::host::InstanceId,
    pub(crate) guide: String,
    pub(crate) types: BTreeMap<String, DeclaredType>,
    /// Distinguishes one declaration of a source from the next, so a
    /// withdraw effect ends only the declaration it installed.
    token: u64,
}

/// The catalog, in memory only (design §2.1).
#[derive(Default)]
pub(crate) struct Catalog {
    state: Mutex<BTreeMap<String, Declaration>>,
    next: AtomicU64,
}

impl Catalog {
    /// Declares `request` for the row that owns `ctx` (design A.1): another
    /// row's source is `duplicate_source`, and the same row declaring its
    /// source again replaces its declaration in place, keeping its token and
    /// adding no effect, so a producer may declare on every `eventhub/ready`.
    /// A new declaration ends with its row, through an effect on its context.
    pub(crate) fn declare(
        self: &Arc<Self>,
        ctx: &Ctx,
        request: &DeclareRequest,
    ) -> Result<(), EventHubError> {
        let owner = ctx.instance();
        let types = request
            .types
            .iter()
            .map(|declared| {
                (
                    declared.r#type.clone(),
                    DeclaredType {
                        description: declared.description.clone(),
                        mode: declared.mode,
                    },
                )
            })
            .collect();
        let token;
        {
            let mut state = self.state.lock().expect("the catalog");
            let existing = state.get(&request.source).cloned();
            if let Some(existing) = existing {
                if existing.owner != owner {
                    return Err(EventHubError::new(
                        "duplicate_source",
                        format!("another row holds source {}", request.source),
                    ));
                }
                token = existing.token;
                state.insert(
                    request.source.clone(),
                    Declaration {
                        owner,
                        guide: request.guide.clone(),
                        types,
                        token,
                    },
                );
                return Ok(());
            }
            token = self.next.fetch_add(1, Ordering::Relaxed);
            state.insert(
                request.source.clone(),
                Declaration {
                    owner,
                    guide: request.guide.clone(),
                    types,
                    token,
                },
            );
        }
        let source = request.source.clone();
        let catalog = self.clone();
        let registered = ctx.effect(move || async move {
            catalog.withdraw(&source, token);
            Ok(())
        });
        match registered {
            Ok(_) => Ok(()),
            Err(error) => {
                // The row is unloading: the declaration cannot outlive the
                // call, so it is not installed.
                self.withdraw(&request.source, token);
                Err(EventHubError::new(
                    "invalid_declaration",
                    format!("the calling row is unloading: {error}"),
                ))
            }
        }
    }

    /// Reads `source` when `owner` holds it (design A.2's `unknown_source`).
    pub(crate) fn held_by(
        &self,
        source: &str,
        owner: rutis::host::InstanceId,
    ) -> Option<Declaration> {
        self.state
            .lock()
            .expect("the catalog")
            .get(source)
            .filter(|declaration| declaration.owner == owner)
            .cloned()
    }

    /// The declaration of `source`, whoever holds it.
    pub(crate) fn source(&self, source: &str) -> Option<Declaration> {
        self.state.lock().expect("the catalog").get(source).cloned()
    }

    /// Whether any declaration holds `declared`, for a subscribe's
    /// `unknown_type` check.
    pub(crate) fn declares_type(&self, declared: &str) -> bool {
        let Some((source, _)) = declared.split_once('.') else {
            return false;
        };
        self.state
            .lock()
            .expect("the catalog")
            .get(source)
            .is_some_and(|declaration| declaration.types.contains_key(declared))
    }

    /// Whether any declaration holds `source`, for a `<source>.*` subscribe.
    pub(crate) fn declares_source(&self, source: &str) -> bool {
        self.state.lock().expect("the catalog").contains_key(source)
    }

    /// The `eventhub.sources` reply (design A.3), in source name order.
    pub(crate) fn report(&self) -> Vec<SourceReport> {
        self.state
            .lock()
            .expect("the catalog")
            .iter()
            .map(|(source, declaration)| SourceReport {
                source: source.clone(),
                guide: declaration.guide.clone(),
                types: declaration
                    .types
                    .iter()
                    .map(|(r#type, declared)| TypeReport {
                        r#type: r#type.clone(),
                        description: declared.description.clone(),
                        mode: declared.mode,
                    })
                    .collect(),
            })
            .collect()
    }

    fn withdraw(&self, source: &str, token: u64) {
        let mut state = self.state.lock().expect("the catalog");
        if state
            .get(source)
            .is_some_and(|declaration| declaration.token == token)
        {
            state.remove(source);
        }
    }
}

/// A source as `eventhub.sources` reports it (design A.3).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReport {
    pub source: String,
    pub guide: String,
    pub types: Vec<TypeReport>,
}

/// One type as `eventhub.sources` reports it (design A.3).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeReport {
    pub r#type: String,
    pub description: String,
    pub mode: InputMode,
}
