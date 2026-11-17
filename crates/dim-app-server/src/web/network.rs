use flate2::{Decompress, FlushDecompress, Status, write::GzDecoder};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    net::{IpAddr, Ipv4Addr},
    sync::OnceLock,
    time::Duration,
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

pub struct NetworkConfig {
    allow_private_hosts: Vec<String>,
    block_private_addresses: bool,
    proxy: Option<(Url, bool)>,
}

impl NetworkConfig {
    pub fn new(
        allow_private_hosts: Vec<String>,
        block_private_addresses: bool,
        proxy: Option<&str>,
        proxy_dns: bool,
    ) -> Result<Self, WebError> {
        let proxy = match proxy {
            Some(value) => Some((parse_proxy(value)?, proxy_dns)),
            None => system_proxy()
                .and_then(|value| parse_proxy(&value).ok().map(|url| (url, proxy_dns))),
        };
        Ok(Self {
            allow_private_hosts,
            block_private_addresses,
            proxy,
        })
    }
}

fn parse_proxy(value: &str) -> Result<Url, WebError> {
    let url = parse_url(value)
        .map_err(|_| WebError::new("connect_failed", "The configured proxy is unusable."))?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(WebError::new(
            "connect_failed",
            "The configured proxy is unusable.",
        ));
    }
    Ok(url)
}

/// The proxy the running desktop session already uses, when the host is
/// configured to route through one. The process environment wins; a desktop
/// setting is the fallback. `DIM_WEB_SYSTEM_PROXY=off` disables both.
fn system_proxy() -> Option<String> {
    if let Ok(value) = std::env::var("DIM_WEB_SYSTEM_PROXY")
        && matches!(value.as_str(), "off" | "none" | "0")
    {
        return None;
    }
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.to_string_lossy().trim().to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    static DESKTOP: OnceLock<Option<String>> = OnceLock::new();
    DESKTOP.get_or_init(desktop_proxy).clone()
}

