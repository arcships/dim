use crate::{
    Result,
    args::Options,
    client,
    model::{
        session::{Command as SessionCommand, SessionPort, Update},
        view::ViewSnapshot,
    },
    session_host::SessionHost,
    sharing,
};
use dim_client::Client;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    runtime::Handle,
};

#[derive(Deserialize)]
pub(crate) struct Device {
    pub name: String,
    pub addresses: Vec<String>,
    pub verified: bool,
}

#[derive(Clone)]
pub(crate) struct Devices {
    pub options: Options,
    pub runtime: Handle,
    local_workspace: String,
}

#[derive(Debug)]
pub(crate) struct Pairing(String);
impl std::fmt::Display for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Pairing {}
#[derive(Deserialize)]
#[serde(untagged)]
enum TunnelReply {
    Ready(Remote),
    Pair { pairing: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Remote {
    name: String,
    address: String,
    identity: String,
    endpoint: PathBuf,
    working_directory: String,
}

pub(crate) struct Target {
    pub name: String,
    pub id: String,
    pub address: Option<String>,
    options: Options,
    pub(crate) config: dim_client::ConnectionConfig,
    pub(crate) instance: String,
    pub(crate) runtime: Handle,
    _lifetime: Option<ChildStdin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub running: bool,
}

#[derive(Deserialize)]
pub(crate) struct Directories {
    pub path: Option<String>,
    pub parent: Option<String>,
    pub directories: Vec<Directory>,
}

#[derive(Deserialize)]
pub(crate) struct Directory {
    pub name: String,
    pub path: String,
}

impl Devices {
    pub fn new(options: Options) -> Self {
        Self {
            local_workspace: options.local_workspace(),
            options,
            runtime: Handle::current(),
        }
    }

    pub async fn list(&self, discover: bool) -> Result<Vec<Device>> {
        let output = Command::from(sharing::command(&self.options.home)?)
            .arg(if discover { "discover" } else { "known" })
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub async fn connect(
        &self,
        address: Option<String>,
        token: String,
        remember: bool,
    ) -> Result<Arc<Target>> {
        let mut options = self.options.clone();
        let mut lifetime = None;
        let (name, id, route, workspace, endpoint) = if let Some(address) = address {
            #[derive(Serialize)]
            struct Pair {
                token: String,
                remember: bool,
            }
            let mut child = Command::from(sharing::command(&options.home)?)
                .args(["tunnel", &address])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let mut input = child.stdin.take().expect("piped input");
            let mut bytes = serde_json::to_vec(&Pair { token, remember })?;
            bytes.push(b'\n');
            input.write_all(&bytes).await?;
            let out = child.stdout.take().expect("piped output");
            let mut stderr = child.stderr.take().expect("piped errors");
            let ended = tokio::spawn(async move {
                let mut message = String::new();
                let _ = stderr.read_to_string(&mut message).await;
                let _ = child.wait().await;
                message
            });
            let mut line = String::new();
            if tokio::time::timeout(
                Duration::from_secs(30),
                BufReader::new(out).read_line(&mut line),
            )
            .await??
                == 0
            {
                drop(input);
                return Err(ended.await?.trim().to_owned().into());
            }
            let remote = match serde_json::from_str(&line)? {
                TunnelReply::Ready(remote) => remote,
                TunnelReply::Pair { pairing } => return Err(Box::new(Pairing(pairing))),
            };
            lifetime = Some(input);
            (
                remote.name,
                remote.identity,
                Some(remote.address),
                remote.working_directory,
                Some(remote.endpoint),
            )
        } else {
            (
                "This computer".into(),
                "local".into(),
                None,
                self.local_workspace.clone(),
                None,
            )
        };
        options.select_workspace(workspace, endpoint);
        let locale = crate::ui::i18n::interface_locale(&options.home);
        let link = client::connect_inner(&options, true, locale)
            .await?
            .ok_or("Computer is unavailable")?;
        Ok(Arc::new(Target {
            name: crate::sanitize::safe_line(&name),
            id,
            address: route,
            options,
            instance: link.client.instance_id().into(),
            config: link.client.connection_config().clone(),
            runtime: self.runtime.clone(),
            _lifetime: lifetime,
        }))
    }

    pub fn initial(&self, client: &Client) -> Arc<Target> {
        let remote = self.options.endpoint.is_some();
        let read = |key| std::env::var(key).ok();
        Arc::new(Target {
            name: if remote {
                read("DIMCODE_TERMINAL_NAME")
                    .map(|name| crate::sanitize::safe_line(&name))
                    .unwrap_or_else(|| "Remote computer".into())
            } else {
                "This computer".into()
            },
            id: if remote {
                read("DIMCODE_TERMINAL_ID").unwrap_or_else(|| client.instance_id().into())
            } else {
                "local".into()
            },
            address: remote.then(|| read("DIMCODE_TERMINAL_ADDRESS")).flatten(),
            options: self.options.clone(),
            instance: client.instance_id().into(),
            config: client.connection_config().clone(),
            runtime: self.runtime.clone(),
            _lifetime: None,
        })
    }
}

impl Target {
    pub(crate) fn keyboard_dir(&self) -> &std::path::Path {
        self.options.launch_dir()
    }

    pub async fn sessions(&self) -> Result<Vec<Session>> {
        Client::connect(&self.config)
            .await?
            .typed(
                "session.active",
                serde_json::Value::Object(Default::default()),
            )
            .await
    }

    pub async fn directories(&self, path: Option<String>) -> Result<Directories> {
        Directories::read(&mut Client::connect(&self.config).await?, path).await
    }

    pub async fn open(self: Arc<Self>, cwd: String, session: Option<String>) -> Result<Prepared> {
        let mut options = self.options.clone();
        options.select_workspace(cwd.clone(), options.endpoint.clone());
        options.resume = session.clone();
        let client = Client::connect(&self.config).await?;
        let locale = crate::ui::i18n::interface_locale(&options.home);
        let host = SessionHost::start(options, client, locale.code(), None);
        let mut view = Prepared {
            target: self,
            cwd,
            session,
            host,
            updates: Vec::new(),
        };
        if view.session.is_some() {
            view.wait_for_history(None).await?;
        }
        Ok(view)
    }
}

pub(crate) struct Prepared {
    pub target: Arc<Target>,
    pub cwd: String,
    pub session: Option<String>,
    pub host: SessionHost,
    pub updates: Vec<Update>,
}

impl Prepared {
    pub fn snapshot(&self) -> Option<&ViewSnapshot> {
        self.updates.iter().rev().find_map(|update| match update {
            Update::Snapshot(snapshot) => Some(snapshot.as_ref()),
            _ => None,
        })
    }

    pub async fn focus(mut self, line: String) -> Result<Self> {
        let available = self
            .snapshot()
            .is_some_and(|s| s.lines.iter().any(|l| l.id == line));
        if !available {
            return Err(format!("The draft's line is unavailable: {line}").into());
        }
        self.host.send(SessionCommand::FocusLine(line.clone()));
        self.wait_for_history(Some(&line)).await?;
        Ok(self)
    }

    async fn wait_for_history(&mut self, line: Option<&str>) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let batch = self.host.drain();
                for update in &batch {
                    match update {
                        Update::Failed(error)
                        | Update::StartupFailed { message: error, .. }
                        | Update::Shutdown { reason: error } => return Err(error.clone().into()),
                        _ => {}
                    }
                }
                self.updates.extend(batch);
                if let Some(s) = self.snapshot()
                    && self.session.as_deref() == Some(s.session_id.as_str())
                    && line.is_none_or(|line| line == s.focus)
                {
                    if let Some(error) = &s.window_error {
                        return Err(error.clone().into());
                    }
                    if s.history_complete {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "Timed out opening session history")?
    }
}

impl Directories {
    pub async fn read(client: &mut Client, path: Option<String>) -> Result<Self> {
        #[derive(Serialize)]
        struct Request {
            path: Option<String>,
        }
        client
            .typed(
                "workspace.files.directories",
                serde_json::to_value(Request { path })?,
            )
            .await
    }
}
