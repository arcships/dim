//! OAuth protocol steps. rmcp's `auth` module runs discovery, dynamic
//! registration and the authorization request (PKCE `S256`, `state`,
//! `resource`); Mcp closes its gaps and sends every token request itself.
use super::{
    McpAuthError,
    store::{AuthMethod, Binding, Client, Record, Registration, Tokens},
};
use crate::mcp::config::{self, McpOAuthSettings};
use reqwest_mcp::{Client as Http, StatusCode, Url};
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationMetadataSource,
    OAuthClientConfig, OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRequest, StateStore,
    StoredAuthorizationState, WWWAuthenticateParams,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CLIENT_NAME: &str = "Dim";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REVOCATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Every OAuth request: no redirects, no retries, the 30 s budget rmcp
/// also uses.
pub(super) fn http() -> Result<Http, McpAuthError> {
    Http::builder()
        .no_proxy()
        .dns_resolver(OAuthDns)
        .redirect(reqwest_mcp::redirect::Policy::none())
        .retry(reqwest_mcp::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| McpAuthError::Unavailable)
}

struct OAuthDns;

impl reqwest_mcp::dns::Resolve for OAuthDns {
    fn resolve(&self, name: reqwest_mcp::dns::Name) -> reqwest_mcp::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str();
            let addresses = tokio::net::lookup_host((host, 0)).await?;
            let allowed: Vec<_> = addresses
                .filter(|address| allowed_dns_address(host, *address))
                .collect();
            if allowed.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "OAuth host resolved to no allowed addresses",
                )
                .into());
            }
            Ok(Box::new(allowed.into_iter()) as reqwest_mcp::dns::Addrs)
        })
    }
}

fn allowed_dns_address(host: &str, address: SocketAddr) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        address.ip().is_loopback()
    } else {
        !restricted_ip(address.ip())
    }
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(super) fn loopback(url: &Url) -> bool {
    url.host().is_some_and(|host| config::loopback(&host))
}

/// OAuth endpoints use HTTPS. A loopback, private, link-local or cloud
/// metadata host is accepted only on loopback, and only for a loopback MCP
/// server (local development), plain HTTP included; so a remote server's
/// metadata cannot point Dim at a local or internal service. This is
/// rmcp's policy for authorization server metadata URLs, applied to every
/// OAuth URL.
fn trusted(url: &Url, local: bool) -> bool {
    let development = local && loopback(url);
    url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && match url.scheme() {
            "https" => development || !restricted(url),
            "http" => development,
            _ => false,
        }
}

pub(super) fn stored_endpoint(endpoint: &str, resource: &str) -> Option<Url> {
    let resource = Url::parse(resource).ok()?;
    Url::parse(endpoint)
        .ok()
        .filter(|url| trusted(url, loopback(&resource)))
}

fn restricted(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost"
                || host.ends_with(".localhost")
                || matches!(
                    host.as_str(),
                    "metadata" | "metadata.google.internal" | "metadata.azure.internal"
                )
        }
        Some(url::Host::Ipv4(ip)) => restricted_ip(ip.into()),
        Some(url::Host::Ipv6(ip)) => restricted_ip(ip.into()),
        None => true,
    }
}

fn restricted_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V4(ip) => return restricted_v4(ip),
        IpAddr::V6(ip) => ip,
    };
    let [a, b, c, d, e, f, g, h] = ip.segments();
    let embedded = match [a, b, c, d, e, f] {
        [0, 0, 0, 0, 0, 0xffff] | [0x64, 0xff9b, 0, 0, 0, 0] => Some([g, h]),
        [0, 0, 0, 0, 0, 0] if !ip.is_loopback() && !ip.is_unspecified() => Some([g, h]),
        [0x2002, ..] => Some([b, c]),
        _ => None,
    };
    if let Some([high, low]) = embedded {
        return restricted_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a & 0xffc0 == 0xfe80
        || a & 0xfe00 == 0xfc00
        || [a, b] == [0x2001, 0]
        || [a, b, c] == [0x64, 0xff9b, 1]
}

