//! Subscribe and unsubscribe (design §3.3, A.3), one core for the methods and
//! the `events` tool: check, write the source's guide input, then store; and
//! on removal, remove the record first and then retire the guide of a source
//! no subscription on that branch names any more.
use crate::{
    error::{EventHubError, TRANSPORT, sessions_code},
    hub::Shared,
    model::{Origin, SubscribeRequest, Target, source_of, validate_subscribe},
    publish::sessions,
    store::Subscription,
    text::{digest, stamp},
};
use chrono::Utc;
use dim_agent_types::markup::escaped;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// At most this many subscriptions per Session (design §2.2).
const PER_SESSION_LIMIT: usize = 32;
/// At most this many subscriptions in total (design §2.2).
const TOTAL_LIMIT: usize = 1024;

/// Creates one subscription for the target branch, in the order 3.3 states.
pub(crate) async fn subscribe(
    shared: &Shared,
    origin: Origin,
    request: SubscribeRequest,
) -> Result<Value, EventHubError> {
    let crate::model::CheckedSubscribe {
        types,
        matches,
        target,
    } = validate_subscribe(&request)?;
    // One lock per target Session, from the first check to the last guide
    // write or retire (design §3.3): an unsubscribe or a delete of the same
    // Session cannot retire a guide this subscribe wrote before its record
    // was stored.
    let _session = shared.state.session_lock(&target.session_id).await;
    for pattern in &types {
        match source_of(pattern) {
            Some(source) if pattern.ends_with(".*") => {
                if !shared.state.catalog().declares_source(source) {
                    return Err(EventHubError::unknown_type(format!(
                        "no declaration holds source {source}"
                    )));
                }
            }
            Some(_) => {
                if !shared.state.catalog().declares_type(pattern) {
                    return Err(EventHubError::unknown_type(format!(
                        "no declaration holds type {pattern}"
                    )));
                }
            }
            None => {
                return Err(EventHubError::invalid_params(format!(
                    "type {pattern} is not a type or <source>.*"
                )));
            }
        }
    }
    check_limits(shared, &target)?;
    let subscription = Subscription {
        id: format!("sub_{}", uuid::Uuid::new_v4()),
        types,
        matches,
        target: target.clone(),
        mode: request.mode,
        origin,
        created_at: stamp(Utc::now()),
        last_failure: None,
    };
    let sources = named_sources(&subscription);
    // Every guide is read before the first is written: a source that is no
    // longer declared refuses the subscribe here, with nothing written
    // (design §3.3). The reads are synchronous, so no unload can slip
    // between the creation check and them.
    let mut guides = Vec::new();
    for source in &sources {
        let (guide, digest) = source_guide(shared, source)?;
        guides.push((source.clone(), guide, digest));
    }
    let mut written = BTreeMap::new();
    for (source, guide, digest) in guides {
        let source = &source;
        match write_guide(shared, &target, &subscription.id, source, &guide).await {
            Ok(()) => {
                written.insert(source.clone(), digest);
            }
            // The input was committed although its publication is unconfirmed:
            // the subscription is stored and the guide is one of its sources
            // (design A.3).
            Err(error) if error.code == "publication_unknown" => {
                written.insert(source.clone(), digest);
            }
            Err(error) => {
                // Refused after earlier guides were written: retire those by
                // the `storage` rule before answering (design §3.3). A
                // transport failure leaves this guide in doubt, so it is
                // retired too; a retire of a guide never written is an empty
                // guide (Appendix C).
                let mut retiring: Vec<&String> = written.keys().collect();
                if error.code == TRANSPORT {
                    retiring.push(source);
                }
                retire_unguarded(shared, &target, &subscription.id, retiring.into_iter()).await;
                return Err(error);
            }
        }
    }
    let created = subscription.clone();
    let records: Vec<(String, String)> = written
        .iter()
        .map(|(source, digest)| (source.clone(), digest.clone()))
        .collect();
    let outcome = shared
        .store
        .change(move |document| {
            check_limits_in(&document.subscriptions, &created.target)?;
            for (source, digest) in &records {
                document.set_guide(&created.target, source, digest);
            }
            document.subscriptions.push(created);
            Ok((true, ()))
        })
        .await;
    match outcome {
        Ok(()) => Ok(json!({ "subscriptionId": subscription.id })),
        Err(error) => {
            // Any refusal after the guide inputs were written retires them,
            // for each source no stored subscription on that branch names
            // (design §3.3, Appendix C): the re-check's `limit_reached` as
            // much as a failed write.
            retire_unguarded(shared, &target, &subscription.id, written.keys()).await;
            Err(error)
        }
    }
}

