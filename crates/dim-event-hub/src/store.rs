//! The subscription file `eventhub/subscriptions.json` (design §2.2, A.5):
//! one inner v1 JSON document inside the host's `{version, text}` envelope,
//! which [`PluginFileTransactions`] reads and writes. The part reads the file
//! once when it applies; the subscriptions and the guide records live in the
//! install's state, and every change writes the file through the running
//! generation's handle under the install's change lock and then replaces that
//! copy.
use crate::{
    error::EventHubError,
    model::{Origin, Target, source_of},
    state::InstanceState,
};
use dim_agent_types::InputMode;
use dim_app_server::plugin_files::{PluginFileError, PluginFileTransactions};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

pub const SUBSCRIPTIONS_FILE: &str = "eventhub/subscriptions.json";
const FORMAT_VERSION: u32 = 1;

/// The newest delivery outcome that was not delivered (design §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastFailure {
    pub code: String,
    pub at: String,
}

/// One durable subscription (design §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Subscription {
    pub id: String,
    pub types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<InputMode>,
    pub origin: Origin,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<LastFailure>,
}

impl Subscription {
    /// The subscription's effective mode: its own, else the type's declared
    /// mode (design §3.1).
    pub(crate) fn effective_mode(&self, declared: InputMode) -> InputMode {
        self.mode.unwrap_or(declared)
    }

    /// Whether the subscription selects an event of `source` and `event_type`
    /// with `attributes` (design §2.2).
    pub(crate) fn selects(
        &self,
        source: &str,
        event_type: &str,
        attributes: &BTreeMap<String, String>,
    ) -> bool {
        let type_matches = self
            .types
            .iter()
            .any(|pattern| crate::model::pattern_matches(pattern, source, event_type));
        type_matches
            && self.matches.as_ref().is_none_or(|matchers| {
                matchers
                    .iter()
                    .all(|(name, value)| attributes.get(name) == Some(value))
            })
    }

    /// Whether any of the subscription's types names `source` (design §3.3).
    pub(crate) fn names_source(&self, source: &str) -> bool {
        self.types
            .iter()
            .filter_map(|pattern| source_of(pattern))
            .any(|named| named == source)
    }
}

/// The guide last written on one branch for one source: the digest a
/// delivery compares against the current declaration (design §2.2, A.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuideRecord {
    pub target: Target,
    pub source: String,
    pub digest: String,
}

/// The file's contents as the part holds them (design §2.2).
#[derive(Debug, Clone, Default)]
pub(crate) struct Document {
    pub(crate) subscriptions: Vec<Subscription>,
    pub(crate) guides: Vec<GuideRecord>,
}

impl Document {
    /// The record of `source` on `target`'s branch, if one is stored.
    pub(crate) fn guide(&self, target: &Target, source: &str) -> Option<&GuideRecord> {
        self.guides
            .iter()
            .find(|record| record.target == *target && record.source == source)
    }

    /// Sets `source`'s record on `target`'s branch, replacing the branch's.
    /// Answers whether the file changed.
    pub(crate) fn set_guide(&mut self, target: &Target, source: &str, digest: &str) -> bool {
        match self
            .guides
            .iter_mut()
            .find(|record| record.target == *target && record.source == source)
        {
            Some(record) => {
                if record.digest == digest {
                    return false;
                }
                record.digest = digest.to_owned();
                true
            }
            None => {
                self.guides.push(GuideRecord {
                    target: target.clone(),
                    source: source.to_owned(),
                    digest: digest.to_owned(),
                });
                true
            }
        }
    }

    /// Whether any stored subscription on `target`'s branch names `source`
    /// (design §3.3).
    pub(crate) fn names_source(&self, target: &Target, source: &str) -> bool {
        self.subscriptions
            .iter()
            .any(|subscription| subscription.target == *target && subscription.names_source(source))
    }

