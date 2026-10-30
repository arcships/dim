use bytes::Bytes;
use flate2::{Decompress, FlushDecompress, Status, write::GzDecoder};
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const DOWNLOAD_LIMIT: usize = 5 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebError {
    pub code: &'static str,
    pub message: String,
}

impl WebError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WebError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone)]
pub struct NetworkConfig {
    pub allow_private_hosts: Vec<String>,
    pub proxy: Option<Url>,
}

pub fn hostname(url: &Url) -> String {
    url.host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_owned()
}

pub fn parse_url(value: &str) -> Result<Url, WebError> {
    if has_authority_at(value) {
        return Err(WebError::new(
            "invalid_input",
            "User information is not allowed in URLs.",
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| WebError::new("invalid_input", "Expected an absolute HTTP or HTTPS URL."))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(WebError::new(
            "invalid_input",
            "Expected an HTTP or HTTPS URL without user information.",
        ));
    }
    Ok(url)
}

fn has_authority_at(value: &str) -> bool {
    let lower = value.as_bytes();
    let prefix = if lower.len() >= 7 && lower[..7].eq_ignore_ascii_case(b"http://") {
        7
    } else if lower.len() >= 8 && lower[..8].eq_ignore_ascii_case(b"https://") {
        8
    } else {
        return false;
    };
    let rest = &value[prefix..];
    let authority = match rest.find(['/', '?', '#']) {
        Some(idx) => &rest[..idx],
        None => rest,
    };
    authority.contains('@')
}

pub fn domain(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 253
        || value.chars().any(|c| {
            c.is_whitespace() || matches!(c, '/' | '@' | ':' | '#' | '?' | '%' | '[' | ']' | '\\')
        })
    {
        return None;
    }
    let url = Url::parse(&format!("http://{value}")).ok()?;
    let host = hostname(&url);
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    let valid = host.split('.').all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 63
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && (bytes[bytes.len() - 1].is_ascii_lowercase()
                || bytes[bytes.len() - 1].is_ascii_digit())
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
    });
    valid.then_some(host)
}

pub fn proxy_url(value: Option<&str>) -> Result<Option<Url>, WebError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let proxy = parse_url(value)
        .map_err(|_| WebError::new("connect_failed", "The configured proxy is unusable."))?;
    if proxy.path() != "/" || proxy.query().is_some() || proxy.fragment().is_some() {
        return Err(WebError::new(
            "connect_failed",
            "The configured proxy is unusable.",
        ));
    }
    Ok(Some(proxy))
}

#[derive(Clone, Copy)]
enum Cidr {
    V4(u32, u32),
    V6(u128, u32),
}

impl Cidr {
    fn parse(raw: &str) -> Option<Self> {
        let (addr_part, prefix_part) = raw.split_once('/')?;
        if prefix_part.is_empty() || !prefix_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let prefix: u32 = prefix_part.parse().ok()?;
        match addr_part.parse::<IpAddr>().ok()? {
            IpAddr::V4(v4) if prefix <= 32 => Some(Self::V4(u32::from(v4), prefix)),
            IpAddr::V6(v6) if prefix <= 128 => Some(Self::V6(u128::from(v6), prefix)),
            _ => None,
        }
    }

    fn matches(self, ip: IpAddr) -> bool {
        match (self, ip) {
            (Self::V4(net, bits), IpAddr::V4(v4)) => {
                let mask = if bits == 0 { 0 } else { !0u32 << (32 - bits) };
                (u32::from(v4) & mask) == (net & mask)
            }
            (Self::V6(net, bits), IpAddr::V6(v6)) => {
                let mask = if bits == 0 { 0 } else { !0u128 << (128 - bits) };
                (u128::from(v6) & mask) == (net & mask)
            }
            _ => false,
        }
    }
}

fn effective_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let octets = v6.octets();
            if octets[..12].iter().all(|&b| b == 0) {
                return IpAddr::V4(Ipv4Addr::new(
                    octets[12], octets[13], octets[14], octets[15],
                ));
            }
            IpAddr::V6(v6)
        }
    }
}