fn restricted_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && matches!(b, 18 | 19))
}

#[cfg(test)]
#[test]
fn oauth_dns_does_not_dial_internal_addresses_returned_for_public_hosts() {
    for address in [
        "127.0.0.1:0",
        "10.0.0.1:0",
        "169.254.169.254:0",
        "[::1]:0",
        "[::ffff:127.0.0.1]:0",
        "[64:ff9b::a00:1]:0",
        "[2002:7f00:1::]:0",
    ] {
        assert!(
            !allowed_dns_address("oauth.example.test", address.parse().unwrap()),
            "{address}"
        );
    }
    assert!(allowed_dns_address(
        "oauth.example.test",
        "8.8.8.8:0".parse().unwrap()
    ));
    assert!(allowed_dns_address("localhost", "[::1]:0".parse().unwrap()));
    assert!(!allowed_dns_address(
        "localhost",
        "10.0.0.1:0".parse().unwrap()
    ));
    assert!(
        stored_endpoint("https://[64:ff9b::a00:1]/token", "https://mcp.example.test").is_none()
    );
    assert!(stored_endpoint("http://127.0.0.1:9000/token", "http://localhost:9001").is_some());
}

/// rmcp's HTTP port for one discovery, through Mcp's client and URL policy.
/// Records whether a failure was transient (network, 5xx, 429, 408), which
/// tells "retry" apart from "fix the server".
struct Port {
    http: Http,
    local: bool,
    transient: AtomicBool,
}
impl Port {
    fn failure(&self, error: AuthError) -> McpAuthError {
        if self.transient.load(Ordering::SeqCst) {
            McpAuthError::Unavailable
        } else {
            match error {
                AuthError::RegistrationFailed(_) => McpAuthError::Failed("registration_rejected"),
                _ => McpAuthError::DiscoveryFailed,
            }
        }
    }
}
impl OAuthHttpClient for Port {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let request = request.request;
            let url = Url::parse(&request.uri().to_string())?;
            if !trusted(&url, self.local) {
                return Err("untrusted OAuth URL".into());
            }
            let response = match self
                .http
                .execute(reqwest_mcp::Request::try_from(request)?)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    self.transient.store(true, Ordering::SeqCst);
                    return Err(error.into());
                }
            };
            let status = response.status();
            if status.is_server_error()
                || matches!(
                    status,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
                )
            {
                self.transient.store(true, Ordering::SeqCst);
            }
            let mut builder = http::Response::builder().status(status);
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            let body = body(response).await.inspect_err(|_| {
                self.transient.store(true, Ordering::SeqCst);
            })?;
            Ok(builder.body(body)?)
        })
    }
}

async fn body(mut response: reqwest_mcp::Response) -> Result<Vec<u8>, McpAuthError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| McpAuthError::Unavailable)?
    {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(McpAuthError::Unavailable);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Captures the PKCE verifier and `state` rmcp generates for one request.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<StoredAuthorizationState>>>);
#[async_trait::async_trait]
impl StateStore for Capture {
    async fn save(&self, _: &str, state: StoredAuthorizationState) -> Result<(), AuthError> {
        *self.0.lock().unwrap() = Some(state);
        Ok(())
    }
    async fn load(&self, _: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        Ok(None)
    }
    async fn delete(&self, _: &str) -> Result<(), AuthError> {
        Ok(())
    }
}

/// A prepared authorization: everything `complete` needs, in memory only.
pub(super) struct Authorization {
    pub url: String,
    pub state: String,
    pub verifier: String,
    pub iss_required: bool,
    pub client: Client,
    pub binding: Binding,
}

pub(super) struct Request<'a> {
    pub server: &'a Url,
    /// The last `WWW-Authenticate` of a `401` from this server.
    pub challenge: Option<&'a str>,
    pub settings: Option<&'a McpOAuthSettings>,
    pub redirect_uri: &'a str,
    pub client_secret: Option<String>,
    /// The key's record, for re-authorization and client reuse.
    pub previous: Option<&'a Record>,
    /// Scopes `403 insufficient_scope` challenges asked for.
    pub missing_scopes: &'a [String],
}

