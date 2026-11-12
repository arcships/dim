//! The process owner's shared state and the parts of the interface that read
//! it: `declare`, `eventhub.sources`, `eventhub.subscriptions`, and the
//! deletion notice (design §2.1, §3.4, A.1, A.3).
use crate::{
    error::EventHubError,
    model::{
        DeclareRequest, ListRequest, SubscribeRequest, UnsubscribeRequest, validate_declaration,
    },
    state::InstanceState,
    store::{Store, Subscription},
};
use rutis::Ctx;
use rutis_bridge::session::HostDispatch;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The state one generation of the process part owns: what it captured when
/// it applied. The catalog, the locks and the budget belong to the install
/// (design §2.1, §2.4, §2.5); the store is this generation's reading of the
/// subscription file.
pub(crate) struct Shared {
    pub(crate) ctx: Ctx,
    pub(crate) activity: Arc<dim_app_server::ProcessActivity>,
    pub(crate) sessions: Arc<dyn HostDispatch>,
    pub(crate) state: Arc<InstanceState>,
    pub(crate) store: Arc<Store>,
    /// The generation's cancellation, passed to the files a publish stores.
    pub(crate) stop: CancellationToken,
}

impl Shared {
    pub(crate) async fn declare(
        &self,
        ctx: &Ctx,
        request: DeclareRequest,
    ) -> Result<(), EventHubError> {
        validate_declaration(&request)?;
        self.state.catalog().declare(ctx, &request)
    }

    pub(crate) fn sources(&self) -> Value {
        json!({ "sources": self.state.catalog().report() })
    }

    pub(crate) fn subscriptions(&self, request: ListRequest) -> Value {
        let subscriptions: Vec<Subscription> = self
            .store
            .read()
            .into_iter()
            .filter(|subscription| {
                request
                    .session_id
                    .as_deref()
                    .is_none_or(|session_id| subscription.target.session_id == session_id)
            })
            .collect();
        json!({ "subscriptions": subscriptions })
    }

    /// Removes every subscription whose target is `session_id` in one
    /// transaction, under that Session's lock (design §3.3, §3.4): a
    /// `subscribe` that wrote its guide before the delete stores its record
    /// first, and this runs after it. A failed write leaves them; any error
    /// of the reconciliation keeps them too (design §3.4).
    pub(crate) async fn remove_session(&self, session_id: &str) {
        let _session = self.state.session_lock(session_id).await;
        let target = session_id.to_owned();
        let result = self
            .store
            .change(move |document| {
                let before = document.subscriptions.len();
                document
                    .subscriptions
                    .retain(|subscription| subscription.target.session_id != target);
                Ok((document.subscriptions.len() != before, ()))
            })
            .await;
        if let Err(error) = result {
            tracing::warn!(%error, session_id, "EventHub could not remove a deleted Session's subscriptions");
        }
    }

    /// The `agent/session-deleted` handler (design §3.4).
    pub(crate) async fn session_deleted(&self, session_id: &str) {
        self.remove_session(session_id).await;
    }

    pub(crate) async fn subscribe(
        &self,
        origin: crate::model::Origin,
        request: SubscribeRequest,
    ) -> Result<Value, EventHubError> {
        let _activity = self.activity.take_hold().ok_or_else(|| {
            EventHubError::new("service_reloading", "Process admission is paused")
        })?;
        crate::subscribe::subscribe(self, origin, request).await
    }

    pub(crate) async fn unsubscribe(
        &self,
        request: UnsubscribeRequest,
    ) -> Result<Value, EventHubError> {
        let _activity = self.activity.take_hold().ok_or_else(|| {
            EventHubError::new("service_reloading", "Process admission is paused")
        })?;
        crate::subscribe::unsubscribe(self, &request.subscription_id).await
    }
}
