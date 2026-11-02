//! The client tree: the rutis root the `dimcode` client process owns
//! (docs/design/plugins/Tui.md). Its plugins hold the resources a client
//! invocation owns and release them in reverse order on every way out. The
//! daemon's own tree is a different process; the client reaches it only over
//! the protocol v3 connection `DaemonLink` provides.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use tokio::sync::oneshot;

use crate::args::Options;
use crate::browser;
use crate::client::{self, Client};
use crate::session_host::SessionHost;
use crate::ui::{
    self,
    tui::{Leave, WebOpened},
};

/// The protocol v3 connection after `hello`, provided by [`DaemonLink`]. One
/// owner at a time drives it: `take` hands it over. `None` when the link was
/// made with `start: false` and no daemon runs, or once it was taken.
pub struct Connection(Mutex<Option<Client>>);

impl Connection {
    /// Whether the connected daemon advertises `method` in `hello`.
    pub fn offers(&self, method: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|client| client.supports(&[method]))
    }

    pub fn take(&self) -> Option<Client> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// What a plugin that needs the connection says when it is gone.
const CONNECTION_TAKEN: &str = "App Server connection is not available";

/// Finds the daemon's endpoint under the product home, starts the daemon
/// when `start` and none answers, and says hello. The connection closes
/// with the service: dropping the last `Client` ends it. A connection that
/// drops while its owner runs is that owner's to report or reconnect; the
/// service is not withdrawn, so nothing depending on it is unloaded.
pub struct DaemonLink {
    pub options: Options,
    /// Start a daemon when none runs; `daemon stop` only looks for one.
    pub start: bool,
}

impl Plugin for DaemonLink {
    type Deps = ();

    fn name(&self) -> &str {
        "DaemonLink"
    }

    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let client = client::connect_inner(&self.options, self.start)
            .await
            .map_err(ErrorKind::PluginFailed)?;
        ctx.provide(Connection(Mutex::new(client)))?;
        Ok(())
    }
}

/// The terminal interface: the session thread (`SessionHost`, on the
/// connection) and the render thread (`ui::tui::run`). How the interface
/// ended goes to the composition root through `leave`.
pub struct Tui {
    pub env: ui::Env,
    pub options: Options,
    /// Taken by the load; the render thread sends the result once.
    pub leave: Mutex<Option<oneshot::Sender<Leave>>>,
}

impl Plugin for Tui {
    type Deps = (Arc<Connection>,);

    fn name(&self) -> &str {
        "Tui"
    }

    async fn apply(&self, ctx: &Ctx, (connection,): Self::Deps) -> rutis::Result {
        ctx.effect(|| async move {
            ui::terminal::restore_if_held();
            Ok(())
        })?;
        let client = connection
            .take()
            .ok_or_else(|| rutis::Error::from(ErrorKind::PluginFailed(CONNECTION_TAKEN.into())))?;
        let leave = self
            .leave
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| {
                rutis::Error::from(ErrorKind::PluginFailed("Tui was loaded twice".into()))
            })?;
        let language = ui::i18n::interface_locale(&self.options.home).code();
        let host = SessionHost::start(self.options.clone(), client, language);
        let env = self.env.clone();
        let render = std::thread::Builder::new()
            .name("dim-tui".into())
            .spawn(move || {
                let _ = leave.send(ui::tui::run(Box::new(host), env));
            })
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
        ctx.effect(|| async move {
            let deadline = std::time::Instant::now() + ui::terminal::EXIT_BUDGET;
            while !render.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if render.is_finished() {
                let _ = render.join();
            }
            Ok(())
        })?;
        Ok(())
    }
}

/// The foreground holder of the daemon's local web entry: opens it, says
/// where it is and keeps the connection, and with it the entry, until
/// Ctrl+C, SIGTERM or the daemon's end. `done` tells the composition root
/// which of those it was.
pub struct WebHold {
    pub port: Option<u16>,
    pub open_browser: bool,
    /// The product home, for the install hint when the Web is missing.
    pub home: std::path::PathBuf,
    /// Taken by the load; the wait reports once.
    pub done: Mutex<Option<oneshot::Sender<crate::Result<i32>>>>,
}

impl Plugin for WebHold {
    type Deps = (Arc<Connection>,);

    fn name(&self) -> &str {
        "WebHold"
    }

    async fn apply(&self, ctx: &Ctx, (connection,): Self::Deps) -> rutis::Result {
        let mut client = connection
            .take()
            .ok_or_else(|| rutis::Error::from(ErrorKind::PluginFailed(CONNECTION_TAKEN.into())))?;
        let done = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| {
                rutis::Error::from(ErrorKind::PluginFailed("WebHold was loaded twice".into()))
            })?;
        let signals = Signals::listen()
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
        let opened = self
            .open(&mut client)
            .await
            .map_err(ErrorKind::PluginFailed)?;
        if !opened {
            let _ = done.send(Ok(1));
            return Ok(());
        }
        let wait = tokio::spawn(async move {
            let _ = done.send(hold(client, signals).await);
        });
        ctx.effect(|| async move {
            wait.abort();
            Ok(())
        })?;
        Ok(())
    }
}

impl WebHold {
    /// Opens the entry and shows where it is; `false` when the daemon has no
    /// Web workspace, after saying how to get it. Only `dimcode web` loads
    /// this holder; TUI `/web` uses the daemon switch.
    async fn open(&self, client: &mut Client) -> crate::Result<bool> {
        #[derive(serde::Deserialize)]
        struct Opened {
            url: String,
            address: String,
        }
        let params = match self.port {
            Some(port) => serde_json::json!({ "port": port }),
            None => serde_json::json!({}),
        };
        let opened: Opened = match client.typed("web.open", params).await {
            Ok(opened) => opened,
            Err(error) => {
                let executable = std::env::current_exe().unwrap_or_default();
                let Some((problem, fix)) = client::web_hint(&error, &executable, &self.home) else {
                    return Err(error);
                };
                eprintln!("{problem}.\n{fix}.");
                return Ok(false);
            }
        };
        let fragment = opened.url.split_once('#').map_or("", |(_, f)| f);
        let url = format!("http://{}/", opened.address);
        let sign_in = format!("{url}#{fragment}");
        let opened_browser = self.open_browser && browser::open(&sign_in);
        ui::tui::print_web_panel(&WebOpened {
            url: &url,
            sign_in: (!opened_browser).then_some(sign_in.as_str()),
            session_id: None,
        });
        Ok(true)
    }
}

/// Ctrl+C and SIGTERM, from the moment they are listened for.
pub(crate) struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Signals {
    pub(crate) fn listen() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    pub(crate) async fn received(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Waits in the foreground until Ctrl+C or SIGTERM, which ends the hold,
/// or until the daemon closes the connection, which is an error.
async fn hold(mut client: Client, mut signals: Signals) -> crate::Result<i32> {
    tokio::select! {
        _ = signals.received() => {}
        error = async { loop { if let Err(error) = client.next().await { break error } } } => {
            return Err(format!("The App Server stopped; the Web workspace is closed ({error})").into());
        }
    }
    println!();
    println!("Web workspace stopped.");
    Ok(0)
}
