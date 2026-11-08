use crate::{context_policy::check_cancel, host::*};
use dim_agent_types::{FailureChain, ModelFailureKind, ModelRequest};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

/// The most attempts one request or one failure chain makes, the first
/// included: eight attempts ride out an outage of tens of seconds.
pub const MAX_ATTEMPTS: u8 = 8;
/// A malformed response is retried once: a repeat of the same unusable
/// output is likely deterministic.
const MALFORMED_RETRIES: u8 = 1;
/// The first retry waits 1 s, so it does not land inside the same outage.
const BASE_DELAY: Duration = Duration::from_secs(1);
/// A wait is capped at 30 s, so the last attempts are not minutes apart.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The wait budget of one request, or of the failure chain it continues: it
/// sleeps at most five minutes in all, and advice that does not fit in what
/// is left stops the request at once.
pub const WAIT_BUDGET: Duration = Duration::from_secs(300);
/// The terminal message of a continuation whose failure chain has spent its
/// retry budget.
const SPENT_FAILURE_CHAIN: &str = "the failure chain's retry budget is spent";

/// Reports the wait a retry is about to sleep through. A consumer without a
/// display implements it for its state as a no-op; `()` already does.
pub trait RetryObserver {
    /// The attempt `retry.attempt` starts at `retry.retry_at_ms` after the
    /// failure `retry.kind` with `retry.reason`; `None` clears the retry when
    /// the attempt starts or the wait is cancelled.
    fn waiting(&mut self, _retry: Option<RetryWaiting<'_>>) {}
}

/// One retry about to sleep, as the progress display shows it.
pub struct RetryWaiting<'a> {
    /// The attempt about to run, at least 2.
    pub attempt: u8,
    /// The total attempt cap.
    pub max_attempts: u8,
    /// Unix milliseconds at which the attempt starts.
    pub retry_at_ms: u64,
    pub kind: ModelFailureKind,
    /// The failure's bounded reason.
    pub reason: Option<&'a str>,
}

impl RetryObserver for () {}

/// Reads one attempt's stream; see [`invoke`].
pub type Consume<S, T> = for<'a> fn(
    ModelStream,
    &'a mut S,
    CancellationToken,
    &'a mut bool,
) -> futures_util::future::BoxFuture<'a, Result<T, HostError>>;

/// Runs one request over its resolved connection under the product's failure
/// policy: transient failures back off, a rejected credential is recovered
/// once, and a terminal rejection is settled with its credential owner. A
/// request that continues a `chain` starts from the attempts and the wait it
/// spent, so the chain keeps one budget. `consume` reads one attempt's stream
/// into `state` and clears `replay_safe` once output that cannot be replayed
/// was handed on.
pub async fn invoke<S: Send + RetryObserver, T>(
    host: &ModelHost,
    request: &Arc<ModelRequest>,
    chain: Option<&FailureChain>,
    resolved: ResolvedModelConnection,
    cancel: &CancellationToken,
    state: &mut S,
    consume: Consume<S, T>,
) -> Result<T, HostError> {
    let ResolvedModelConnection {
        mut connection,
        failure_policy: policy,
        mut recovery,
    } = resolved;
    let mut recovery_state = RecoveryState::default();
    if let Some(chain) = chain {
        recovery_state.attempts = chain.attempts;
        recovery_state.waited = Duration::from_millis(chain.waited_ms);
    }
    let mut retry_forbidden = false;
    let result = if recovery_state.attempts >= MAX_ATTEMPTS || recovery_state.waited >= WAIT_BUDGET
    {
        Err(HostError::failed(SPENT_FAILURE_CHAIN))
    } else {
        loop {
            check_cancel(cancel)?;
            recovery_state.attempts += 1;
            let attempt = cancel.child_token();
            let mut replay_safe = true;
            let result = match host
                .0
                .invoke(request.clone(), connection.clone(), attempt.clone())
                .await
            {
                Ok(stream) => consume(stream, state, attempt.clone(), &mut replay_safe).await,
                Err(error) => Err(error),
            };
            attempt.cancel();
            let Err(error @ HostError::Failed { model, .. }) = &result else {
                break result;
            };
            let forbidden = policy
                .as_ref()
                .is_some_and(|policy| policy.forbids_retry(error));
            retry_forbidden = forbidden;
            let rejection = recovery
                .as_ref()
                .map_or(CredentialRejection::Unrelated, |recovery| {
                    recovery.rejection(error, recovery_state.phase)
                });
            let failure = model
                .as_deref()
                .map(|facts| (facts.kind, facts.reason.clone()));
            let action = recovery_state.decide(
                model.as_deref(),
                rejection,
                forbidden,
                replay_safe,
                uuid::Uuid::new_v4().as_bytes()[0],
            );
            let delay = match action {
                FailureAction::Stop => break result,
                FailureAction::SettleCredentials => {
                    check_cancel(cancel)?;
                    let settled = recovery
                        .as_ref()
                        .expect("terminal rejection has a credential owner")
                        .finish_rejected(cancel.clone())
                        .await;
                    if let Err(error) = settled {
                        break Err(error);
                    }
                    break result;
                }
                FailureAction::Retry(delay) | FailureAction::RecoverCredentials(delay) => delay,
            };
            if !delay.is_zero() {
                let (kind, reason) = failure.unwrap_or((ModelFailureKind::Unknown, None));
                state.waiting(Some(RetryWaiting {
                    attempt: recovery_state.attempts + 1,
                    max_attempts: MAX_ATTEMPTS,
                    retry_at_ms: unix_millis_now()
                        .saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
                    kind,
                    reason: reason.as_deref(),
                }));
                let waiting = tokio::time::Instant::now();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        state.waiting(None);
                        return Err(HostError::Cancelled("Model request cancelled".into()));
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
                recovery_state.waited += waiting.elapsed();
                state.waiting(None);
            }
            if let FailureAction::RecoverCredentials(_) = action {
                check_cancel(cancel)?;
                let recovered = match recovery
                    .as_ref()
                    .expect("recoverable rejection has a credential owner")
                    .recover(cancel.clone())
                    .await
                {
                    Ok(recovered) => recovered,
                    Err(error) => break Err(error),
                };
                connection = recovered.connection;
                recovery = Some(recovered.recovery);
                recovery_state.phase = CredentialRecoveryPhase::Recovered;
            }
        }
    };
    result.map_err(|mut error| {
        if matches!(
            &error,
            HostError::Failed {
                presentation: None,
                ..
            }
        ) {
            let display = policy
                .as_ref()
                .and_then(|policy| policy.presentation(&error));
            if let HostError::Failed { presentation, .. } = &mut error {
                *presentation = display.map(Box::new);
            }
        }
        if let HostError::Failed { message, model, .. } = &mut error {
            let facts = model
                .get_or_insert_with(|| Box::new(ModelFailureFacts::unknown(Some(message.clone()))));
            facts.attempts = recovery_state.attempts.max(1);
            facts.waited = recovery_state.waited;
            facts.retry_after_stop = recovery_state.retry_after_stop;
            facts.retry_forbidden = retry_forbidden;
        }
        error
    })
}

