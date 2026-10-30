use crate::{context_policy::check_cancel, host::*};
use dim_agent_types::ModelRequest;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Reads one attempt's stream; see [`invoke`].
pub type Consume<S, T> = for<'a> fn(
    ModelStream,
    &'a mut S,
    CancellationToken,
    &'a mut bool,
) -> futures_util::future::BoxFuture<'a, Result<T, HostError>>;

/// Runs one request over its resolved connection under the product's failure
/// policy: transient failures back off, a rejected credential is recovered
/// once, and a terminal rejection is settled with its credential owner.
/// `consume` reads one attempt's stream into `state` and clears `replay_safe`
/// once output that cannot be replayed was handed on.
pub async fn invoke<S: Send, T>(
    host: &ModelHost,
    request: &Arc<ModelRequest>,
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
    let result = loop {
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
        let rejection = recovery
            .as_ref()
            .map_or(CredentialRejection::Unrelated, |recovery| {
                recovery.rejection(error, recovery_state.phase)
            });
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
                recovery
                    .as_ref()
                    .expect("terminal rejection has a credential owner")
                    .finish_rejected(cancel.clone())
                    .await?;
                break result;
            }
            FailureAction::Retry(delay) | FailureAction::RecoverCredentials(delay) => delay,
        };
        if !delay.is_zero() {
            let waiting = tokio::time::Instant::now();
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(HostError::Cancelled("Model request cancelled".into())),
                _ = tokio::time::sleep(delay) => {}
            }
            recovery_state.waited += waiting.elapsed();
        }
        if let FailureAction::RecoverCredentials(_) = action {
            check_cancel(cancel)?;
            let recovered = recovery
                .as_ref()
                .expect("recoverable rejection has a credential owner")
                .recover(cancel.clone())
                .await?;
            connection = recovered.connection;
            recovery = Some(recovered.recovery);
            recovery_state.phase = CredentialRecoveryPhase::Recovered;
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
        error
    })
}

/// Invocation-local I/O control, not replayable business state or a service.
#[derive(Default)]
struct RecoveryState {
    pub attempts: u8,
    pub waited: Duration,
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
        &self,
        facts: Option<&ModelFailureFacts>,
        rejection: CredentialRejection,
        forbidden: bool,
        replay_safe: bool,
        jitter: u8,
    ) -> FailureAction {
        if rejection == CredentialRejection::Terminal {
            return FailureAction::SettleCredentials;
        }
        if forbidden || !replay_safe || self.attempts >= 3 {
            return FailureAction::Stop;
        }
        let recovering = rejection == CredentialRejection::Recoverable;
        if recovering {
            if self.phase == CredentialRecoveryPhase::Recovered {
                return FailureAction::Stop;
            }
        } else if !facts.is_some_and(|facts| facts.transient) {
            return FailureAction::Stop;
        }
        let delay = facts
            .and_then(|facts| facts.retry_after)
            .unwrap_or_else(|| {
                if recovering {
                    Duration::ZERO
                } else {
                    Duration::from_secs_f64(
                        0.5 * f64::from(self.attempts) * (0.75 + 0.25 * f64::from(jitter) / 255.0),
                    )
                }
            });
        if delay > Duration::from_secs(60).saturating_sub(self.waited) {
            return FailureAction::Stop;
        }
        if recovering {
            FailureAction::RecoverCredentials(delay)
        } else {
            FailureAction::Retry(delay)
        }
    }
}
