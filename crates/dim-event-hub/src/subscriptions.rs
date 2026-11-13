//! The `eventSubscriptions` process service (design §6, Appendix F): the
//! subscription core for the `events` tool.
//!
//! Its operations are data ([`SubscriptionCall`], [`UnsubscribeCall`],
//! [`ListCall`]) that name the Session they manage, as every data face's ids
//! are values and not authorization ([service faces §1]). `eventHub` itself
//! stays producer-only: this service only manages subscriptions, and it
//! answers what the management core answers, adding no rule of its own.
use crate::{
    error::EventHubError,
    hub::Shared,
    model::{ListRequest, Origin, SubscribeRequest, SubscribeTarget, UnsubscribeRequest},
    subscribe::subscription_unknown,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

/// `eventSubscriptions.subscribe`: the subscription to create for
/// `session_id` with origin `tool` (design A.3, §3.3).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionCall {
    pub session_id: String,
    pub types: Vec<String>,
    #[serde(default, rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub mode: Option<dim_agent_types::InputMode>,
    /// Defaults to `main`, as [`SubscribeTarget`] states it.
    #[serde(default)]
    pub branch_id: Option<String>,
}

/// `eventSubscriptions.unsubscribe`: the subscription to remove from
/// `session_id`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnsubscribeCall {
    pub session_id: String,
    pub subscription_id: String,
}

/// `eventSubscriptions.list`: the Session whose subscriptions to answer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListCall {
    pub session_id: String,
}

/// The subscription service: `names::name_of::<EventSubscriptions>()` is
/// `eventSubscriptions` (design Appendix F). The Session part's `events` tool
/// reads it with `Key<EventSubscriptions>`.
pub struct EventSubscriptions {
    shared: Arc<Shared>,
}

impl EventSubscriptions {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The declared sources and their types, what `eventhub.sources` answers
    /// (design A.3).
    pub fn sources(&self) -> Value {
        self.shared.sources()
    }

    /// Creates one subscription for `call.session_id`'s `call.branch_id` with
    /// origin `tool`; the lock, guide write and storage rules are
    /// `eventhub.subscribe`'s own (design A.3, §3.3).
    pub async fn subscribe(&self, call: SubscriptionCall) -> Result<Value, EventHubError> {
        self.shared
            .subscribe(
                Origin::Tool,
                SubscribeRequest {
                    types: call.types,
                    matches: call.matches,
                    target: SubscribeTarget {
                        session_id: call.session_id,
                        branch_id: call.branch_id,
                    },
                    mode: call.mode,
                },
            )
            .await
    }

    /// Removes one of `call.session_id`'s subscriptions. A subscription of
    /// another Session answers `subscription_unknown` in the same words as an
    /// unknown id, so the caller learns nothing about it (design §6). A
    /// subscription's target never changes, so the check needs no lock of its
    /// own.
    pub async fn unsubscribe(&self, call: UnsubscribeCall) -> Result<Value, EventHubError> {
        let belongs = self.shared.store.read().into_iter().any(|subscription| {
            subscription.id == call.subscription_id
                && subscription.target.session_id == call.session_id
        });
        if !belongs {
            return Err(subscription_unknown(&call.subscription_id));
        }
        self.shared
            .unsubscribe(UnsubscribeRequest {
                subscription_id: call.subscription_id,
            })
            .await
    }

    /// The subscriptions of `call.session_id`, what `eventhub.subscriptions
    /// {sessionId}` answers (design A.3).
    pub fn list(&self, call: ListCall) -> Value {
        self.shared.subscriptions(ListRequest {
            session_id: Some(call.session_id),
        })
    }
}