const BENCHMARK_V4: Cidr = Cidr::V4(0xc612_0000, 15);

fn is_public_ip(ip: IpAddr) -> bool {
    match effective_ip(ip) {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            const NON_PUBLIC_V4: &[Cidr] = &[
                Cidr::V4(0x0000_0000, 8),
                Cidr::V4(0x0a00_0000, 8),
                Cidr::V4(0x6440_0000, 10),
                Cidr::V4(0x7f00_0000, 8),
                Cidr::V4(0xa9fe_0000, 16),
                Cidr::V4(0xac10_0000, 12),
                Cidr::V4(0xc000_0000, 24),
                Cidr::V4(0xc000_0200, 24),
                Cidr::V4(0xc058_6300, 24),
                Cidr::V4(0xc0a8_0000, 16),
                BENCHMARK_V4,
                Cidr::V4(0xc633_6400, 24),
                Cidr::V4(0xcb00_7100, 24),
                Cidr::V4(0xe000_0000, 4),
                Cidr::V4(0xf000_0000, 4),
            ];
            !NON_PUBLIC_V4
                .iter()
                .any(|cidr| cidr.matches(IpAddr::V4(Ipv4Addr::from(bits))))
        }
        IpAddr::V6(v6) => {
            let octets = v6.octets();
            if (octets[0] & 0xe0) != 0x20 {
                return false;
            }
            const NON_PUBLIC_V6: &[Cidr] = &[
                Cidr::V6(0x2001_0000_0000_0000_0000_0000_0000_0000, 23),
                Cidr::V6(0x2001_0db8_0000_0000_0000_0000_0000_0000, 32),
                Cidr::V6(0x2002_0000_0000_0000_0000_0000_0000_0000, 16),
                Cidr::V6(0x3fff_0000_0000_0000_0000_0000_0000_0000, 20),
            ];
            !NON_PUBLIC_V6
                .iter()
                .any(|cidr| cidr.matches(IpAddr::V6(v6)))
        }
    }
}

struct Policy {
    allowed_host: bool,
    ranges: Vec<Cidr>,
}

impl Policy {
    fn new(
        host: &str,
        entries: &[String],
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Self, WebError> {
        let mut hosts = HashSet::new();
        let mut ranges = Vec::new();
        let mut invalid = false;
        for entry in entries {
            if entry.contains('/') {
                if let Some(cidr) = Cidr::parse(entry) {
                    ranges.push(cidr);
                } else {
                    invalid = true;
                }
            } else if let Some(valid) = domain(entry) {
                hosts.insert(valid);
            } else {
                invalid = true;
            }
        }
        if invalid {
            diagnostics.push(Diagnostic {
                code: "invalid_private_host",
                message: "Ignored an invalid private-host exception.".into(),
            });
        }
        let allowed_host = hosts.contains(host);
        if !allowed_host && (host == "localhost" || host.ends_with(".localhost")) {
            return Err(WebError::new(
                "blocked_address",
                "Localhost names are blocked.",
            ));
        }
        Ok(Self {
            allowed_host,
            ranges,
        })
    }

    fn check(&self, addresses: &[IpAddr]) -> Result<(), WebError> {
        for &address in addresses {
            let eff = effective_ip(address);
            let permitted = self.allowed_host
                || self
                    .ranges
                    .iter()
                    .any(|cidr| cidr.matches(address) || cidr.matches(eff));
            if !permitted && !is_public_ip(address) {
                if BENCHMARK_V4.matches(eff) {
                    return Err(WebError::new(
                        "blocked_address",
                        "Reserved benchmark addresses are blocked. If using Fake-IP DNS, configure real-address DNS answers or an explicit trusted-host exception; see docs/design/plugins/Web.md (Address policy).",
                    ));
                }
                return Err(WebError::new(
                    "blocked_address",
                    "Non-public addresses are blocked.",
                ));
            }
        }
        Ok(())
    }
}

/// Names the library tests resolve without the system resolver. A hostname
/// absent from the map uses the system resolver.
#[cfg(test)]
mod test_dns {
    use std::net::IpAddr;
    use std::sync::Mutex;

