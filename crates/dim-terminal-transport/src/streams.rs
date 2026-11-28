use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex as AsyncMutex,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const CHUNK: usize = 48 * 1024;
pub trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}
type Socket = Box<dyn Duplex>;
struct Stream {
    close_on_input_eof: bool,
    read: AsyncMutex<tokio::io::ReadHalf<Socket>>,
    write: AsyncMutex<tokio::io::WriteHalf<Socket>>,
    closed: CancellationToken,
}

#[derive(Serialize, Deserialize)]
pub struct ReturnAddress {
    pub id: String,
    pub address: String,
}

struct PendingLeaf {
    listener: Mutex<Option<TcpListener>>,
    token: String,
    closed: CancellationToken,
}

pub struct TerminalTransport {
    endpoint: PathBuf,
    streams: Mutex<HashMap<String, Arc<Stream>>>,
    listeners: Arc<Mutex<HashMap<String, Arc<PendingLeaf>>>>,
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "unknown terminal stream")
}
impl TerminalTransport {
    pub fn new(endpoint: PathBuf) -> Self {
        Self {
            endpoint,
            streams: Default::default(),
            listeners: Default::default(),
        }
    }
    fn insert(&self, id: String, socket: Socket, close_on_input_eof: bool) {
        let (read, write) = tokio::io::split(socket);
        self.streams.lock().unwrap().insert(
            id,
            Arc::new(Stream {
                close_on_input_eof,
                read: AsyncMutex::new(read),
                write: AsyncMutex::new(write),
                closed: CancellationToken::new(),
            }),
        );
    }
    fn stream(&self, id: &str) -> io::Result<Arc<Stream>> {
        self.streams
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(gone)
    }
}

#[rutis::service]
impl TerminalTransport {
    pub async fn open(&self) -> io::Result<String> {
        #[cfg(unix)]
        let socket = {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            let meta = std::fs::symlink_metadata(&self.endpoint)?;
            let uid = unsafe { libc::geteuid() };
            if !meta.file_type().is_socket() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "expected private App Server socket",
                ));
            }
            let stream = tokio::net::UnixStream::connect(&self.endpoint).await?;
            if stream.peer_cred()?.uid() != uid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unexpected App Server owner",
                ));
            }
            stream
        };
        #[cfg(windows)]
        let socket = dim_client::pipe::connect(
            self.endpoint
                .to_str()
                .ok_or_else(|| io::Error::other("invalid pipe name"))?,
        )
        .await
        .map_err(|(_, error)| error)?;
        let id = Uuid::new_v4().to_string();
        self.insert(id.clone(), Box::new(socket), cfg!(windows));
        Ok(id)
    }
    pub async fn prepare_leaf(&self) -> io::Result<ReturnAddress> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let token = Uuid::new_v4().simple().to_string();
        let address = format!("tcp:{}#{token}", listener.local_addr()?);
        let id = Uuid::new_v4().to_string();
        let pending = Arc::new(PendingLeaf {
            listener: Mutex::new(Some(listener)),
            token,
            closed: CancellationToken::new(),
        });
        let closed = pending.closed.clone();
        let listeners = self.listeners.clone();
        let expires = id.clone();
        listeners.lock().unwrap().insert(id.clone(), pending);
        tokio::spawn(async move {
            tokio::select! {
                _=closed.cancelled()=>{},
                _=tokio::time::sleep(Duration::from_secs(15))=>{
                    listeners.lock().unwrap().remove(&expires);
                    closed.cancel();
                }
            }
        });
        Ok(ReturnAddress { id, address })
    }
    pub async fn accept_leaf(&self, id: String) -> io::Result<()> {
        let pending = self
            .listeners
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(gone)?;
        let listener = pending.listener.lock().unwrap().take().ok_or_else(gone)?;
        let expected = format!("{}\n", pending.token);
        let accept = async {
            loop {
                let (mut conn, _) = listener.accept().await?;
                let mut supplied = vec![0; expected.len()];
                if matches!(
                    tokio::time::timeout(Duration::from_secs(1), conn.read_exact(&mut supplied))
                        .await,
                    Ok(Ok(_))
                ) && supplied == expected.as_bytes()
                {
                    return Ok::<_, io::Error>(conn);
                }
            }
        };
        let socket = tokio::select! {
            _ = pending.closed.cancelled() => return Err(gone()),
            socket = accept => socket?,
        };
        let mut listeners = self.listeners.lock().unwrap();
        listeners.remove(&id).ok_or_else(gone)?.closed.cancel();
        self.insert(id, Box::new(socket), false);
        Ok(())
    }
    pub async fn read(&self, id: String) -> io::Result<Option<String>> {
        let stream = self.stream(&id)?;
        let mut buf = vec![0; CHUNK];
        let mut reader = stream.read.lock().await;
        let count = tokio::select! {
            _ = stream.closed.cancelled() => return Err(gone()),
            n = reader.read(&mut buf) => n?,
        };
        Ok((count != 0).then(|| STANDARD.encode(&buf[..count])))
    }
    pub async fn write(&self, id: String, data: String) -> io::Result<()> {
        if data.len() > CHUNK.div_ceil(3) * 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal block too large",
            ));
        }
        let bytes = STANDARD.decode(data).map_err(io::Error::other)?;
        let stream = self.stream(&id)?;
        let mut writer = stream.write.lock().await;
        tokio::select! {
            _ = stream.closed.cancelled() => Err(gone()),
            result = writer.write_all(&bytes) => result,
        }
    }
    pub async fn finish_write(&self, id: String) -> io::Result<()> {
        let stream = self.stream(&id)?;
        if stream.close_on_input_eof {
            self.close(id);
            return Ok(());
        }
        stream.write.lock().await.shutdown().await
    }
    pub fn close(&self, id: String) {
        if let Some(pending) = self.listeners.lock().unwrap().remove(&id) {
            pending.closed.cancel();
        }
        if let Some(stream) = self.streams.lock().unwrap().remove(&id) {
            stream.closed.cancel()
        }
    }
}