/// Removes a subscription and retires the guide of each source no remaining
/// subscription on that branch names (design §3.3).
pub(crate) async fn unsubscribe(
    shared: &Shared,
    subscription_id: &str,
) -> Result<Value, EventHubError> {
    // The target is read first, so the Session's lock covers the whole change
    // (design §3.3).
    let Some(target) = shared
        .store
        .read()
        .into_iter()
        .find(|subscription| subscription.id == subscription_id)
        .map(|subscription| subscription.target)
    else {
        return Err(subscription_unknown(subscription_id));
    };
    let _session = shared.state.session_lock(&target.session_id).await;
    let id = subscription_id.to_owned();
    let removed = shared
        .store
        .change(move |document| {
            let Some(index) = document
                .subscriptions
                .iter()
                .position(|subscription| subscription.id == id)
            else {
                return Err(subscription_unknown(&id));
            };
            Ok((true, document.subscriptions.remove(index)))
        })
        .await?;
    for source in named_sources(&removed) {
        if !still_named(shared, &removed.target, &source) {
            retire_guide(shared, &removed.target, &removed.id, &source).await;
        }
    }
    Ok(json!({}))
}

pub(crate) fn subscription_unknown(id: &str) -> EventHubError {
    EventHubError::new("subscription_unknown", format!("no subscription {id}"))
}

/// The sources a subscription's types name (design §3.3).
fn named_sources(subscription: &Subscription) -> BTreeSet<String> {
    subscription
        .types
        .iter()
        .filter_map(|pattern| source_of(pattern).map(str::to_owned))
        .collect()
}

/// Whether any stored subscription on `target`'s branch names `source`.
fn still_named(shared: &Shared, target: &Target, source: &str) -> bool {
    shared.store.document().names_source(target, source)
}

/// The creation limits, checked against the in-memory set before the guide
/// input is written and again under the store's lock (design §2.2,
/// Appendix B).
fn check_limits(shared: &Shared, target: &Target) -> Result<(), EventHubError> {
    check_limits_in(&shared.store.read(), target)
}

fn check_limits_in(subscriptions: &[Subscription], target: &Target) -> Result<(), EventHubError> {
    let per_session = subscriptions
        .iter()
        .filter(|subscription| subscription.target.session_id == target.session_id)
        .count();
    if per_session >= PER_SESSION_LIMIT {
        return Err(EventHubError::new(
            "limit_reached",
            format!(
                "Session {} has {PER_SESSION_LIMIT} subscriptions",
                target.session_id
            ),
        ));
    }
    if subscriptions.len() >= TOTAL_LIMIT {
        return Err(EventHubError::new(
            "limit_reached",
            format!("the host has {TOTAL_LIMIT} subscriptions"),
        ));
    }
    Ok(())
}

/// The current declaration's guide of `source` and its digest, the two things
/// the subscription file records (design §2.2, §3.3).
fn source_guide(shared: &Shared, source: &str) -> Result<(String, String), EventHubError> {
    let guide = shared
        .state
        .catalog()
        .source(source)
        .map(|declaration| declaration.guide)
        .ok_or_else(|| {
            EventHubError::unknown_type(format!("no declaration holds source {source}"))
        })?;
    let digest = digest(&guide);
    Ok((guide, digest))
}

