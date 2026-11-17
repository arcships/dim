use crate::{DimOAuth, Inner, ManagedError, PROVIDER_ID, http::text};
use dim_session_title::{GenerateTitleReply, GenerateTitleRequest, TitleUnavailableReason};
use rutis_bridge::session::{HostDispatch, Reply, Value, arg, settle};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

impl HostDispatch for DimOAuth {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"generate":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let invalid = || {
            rutis_bridge::exports::failure(
                "TitleGenerationError",
                "invalid_request",
                "Invalid title generation request".into(),
                None,
            )
        };
        if method != "generate" {
            return Err(invalid());
        }
        let mut args = args.list().map_err(|_| invalid())?.into_iter();
        let request: GenerateTitleRequest = serde_json::from_value(
            args.next()
                .ok_or_else(invalid)?
                .json()
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
        if request.provider_id.trim().is_empty()
            || request.text.trim().is_empty()
            || request.text.chars().count() > 2000
        {
            return Err(invalid());
        }
        let cancelled = match args.next() {
            None | Some(Value::Undefined) => None,
            Some(Value::Reference(callback)) if callback.is_function() => Some(callback),
            Some(_) => return Err(invalid()),
        };
        if args.next().is_some() {
            return Err(invalid());
        }
        let cancel = self.0.stop.child_token();
        let operation_cancel = cancel.clone();
        let call = self.owned(move |inner| async move {
            Ok(inner.generate_title(request, operation_cancel).await)
        });
        Ok(Value::future(async move {
            let _dropped = cancel.clone().drop_guard();
            tokio::pin!(call);
            let result = if let Some(cancelled) = cancelled {
                tokio::select! {
                    result = &mut call => result,
                    _ = async {
                        let returned = cancelled.call_async(Value::List(vec![])).await?;
                        settle(returned).await
                    } => {
                        cancel.cancel();
                        call.await
                    }
                }
            } else {
                call.await
            };
            let result = result.unwrap_or_else(title_failure);
            arg(&result)
        }))
    }
}

impl Inner {
    async fn generate_title(
        self: Arc<Self>,
        request: GenerateTitleRequest,
        cancel: CancellationToken,
    ) -> GenerateTitleReply {
        if cancel.is_cancelled() {
            return GenerateTitleReply::Cancelled;
        }
        if request.provider_id != PROVIDER_ID {
            return GenerateTitleReply::Unavailable {
                reason: TitleUnavailableReason::UnsupportedProvider,
            };
        }
        let result = self
            .authorized(
                |client, tokens| {
                    client
                        .post(format!(
                            "{}/v1/session/rename",
                            tokens.str("issuer").trim_end_matches('/')
                        ))
                        .timeout(Duration::from_secs(10))
                        .json(&json!({"messages":[{"role":"user","content":request.text}]}))
                },
                Some(&cancel),
            )
            .await;
        if cancel.is_cancelled() {
            return GenerateTitleReply::Cancelled;
        }
        let (_, status, body) = match result {
            Ok(response) => response,
            Err(error) => return title_failure(error),
        };
        if !status.is_success() {
            let reason = match body
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(|code| code.as_str())
            {
                Some("session_rename_disabled") => TitleUnavailableReason::Disabled,
                Some("session_rename_not_entitled") => TitleUnavailableReason::NotEntitled,
                Some("session_rename_quota_exhausted") => TitleUnavailableReason::QuotaExhausted,
                _ => return GenerateTitleReply::Failed,
            };
            return GenerateTitleReply::Unavailable { reason };
        }
        match text(body.get("title"), ManagedError::InvalidResponse) {
            Ok(title) => GenerateTitleReply::Generated { title },
            Err(_) => GenerateTitleReply::Failed,
        }
    }
}

fn title_failure(error: ManagedError) -> GenerateTitleReply {
    let reason = match error {
        ManagedError::Cancelled => return GenerateTitleReply::Cancelled,
        ManagedError::ServiceReloading => TitleUnavailableReason::ServiceReloading,
        ManagedError::TokenMissing
        | ManagedError::RefreshRejected
        | ManagedError::InvalidStore
        | ManagedError::UnsupportedSchema
        | ManagedError::AccountRecoveryRequired
        | ManagedError::AccountResetBlocked
        | ManagedError::IdentityRejected
        | ManagedError::ConfigError
        | ManagedError::AccountChanged => TitleUnavailableReason::AccountUnavailable,
        _ => return GenerateTitleReply::Failed,
    };
    GenerateTitleReply::Unavailable { reason }
}
