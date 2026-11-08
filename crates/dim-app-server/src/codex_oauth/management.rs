//! Codex OAuth account wire boundary.
use super::OpenAICodexOAuthService;
use crate::{
    managed_provider::management::{
        BrowserLogin, LoginAttempt, attempt_id, managed_attempt, managed_oauth_error as error,
    },
    methods::ManagementMethods,
    protocol::Empty,
};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use rutis::{Ctx, Error};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLoginComplete {
    pub attempt_id: String,
    pub callback_url: String,
}

pub(super) fn provide(
    ctx: &Ctx,
    methods: &ManagementMethods,
    oauth: Arc<OpenAICodexOAuthService>,
) -> Result<(), Error> {
    methods.register(
        ctx,
        "account.codex",
        [
            "account.codex.status",
            "account.codex.recover",
            "account.codex.login.browser.begin",
            "account.codex.login.browser.complete",
            "account.codex.login.status",
            "account.codex.login.cancel",
            "account.codex.logout",
        ]
        .into_iter()
        .map(|name| {
            MethodDecl::new(
                name,
                !matches!(name, "account.codex.status" | "account.codex.login.status"),
                Interruption::Drain,
            )
        })
        .collect(),
        Arc::new(move |call, stopping, _| {
            let MethodCall {
                method,
                params: value,
                ..
            } = call;
            let oauth = oauth.clone();
            Box::pin(async move {
                let cancel = &stopping;
                match method.as_str() {
                    "account.codex.status" => {
                        let _: Empty = params(value)?;
                        reply(oauth.status(cancel).await.map_err(error)?)
                    }
                    "account.codex.recover" => {
                        let _: Empty = params(value)?;
                        oauth.recover(cancel).await.map_err(error)?;
                        reply(Empty {})
                    }
                    "account.codex.login.browser.begin" => {
                        let _: Empty = params(value)?;
                        let login = oauth.begin_browser(cancel).await.map_err(error)?;
                        reply(BrowserLogin {
                            attempt_id: login.attempt_id.to_string(),
                            authorization_url: login.authorization_url.into(),
                            expires_at: login.expires_at,
                        })
                    }
                    "account.codex.login.browser.complete" => {
                        let p: BrowserLoginComplete = params(value)?;
                        let id = uuid::Uuid::parse_str(&p.attempt_id)
                            .map_err(|_| RpcError::invalid("attemptId must be a UUID"))?;
                        let callback = url::Url::parse(&p.callback_url)
                            .map_err(|_| RpcError::invalid("callbackUrl must be a callback URL"))?;
                        oauth
                            .complete_browser(id, callback, cancel)
                            .await
                            .map_err(error)?;
                        reply(Empty {})
                    }
                    "account.codex.login.status" => {
                        let p: LoginAttempt = params(value)?;
                        reply(managed_attempt(
                            oauth.attempt_status(attempt_id(&p)?).await.map_err(error)?,
                        ))
                    }
                    "account.codex.login.cancel" => {
                        let p: LoginAttempt = params(value)?;
                        oauth.cancel_attempt(attempt_id(&p)?).await.map_err(error)?;
                        reply(Empty {})
                    }
                    "account.codex.logout" => {
                        let _: Empty = params(value)?;
                        oauth.logout(cancel).await.map_err(error)?;
                        reply(Empty {})
                    }
                    _ => unreachable!("registered Codex account method"),
                }
            })
        }),
    )?;
    Ok(())
}
