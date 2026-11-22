use super::{ChatGpt, NAMESPACE};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Begin {
    redirect_uri: String,
    profile_id: Option<Uuid>,
    #[serde(default)]
    migrate_legacy: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Complete {
    attempt_id: Uuid,
    callback_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Select {
    profile_id: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Logout {
    revocation_confirmed: bool,
}

pub(in crate::managed_provider) fn provide(
    ctx: &rutis::Ctx,
    methods: &crate::methods::ManagementMethods,
    owner: Arc<ChatGpt>,
) -> rutis::Result<()> {
    use super::super::management::{LoginAttempt, attempt_id, error};
    use crate::protocol::Empty;
    use dim_agent_capabilities::methods::{Interruption, MethodDecl, params, reply};

    methods.register(
        ctx,
        NAMESPACE,
        [
            "account.chatgpt.status",
            "account.chatgpt.profiles",
            "account.chatgpt.select",
            "account.chatgpt.login.browser.begin",
            "account.chatgpt.login.browser.complete",
            "account.chatgpt.login.status",
            "account.chatgpt.login.cancel",
            "account.chatgpt.logout",
        ]
        .into_iter()
        .map(|method| {
            MethodDecl::new(
                method,
                !matches!(
                    method,
                    "account.chatgpt.status"
                        | "account.chatgpt.profiles"
                        | "account.chatgpt.login.status"
                ),
                Interruption::Drain,
            )
        })
        .collect(),
        Arc::new(move |call, stopping, _| {
            let owner = owner.clone();
            Box::pin(async move {
                let value = call.params;
                let cancel = &stopping;
                match call.method.as_str() {
                    "account.chatgpt.status" => {
                        let _: Empty = params(value)?;
                        reply(owner.status(cancel).await.map_err(|e| error(e.into()))?)
                    }
                    "account.chatgpt.profiles" => {
                        let _: Empty = params(value)?;
                        reply(owner.profiles(cancel).await.map_err(|e| error(e.into()))?)
                    }
                    "account.chatgpt.select" => {
                        let p: Select = params(value)?;
                        reply(
                            owner
                                .select(p.profile_id, cancel)
                                .await
                                .map_err(|e| error(e.into()))?,
                        )
                    }
                    "account.chatgpt.login.browser.begin" => {
                        let p: Begin = params(value)?;
                        reply(
                            owner
                                .begin(&p.redirect_uri, p.profile_id, p.migrate_legacy, cancel)
                                .await
                                .map_err(|e| error(e.into()))?,
                        )
                    }
                    "account.chatgpt.login.browser.complete" => {
                        let p: Complete = params(value)?;
                        reply(
                            owner
                                .complete(p.attempt_id, &p.callback_url)
                                .await
                                .map_err(|e| error(e.into()))?,
                        )
                    }
                    "account.chatgpt.login.status" => {
                        let p: LoginAttempt = params(value)?;
                        reply(
                            owner
                                .attempt_status(attempt_id(&p)?)
                                .map_err(|e| error(e.into()))?,
                        )
                    }
                    "account.chatgpt.login.cancel" => {
                        let p: LoginAttempt = params(value)?;
                        owner.cancel(attempt_id(&p)?).map_err(|e| error(e.into()))?;
                        reply(Empty {})
                    }
                    "account.chatgpt.logout" => {
                        let _: Empty = params(value)?;
                        let revocation_confirmed =
                            owner.logout(cancel).await.map_err(|e| error(e.into()))?;
                        reply(Logout {
                            revocation_confirmed,
                        })
                    }
                    _ => unreachable!("registered ChatGPT method"),
                }
            })
        }),
    )?;
    Ok(())
}