pub(super) async fn authorize(
    http: &Http,
    request: Request<'_>,
) -> Result<Authorization, McpAuthError> {
    let challenge = request
        .challenge
        .map(|header| WWWAuthenticateParams::parse(header, request.server));
    if challenge
        .as_ref()
        .and_then(|params| params.resource_metadata_url.as_ref())
        .is_some_and(|url| url.origin() != request.server.origin())
    {
        return Err(McpAuthError::DiscoveryFailed);
    }
    let local = loopback(request.server);
    let port = Arc::new(Port {
        http: http.clone(),
        local,
        transient: AtomicBool::new(false),
    });
    let mut manager =
        AuthorizationManager::new_with_oauth_http_client(request.server.as_str(), port.clone())
            .await
            .map_err(|_| McpAuthError::DiscoveryFailed)?;
    let resolution = manager
        .resolve_metadata_from_challenge(request.challenge)
        .await
        .map_err(|error| port.failure(error))?;
    let metadata = match resolution.source {
        AuthorizationMetadataSource::ProtectedResourceMetadata => resolution.metadata,
        _ => match request
            .settings
            .and_then(|settings| settings.auth_server_metadata_url.as_deref())
        {
            Some(url) => configured_metadata(http, url, local).await?,
            None => return Err(McpAuthError::DiscoveryFailed),
        },
    };
    let checked = check(&metadata, local)?;
    // Nothing recorded for another authorization server is used (§5.2).
    let previous = request
        .previous
        .filter(|previous| previous.issuer == checked.issuer);
    let challenged = union(
        &challenge
            .and_then(|params| params.scope)
            .map(|scope| split(&scope))
            .unwrap_or_default(),
        request.missing_scopes,
    );
    let mut scopes = match request.settings.and_then(|s| s.scopes.as_deref()) {
        Some(pinned) => split(pinned),
        None => match previous {
            Some(previous) => union(&previous.requested_scopes, &challenged),
            None if !challenged.is_empty() => challenged,
            None => manager.select_scopes(None, &[]),
        },
    };
    if !scopes.is_empty()
        && !scopes.iter().any(|scope| scope == "offline_access")
        && metadata
            .scopes_supported
            .as_ref()
            .is_some_and(|supported| supported.iter().any(|s| s == "offline_access"))
    {
        scopes.push("offline_access".into());
    }
    let registration_endpoint = metadata.registration_endpoint.is_some();
    manager.set_metadata(metadata);
    let configured = request.settings.and_then(|s| s.client_id.clone());
    let (registration, client_id, secret) = match configured {
        Some(client_id) => {
            if request
                .previous
                .is_some_and(|previous| previous.issuer != checked.issuer)
            {
                return Err(McpAuthError::ClientRequired);
            }
            let secret = request.client_secret.or_else(|| {
                previous
                    .and_then(|p| p.client.as_ref())
                    .filter(|c| {
                        c.registration == Registration::Preregistered && c.client_id == client_id
                    })
                    .and_then(|c| c.client_secret.clone())
            });
            (Registration::Preregistered, client_id, secret)
        }
        None => match previous.and_then(|p| p.client.as_ref()).filter(|c| {
            c.registration == Registration::Dynamic && c.redirect_uri == request.redirect_uri
        }) {
            Some(client) => (
                Registration::Dynamic,
                client.client_id.clone(),
                client.client_secret.clone(),
            ),
            None if registration_endpoint => {
                let config = manager
                    .register_client(CLIENT_NAME, request.redirect_uri, &[])
                    .await
                    .map_err(|error| port.failure(error))?;
                (
                    Registration::Dynamic,
                    config.client_id,
                    config.client_secret,
                )
            }
            None => return Err(McpAuthError::ClientRequired),
        },
    };
    let mut config = OAuthClientConfig::new(client_id.clone(), request.redirect_uri);
    if let Some(secret) = &secret {
        config = config.with_client_secret(secret.clone());
    }
    manager
        .configure_client(config)
        .map_err(|_| McpAuthError::DiscoveryFailed)?;
    let capture = Capture::default();
    manager.set_state_store(capture.clone());
    let refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let url = manager
        .get_authorization_url(&refs)
        .await
        .map_err(|_| McpAuthError::DiscoveryFailed)?;
    let state = capture
        .0
        .lock()
        .unwrap()
        .take()
        .ok_or(McpAuthError::DiscoveryFailed)?;
    let resource = Url::parse(&url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(name, _)| name == "resource")
                .map(|(_, value)| value.into_owned())
        })
        .ok_or(McpAuthError::DiscoveryFailed)?;
    Ok(Authorization {
        url,
        state: state.csrf_token.clone(),
        verifier: state.pkce_verifier.clone(),
        iss_required: checked.iss_required,
        client: Client {
            client_id,
            auth_method: match secret {
                None => AuthMethod::None,
                Some(_) if checked.post_only => AuthMethod::ClientSecretPost,
                Some(_) => AuthMethod::ClientSecretBasic,
            },
            client_secret: secret,
            registration,
            redirect_uri: request.redirect_uri.to_owned(),
        },
        binding: Binding {
            resource,
            issuer: checked.issuer,
            token_endpoint: checked.token_endpoint,
            revocation_endpoint: checked.revocation_endpoint,
            requested_scopes: scopes,
        },
    })
}

