//! Managed-provider login stays in the provider; the host supplies browser I/O policy.
use crate::rpc::Rpc;
use agent_client_protocol::{Error, RequestCancellation};
use dim_client::{Client, callback::Capture};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Login {
    pub directory: String,
    pub redirect_uri: String,
    pub open_browser: Option<fn(&str) -> bool>,
    pub terminal_args: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Provider {
    provider_id: String,
    display_name: String,
    login_methods: Vec<String>,
}
#[derive(Clone)]
pub(crate) struct Method {
    pub id: String,
    pub provider: String,
    pub flow: String,
    pub terminal: Option<Vec<String>>,
    name: String,
}
impl Method {
    pub(crate) fn value(&self) -> Value {
        if let Some(args) = &self.terminal {
            json!({"type":"terminal","id":self.id,"name":self.name,"args":args})
        } else {
            json!({"id":self.id,"name":self.name})
        }
    }
}
fn methods(
    providers: Vec<Provider>,
    browser: bool,
    terminal_args: Option<&[String]>,
) -> Vec<Method> {
    let (flow, handler) = if terminal_args.is_some() {
        ("device", "terminal")
    } else if browser {
        ("browser", "agent")
    } else {
        return Vec::new();
    };
    let mut methods = Vec::new();
    for provider in providers {
        if !provider
            .login_methods
            .iter()
            .any(|candidate| candidate == flow)
        {
            continue;
        }
        let id = json!(["dim", provider.provider_id, flow, handler]).to_string();
        methods.push(Method {
            terminal: terminal_args.map(|args| {
                let mut args = args.to_vec();
                args.extend(["--auth-setup".into(), id.clone()]);
                args
            }),
            id,
            provider: provider.provider_id,
            flow: flow.to_owned(),
            name: format!("{} ({flow})", provider.display_name),
        });
    }
    methods
}

pub(crate) async fn discover(
    rpc: &Rpc,
    login: Option<&Login>,
    terminal: bool,
) -> Result<Vec<Method>, Error> {
    if !rpc.supports("account.providers") {
        return Ok(Vec::new());
    }
    let providers: Vec<Provider> = rpc.typed("account.providers", json!({})).await?;
    for provider in &providers {
        rpc.call("account.status", json!({"providerId":provider.provider_id}))
            .await?;
    }
    Ok(methods(
        providers,
        login.is_some_and(|login| login.open_browser.is_some()),
        login
            .filter(|_| terminal)
            .and_then(|login| login.terminal_args.as_deref()),
    ))
}

/// Resolve an advertised terminal ID again against the current native discovery.
pub async fn terminal_method(client: &mut Client, id: &str) -> dim_client::Result<(String, bool)> {
    let providers: Vec<Provider> = client.typed("account.providers", json!({})).await?;
    let method = methods(providers, false, Some(&[]))
        .into_iter()
        .find(|method| method.id == id && method.terminal.is_some())
        .ok_or("Unknown terminal authentication method")?;
    Ok((method.provider, method.flow == "device"))
}

fn failed(message: &str) -> Error {
    Error::new(-32001, message).data(json!({"code":"authentication_failed"}))
}

pub(crate) async fn ensure(rpc: &Rpc, provider: &str, managed: &[String]) -> Result<(), Error> {
    if managed.iter().any(|candidate| candidate == provider) {
        #[derive(Deserialize)]
        struct Account {
            account: Option<Value>,
        }
        let account: Account = rpc
            .typed("account.status", json!({"providerId":provider}))
            .await?;
        if account.account.is_none() {
            return Err(Error::new(
                -32000,
                "Authentication required for the selected provider",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn authenticate(
    rpc: Rpc,
    login: Login,
    method: Method,
    cancel: RequestCancellation,
    stop: CancellationToken,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    struct Account {
        account: Option<Value>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Browser {
        attempt_id: String,
        authorization_url: String,
        expires_at: u64,
    }
    let provider = &method.provider;
    let account: Account = rpc
        .typed("account.status", json!({"providerId":provider}))
        .await?;
    if account.account.is_some() {
        return Ok(json!({}));
    }
    let capture = Capture::bind(&login.redirect_uri)
        .await
        .ok_or_else(|| failed("Cannot bind the configured login callback"))?;
    let browser: Browser = rpc.typed("account.login.browser.begin",json!({"providerId":provider,"directory":login.directory,"redirectUri":login.redirect_uri})).await?;
    let mut completing = false;
    let result = async {
        if !login.open_browser.is_some_and(|open|open(&browser.authorization_url)) { return Err(failed("Cannot open the login browser")); }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|Error::internal_error())?.as_millis() as u64;
        let deadline = tokio::time::sleep(Duration::from_millis(browser.expires_at.saturating_sub(now)));
        tokio::pin!(deadline);
        let callback = capture.receive();
        tokio::pin!(callback);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(Error::request_cancelled()),
                () = stop.cancelled() => return Err(Error::request_cancelled()),
                () = &mut deadline => return Err(failed("Login expired")),
                callback = &mut callback => {
                    let (url,mut stream) = callback.map_err(|_|failed("Cannot capture the login callback"))?;
                    completing = true;
                    let result = tokio::select! {
                        () = cancel.cancelled()=>Err(Error::request_cancelled()),
                        () = stop.cancelled()=>Err(Error::request_cancelled()),
                        result = rpc.call("account.login.browser.complete",json!({"providerId":provider,"attemptId":browser.attempt_id,"callbackUrl":url}))=>result,
                    };
                    let response = if result.is_ok() { b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice() } else { b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice() };
                    let _ = stream.write_all(response).await;
                    result?;
                    let account: Account = rpc.typed("account.status",json!({"providerId":provider})).await?;
                    return if account.account.is_some() { Ok(json!({})) } else { Err(failed("Login did not produce an authenticated account")) };
                }
                () = tokio::time::sleep(Duration::from_secs(1)) => {
                    let state = tokio::select! {
                        () = cancel.cancelled()=>return Err(Error::request_cancelled()),
                        () = stop.cancelled()=>return Err(Error::request_cancelled()),
                        () = &mut deadline=>return Err(failed("Login expired")),
                        state = rpc.call("account.login.status",json!({"providerId":provider,"attemptId":browser.attempt_id}))=>state?,
                    };
                    match state["status"].as_str() {
                        Some("pending")=>{},
                        Some("completed") => {
                            let account: Account = rpc.typed("account.status",json!({"providerId":provider})).await?;
                            return if account.account.is_some() { Ok(json!({})) } else { Err(failed("Login completed without an account")) };
                        }
                        Some("cancelled"|"expired"|"failed")=>return Err(failed("Login ended without an account")),
                        _ => return Err(Error::internal_error()),
                    }
                }
            }
        }
    }.await;
    if result.is_err() && !completing {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            rpc.call(
                "account.login.cancel",
                json!({"providerId":provider,"attemptId":browser.attempt_id}),
            ),
        )
        .await;
    }
    result
}