    /// Drops every guide record no stored subscription backs, so the file
    /// holds one record per subscribed branch and source (design A.5).
    fn prune_guides(&mut self) -> bool {
        let before = self.guides.len();
        let subscriptions = &self.subscriptions;
        self.guides.retain(|record| {
            subscriptions.iter().any(|subscription| {
                subscription.target == record.target && subscription.names_source(&record.source)
            })
        });
        self.guides.len() != before
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionFile {
    version: u32,
    subscriptions: Vec<Subscription>,
    #[serde(default)]
    guides: Vec<GuideRecord>,
}

/// Decodes the inner text of the file; `None` is a missing file, which holds
/// no subscriptions and no guide records. An unreadable or inconsistent
/// document fails the part and leaves its bytes unchanged (design A.5).
fn decode(text: Option<&str>) -> Result<Document, String> {
    let Some(text) = text else {
        return Ok(Document::default());
    };
    let file: SubscriptionFile =
        serde_json::from_str(text).map_err(|error| format!("subscriptions.json: {error}"))?;
    if file.version != FORMAT_VERSION {
        return Err(format!(
            "subscriptions.json has unknown version {}",
            file.version
        ));
    }
    let mut seen = HashSet::new();
    for subscription in &file.subscriptions {
        if subscription.id.is_empty() || !seen.insert(subscription.id.as_str()) {
            return Err(format!(
                "subscriptions.json repeats or lacks an id: {}",
                subscription.id
            ));
        }
        if subscription.target.session_id.is_empty() || subscription.target.branch_id.is_empty() {
            return Err(format!(
                "subscription {} has a target without a Session",
                subscription.id
            ));
        }
    }
    let mut recorded = HashSet::new();
    for record in &file.guides {
        if record.source.is_empty() || record.digest.is_empty() {
            return Err("subscriptions.json has a guide record without a source".into());
        }
        if record.target.session_id.is_empty() || record.target.branch_id.is_empty() {
            return Err(format!(
                "guide record of {} has a target without a Session",
                record.source
            ));
        }
        if !recorded.insert((
            &record.target.session_id,
            &record.target.branch_id,
            &record.source,
        )) {
            return Err(format!(
                "subscriptions.json has two guide records for {}/{} of {}",
                record.target.session_id, record.target.branch_id, record.source
            ));
        }
    }
    Ok(Document {
        subscriptions: file.subscriptions,
        guides: file.guides,
    })
}

fn encode(document: &Document) -> String {
    serde_json::to_string(&SubscriptionFile {
        version: FORMAT_VERSION,
        subscriptions: document.subscriptions.clone(),
        guides: document.guides.clone(),
    })
    .expect("subscription records contain only serializable fields")
}

/// This generation's file handle and the install's subscription view. The
/// view lives in [`InstanceState`](crate::state::InstanceState), so a change
/// a detached publish still runs after its generation ended lands where the
/// next generation reads it (design §2.1, A.5).
pub(crate) struct Store {
    files: Arc<PluginFileTransactions>,
    state: Arc<InstanceState>,
}

impl Store {
    pub(crate) fn new(files: Arc<PluginFileTransactions>, state: Arc<InstanceState>) -> Self {
        Self { files, state }
    }

    /// Reads the file once, when the part applies, and replaces the install's
    /// view with what it holds. The change lock holds across both, so an old
    /// generation's change cannot interleave (design A.5). A missing file is
    /// no subscriptions and no guide records; a corrupt one is an error, and
    /// its bytes stay unchanged.
    pub(crate) async fn reload(&self) -> Result<(), String> {
        let _guard = self.state.change_lock().await;
        let document = self
            .files
            .transact(SUBSCRIPTIONS_FILE, |current| (None, decode(current)))
            .await
            .map_err(|error| error.to_string())??;
        self.state.replace_subscriptions(document);
        Ok(())
    }

    /// The subscriptions as they are now: what a publish matches against.
    pub(crate) fn read(&self) -> Vec<Subscription> {
        self.state.subscriptions().subscriptions
    }

    /// The whole document as it is now: the subscriptions and the guide
    /// records a publish compares (design §2.2).
    pub(crate) fn document(&self) -> Document {
        self.state.subscriptions()
    }

    /// Applies `change` under the store's lock. `change` returns whether it
    /// modified the document, the value it answers with, and errors as its
    /// caller answers them. A modified document drops every guide record no
    /// stored subscription backs (design A.5). On success the in-memory copy
    /// is replaced with what the file now holds; a failed write leaves both
    /// the file and the copy as they were.
    pub(crate) async fn change<R>(
        &self,
        change: impl FnOnce(&mut Document) -> Result<(bool, R), EventHubError> + Send + 'static,
    ) -> Result<R, EventHubError>
    where
        R: Send + 'static,
    {
        let _guard = self.state.change_lock().await;
        let outcome = self
            .files
            .transact(SUBSCRIPTIONS_FILE, move |current| {
                let mut document = match decode(current) {
                    Ok(document) => document,
                    Err(error) => {
                        return (None, Err(EventHubError::storage(error)));
                    }
                };
                match change(&mut document) {
                    Ok((true, value)) => {
                        document.prune_guides();
                        (Some(encode(&document)), Ok((Some(document), value)))
                    }
                    Ok((false, value)) => (None, Ok((None, value))),
                    Err(error) => (None, Err(error)),
                }
            })
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return Err(storage_error(error)),
        };
        let (document, value) = outcome?;
        if let Some(document) = document {
            self.state.replace_subscriptions(document);
        }
        Ok(value)
    }
}

/// A failed file transaction is the design's `storage`: the change did not
/// happen and the in-memory copy is unchanged (design A.5, Appendix C).
fn storage_error(error: PluginFileError) -> EventHubError {
    EventHubError::storage(error.to_string())
}