struct Checked {
    issuer: String,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    iss_required: bool,
    post_only: bool,
}

/// The specification's MUSTs that rmcp leaves open: PKCE `S256` advertised,
/// and HTTPS endpoints.
fn check(metadata: &AuthorizationMetadata, local: bool) -> Result<Checked, McpAuthError> {
    let endpoint = |value: &str| {
        Url::parse(value)
            .ok()
            .filter(|url| trusted(url, local))
            .map(|_| value.to_owned())
            .ok_or(McpAuthError::DiscoveryFailed)
    };
    let field = |name: &str| metadata.additional_fields.get(name);
    if !metadata
        .code_challenge_methods_supported
        .as_ref()
        .is_some_and(|methods| methods.iter().any(|m| m == "S256"))
    {
        return Err(McpAuthError::DiscoveryFailed);
    }
    let issuer = metadata
        .issuer
        .clone()
        .ok_or(McpAuthError::DiscoveryFailed)?;
    endpoint(&issuer)?;
    endpoint(&metadata.authorization_endpoint)?;
    let token_endpoint = endpoint(&metadata.token_endpoint)?;
    if let Some(registration) = &metadata.registration_endpoint {
        endpoint(registration)?;
    }
    let revocation_endpoint = match field("revocation_endpoint") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(endpoint(value)?),
        Some(_) => return Err(McpAuthError::DiscoveryFailed),
    };
    let methods = field("token_endpoint_auth_methods_supported").and_then(Value::as_array);
    let supports = |name: &str| methods.is_some_and(|m| m.iter().any(|v| v.as_str() == Some(name)));
    Ok(Checked {
        issuer,
        token_endpoint,
        revocation_endpoint,
        iss_required: field("authorization_response_iss_parameter_supported")
            == Some(&Value::Bool(true)),
        post_only: supports("client_secret_post") && !supports("client_secret_basic"),
    })
}

/// `oauth.authServerMetadataUrl`, used only when the server publishes no
/// PRM. A well-known URL must name the issuer it was built from; any other
/// URL is the configured trust anchor and its `issuer` is recorded.
async fn configured_metadata(
    http: &Http,
    url: &str,
    local: bool,
) -> Result<AuthorizationMetadata, McpAuthError> {
    let url = Url::parse(url)
        .ok()
        .filter(|url| trusted(url, local))
        .ok_or(McpAuthError::DiscoveryFailed)?;
    let response = http
        .get(url.clone())
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| McpAuthError::Unavailable)?;
    let status = response.status();
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(McpAuthError::Unavailable);
    }
    if status != StatusCode::OK {
        return Err(McpAuthError::DiscoveryFailed);
    }
    let metadata: AuthorizationMetadata = serde_json::from_slice(&body(response).await?)
        .map_err(|_| McpAuthError::DiscoveryFailed)?;
    if let Some(expected) = well_known_issuer(&url)
        && metadata
            .issuer
            .as_deref()
            .map(|issuer| issuer.trim_end_matches('/'))
            != Some(expected.trim_end_matches('/'))
    {
        return Err(McpAuthError::DiscoveryFailed);
    }
    Ok(metadata)
}