#[cfg(target_os = "linux")]
fn desktop_proxy() -> Option<String> {
    let read = |schema: &str, key: &str| {
        let output = std::process::Command::new("gsettings")
            .args(["get", schema, key])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    if read("org.gnome.system.proxy", "mode")?.trim_matches('\'') != "manual" {
        return None;
    }
    let host = read("org.gnome.system.proxy.http", "host")?;
    let host = host.trim_matches('\'');
    if host.is_empty() {
        return None;
    }
    let port: u16 = read("org.gnome.system.proxy.http", "port")?.parse().ok()?;
    Some(format!("http://{host}:{port}"))
}

#[cfg(not(target_os = "linux"))]
fn desktop_proxy() -> Option<String> {
    None
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
    block_private: bool,
    allowed_host: bool,
    ranges: Vec<Cidr>,
}

impl Policy {
    fn new(
        host: &str,
        entries: &[String],
        block_private: bool,
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
        if block_private && !allowed_host && (host == "localhost" || host.ends_with(".localhost")) {
            return Err(WebError::new(
                "blocked_address",
                "Localhost names are blocked.",
            ));
        }
        Ok(Self {
            block_private,
            allowed_host,
            ranges,
        })
    }

    fn check(&self, addresses: &[IpAddr], hostname: bool) -> Result<(), WebError> {
        if !self.block_private {
            return Ok(());
        }
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
                        if hostname {
                            "Reserved benchmark addresses are blocked. A Fake-IP resolver needs a session or configured proxy so the proxy resolves hostnames, an allowPrivateHosts exception, or blockPrivateAddresses off. See docs/design/plugins/Web.md (Address policy)."
                        } else {
                            "Reserved benchmark addresses are blocked. Proxy DNS applies only to hostname targets. See docs/design/plugins/Web.md (Address policy)."
                        },
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
    let policy = Policy::new(
        &host,
        &config.allow_private_hosts,
        config.block_private_addresses,
        diagnostics,
    )?;
    let domain = matches!(url.host(), Some(url::Host::Domain(_)));
    // A proxy resolves the target itself, so the local answer is not what the
    // connection uses; with `proxyDns` the check is skipped entirely.
    let proxied = config.proxy.is_some();
    let delegated = proxied && matches!(&config.proxy, Some((_, true))) && domain;
    let mut pinned = None;
    if config.block_private_addresses && !delegated {
        let ips = tokio::select! {
            _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
            res = tokio::time::timeout(CONNECT_TIMEOUT, resolve_addresses(&host, cancel)) => {
                res.map_err(|_| WebError::new("timeout", "Connection timed out."))??
            }
        };
        policy.check(&ips, domain)?;
        if domain && !proxied {
            let port = url.port_or_known_default().ok_or_else(|| {
                WebError::new("invalid_input", "Expected an absolute HTTP or HTTPS URL.")
            })?;
            pinned = Some(std::net::SocketAddr::new(ips[0], port));
        }
    }
    let client = client_for(config, &host, pinned)?;
    attempt(&client, url, cancel, headers).await
}

fn client_for(
    config: &NetworkConfig,
    host: &str,
    pinned: Option<std::net::SocketAddr>,
) -> Result<reqwest::Client, WebError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .http1_only()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(format!("dim-web/{}", env!("CARGO_PKG_VERSION")));
    builder = match &config.proxy {
        Some((proxy, _)) => {
            builder.proxy(reqwest::Proxy::all(proxy.as_str()).map_err(|_| {
                WebError::new("connect_failed", "The configured proxy is unusable.")
            })?)
        }
        // `reqwest` reads the environment on its own unless told not to, which
        // would bypass the documented fallback order and the off switch.
        None => builder.no_proxy(),
    };
    if let Some(pinned) = pinned {
        builder = builder.resolve(host, pinned);
    }
    builder
        .build()
        .map_err(|_| WebError::new("connect_failed", "The connection failed."))
}

async fn attempt(
    client: &reqwest::Client,
    url: &Url,
    cancel: &CancellationToken,
    headers: &HashMap<String, String>,
) -> Result<Response, WebError> {
    let mut request = client
        .get(url.as_str())
        .header(
            reqwest::header::ACCEPT,
            "text/html, text/plain, application/json, application/xml;q=0.9, */*;q=0.1",
        )
        .header(reqwest::header::ACCEPT_ENCODING, "gzip, deflate, identity")
        .header(reqwest::header::CONNECTION, "close");
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let mut response = tokio::select! {
        _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
        res = request.send() => res.map_err(|_| WebError::new("connect_failed", "The connection failed."))?,
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
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
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok());
    let mut decoder = Decoder::new(encoding, DOWNLOAD_LIMIT)?;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(WebError::new("cancelled", "Call cancelled.")),
            chunk = response.chunk() => chunk.map_err(|_| WebError::new("connect_failed", "The connection failed."))?,
        };
        let Some(chunk) = next else {
            break;
        };
        if decoder.push(&chunk)? {
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

    /// The library tests never fall back to the host's own proxy.
    fn no_system_proxy() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe { std::env::set_var("DIM_WEB_SYSTEM_PROXY", "off") });
    }

    fn dns(
        host: &'static str,
        ips: &[&str],
    ) -> std::mem::DropGuard<&'static str, fn(&'static str)> {
        test_dns::insert(
            host,
            test_dns::Answer::Ips(ips.iter().map(|ip| ip.parse().unwrap()).collect()),
        );
        std::mem::DropGuard::new(host, test_dns::remove)
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
        no_system_proxy();
        NetworkConfig::new(vec![host.to_owned()], true, proxy, false).unwrap()
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
                            let request_line = head.lines().next().unwrap_or_default().to_owned();
                            let mut parts = request_line.split_whitespace();
                            let method = parts.next().unwrap_or_default().to_owned();
                            let target = parts.next().unwrap_or_default().to_owned();
                            targets.lock().unwrap().push(target.clone());
                            let mut upstream = TcpStream::connect(origin).await.unwrap();
                            if method == "CONNECT" {
                                client
                                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                                    .await
                                    .unwrap();
                                if !rest.is_empty() {
                                    upstream.write_all(&rest).await.unwrap();
                                }
                            } else {
                                // The request arrives in absolute form; the origin
                                // wants origin form.
                                let path = target
                                    .split_once("://")
                                    .and_then(|(_, authority)| authority.find('/').map(|i| &authority[i..]))
                                    .unwrap_or("/");
                                let version = parts.next().unwrap_or("HTTP/1.1");
                                let tail = head.split_once("\r\n").map(|(_, tail)| tail).unwrap_or("");
                                let rewritten = format!("{method} {path} {version}\r\n{tail}");
                                upstream.write_all(rewritten.as_bytes()).await.unwrap();
                                if !rest.is_empty() {
                                    upstream.write_all(&rest).await.unwrap();
                                }
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
        no_system_proxy();
        let blocked = NetworkConfig::new(Vec::new(), true, Some(&proxy_base), false).unwrap();
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
        assert!(err.message.contains("blockPrivateAddresses"), "{err}");
        assert!(targets.lock().unwrap().is_empty(), "{targets:?}");
        stop.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn direct_and_proxied_fetches_reach_the_origin() {
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
        assert_eq!(
            test_dns::hits("pinned.test"),
            1,
            "the policy check resolves once"
        );
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
            [format!("http://pinned.test:{}/proxy", origin_addr.port())]
        );
        origin_stop.cancel();
        proxy_stop.cancel();
        origin_task.await.unwrap();
        proxy_task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_dns_uses_the_hostname_for_connect_and_tls_without_local_resolution() {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_base = format!("http://{}", proxy.local_addr().unwrap());
        let _name = dns("fake-tls.test", &["198.18.0.1"]);
        let handshake = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            let (head, _) = read_head(&mut socket).await;
            assert_eq!(
                head.lines().next(),
                Some("CONNECT fake-tls.test:443 HTTP/1.1")
            );
            socket
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let start =
                tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), socket)
                    .await
                    .unwrap();
            assert_eq!(start.client_hello().server_name(), Some("fake-tls.test"));
        });
        let url = Url::parse("https://fake-tls.test/").unwrap();
        let Err(err) = get(
            &url,
            &NetworkConfig::new(Vec::new(), true, Some(&proxy_base), true).unwrap(),
            &CancellationToken::new(),
            &mut Vec::new(),
            &HashMap::new(),
        )
        .await
        else {
            panic!("TLS requires a completed handshake");
        };
        assert_eq!(err.code, "connect_failed", "{err}");
        handshake.await.unwrap();
        assert_eq!(test_dns::hits("fake-tls.test"), 0);
    }

    #[tokio::test]
    async fn the_off_switch_stops_the_automatic_proxy() {
        let (origin_listener, origin_addr, seen) = origin().await;
        let origin_stop = CancellationToken::new();
        let origin_task = serve_origin(origin_listener, seen, origin_stop.clone());
        no_system_proxy();
        unsafe {
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
                std::env::set_var(name, "http://127.0.0.1:9");
            }
        }
        let url = Url::parse(&format!("http://127.0.0.1:{}/direct", origin_addr.port())).unwrap();
        let fetched = get(
            &url,
            &NetworkConfig::new(Vec::new(), false, None, false).unwrap(),
            &CancellationToken::new(),
            &mut Vec::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
        unsafe {
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
                std::env::remove_var(name);
            }
        }
        assert_eq!(fetched.body, b"pinned");
        origin_stop.cancel();
        origin_task.await.unwrap();
    }

    #[tokio::test]
    async fn a_resolution_that_does_not_finish_hits_the_connect_deadline() {
        test_dns::insert("never.test", test_dns::Answer::Hang);
        let _name = std::mem::DropGuard::new("never.test", test_dns::remove);
        let started = std::time::Instant::now();
        let url = Url::parse("http://never.test/").unwrap();
        no_system_proxy();
        let err = match get(
            &url,
            &NetworkConfig::new(Vec::new(), true, None, false).unwrap(),
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
