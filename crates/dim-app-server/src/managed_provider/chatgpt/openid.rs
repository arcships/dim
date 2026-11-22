use super::{ChatGpt, Grant, ManagedError, Result, now};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;
use url::Url;

const OIDC_DOCUMENT: &str = "/.well-known/openid-configuration";

#[derive(Deserialize)]
pub(super) struct Discovery {
    issuer: Url,
    jwks_uri: Url,
    pub(super) revocation_endpoint: Url,
}

#[derive(Deserialize)]
pub(super) struct TokenReply {
    access_token: String,
    refresh_token: String,
    pub(super) id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    earliest_refresh_at: Option<RefreshTime>,
    scope: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RefreshTime {
    Unix(u64),
    Rfc3339(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
pub(super) struct Identity {
    iss: String,
    pub(super) sub: String,
    aud: Audience,
    azp: Option<String>,
    nonce: String,
    exp: u64,
    pub(super) email: Option<String>,
    pub(super) name: Option<String>,
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
struct Keys {
    keys: Vec<Key>,
}

#[derive(Deserialize)]
struct Key {
    kid: String,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

impl ChatGpt {
    pub(super) async fn client(&self) -> Result<&Client> {
        self.client
            .get_or_try_init(|| async {
                Client::builder()
                    .timeout(Duration::from_secs(15))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| ManagedError::Transient)
            })
            .await
    }

    pub(super) async fn discovery(&self) -> Result<&Discovery> {
        self.discovery
            .get_or_try_init(|| async {
                let url = self.issuer.join(OIDC_DOCUMENT).unwrap();
                let discovery: Discovery =
                    receive(self.client().await?.get(url), 64 * 1024).await?;
                if discovery.issuer.origin() != self.issuer.origin()
                    || [&discovery.jwks_uri, &discovery.revocation_endpoint]
                        .iter()
                        .any(|url| {
                            url.origin() != self.issuer.origin()
                                || !url.username().is_empty()
                                || url.password().is_some()
                                || url.fragment().is_some()
                        })
                {
                    return Err(ManagedError::InvalidResponse);
                }
                Ok(discovery)
            })
            .await
    }

    pub(super) async fn identity(
        &self,
        id_token: &str,
        client_id: &str,
        nonce: &str,
    ) -> Result<Identity> {
        let (signed, signature) = id_token
            .rsplit_once('.')
            .ok_or(ManagedError::IdentityRejected)?;
        let (header, claims) = signed
            .split_once('.')
            .ok_or(ManagedError::IdentityRejected)?;
        let header: JwtHeader = jwt_part(header)?;
        if header.alg != "RS256" || header.kid.is_empty() {
            return Err(ManagedError::IdentityRejected);
        }
        let keys: Keys = receive(
            self.client()
                .await?
                .get(self.discovery().await?.jwks_uri.clone()),
            256 * 1024,
        )
        .await?;
        let key = keys
            .keys
            .into_iter()
            .find(|key| key.kid == header.kid && key.kty == "RSA")
            .ok_or(ManagedError::IdentityRejected)?;
        let n = jwt_bytes(key.n.as_deref().ok_or(ManagedError::IdentityRejected)?)?;
        let e = jwt_bytes(key.e.as_deref().ok_or(ManagedError::IdentityRejected)?)?;
        ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
            .verify(
                &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                signed.as_bytes(),
                &jwt_bytes(signature)?,
            )
            .map_err(|_| ManagedError::IdentityRejected)?;
        let identity: Identity = jwt_part(claims)?;
        let audience = match &identity.aud {
            Audience::One(id) => id == client_id,
            Audience::Many(ids) => {
                ids.iter().any(|id| id == client_id)
                    && (ids.len() == 1 || identity.azp.as_deref() == Some(client_id))
            }
        };
        if identity.iss != self.issuer.origin().ascii_serialization()
            || !audience
            || identity
                .azp
                .as_deref()
                .is_some_and(|party| party != client_id)
            || identity.exp <= now()
            || identity.nonce != nonce
            || identity.sub.trim().is_empty()
        {
            return Err(ManagedError::IdentityRejected);
        }
        Ok(identity)
    }
}

impl TokenReply {
    pub(super) fn grant(self) -> Result<Grant> {
        let mut scopes: Vec<String> = self
            .scope
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect();
        scopes.sort_unstable();
        scopes.dedup();
        if !has_plan(&scopes) {
            return Err(ManagedError::PlanNotGranted);
        }
        if self.access_token.is_empty()
            || self.refresh_token.is_empty()
            || self.expires_in == 0
            || !self.token_type.eq_ignore_ascii_case("Bearer")
        {
            return Err(ManagedError::InvalidResponse);
        }
        Ok(Grant {
            access: self.access_token,
            refresh: self.refresh_token,
            expires_at: now()
                .checked_add(self.expires_in)
                .ok_or(ManagedError::InvalidResponse)?,
            earliest_refresh_at: self
                .earliest_refresh_at
                .map(|refresh| match refresh {
                    RefreshTime::Unix(seconds) => Ok(seconds),
                    RefreshTime::Rfc3339(text) => time::OffsetDateTime::parse(
                        &text,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .ok()
                    .and_then(|date| date.unix_timestamp().try_into().ok())
                    .ok_or(ManagedError::InvalidResponse),
                })
                .transpose()?,
            scopes,
        })
    }
}

pub(super) fn has_plan(scopes: &[String]) -> bool {
    [
        "chatgpt.tokens.use.direct",
        "resource.invoke",
        "offline_access",
    ]
    .iter()
    .all(|required| scopes.iter().any(|scope| scope == required))
}

pub(super) async fn receive<T: DeserializeOwned>(
    request: RequestBuilder,
    max_bytes: usize,
) -> Result<T> {
    let mut response = request.send().await.map_err(|_| ManagedError::Transient)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ManagedError::Transient)?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ManagedError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        #[derive(Deserialize)]
        struct OAuthFailure {
            error: String,
        }
        if let Ok(failure) = serde_json::from_slice::<OAuthFailure>(&bytes)
            && matches!(
                failure.error.as_str(),
                "invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused"
            )
        {
            return Err(ManagedError::RefreshRejected);
        }
        return Err(ManagedError::Transient);
    }
    serde_json::from_slice(&bytes).map_err(|_| ManagedError::InvalidResponse)
}

fn jwt_bytes(part: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| ManagedError::IdentityRejected)
}

fn jwt_part<T: DeserializeOwned>(part: &str) -> Result<T> {
    serde_json::from_slice(&jwt_bytes(part)?).map_err(|_| ManagedError::IdentityRejected)
}