/// The issuer a well-known metadata URL was derived from (RFC 8414 §3.1,
/// OpenID Connect Discovery §4).
fn well_known_issuer(url: &Url) -> Option<String> {
    let path = url.path();
    let suffix = path
        .strip_prefix("/.well-known/oauth-authorization-server")
        .or_else(|| path.strip_prefix("/.well-known/openid-configuration"))
        .or_else(|| path.strip_suffix("/.well-known/openid-configuration"))?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    Some(format!("{origin}{suffix}"))
}

fn split(scopes: &str) -> Vec<String> {
    scopes.split_whitespace().map(str::to_owned).collect()
}
fn union(first: &[String], second: &[String]) -> Vec<String> {
    let mut scopes = first.to_vec();
    for scope in second {
        if !scopes.contains(scope) {
            scopes.push(scope.clone());
        }
    }
    scopes
}

pub(super) enum Callback {
    Code(String),
    /// The authorization server's RFC 6749 error code.
    Refused(&'static str),
}

/// The callback validation of design §5.4, in order. Any failure here is
/// `CallbackInvalid` and leaves the attempt pending.
pub(super) fn callback(
    callback: &str,
    redirect_uri: &str,
    state: &str,
    issuer: &str,
    iss_required: bool,
) -> Result<Callback, McpAuthError> {
    let invalid = McpAuthError::CallbackInvalid;
    let query = callback
        .strip_prefix(redirect_uri)
        .and_then(|rest| rest.strip_prefix('?'))
        .filter(|query| !query.contains('#'))
        .ok_or(McpAuthError::CallbackInvalid)?;
    let mut parameters = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if parameters.insert(name, value).is_some() {
            return Err(invalid);
        }
    }
    if !parameters
        .get("state")
        .is_some_and(|value| constant_time_eq(value.as_bytes(), state.as_bytes()))
    {
        return Err(invalid);
    }
    match parameters.get("iss") {
        Some(iss) if iss != issuer => return Err(invalid),
        None if iss_required => return Err(invalid),
        _ => {}
    }
    if let Some(error) = parameters.get("error") {
        return Ok(Callback::Refused(error_code(error)));
    }
    parameters
        .get("code")
        .filter(|code| !code.is_empty())
        .map(|code| Callback::Code(code.to_string()))
        .ok_or(invalid)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0, |acc, (l, r)| acc | (l ^ r)) == 0
}

/// Registered RFC 6749 and OpenID Connect codes are passed on; anything else
/// is reported generically, so no server text reaches the client.
fn error_code(code: &str) -> &'static str {
    [
        "access_denied",
        "invalid_request",
        "unauthorized_client",
        "unsupported_response_type",
        "invalid_scope",
        "server_error",
        "temporarily_unavailable",
        "invalid_grant",
        "invalid_client",
        "unsupported_grant_type",
        "interaction_required",
        "login_required",
        "consent_required",
        "account_selection_required",
        "invalid_target",
    ]
    .into_iter()
    .find(|known| *known == code)
    .unwrap_or("authorization_error")
}

pub(super) enum Grant<'a> {
    Code { code: &'a str, verifier: &'a str },
    Refresh(&'a str),
}

