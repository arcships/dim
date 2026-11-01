use crate::{
    Result, args::Options, client::Client, input::Input, model::session::ACCOUNT_NAMESPACE,
    sanitize::safe,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::IsTerminal, time::Duration};

/// What an account command reports when the build has no Dim account
/// provider.
pub const ACCOUNT_UNAVAILABLE: &str = "This build has no Dim account provider, so the Dim account is unavailable; configure an API-key provider instead.";

/// `provider.management`'s `account` descriptor, reduced to sign-in needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    namespace: String,
    login_methods: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    provider_id: String,
    display_name: String,
    account: Option<Account>,
}
#[derive(Deserialize)]
struct Management {
    providers: Vec<Listed>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Browser {
    attempt_id: String,
    authorization_url: String,
    expires_at: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    attempt_id: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    user_code: String,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum State {
    Pending,
    Completed,
    Failed { error: String },
    Cancelled,
    Expired,
}

/// One Provider the daemon describes an account for.
struct Described {
    provider_id: String,
    display_name: String,
    namespace: String,
    login_methods: Vec<String>,
}

/// The params a namespace's account calls take. The managed account
/// namespace targets one provider; a native OAuth namespace has one account.
pub(crate) fn target(namespace: &str, provider_id: &str) -> Value {
    if namespace == ACCOUNT_NAMESPACE {
        json!({"providerId": provider_id})
    } else {
        json!({})
    }
}

/// The params a sign-in with `attempt_id` takes in `namespace`.
pub(crate) fn attempt(namespace: &str, provider_id: &str, attempt_id: &str) -> Value {
    let mut params = target(namespace, provider_id);
    params["attemptId"] = json!(attempt_id);
    params
}

/// The namespace's browser sign-in begin method.
pub(crate) fn browser_begin(namespace: &str) -> String {
    format!("{namespace}.login.browser.begin")
}

/// The namespace's device sign-in begin method.
pub(crate) fn device_begin(namespace: &str) -> String {
    format!("{namespace}.login.begin")
}

/// The Dim account directory of the entry the CLI was started from: the npm
/// launcher names its package in `DIMCODE_NPM_PACKAGE`, a standalone binary
/// goes by its command name. `dimagent` signs in at dimagent.com; `dimcode`
/// and anything else at dimagent.cn.
pub fn account_directory() -> &'static str {
    let entry = std::env::var("DIMCODE_NPM_PACKAGE").ok().or_else(|| {
        std::env::args_os()
            .next()
            .map(std::path::PathBuf::from)
            .and_then(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
    });
    match entry.as_deref() {
        Some("dimagent") => "https://dimagent.com",
        _ => "https://dimagent.cn",
    }
}

/// The accounts the daemon describes, in its own order.
async fn described(client: &mut Client) -> Result<Vec<Described>> {
    Ok(listed(client)
        .await?
        .into_iter()
        .filter_map(|listed| {
            listed.account.map(|account| Described {
                provider_id: listed.provider_id,
                display_name: listed.display_name,
                namespace: account.namespace,
                login_methods: account.login_methods,
            })
        })
        .collect())
}

/// Every Provider `provider.management` reports.
async fn listed(client: &mut Client) -> Result<Vec<Listed>> {
    if !client.supports(&["provider.management"]) {
        return Err("This daemon cannot describe account providers".into());
    }
    let view: Management = client.typed("provider.management", json!({})).await?;
    Ok(view.providers)
}

/// The described account `provider_id` names, or why this daemon has none.
async fn described_account(client: &mut Client, provider_id: &str) -> Result<Described> {
    let providers = listed(client).await?;
    if let Some(listed) = providers
        .iter()
        .find(|provider| provider.provider_id == provider_id)
    {
        let Some(account) = &listed.account else {
            return Err(format!("Provider {provider_id} does not offer sign-in").into());
        };
        return Ok(Described {
            provider_id: listed.provider_id.clone(),
            display_name: listed.display_name.clone(),
            namespace: account.namespace.clone(),
            login_methods: account.login_methods.clone(),
        });
    }
    if !providers.iter().any(|provider| {
        provider
            .account
            .as_ref()
            .is_some_and(|account| account.namespace == ACCOUNT_NAMESPACE)
    }) {
        return Err(ACCOUNT_UNAVAILABLE.into());
    }
    Err(format!("Unknown account provider {provider_id}; run dimcode login to list them").into())
}

pub async fn login(
    client: &mut Client,
    options: &Options,
    provider_id: Option<&str>,
) -> Result<i32> {
    let Some(provider_id) = provider_id else {
        let providers = described(client).await?;
        if providers.is_empty() {
            return Err(ACCOUNT_UNAVAILABLE.into());
        }
        println!("Sign in to an account provider:");
        for provider in providers {
            println!(
                "  {} ({}): {}",
                provider.provider_id,
                safe(&provider.display_name),
                provider.login_methods.join(", ")
            );
        }
        println!("Run dimcode login PROVIDER.");
        return Ok(0);
    };
    let provider = described_account(client, provider_id).await?;
    let method = if options.device {
        "device"
    } else if provider
        .login_methods
        .iter()
        .any(|method| method == "browser")
    {
        "browser"
    } else {
        "device"
    };
    if !provider
        .login_methods
        .iter()
        .any(|available| available == method)
    {
        return Err("The provider does not support the requested login method".into());
    }
    println!("Sign in to {}", safe(&provider.display_name));
    let directory = options.directory.as_deref().unwrap_or(account_directory());
    let mut params = target(&provider.namespace, provider_id);
    if provider.namespace == ACCOUNT_NAMESPACE {
        params["directory"] = json!(directory);
    }
    if method == "device" {
        let begin = device_begin(&provider.namespace);
        if !client.supports(&[&begin]) {
            return Err(format!("This daemon does not offer {begin}").into());
        }
        let device: Device = client.typed(&begin, params).await?;
        println!(
            "Open: {}\nCode: {}",
            safe(
                device
                    .verification_uri_complete
                    .as_deref()
                    .unwrap_or(&device.verification_uri)
            ),
            safe(&device.user_code)
        );
        return poll(client, &provider, &device.attempt_id).await;
    }
    let begin = browser_begin(&provider.namespace);
    if !client.supports(&[&begin]) {
        return Err(format!("This daemon does not offer {begin}").into());
    }
    if !std::io::stdin().is_terminal() {
        return Err("Browser login requires a terminal to enter the callback URL; use --device if supported".into());
    }
    if provider.namespace == ACCOUNT_NAMESPACE {
        params["redirectUri"] = json!(options.redirect_uri);
    }
    let browser: Browser = client.typed(&begin, params).await?;
    println!("Open: {}", safe(&browser.authorization_url));
    crate::browser::open(&browser.authorization_url);
    let mut input = Input::new()?;
    let echo = input.hide_echo()?;
    println!("After authorization, paste the full callback URL. Input is hidden.");
    input.prompt("Callback URL: ")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let callback = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?; drop(echo); println!(); cancel(client, &provider, &browser.attempt_id).await?; return Ok(130);
        }
        result = input.line() => result?,
        _ = tokio::time::sleep(Duration::from_millis(browser.expires_at.saturating_sub(now))) => {
            drop(echo); println!(); cancel(client, &provider, &browser.attempt_id).await?; return Err("Login expired; start login again".into());
        }
    };
    drop(echo);
    println!();
    let Some(callback) = callback.filter(|value| !value.trim().is_empty()) else {
        cancel(client, &provider, &browser.attempt_id).await?;
        return Ok(130);
    };
    let method = format!("{}.login.browser.complete", provider.namespace);
    let mut complete = attempt(&provider.namespace, provider_id, &browser.attempt_id);
    complete["callbackUrl"] = json!(callback);
    let complete = client.rpc(&method, complete);
    tokio::pin!(complete);
    tokio::select! {
        result = &mut complete => { result?; }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            eprintln!("Login is still settling; use account {provider_id} to inspect the result");
            return Ok(130);
        }
    }
    println!("Signed in.");
    Ok(0)
}

