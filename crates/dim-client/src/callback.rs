//! Host-owned loopback callback I/O; OAuth state and credentials stay with its caller.
use crate::Result;
use std::{
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use url::Url;

pub struct Capture {
    ipv4: TcpListener,
    ipv6: Option<TcpListener>,
    redirect: Url,
}
impl Capture {
    pub async fn bind(redirect: &str) -> Option<Self> {
        let redirect = Url::parse(redirect).ok()?;
        if redirect.scheme() != "http"
            || !matches!(
                redirect.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            )
        {
            return None;
        }
        let port = redirect.port_or_known_default()?;
        let ipv4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await.ok()?;
        let ipv6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await.ok()?;
        Some(Self {
            ipv4,
            ipv6: Some(ipv6),
            redirect,
        })
    }
    pub async fn bind_ipv4(path: &str) -> Result<Self> {
        let ipv4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let redirect = Url::parse(&format!("http://{}{path}", ipv4.local_addr()?))?;
        Ok(Self {
            ipv4,
            ipv6: None,
            redirect,
        })
    }

    pub fn redirect_uri(&self) -> &str {
        self.redirect.as_str()
    }

    pub async fn receive(self) -> Result<(String, TcpStream)> {
        loop {
            let (mut stream, _) = if let Some(ipv6) = &self.ipv6 {
                tokio::select! {
                    value = self.ipv4.accept() => value?,
                    value = ipv6.accept() => value?,
                }
            } else {
                self.ipv4.accept().await?
            };
            let request = tokio::time::timeout(Duration::from_secs(2), async {
                let mut request = Vec::new();
                let mut reader = BufReader::new((&mut stream).take(16385));
                loop {
                    let start = request.len();
                    if reader.read_until(b'\n', &mut request).await? == 0 || request.len() > 16384 {
                        return Err("Incomplete or oversized local callback request".into());
                    }
                    if &request[start..] == b"\r\n" {
                        return Ok::<_, crate::Error>(request);
                    }
                }
            })
            .await;
            let Ok(Ok(request)) = request else { continue };
            let line = request
                .split_inclusive(|byte| *byte == b'\n')
                .next()
                .unwrap();
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            if line.len() > 8192
                || fields.len() != 3
                || fields[0] != "GET"
                || !fields[1].starts_with('/')
                || fields[1].starts_with("//")
                || !matches!(fields[2], "HTTP/1.0" | "HTTP/1.1")
            {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            }
            if fields[1].split('?').next() != Some(self.redirect.path()) {
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            }
            return Ok((
                format!(
                    "{}{}",
                    self.redirect.origin().ascii_serialization(),
                    fields[1]
                ),
                stream,
            ));
        }
    }
}