pub(super) enum TokenError {
    /// `invalid_grant`: the code or refresh token is no longer valid.
    Rejected,
    /// `invalid_client`: the registration is gone.
    InvalidClient,
    /// Another refusal, with its error code.
    Failed(&'static str),
    /// Network failure, timeout, a redirect, 5xx, 429 or a response that is
    /// not an OAuth error: nothing is known to have changed.
    Unavailable,
}

/// One token request (RFC 6749 §4.1.3, §6) with the record's `resource`
/// (RFC 8707) and client authentication. Never logged.
pub(super) async fn token(
    http: &Http,
    endpoint: &str,
    client: &Client,
    resource: &str,
    grant: Grant<'_>,
    requested: &[String],
) -> Result<Tokens, TokenError> {
    let endpoint = stored_endpoint(endpoint, resource).ok_or(TokenError::Unavailable)?;
    let mut form: Vec<(&str, &str)> = match &grant {
        Grant::Code { code, verifier } => vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", &client.redirect_uri),
        ],
        Grant::Refresh(refresh) => {
            vec![("grant_type", "refresh_token"), ("refresh_token", refresh)]
        }
    };
    form.push(("resource", resource));
    let request = authenticate(http.post(endpoint), &mut form, client);
    let response = form_body(request, &form)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| TokenError::Unavailable)?;
    let status = response.status();
    if status.is_server_error()
        || status.is_redirection()
        || matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
        )
    {
        return Err(TokenError::Unavailable);
    }
    let bytes = body(response).await.map_err(|_| TokenError::Unavailable)?;
    if !status.is_success() {
        // Only an OAuth error response (RFC 6749 §5.2) is a refusal; anything
        // else says nothing about the grant.
        let error = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value.get("error")?.as_str().map(str::to_owned));
        return Err(match error.as_deref() {
            Some("invalid_grant") => TokenError::Rejected,
            Some("invalid_client") => TokenError::InvalidClient,
            Some(code) => TokenError::Failed(error_code(code)),
            None => TokenError::Unavailable,
        });
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| TokenError::Failed("invalid_response"))?;
    let string = |name: &str| match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) if !value.chars().any(char::is_control) => {
            Ok(Some(value.clone()))
        }
        _ => Err(TokenError::Failed("invalid_response")),
    };
    let access_token = string("access_token")?.ok_or(TokenError::Failed("invalid_response"))?;
    if string("token_type")?.is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer")) {
        return Err(TokenError::Failed("invalid_response"));
    }
    let expires_at = match value.get("expires_in") {
        None | Some(Value::Null) => None,
        Some(seconds) => Some(
            seconds
                .as_u64()
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| seconds.checked_mul(1000))
                .and_then(|ms| now().checked_add(ms))
                .ok_or(TokenError::Failed("invalid_response"))?,
        ),
    };
    Ok(Tokens {
        access_token,
        refresh_token: string("refresh_token")?,
        expires_at,
        // RFC 6749 §5.1: an omitted scope means the request was granted.
        scopes: string("scope")?.map_or_else(|| requested.to_vec(), |scope| split(&scope)),
    })
}

/// RFC 7009 revocation of one token, best effort. Returns whether the
/// authorization server confirmed it.
pub(super) async fn revoke(
    http: &Http,
    endpoint: &Url,
    client: &Client,
    token: &str,
    hint: &str,
) -> bool {
    let mut form = vec![("token", token), ("token_type_hint", hint)];
    let request = authenticate(http.post(endpoint.clone()), &mut form, client);
    form_body(request, &form)
        .timeout(REVOCATION_TIMEOUT)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn form_body(
    request: reqwest_mcp::RequestBuilder,
    form: &[(&str, &str)],
) -> reqwest_mcp::RequestBuilder {
    request
        .header(
            reqwest_mcp::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form)
                .finish(),
        )
}

/// RFC 6749 §2.3.1: Basic credentials are form-encoded before Base64.
fn authenticate<'a>(
    request: reqwest_mcp::RequestBuilder,
    form: &mut Vec<(&'a str, &'a str)>,
    client: &'a Client,
) -> reqwest_mcp::RequestBuilder {
    match (client.auth_method, client.client_secret.as_deref()) {
        (AuthMethod::ClientSecretBasic, Some(secret)) => {
            let encode = |value: &str| {
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            };
            request.basic_auth(encode(&client.client_id), Some(encode(secret)))
        }
        (AuthMethod::ClientSecretPost, Some(secret)) => {
            form.push(("client_id", &client.client_id));
            form.push(("client_secret", secret));
            request
        }
        _ => {
            form.push(("client_id", &client.client_id));
            request
        }
    }
}
