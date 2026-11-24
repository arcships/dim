//! Client-owned ACP transport and protocol projection over the public App Server.

pub mod auth;
mod commands;
mod config;
mod content;
mod gateway;
mod interaction;
mod lody;
mod mcp;
mod outbox;
mod projection;
mod providers;
mod rpc;

use agent_client_protocol::{Agent, Error, UntypedMessage};
use dim_client::Client;
use futures_util::{SinkExt, StreamExt};
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use std::{io, sync::Mutex};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::oneshot,
};
use tokio_util::{
    codec::{FramedRead, FramedWrite, LinesCodec},
    sync::CancellationToken,
    task::TaskTracker,
};

const FRAME_LIMIT: usize = 16 * 1024 * 1024;

/// Exclusive transport halves supplied by the native host.
pub struct Io {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
}

struct Inputs {
    client: Client,
    io: Io,
    agent_id: Option<String>,
    version: String,
    done: oneshot::Sender<dim_client::Result<()>>,
    login: Option<auth::Login>,
}

/// One ACP connection, its independent native connection and owned work.
pub struct AcpGateway(Mutex<Option<Inputs>>);

impl AcpGateway {
    /// `version` is the identity `initialize` advertises for this build, the
    /// label the native CLI reports.
    pub fn new(
        client: Client,
        io: Io,
        agent_id: Option<String>,
        version: String,
    ) -> (Self, oneshot::Receiver<dim_client::Result<()>>) {
        let (done, finished) = oneshot::channel();
        (
            Self(Mutex::new(Some(Inputs {
                client,
                io,
                agent_id,
                version,
                done,
                login: None,
            }))),
            finished,
        )
    }

    pub fn login(self, login: auth::Login) -> Self {
        self.0.lock().unwrap().as_mut().unwrap().login = Some(login);
        self
    }
}

impl Plugin for AcpGateway {
    type Deps = ();

    fn name(&self) -> &str {
        "AcpGateway"
    }

    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let inputs =
            self.0.lock().unwrap().take().ok_or_else(|| {
                ErrorKind::PluginFailed("AcpGateway I/O was already taken".into())
            })?;
        let stop = CancellationToken::new();
        let tasks = TaskTracker::new();
        let closing = stop.clone();
        let draining = tasks.clone();
        ctx.effect(move || async move {
            closing.cancel();
            draining.close();
            draining.wait().await;
            Ok(())
        })?;
        tasks.spawn(async move {
            let Inputs {
                client,
                io,
                agent_id,
                version,
                done,
                login,
            } = inputs;
            let result = serve(client, io, agent_id, version, login, stop).await;
            let _ = done.send(result);
        });
        Ok(())
    }
}

async fn serve(
    client: Client,
    io: Io,
    agent_id: Option<String>,
    version: String,
    login: Option<auth::Login>,
    stop: CancellationToken,
) -> dim_client::Result<()> {
    let tasks = TaskTracker::new();
    let native_tasks = TaskTracker::new();
    let native_stop = CancellationToken::new();
    let incoming_frames = FramedRead::new(io.reader, LinesCodec::new_with_max_length(FRAME_LIMIT))
        .map(|line| line.map_err(io::Error::other));
    let outbox = outbox::Outbox::default();
    let flushed = outbox.clone();
    let outgoing = futures_util::sink::unfold(
        FramedWrite::new(io.writer, LinesCodec::new()),
        move |mut writer, line: String| {
            let flushed = flushed.clone();
            async move {
                if line.len() > FRAME_LIMIT {
                    return Err(io::Error::other("ACP frame exceeds 16 MiB"));
                }
                tokio::time::timeout(std::time::Duration::from_secs(5), writer.send(line.clone()))
                    .await
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "ACP writer did not flush a frame within five seconds",
                        )
                    })?
                    .map_err(io::Error::other)?;
                flushed.flushed(&line);
                Ok(writer)
            }
        },
    );
    let (rpc, native) = rpc::Rpc::start(client, &native_tasks, native_stop.clone());
    let mcp = mcp::Bindings::start(rpc.clone(), &tasks, stop.clone());
    let gateway_mcp = mcp.clone();
    let (incoming, messages) = tokio::sync::mpsc::channel(128);
    let notifications = incoming.clone();
    let foreground = incoming.clone();
    let foreground_stop = stop.clone();
    let replies = outbox.clone();
    let foreground_outbox = outbox.clone();
    let foreground_tasks = tasks.clone();
    let reply_tasks = tasks.clone();
    let reply_stop = stop.clone();
    let requests = std::sync::Arc::new(tokio::sync::Semaphore::new(128));
    let connection = Agent
        .builder()
        .name("Dim ACP Gateway")
        .on_receive_request(
            async move |request: UntypedMessage, responder, _cx| {
                let permit = requests.clone().try_acquire_owned().ok();
                let admitted = permit.is_some();
                let responder = outbox::Reply::new(
                    responder,
                    replies.clone(),
                    &reply_tasks,
                    reply_stop.clone(),
                    permit,
                );
                if !admitted {
                    return responder
                        .respond_with_error(Error::new(-32001, "ACP has 128 requests in flight"));
                }
                if let Err(error) =
                    incoming.try_send(gateway::Incoming::Request(request, Box::new(responder)))
                    && let gateway::Incoming::Request(_, responder) = error.into_inner()
                {
                    (*responder)
                        .respond_with_error(Error::new(-32001, "ACP request queue is full"))?;
                }
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: UntypedMessage, _cx| {
                notifications
                    .try_send(gateway::Incoming::Notification(notification))
                    .map_err(|_| Error::new(-32001, "ACP notification queue is full"))?;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(
            agent_client_protocol::Lines::new(Box::pin(outgoing), incoming_frames),
            async move |cx| {
                gateway::Gateway::new(
                    rpc,
                    cx,
                    foreground,
                    foreground_tasks,
                    foreground_stop,
                    agent_id,
                    version,
                    login,
                    foreground_outbox,
                    gateway_mcp,
                )
                .run(messages, native)
                .await
            },
        );
    let result = tokio::select! {
        result = connection => result.map_err(Into::into),
        () = stop.cancelled() => Ok(()),
        () = outbox.failed() => Err(outbox.error().into()),
    };
    stop.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), mcp.close()).await;
    tasks.close();
    // Login waiters can cancel their owned pending attempts while RPC is live.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), tasks.wait()).await;
    native_stop.cancel();
    native_tasks.close();
    native_tasks.wait().await;
    tasks.wait().await;
    result
}
