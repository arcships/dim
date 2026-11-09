//! A process plugin that mounts a rutis session over WebSocket.
//!
//! The link is one `Channel`: dialing it, or accepting it as a listener. A
//! session mounted on it is the usual rutis session; the link owns nothing
//! else. The host root provides `LocalId`; a link without one cannot greet.

use std::{path::PathBuf, sync::Arc, time::Duration};

use rutis::host::{BoxFuture, DynPlugin, Effect, ErrorKind, FiberState};
use rutis::{Ctx, Error};
use rutis_bridge::{
    LocalId, Session,
    channel::{Channel, Close},
    websocket::{
        self, Admitted, Credential, Limits, ListenerConfig, Refused, ServerTls, Trust, Upgrade,
    },
};
use serde::Deserialize;
use tracing::{info, warn};

/// How the link reaches its far end.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkMode {
    Dial,
    Listen,
}

/// Structural configuration of one link row.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkConfig {
    pub mode: LinkMode,
    /// `ws://`/`wss://` URL for dial, or the bind address for listen.
    pub address: String,
    #[serde(default)]
    pub token: Option<String>,
    /// The CA that signed the listener's certificate, for dial.
    #[serde(default)]
    pub ca_pem: Option<PathBuf>,
    /// The listener's certificate and key, for listen.
    #[serde(default)]
    pub cert_pem: Option<PathBuf>,
    #[serde(default)]
    pub key_pem: Option<PathBuf>,
}

pub struct LinkPlugin {
    config: LinkConfig,
}

impl LinkPlugin {
    pub fn new(config: LinkConfig) -> Self {
        Self { config }
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::from(ErrorKind::PluginFailed(Box::new(std::io::Error::other(
        message.into(),
    ))))
}

fn protocol() -> String {
    format!("rutis.{}", rutis_bridge::session::ENDPOINT_PROTOCOL)
}

impl DynPlugin for LinkPlugin {
    fn name(&self) -> &str {
        "WebSocketLink"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            if ctx.get::<LocalId>().is_none() {
                return Err(failed("the host provides no LocalId for its sessions"));
            }
            let task_ctx = ctx.clone();
            let config = self.config.clone();
            let task = tokio::spawn(async move {
                match config.mode {
                    LinkMode::Dial => dial(task_ctx, config).await,
                    LinkMode::Listen => listen(task_ctx, config).await,
                }
            });
            ctx.effect(move || async move {
                task.abort();
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

async fn dial(ctx: Ctx, config: LinkConfig) {
    let Some(token) = config.token.clone() else {
        warn!("link: a dialer requires a token");
        return;
    };
    let credential = Credential::Bearer(token);
    let trust = match &config.ca_pem {
        Some(ca) => match std::fs::read(ca) {
            Ok(ca) => Trust::only(ca),
            Err(error) => {
                warn!(ca = %ca.display(), %error, "link: the CA could not be read");
                return;
            }
        },
        None => Trust::default(),
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        match websocket::dial(
            &config.address,
            &protocol(),
            Some(&credential),
            &trust,
            &Limits::default(),
        )
        .await
        {
            Ok(channel) => {
                info!(address = %config.address, "link: connected");
                backoff = Duration::from_secs(1);
                let session = ctx.plugin(Session::new(channel));
                let mut states = session.watch();
                let _ = states
                    .wait_for(|snapshot| {
                        matches!(snapshot.state, FiberState::Failed | FiberState::Disposed)
                    })
                    .await;
                let _ = session.dispose().await;
                tokio::time::sleep(backoff).await;
            }
            Err(error) => {
                warn!(address = %config.address, %error, "link: dial failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

async fn listen(ctx: Ctx, config: LinkConfig) {
    let Some(token) = config.token.clone() else {
        warn!("link: a listener requires a token");
        return;
    };
    let (sender, mut accepted) = tokio::sync::mpsc::unbounded_channel::<Channel>();
    let protocol = protocol();
    let admit: websocket::Admit = Arc::new(move |upgrade: Upgrade<'_>| {
        if upgrade.bearer != Some(token.as_str()) {
            return Err(Refused::Forbidden("the link token is not accepted".into()));
        }
        let sender = sender.clone();
        Ok(Admitted {
            protocol: protocol.clone(),
            deliver: Box::new(move |channel| {
                let closer = channel.closer.clone();
                if sender.send(channel).is_err() {
                    closer.close(Close::Done);
                }
            }),
        })
    });
    let mut listener = match config.address.parse() {
        Ok(address) => ListenerConfig::new("link", address),
        Err(error) => {
            warn!(address = %config.address, %error, "link: invalid listen address");
            return;
        }
    };
    if let (Some(cert), Some(key)) = (&config.cert_pem, &config.key_pem) {
        match (std::fs::read(cert), std::fs::read(key)) {
            (Ok(certificate_pem), Ok(key_pem)) => {
                listener = listener.tls(ServerTls {
                    certificate_pem,
                    key_pem,
                    client_ca_pem: None,
                });
            }
            (certificate_bytes, key_bytes) => {
                warn!(
                    cert = %cert.display(),
                    key = %key.display(),
                    cert_error = ?certificate_bytes.err(),
                    key_error = ?key_bytes.err(),
                    "link: the listener TLS material could not be read"
                );
                return;
            }
        }
    }
    let _listener = match websocket::listen(&listener, &Limits::default(), admit) {
        Ok(listener) => listener,
        Err(error) => {
            warn!(address = %config.address, %error, "link: listen failed");
            return;
        }
    };
    info!(address = %config.address, "link: listening");
    while let Some(channel) = accepted.recv().await {
        info!("link: accepted a connection");
        let session = ctx.plugin(Session::new(channel));
        tokio::spawn(async move {
            let mut states = session.watch();
            let _ = states
                .wait_for(|snapshot| {
                    matches!(snapshot.state, FiberState::Failed | FiberState::Disposed)
                })
                .await;
            let _ = session.dispose().await;
        });
    }
}