/// Invocation-local I/O control, not replayable business state or a service.
#[derive(Default)]
struct RecoveryState {
    pub attempts: u8,
    /// Malformed responses met so far, which are limited on their own.
    pub malformed: u8,
    /// Backoff waits caused so far, which number the delay curve.
    pub backoffs: u8,
    pub waited: Duration,
    /// The provider's wait that did not fit in the budget left.
    pub retry_after_stop: Option<Duration>,
    pub phase: CredentialRecoveryPhase,
}

enum FailureAction {
    Stop,
    SettleCredentials,
    Retry(Duration),
    RecoverCredentials(Duration),
}

impl RecoveryState {
    pub fn decide(
        &mut self,
        facts: Option<&ModelFailureFacts>,
        rejection: CredentialRejection,
        forbidden: bool,
        replay_safe: bool,
        jitter: u8,
    ) -> FailureAction {
        if rejection == CredentialRejection::Terminal {
            return FailureAction::SettleCredentials;
        }
        if forbidden || !replay_safe || self.attempts >= MAX_ATTEMPTS {
            return FailureAction::Stop;
        }
        let recovering = rejection == CredentialRejection::Recoverable;
        if recovering {
            if self.phase == CredentialRecoveryPhase::Recovered {
                return FailureAction::Stop;
            }
            // Recovery never sleeps: a rejected credential is refreshed once
            // at once, whatever `Retry-After` the rejection carried.
            return FailureAction::RecoverCredentials(Duration::ZERO);
        }
        let Some(facts) = facts else {
            return FailureAction::Stop;
        };
        if !retryable(facts.kind) {
            return FailureAction::Stop;
        }
        if facts.kind == ModelFailureKind::MalformedResponse {
            if self.malformed >= MALFORMED_RETRIES {
                return FailureAction::Stop;
            }
            self.malformed += 1;
        }
        // The counter is the retry's number: every retry advances it, hinted
        // or not, so an unhinted n-th retry waits 1 s × 2^(n−1).
        let retry = self.backoffs.saturating_add(1);
        let delay = facts.retry_after.unwrap_or_else(|| backoff(retry, jitter));
        if delay > WAIT_BUDGET.saturating_sub(self.waited) {
            if facts.retry_after.is_some() {
                self.retry_after_stop = facts.retry_after;
            }
            return FailureAction::Stop;
        }
        self.backoffs = retry;
        FailureAction::Retry(delay)
    }
}

/// The n-th retry (1-based) waits 1 s doubled per retry, capped at 30 s,
/// times a uniform jitter in [0.8, 1.0].
fn backoff(retry: u8, jitter: u8) -> Duration {
    let exponent = u32::from(retry.saturating_sub(1)).min(6);
    let nominal = BASE_DELAY.saturating_mul(1u32 << exponent).min(MAX_BACKOFF);
    let jitter = 0.8 + 0.2 * f64::from(jitter) / 255.0;
    Duration::from_secs_f64(nominal.as_secs_f64() * jitter)
}

/// Unix milliseconds now, the unit `RetryProgress::retry_at_ms` uses.
fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The retried kinds: transient transport, rate-limit, server and malformed
/// responses. A malformed response is retried at most once per request.
fn retryable(kind: ModelFailureKind) -> bool {
    matches!(
        kind,
        ModelFailureKind::Network
            | ModelFailureKind::RateLimited
            | ModelFailureKind::ServerError
            | ModelFailureKind::MalformedResponse
    )
}