/// Writes one source's guide input to the target branch (design §3.3): an
/// `append` input whose single keyed block carries the source's guide.
async fn write_guide(
    shared: &Shared,
    target: &Target,
    subscription_id: &str,
    source: &str,
    guide: &str,
) -> Result<(), EventHubError> {
    write_live_guide(
        shared.sessions.as_ref(),
        target,
        guide_submission(subscription_id, source),
        source,
        guide,
    )
    .await
}

/// One keyed `append` that writes `source`'s guide on a branch, under the
/// caller's submission id (design §3.3). Subscribe uses
/// `eventhub-guide:<subscriptionId>:<source>` and a delivery's refresh
/// `eventhub-guide-refresh:<source>:<uuid>`.
pub(crate) async fn write_live_guide(
    service: &dyn rutis_bridge::session::HostDispatch,
    target: &Target,
    submission_id: String,
    source: &str,
    guide: &str,
) -> Result<(), EventHubError> {
    let write = json!({
        "sessionId": target.session_id,
        "branchId": target.branch_id,
        "mode": "append",
        "submissionId": submission_id,
        "rawBlocks": [{
            "text": guide_block(source, Some(guide)),
            "key": { "use": "live", "name": guide_key(source) },
        }],
        "content": [],
    });
    match sessions(service, "input", write).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(failure)) => Err(EventHubError::new(
            sessions_code(&failure.code),
            format!("the guide input was refused: {}", failure.message),
        )),
        Err(message) => Err(EventHubError::transport(format!(
            "the guide input was not delivered: {message}"
        ))),
    }
}

/// Retires the guide of `source` on the target branch after a last removal
/// (design §3.3). A retire that cannot be written is skipped.
async fn retire_guide(shared: &Shared, target: &Target, subscription_id: &str, source: &str) {
    let write = json!({
        "sessionId": target.session_id,
        "branchId": target.branch_id,
        "mode": "append",
        "submissionId": retire_submission(subscription_id, source),
        "rawBlocks": [{
            "text": guide_block(source, None),
            "key": { "use": "retire", "name": guide_key(source) },
        }],
        "content": [],
    });
    let _ = sessions(shared.sessions.as_ref(), "input", write).await;
}

/// Retires the guide of each source a failed creation named that no stored
/// subscription on that branch names (design §3.3, Appendix C).
async fn retire_unguarded<'a>(
    shared: &Shared,
    target: &Target,
    subscription_id: &str,
    sources: impl Iterator<Item = &'a String>,
) {
    for source in sources {
        if !still_named(shared, target, source) {
            retire_guide(shared, target, subscription_id, source).await;
        }
    }
}

/// `<dim-event-guide source="im">guide</dim-event-guide>`, the guide escaped,
/// or the empty element that retires the key (design §3.3, A.6).
pub(crate) fn guide_block(source: &str, guide: Option<&str>) -> String {
    format!(
        "<dim-event-guide source=\"{}\">{}</dim-event-guide>",
        escaped(source),
        escaped(guide.unwrap_or(""))
    )
}

/// The key the guide block lives under (design §3.3).
pub(crate) fn guide_key(source: &str) -> String {
    format!("eventhub:guide:{source}")
}

/// The submission id of a subscribe's guide write (design §3.3).
fn guide_submission(subscription_id: &str, source: &str) -> String {
    format!("eventhub-guide:{subscription_id}:{source}")
}

/// The submission id of a guide retire (design §3.3).
fn retire_submission(subscription_id: &str, source: &str) -> String {
    format!("eventhub-guide-retire:{subscription_id}:{source}")
}

/// The submission id of a delivery's guide refresh (design §3.3).
pub(crate) fn refresh_submission(source: &str) -> String {
    format!("eventhub-guide-refresh:{source}:{}", uuid::Uuid::new_v4())
}