/// `dimcode account PROVIDER`: the non-secret identity the account reports.
pub async fn account(client: &mut Client, provider_id: &str) -> Result<i32> {
    let provider = described_account(client, provider_id).await?;
    let method = format!("{}.status", provider.namespace);
    if !client.supports(&[&method]) {
        return Err(format!("This daemon does not offer {method}").into());
    }
    if provider.namespace == ACCOUNT_NAMESPACE {
        #[derive(Deserialize)]
        struct Snapshot {
            account: Option<Identity>,
        }
        #[derive(Deserialize)]
        struct Identity {
            issuer: String,
            subject: String,
        }
        let snapshot: Snapshot = client
            .typed(&method, target(&provider.namespace, provider_id))
            .await?;
        match snapshot.account {
            Some(account) => println!(
                "Signed in: {} ({})",
                safe(&account.subject),
                safe(&account.issuer)
            ),
            None => println!("Signed out."),
        }
        return Ok(0);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Status {
        authenticated: bool,
        account_id_preview: Option<String>,
    }
    let status: Status = client
        .typed(&method, target(&provider.namespace, provider_id))
        .await?;
    match status.account_id_preview.filter(|_| status.authenticated) {
        Some(preview) => println!("Signed in: {}", safe(&preview)),
        None if status.authenticated => println!("Signed in."),
        None => println!("Signed out."),
    }
    Ok(0)
}

/// `dimcode logout PROVIDER`: sign the described account out.
pub async fn logout(client: &mut Client, provider_id: &str) -> Result<i32> {
    let provider = described_account(client, provider_id).await?;
    let method = format!("{}.logout", provider.namespace);
    if !client.supports(&[&method]) {
        return Err(format!("This daemon does not offer {method}").into());
    }
    client
        .rpc(&method, target(&provider.namespace, provider_id))
        .await?;
    println!("Signed out.");
    Ok(0)
}

async fn poll(client: &mut Client, provider: &Described, attempt_id: &str) -> Result<i32> {
    let method = format!("{}.login.status", provider.namespace);
    loop {
        let status: State = client
            .typed(
                &method,
                attempt(&provider.namespace, &provider.provider_id, attempt_id),
            )
            .await?;
        match status {
            State::Pending => {}
            State::Completed => {
                println!("Signed in.");
                return Ok(0);
            }
            State::Failed { error } => return Err(format!("Login failed: {}", safe(&error)).into()),
            State::Cancelled => return Err("Login was cancelled".into()),
            State::Expired => return Err("Login expired; start login again".into()),
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; cancel(client, provider, attempt_id).await?; return Ok(130); }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

async fn cancel(client: &mut Client, provider: &Described, attempt_id: &str) -> Result<()> {
    client
        .rpc(
            &format!("{}.login.cancel", provider.namespace),
            attempt(&provider.namespace, &provider.provider_id, attempt_id),
        )
        .await?;
    Ok(())
}
