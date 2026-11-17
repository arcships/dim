pub(crate) mod management;
use crate::managed_oauth::{self, Service};
use dim_agent_capabilities::file_storage::FileStorage;
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

pub use managed_oauth::{
    AttemptStatus, AuthStatus, BrowserBegin, Config as OpenAICodexOAuthConfig, CredentialLease,
    OAuthError,
};
pub type Result<T> = managed_oauth::Result<T>;

pub struct OpenAICodexOAuthService(Arc<Service>);
impl OpenAICodexOAuthService {
    pub async fn status(&self, c: &CancellationToken) -> Result<AuthStatus> {
        self.0.status(c).await
    }
    pub async fn begin_browser(&self, c: &CancellationToken) -> Result<BrowserBegin> {
        self.0.begin_browser(c).await
    }
    pub async fn complete_browser(
        &self,
        id: Uuid,
        callback: Url,
        c: &CancellationToken,
    ) -> Result<()> {
        self.0.complete_browser(id, callback, c).await
    }
    pub async fn attempt_status(&self, id: Uuid) -> Result<AttemptStatus> {
        self.0.attempt_status(id).await
    }
    pub async fn cancel_attempt(&self, id: Uuid) -> Result<()> {
        self.0.cancel_attempt(id).await
    }
    pub async fn logout(&self, c: &CancellationToken) -> Result<()> {
        self.0.logout(c).await
    }
    pub async fn recover(&self, c: &CancellationToken) -> Result<()> {
        self.0.recover(c).await
    }
    pub async fn credential(
        &self,
        rejected: Option<&CredentialLease>,
        c: &CancellationToken,
    ) -> Result<CredentialLease> {
        self.0.credential(rejected, c).await
    }
    pub async fn finish_rejected(
        &self,
        rejected: &CredentialLease,
        c: &CancellationToken,
    ) -> Result<()> {
        self.0.finish_rejected(rejected, c).await
    }
    /// Feature `testing` only: whether a token operation has built the client.
    #[cfg(feature = "testing")]
    pub fn http_client_initialized(&self) -> bool {
        self.0.client_initialized()
    }
}

pub struct OpenAICodexOAuthPlugin {
    root: usize,
    config: OpenAICodexOAuthConfig,
    files: Key<FileStorage>,
    methods: Key<crate::methods::ManagementMethods>,
}
impl OpenAICodexOAuthPlugin {
    pub fn new(names: &Names<'_>, root: usize) -> rutis::Result<Self> {
        Self::with_config(names, root, production())
    }
    pub fn with_config(
        names: &Names<'_>,
        root: usize,
        config: OpenAICodexOAuthConfig,
    ) -> rutis::Result<Self> {
        Ok(Self {
            root,
            config,
            files: names.key()?,
            methods: names.key()?,
        })
    }
}
pub(crate) fn production() -> OpenAICodexOAuthConfig {
    OpenAICodexOAuthConfig {
        issuer: Url::parse("https://auth.openai.com").unwrap(),
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
        scope: "openid profile email offline_access api.connectors.read api.connectors.invoke"
            .into(),
        authorization: Some(Url::parse("https://auth.openai.com/oauth/authorize").unwrap()),
        token: Url::parse("https://auth.openai.com/oauth/token").unwrap(),
        device: None,
        redirect: Some(Url::parse("http://localhost:1455/auth/callback").unwrap()),
        relay: Url::parse("https://chatgpt.com/backend-api/codex").unwrap(),
    }
}
impl DynPlugin for OpenAICodexOAuthPlugin {
    fn name(&self) -> &str {
        "OpenAICodexOAuth"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.files.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let service = Service::new(
                files,
                self.root,
                "openaiCodex",
                self.config.clone(),
                &production(),
            )
            .map_err(|e| rutis::Error::from(ErrorKind::PluginFailed(e.into())))?;
            let closing = service.clone();
            if let Err(error) = ctx.effect(move || async move {
                closing.close().await;
                Ok(())
            }) {
                service.close().await;
                return Err(error);
            }
            let service = Arc::new(OpenAICodexOAuthService(service));
            management::provide(ctx, &methods, service.clone())?;
            ctx.provide_as::<OpenAICodexOAuthService>(service)?;
            Ok(Effect::Done)
        })
    }
}
