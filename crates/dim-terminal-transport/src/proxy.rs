use crate::{
    Error,
    streams::{CHUNK, Duplex, ReturnAddress},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis_bridge::session::{HostDispatch, Value, settle};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinSet;

type Face = Arc<dyn HostDispatch>;

async fn call<T: DeserializeOwned>(
    face: &Face,
    method: &str,
    args: impl Serialize,
) -> Result<T, Error> {
    let args = Value::Data(serde_json::to_value(args)?);
    let value = settle(face.invoke(method, args)?).await?;
    Ok(serde_json::from_value(
        if matches!(value, Value::Undefined) {
            serde_json::Value::Null
        } else {
            value.json()?
        },
    )?)
}

async fn relay(face: &Face, id: &str, socket: impl Duplex, named_pipe: bool) -> Result<(), Error> {
    let (mut input, mut output) = tokio::io::split(socket);
    let send = async {
        let mut buf = vec![0; CHUNK];
        loop {
            let n = input.read(&mut buf).await?;
            if n == 0 {
                return call::<()>(face, "finishWrite", [id]).await;
            }
            call::<()>(face, "write", (id, STANDARD.encode(&buf[..n]))).await?;
        }
    };
    let receive = async {
        while let Some(chunk) = call::<Option<String>>(face, "read", [id]).await? {
            output.write_all(&STANDARD.decode(chunk)?).await?;
        }
        output.shutdown().await?;
        Ok::<_, Error>(())
    };
    if named_pipe {
        return tokio::select! {
            result = send => result,
            result = receive => result,
        };
    }
    tokio::try_join!(send, receive)?;
    Ok(())
}

async fn serve_rpc(face: Face, stream: impl Duplex) -> Result<(), Error> {
    let id: String = call(&face, "open", Vec::<()>::new()).await?;
    let result = relay(&face, &id, stream, cfg!(windows)).await;
    let _ = call::<()>(&face, "close", [id]).await;
    result
}

#[cfg(unix)]
async fn local_leaf(address: &str) -> Result<Box<dyn Duplex>, Error> {
    let path = address
        .strip_prefix("unix:")
        .ok_or("invalid local leaf socket")?;
    Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
}
#[cfg(windows)]
async fn local_leaf(address: &str) -> Result<Box<dyn Duplex>, Error> {
    let (address, token) = address
        .strip_prefix("tcp:")
        .and_then(|a| a.split_once('#'))
        .ok_or("invalid local leaf address")?;
    let address: std::net::SocketAddr = address.parse()?;
    if !address.ip().is_loopback() {
        return Err("expected a loopback leaf".into());
    }
    let mut s = tokio::net::TcpStream::connect(address).await?;
    s.write_all(format!("{token}\n").as_bytes()).await?;
    Ok(Box::new(s))
}

async fn serve_leaf(face: Face, mut control: impl Duplex) -> Result<(), Error> {
    let mut addr = String::new();
    let n = BufReader::new((&mut control).take(4097))
        .read_line(&mut addr)
        .await?;
    if n > 4096 || !addr.ends_with('\n') {
        return Err("invalid leaf request".into());
    }
    let remote: ReturnAddress = call(&face, "prepareLeaf", Vec::<()>::new()).await?;
    let result = async {
        control
            .write_all(format!("{}\n", remote.address).as_bytes())
            .await?;
        call::<()>(&face, "acceptLeaf", [&remote.id]).await?;
        relay(&face, &remote.id, local_leaf(addr.trim_end()).await?, false).await
    }
    .await;
    let _ = call::<()>(&face, "close", [remote.id]).await;
    result
}

#[cfg(unix)]
struct Listener {
    socket: tokio::net::UnixListener,
    path: PathBuf,
}
#[cfg(unix)]
impl Listener {
    fn bind(path: PathBuf) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let socket = tokio::net::UnixListener::bind(&path)?;
        let owner = Self { socket, path };
        std::fs::set_permissions(&owner.path, std::fs::Permissions::from_mode(0o600))?;
        Ok(owner)
    }
    async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
        Ok(self.socket.accept().await?.0)
    }
}
#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
struct Listener {
    socket: tokio::net::windows::named_pipe::NamedPipeServer,
    path: PathBuf,
}
#[cfg(windows)]
impl Listener {
    fn bind(path: PathBuf) -> io::Result<Self> {
        let socket = dim_client::pipe::bind(
            path.to_str()
                .ok_or_else(|| io::Error::other("invalid pipe name"))?,
            true,
        )?;
        Ok(Self { socket, path })
    }
    async fn accept(&mut self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        self.socket.connect().await?;
        let next = dim_client::pipe::bind(
            self.path
                .to_str()
                .ok_or_else(|| io::Error::other("invalid pipe name"))?,
            false,
        )?;
        Ok(std::mem::replace(&mut self.socket, next))
    }
}

pub async fn serve(face: Face, endpoint: &Path, ready: &Path) -> Result<(), Error> {
    let mut rpc = Listener::bind(endpoint.to_owned())?;
    let mut leaf = Listener::bind(endpoint.with_extension("leaf"))?;
    std::fs::write(ready, b"ready\n")?;
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            stream=rpc.accept()=>{tasks.spawn(serve_rpc(face.clone(),stream?));}
            stream=leaf.accept()=>{tasks.spawn(serve_leaf(face.clone(),stream?));}
            Some(_)=tasks.join_next()=>{}
        }
    }
}
