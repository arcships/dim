//! Grok OAuth account wire boundary.
use super::GrokOAuthService;
use crate::{
    managed_provider::management::{
        DeviceLogin, LoginAttempt, attempt_id, managed_attempt, managed_oauth_error as error,
    },
    methods::ManagementMethods,
    protocol::Empty,
};
use dim_agent_capabilities::methods::{Interruption, MethodCall, MethodDecl, params, reply};
use rutis::{Ctx, Error};
use std::sync::Arc;

pub(super) fn provide(
    ctx: &Ctx,
    methods: &ManagementMethods,
    oauth: Arc<GrokOAuthService>,
) -> Result<(), Error> {
    methods.register(
        ctx,
        "account.grok",
        [
            "account.grok.status",
            "account.grok.recover",
            "account.grok.login.begin",
            "account.grok.login.status",
            "account.grok.login.cancel",
            "account.grok.logout",
        ]
        .into_iter()
        .map(|name| {
            MethodDecl::new(
                name,
                !matches!(name, "account.grok.status" | "account.grok.login.status"),
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
                    "account.grok.status" => {
                        let _: Empty = params(value)?;
                        reply(oauth.status(cancel).await.map_err(error)?)
                    }
                    "account.grok.recover" => {
                        let _: Empty = params(value)?;
                        oauth.recover(cancel).await.map_err(error)?;
                        reply(Empty {})
                    }
                    "account.grok.login.begin" => {
                        let _: Empty = params(value)?;
                        let login = oauth.begin_device(cancel).await.map_err(error)?;
                        reply(DeviceLogin {
                            attempt_id: login.attempt_id.to_string(),
                            verification_uri: login.verification_uri.into(),
                            verification_uri_complete: login
                                .verification_uri_complete
                                .map(Into::into),
                            user_code: login.user_code,
                            expires_at: login.expires_at,
                        })
                    }
                    "account.grok.login.status" => {
                        let p: LoginAttempt = params(value)?;
                        reply(managed_attempt(
                            oauth.attempt_status(attempt_id(&p)?).await.map_err(error)?,
                        ))
                    }
                    "account.grok.login.cancel" => {
                        let p: LoginAttempt = params(value)?;
                        oauth.cancel_attempt(attempt_id(&p)?).await.map_err(error)?;
                        reply(Empty {})
                    }
                    "account.grok.logout" => {
                        let _: Empty = params(value)?;
                        oauth.logout(cancel).await.map_err(error)?;
                        reply(Empty {})
                    }
                    _ => unreachable!("registered Grok account method"),
                }
            })
        }),
    )?;
    Ok(())
}
