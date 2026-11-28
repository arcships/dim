mod proxy;
mod streams;
use rutis::{
    Ctx,
    host::{CtxHost, FiberState},
};
use rutis_bridge::{Close, LocalId, PeerId, Session, channel::Closer, pipe};
use std::{io, path::PathBuf, sync::Arc};

struct ShareEndpoint(PathBuf);

impl rutis::host::DynPlugin for ShareEndpoint {
    fn name(&self) -> &str {
        "terminal-transport"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> rutis::host::BoxFuture<'a, Result<rutis::host::Effect, rutis::Error>> {
        Box::pin(async move {
            ctx.provide(streams::TerminalTransport::new(self.0.clone()))?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

struct PipeEnd;
impl Closer for PipeEnd {
    fn close(&self, _: Close) {}
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
type Error = Box<dyn std::error::Error + Send + Sync>;
async fn run() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().ok_or("missing terminal transport role")?;
    let target = PathBuf::from(args.next().ok_or("missing endpoint")?);
    if mode == "discover" || mode == "announce" {
        return discovery(&target, if mode == "discover" { Some(args) } else { None }).await;
    }
    let root = Ctx::root()?;
    let serving = mode == "serve";
    if serving {
        root.plugin(ShareEndpoint(target.clone())).await?;
    } else if mode != "connect" {
        return Err("unknown terminal role".into());
    }
    root.provide(LocalId(PeerId::new(if serving {
        "dim-sharing"
    } else {
        "dim-terminal"
    })?))?;
    let channel = pipe::framed(io::stdin(), io::stdout(), Arc::new(PipeEnd));
    let greeted = Session::handshake(&root, channel).await?;
    if greeted.id()
        != if serving {
            "dim-terminal"
        } else {
            "dim-sharing"
        }
    {
        return Err("unexpected terminal peer".into());
    }
    let session = root.plugin(greeted);
    (&session).await?;
    let mut state = session.watch();
    let closed = state.wait_for(|s| matches!(s.state, FiberState::Disposed | FiberState::Failed));
    if serving {
        let _ = closed.await;
    } else {
        let ready = args.next().ok_or("missing readiness path")?;
        let connect = async {
            let service = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let key = rutis_bridge::session::host_key("terminalTransport");
                loop {
                    if let Some(service) = root.get_at(&key) {
                        return service;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await?;
            proxy::serve(service, &target, std::path::Path::new(&ready)).await
        };
        tokio::select! {
            _ = closed => {},
            result = connect => result?,
        }
    }
    root.shutdown().await?;
    Ok(())
}

async fn discovery(
    path: &std::path::Path,
    services: Option<impl Iterator<Item = std::ffi::OsString>>,
) -> Result<(), Error> {
    use rutis_host::{config::HostConfig, host::Host};
    let config = HostConfig::read(path)?;
    let host = Host::start(&config).await?;
    let run = async {
        host.start_rows(config.rows()).await?;
        match services {
            None => std::future::pending::<()>().await,
            Some(names) => {
                #[derive(serde::Deserialize, serde::Serialize)]
                struct Snapshot {
                    candidates: Vec<serde_json::Value>,
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let mut candidates = Vec::new();
                for name in names {
                    let name = name.to_str().ok_or("invalid discovery service")?;
                    if host.service(name).is_some() {
                        let snapshot: Snapshot =
                            rutis_bridge::session::call_service(&host.root(), name, "snapshot", ())
                                .await?;
                        candidates.extend(snapshot.candidates);
                    }
                }
                println!("{}", serde_json::to_string(&candidates)?);
            }
        }
        Ok::<_, Error>(())
    };
    let (stop, ended) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0; 1]);
        let _ = stop.send(());
    });
    let result = tokio::select! { r=run=>r, _=ended=>Ok(()) };
    host.root().shutdown().await?;
    result
}