    #[derive(Clone)]
    pub enum Answer {
        Ips(Vec<IpAddr>),
        Hang,
    }

    struct Slot {
        answer: Answer,
        hits: usize,
    }

    static MAP: Mutex<Option<std::collections::HashMap<String, Slot>>> = Mutex::new(None);

    pub fn insert(host: &str, answer: Answer) {
        let mut guard = MAP.lock().unwrap();
        guard
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(host.to_owned(), Slot { answer, hits: 0 });
    }

    pub fn remove(host: &str) {
        if let Some(map) = MAP.lock().unwrap().as_mut() {
            map.remove(host);
        }
    }

    pub fn hits(host: &str) -> usize {
        MAP.lock()
            .unwrap()
            .as_ref()
            .and_then(|map| map.get(host))
            .map(|slot| slot.hits)
            .unwrap_or(0)
    }

    pub fn lookup(host: &str) -> Option<Answer> {
        let mut guard = MAP.lock().unwrap();
        let slot = guard.as_mut()?.get_mut(host)?;
        slot.hits += 1;
        Some(slot.answer.clone())
    }
}

async fn resolve_addresses(
    host: &str,
    cancel: &CancellationToken,
) -> Result<Vec<IpAddr>, WebError> {
    if cancel.is_cancelled() {
        return Err(WebError::new("cancelled", "Call cancelled."));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    #[cfg(test)]
    if let Some(answer) = test_dns::lookup(host) {
        return match answer {
            test_dns::Answer::Hang => std::future::pending().await,
            test_dns::Answer::Ips(ips) if ips.is_empty() => {
                Err(WebError::new("dns_failed", "The host has no addresses."))
            }
            test_dns::Answer::Ips(ips) => Ok(ips),
        };
    }
    let resolved: Vec<IpAddr> = tokio::select! {
        _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
        res = tokio::net::lookup_host((host, 0)) => {
            res.map_err(|_| WebError::new("dns_failed", "The host could not be resolved."))?
                .map(|addr| addr.ip())
                .collect()
        }
    };
    if resolved.is_empty() {
        return Err(WebError::new("dns_failed", "The host has no addresses."));
    }
    Ok(resolved)
}

pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

struct PrebufferedStream<S> {
    prefix: Bytes,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for PrebufferedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            buf.put_slice(&self.prefix.split_to(n));
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrebufferedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, WebError> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, WebError>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut store = rustls::RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = store.add(cert);
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|_| {
                    WebError::new("tls_failed", "TLS verification or negotiation failed.")
                })?
                .with_root_certificates(store)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

async fn connect_tls(
    stream: Box<dyn AsyncStream>,
    host: &str,
) -> Result<Box<dyn AsyncStream>, WebError> {
    let config = tls_config()?;
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| WebError::new("tls_failed", "TLS verification or negotiation failed."))?;
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .map_err(|_| WebError::new("tls_failed", "TLS verification or negotiation failed."))?;
    Ok(Box::new(tls_stream))
}

async fn open_tunnel(proxy: &Url, ip: IpAddr, port: u16) -> Result<Box<dyn AsyncStream>, WebError> {
    let proxy_host = hostname(proxy);
    let proxy_port = proxy
        .port_or_known_default()
        .ok_or_else(|| WebError::new("connect_failed", "The configured proxy is unusable."))?;
    let proxy_ip = if let Ok(pip) = proxy_host.parse::<IpAddr>() {
        pip
    } else {
        tokio::net::lookup_host((proxy_host.as_str(), proxy_port))
            .await
            .map_err(|_| WebError::new("connect_failed", "The connection failed."))?
            .next()
            .map(|addr| addr.ip())
            .ok_or_else(|| WebError::new("connect_failed", "The connection failed."))?
    };
    let tcp = TcpStream::connect(SocketAddr::new(proxy_ip, proxy_port))
        .await
        .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
    let mut stream: Box<dyn AsyncStream> = if proxy.scheme() == "https" {
        connect_tls(Box::new(tcp), &proxy_host).await?
    } else {
        Box::new(tcp)
    };
    let authority = match ip {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
    };
    let req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
    stream
        .flush()
        .await
        .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
    let mut buf = Vec::with_capacity(256);
    let mut scratch = [0u8; 1024];
    let header_end = loop {
        if buf.len() > 16 * 1024 {
            return Err(WebError::new(
                "connect_failed",
                "The proxy refused the tunnel.",
            ));
        }
        let n = stream
            .read(&mut scratch)
            .await
            .map_err(|_| WebError::new("connect_failed", "The proxy refused the tunnel."))?;
        if n == 0 {
            return Err(WebError::new(
                "connect_failed",
                "The proxy refused the tunnel.",
            ));
        }
        buf.extend_from_slice(&scratch[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head_str = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| WebError::new("connect_failed", "The proxy refused the tunnel."))?;
    let status_line = head_str.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status != 200 {
        return Err(WebError::new(
            "connect_failed",
            "The proxy refused the tunnel.",
        ));
    }
    let remainder = Bytes::copy_from_slice(&buf[header_end..]);
    Ok(Box::new(PrebufferedStream {
        prefix: remainder,
        inner: stream,
    }))
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub location: Option<String>,
    pub body: Vec<u8>,
    pub download_limited: bool,
}

struct CappedBuffer {
    buf: Vec<u8>,
    limit: usize,
    limited: bool,
}

impl CappedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
            limited: false,
        }
    }
}

impl Write for CappedBuffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.buf.len());
        if data.len() > remaining {
            self.buf.extend_from_slice(&data[..remaining]);
            self.limited = true;
        } else if remaining > 0 {
            self.buf.extend_from_slice(data);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Inflate {
    decompress: Decompress,
    out: CappedBuffer,
    ended: bool,
}

impl Inflate {
    fn new(zlib_header: bool, limit: usize) -> Self {
        Self {
            decompress: Decompress::new(zlib_header),
            out: CappedBuffer::new(limit),
            ended: false,
        }
    }

    fn push(&mut self, mut input: &[u8]) -> Result<bool, flate2::DecompressError> {
        let mut scratch = [0u8; 16 * 1024];
        while !self.ended && !self.out.limited {
            let (read, written) = (self.decompress.total_in(), self.decompress.total_out());
            let status = self
                .decompress
                .decompress(input, &mut scratch, FlushDecompress::None)?;
            let read = (self.decompress.total_in() - read) as usize;
            let written = (self.decompress.total_out() - written) as usize;
            let _ = self.out.write(&scratch[..written]);
            input = &input[read..];
            self.ended = status == Status::StreamEnd;
            if read == 0 && written == 0 {
                break;
            }
        }
        Ok(self.out.limited)
    }

    fn finish(self) -> Result<(Vec<u8>, bool), WebError> {
        if !self.ended && !self.out.limited {
            return Err(WebError::new("connect_failed", "The connection failed."));
        }
        Ok((self.out.buf, self.out.limited))
    }
}

enum Decoder {
    Identity(CappedBuffer),
    Gzip(GzDecoder<CappedBuffer>),
    Deflate {
        inflate: Inflate,
        raw: bool,
        compressed: Vec<u8>,
    },
}

impl Decoder {
    fn new(encoding: Option<&str>, limit: usize) -> Result<Self, WebError> {
        match encoding.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("identity") => Ok(Self::Identity(CappedBuffer::new(limit))),
            Some("gzip" | "x-gzip") => Ok(Self::Gzip(GzDecoder::new(CappedBuffer::new(limit)))),
            Some("deflate") => Ok(Self::Deflate {
                inflate: Inflate::new(true, limit),
                raw: false,
                compressed: Vec::new(),
            }),
            _ => Err(WebError::new(
                "connect_failed",
                "The response encoding is unsupported.",
            )),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<bool, WebError> {
        match self {
            Self::Identity(cap) => {
                let _ = cap.write(chunk);
                Ok(cap.limited)
            }
            Self::Gzip(gz) => {
                gz.write_all(chunk)
                    .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
                let _ = gz.flush();
                Ok(gz.get_ref().limited)
            }
            Self::Deflate {
                inflate,
                raw,
                compressed,
            } => {
                let failed = || WebError::new("connect_failed", "The connection failed.");
                if *raw {
                    return inflate.push(chunk).map_err(|_| failed());
                }
                if compressed.len() < 64 * 1024 {
                    compressed.extend_from_slice(chunk);
                }
                match inflate.push(chunk) {
                    Ok(limited) => Ok(limited),
                    Err(_) => {
                        *raw = true;
                        *inflate = Inflate::new(false, inflate.out.limit);
                        let replay = std::mem::take(compressed);
                        inflate.push(&replay).map_err(|_| failed())
                    }
                }
            }
        }
    }

    fn finish(self) -> Result<(Vec<u8>, bool), WebError> {
        match self {
            Self::Identity(cap) => Ok((cap.buf, cap.limited)),
            Self::Gzip(mut gz) => {
                let finished = gz.try_finish();
                let limited = gz.get_ref().limited;
                if finished.is_err() && !limited {
                    return Err(WebError::new("connect_failed", "The connection failed."));
                }
                let buf = std::mem::take(&mut gz.get_mut().buf);
                Ok((buf, limited))
            }
            Self::Deflate { inflate, .. } => inflate.finish(),
        }
    }
}

pub async fn get(
    url: &Url,
    config: &NetworkConfig,
    cancel: &CancellationToken,
    diagnostics: &mut Vec<Diagnostic>,
    headers: &HashMap<String, String>,
) -> Result<Response, WebError> {
    let host = hostname(url);
    let policy = Policy::new(&host, &config.allow_private_hosts, diagnostics)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| WebError::new("invalid_input", "Expected an absolute HTTP or HTTPS URL."))?;
    let connect_phase = async {
        let ips = resolve_addresses(&host, cancel).await?;
        policy.check(&ips)?;
        let mut last_err = WebError::new("connect_failed", "The connection failed.");
        for ip in ips {
            let attempt = async {
                let base_stream: Box<dyn AsyncStream> = if let Some(proxy) = &config.proxy {
                    open_tunnel(proxy, ip, port).await?
                } else {
                    let tcp = TcpStream::connect(SocketAddr::new(ip, port))
                        .await
                        .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
                    Box::new(tcp)
                };
                if url.scheme() == "https" {
                    connect_tls(base_stream, &host).await
                } else {
                    Ok(base_stream)
                }
            };
            match attempt.await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if cancel.is_cancelled() {
                        return Err(WebError::new("cancelled", "Call cancelled."));
                    }
                    last_err = err;
                }
            }
        }
        Err(last_err)
    };
    let stream = tokio::select! {
        _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
        res = tokio::time::timeout(CONNECT_TIMEOUT, connect_phase) => {
            res.map_err(|_| WebError::new("timeout", "Connection timed out."))??
        }
    };
    if cancel.is_cancelled() {
        return Err(WebError::new("cancelled", "Call cancelled."));
    }
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let result = async {
        let path_and_query = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let host_header = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let mut builder = http::Request::builder()
            .method("GET")
            .uri(path_and_query)
            .header("Host", host_header)
            .header(
                "User-Agent",
                format!("dim-web/{}", env!("CARGO_PKG_VERSION")),
            )
            .header(
                "Accept",
                "text/html, text/plain, application/json, application/xml;q=0.9, */*;q=0.1",
            )
            .header("Accept-Encoding", "gzip, deflate, identity")
            .header("Connection", "close");
        for (k, v) in headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder
            .body(Empty::<Bytes>::new())
            .map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
            res = sender.send_request(req) => res.map_err(|_| WebError::new("connect_failed", "The connection failed."))?,
        };
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if !(200..300).contains(&status) {
            return Ok(Response {
                status,
                content_type,
                location,
                body: Vec::new(),
                download_limited: false,
            });
        }
        let encoding = response
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok());
        let mut decoder = Decoder::new(encoding, DOWNLOAD_LIMIT)?;
        let mut body_stream = response.into_body();
        loop {
            let next_frame = tokio::select! {
                _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
                frame = body_stream.frame() => frame,
            };
            let Some(frame) = next_frame else {
                break;
            };
            let frame =
                frame.map_err(|_| WebError::new("connect_failed", "The connection failed."))?;
            if let Ok(data) = frame.into_data()
                && decoder.push(&data)?
            {
                break;
            }
        }
        let (body, download_limited) = decoder.finish()?;
        Ok(Response {
            status,
            content_type,
            location,
            body,
            download_limited,
        })
    }
    .await;
    conn_task.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::sync::CancellationToken;
    use url::Url;

    struct Dns(&'static str);

    impl Drop for Dns {
        fn drop(&mut self) {
            test_dns::remove(self.0);
        }
    }

    fn dns(host: &'static str, ips: &[&str]) -> Dns {
        test_dns::insert(
            host,
            test_dns::Answer::Ips(ips.iter().map(|ip| ip.parse().unwrap()).collect()),
        );
        Dns(host)
    }

    async fn read_head(socket: &mut TcpStream) -> (String, Vec<u8>) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let end = loop {
            let n = socket.read(&mut tmp).await.unwrap();
            assert!(n > 0, "peer closed before headers");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|part| part == b"\r\n\r\n") {
                break pos + 4;
            }
            assert!(buf.len() < 16 * 1024);
        };
        let head = String::from_utf8(buf[..end].to_vec()).unwrap();
        let rest = buf[end..].to_vec();
        (head, rest)
    }

    fn config(host: &str, proxy: Option<&str>) -> NetworkConfig {
        NetworkConfig {
            allow_private_hosts: vec![host.to_owned()],
            proxy: proxy_url(proxy).unwrap(),
        }
    }

    async fn origin() -> (
        TcpListener,
        std::net::SocketAddr,
        Arc<std::sync::Mutex<String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        (listener, address, seen)
    }

    fn serve_origin(
        listener: TcpListener,
        seen: Arc<std::sync::Mutex<String>>,
        stop: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let (head, _) = read_head(&mut socket).await;
                        *seen.lock().unwrap() = head;
                        let body = "pinned";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    }
                }
            }
        })
    }

    fn serve_proxy(
        listener: TcpListener,
        origin: std::net::SocketAddr,
        targets: Arc<std::sync::Mutex<Vec<String>>>,
        stop: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let (mut client, _) = accepted.unwrap();
                        let targets = targets.clone();
                        let stop = stop.clone();
                        tokio::spawn(async move {
                            let (head, rest) = read_head(&mut client).await;
                            let target = head
                                .lines()
                                .next()
                                .unwrap()
                                .split_whitespace()
                                .nth(1)
                                .unwrap()
                                .to_owned();
                            targets.lock().unwrap().push(target.clone());
                            if target.starts_with("[::1]:") {
                                let _ = client
                                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                                    .await;
                                return;
                            }
                            let mut upstream = TcpStream::connect(origin).await.unwrap();
                            client
                                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                                .await
                                .unwrap();
                            if !rest.is_empty() {
                                upstream.write_all(&rest).await.unwrap();
                            }
                            tokio::select! {
                                _ = stop.cancelled() => {}
                                _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                            }
                        });
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn a_private_dns_answer_is_rejected_before_a_tunnel_opens() {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_base = format!("http://{}", proxy.local_addr().unwrap());
        let targets = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = CancellationToken::new();
        let task = serve_proxy(
            proxy,
            "127.0.0.1:9".parse().unwrap(),
            targets.clone(),
            stop.clone(),
        );
        let blocked = NetworkConfig {
            allow_private_hosts: Vec::new(),
            proxy: proxy_url(Some(&proxy_base)).unwrap(),
        };
        let _name = dns("private.test", &["8.8.8.8", "10.0.0.1"]);
        let url = Url::parse("http://private.test/secret").unwrap();
        let mut diagnostics = Vec::new();
        let err = match tokio::time::timeout(
            Duration::from_secs(2),
            get(
                &url,
                &blocked,
                &CancellationToken::new(),
                &mut diagnostics,
                &HashMap::new(),
            ),
        )
        .await
        {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("expected a blocked address"),
            Err(_) => panic!("a private answer is rejected before connect"),
        };
        assert_eq!(err.code, "blocked_address", "{err}");
        assert!(targets.lock().unwrap().is_empty(), "{targets:?}");
        assert_eq!(test_dns::hits("private.test"), 1);

        let _fake = dns("fake.test", &["198.18.0.1"]);
        let url = Url::parse("https://fake.test/hidden").unwrap();
        let err = match get(
            &url,
            &blocked,
            &CancellationToken::new(),
            &mut diagnostics,
            &HashMap::new(),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("expected a blocked address"),
        };
        assert_eq!(err.code, "blocked_address", "{err}");
        assert!(err.message.contains("benchmark"), "{err}");
        assert!(targets.lock().unwrap().is_empty(), "{targets:?}");
        stop.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn direct_and_proxy_fetches_pin_the_resolved_address() {
        let (origin_listener, origin_addr, seen) = origin().await;
        let origin_stop = CancellationToken::new();
        let origin_task = serve_origin(origin_listener, seen.clone(), origin_stop.clone());
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_base = format!("http://{}", proxy.local_addr().unwrap());
        let targets = Arc::new(std::sync::Mutex::new(Vec::new()));
        let proxy_stop = CancellationToken::new();
        let proxy_task = serve_proxy(proxy, origin_addr, targets.clone(), proxy_stop.clone());
        let _name = dns("pinned.test", &["127.0.0.1"]);
        let url = Url::parse(&format!("http://pinned.test:{}/direct", origin_addr.port())).unwrap();
        let cancel = CancellationToken::new();
        let mut diagnostics = Vec::new();
        let direct = get(
            &url,
            &config("pinned.test", None),
            &cancel,
            &mut diagnostics,
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(direct.status, 200);
        assert_eq!(direct.body, b"pinned");
        assert_eq!(test_dns::hits("pinned.test"), 1);
        let host = seen.lock().unwrap().clone();
        assert!(
            host.to_ascii_lowercase()
                .contains(&format!("host: pinned.test:{}", origin_addr.port())),
            "{host}"
        );
        assert!(targets.lock().unwrap().is_empty());

        let through = get(
            &Url::parse(&format!("http://pinned.test:{}/proxy", origin_addr.port())).unwrap(),
            &config("pinned.test", Some(&proxy_base)),
            &cancel,
            &mut diagnostics,
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(through.body, b"pinned");
        assert_eq!(test_dns::hits("pinned.test"), 2);
        assert_eq!(
            targets.lock().unwrap().as_slice(),
            [format!("127.0.0.1:{}", origin_addr.port())]
        );

        let _fallback = dns("pinned.test", &["::1", "127.0.0.1"]);
        let again = get(
            &Url::parse(&format!(
                "http://pinned.test:{}/fallback",
                origin_addr.port()
            ))
            .unwrap(),
            &config("pinned.test", Some(&proxy_base)),
            &cancel,
            &mut diagnostics,
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(again.body, b"pinned");
        assert_eq!(
            test_dns::hits("pinned.test"),
            1,
            "fallback reuses one lookup"
        );
        assert_eq!(
            targets.lock().unwrap().as_slice(),
            [
                format!("127.0.0.1:{}", origin_addr.port()),
                format!("[::1]:{}", origin_addr.port()),
                format!("127.0.0.1:{}", origin_addr.port()),
            ]
        );
        origin_stop.cancel();
        proxy_stop.cancel();
        origin_task.await.unwrap();
        proxy_task.await.unwrap();
    }

    #[tokio::test]
    async fn a_resolution_that_does_not_finish_hits_the_connect_deadline() {
        test_dns::insert("never.test", test_dns::Answer::Hang);
        let _name = Dns("never.test");
        let started = std::time::Instant::now();
        let url = Url::parse("http://never.test/").unwrap();
        let err = match get(
            &url,
            &NetworkConfig {
                allow_private_hosts: Vec::new(),
                proxy: None,
            },
            &CancellationToken::new(),
            &mut Vec::new(),
            &HashMap::new(),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("expected a connect timeout"),
        };
        assert_eq!(err.code, "timeout", "{err}");
        assert_eq!(err.message, "Connection timed out.");
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_secs(9), "{elapsed:?}");
    }
}
