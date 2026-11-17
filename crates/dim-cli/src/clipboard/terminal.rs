//! Reading the clipboard through the terminal with the kitty clipboard
//! protocol (OSC 5522, <https://sw.kovidgoyal.net/kitty/clipboard/>). It
//! reaches the clipboard of the machine the terminal runs on, so Ctrl+V
//! pastes a local screenshot into a session running over SSH, where the
//! native readers see the remote host's clipboard or none.
//!
//! A read lists the clipboard's types first (no permission prompt), then
//! asks for the one it inserts; the terminal may ask the user to allow it.

use super::{IMAGE_READ_LIMIT, Source};
use crate::{Result, images};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::time::{Duration, Instant};

/// How long the terminal may take to answer, its permission prompt included.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
const TEXT_LIMIT: usize = 10 * 1024 * 1024;
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// The native readers would not see the terminal's clipboard: the session
/// runs over SSH, or on Linux without a display server to ask.
pub fn native_unreachable() -> bool {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    set("SSH_CONNECTION")
        || set("SSH_TTY")
        || (cfg!(target_os = "linux") && !set("DISPLAY") && !set("WAYLAND_DISPLAY"))
}

/// One `type=read` reply of the terminal, decoded where it leaves the input
/// stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The request id echoed back; absent from a terminal that drops it.
    pub id: Option<String>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Data {
        mime: String,
        data: Vec<u8>,
    },
    Done,
    /// `EPERM`, `ENOSYS`, `EBUSY`, `EIO` or another error code.
    Error(String),
}

/// The reply in the body of an OSC string (between `ESC ]` and its
/// terminator), or `None` when it is not a well-formed OSC 5522 read reply.
pub fn parse(body: &[u8]) -> Option<Reply> {
    let body = std::str::from_utf8(body.strip_prefix(b"5522;")?).ok()?;
    let (metadata, payload) = body.split_once(';').unwrap_or((body, ""));
    let mut kind = None;
    let mut status = None;
    let mut mime = None;
    let mut id = None;
    for field in metadata.split(':') {
        match field.split_once('=')? {
            ("type", value) => kind = Some(value),
            ("status", value) => status = Some(value),
            ("mime", value) => mime = Some(String::from_utf8(STANDARD.decode(value).ok()?).ok()?),
            ("id", value) => id = Some(value.to_owned()),
            _ => {}
        }
    }
    if kind != Some("read") {
        return None;
    }
    let status = match status? {
        "OK" => Status::Ok,
        "DONE" => Status::Done,
        "DATA" => Status::Data {
            mime: mime?,
            data: STANDARD.decode(payload).ok()?,
        },
        error => Status::Error(error.to_owned()),
    };
    Some(Reply { id, status })
}

/// A clipboard read in flight.
pub struct Read {
    id: String,
    /// The type being read; `None` while the types are listed.
    mime: Option<String>,
    data: Vec<u8>,
    deadline: Instant,
}

/// What a reply moved the read to.
pub enum Step {
    Pending,
    /// Write these bytes to the terminal and keep waiting.
    Send(Vec<u8>),
    Done(Result<Source>),
}

impl Read {
    /// Starts a read with request `id` (`[A-Za-z0-9-_+.]`); the returned
    /// bytes go to the terminal.
    pub fn start(id: String, now: Instant) -> (Self, Vec<u8>) {
        let request = request(&id, ".");
        let read = Self {
            id,
            mime: None,
            data: Vec::new(),
            deadline: now + ANSWER_TIMEOUT,
        };
        (read, request)
    }

    /// The terminal has not finished answering in time.
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub fn feed(&mut self, reply: Reply, now: Instant) -> Step {
        if reply.id.as_ref().is_some_and(|id| *id != self.id) {
            return Step::Pending;
        }
        match reply.status {
            Status::Ok => Step::Pending,
            Status::Error(code) => Step::Done(Err(match code.as_str() {
                "EPERM" => "the terminal denied access".into(),
                "EBUSY" => "the terminal is busy; try again".into(),
                _ => format!("the terminal could not read it ({code})").into(),
            })),
            Status::Data { mime, data } => {
                let (wanted, limit) = match &self.mime {
                    None => (".", TEXT_LIMIT),
                    Some(mime) if mime.starts_with("image/") => (mime.as_str(), IMAGE_READ_LIMIT),
                    Some(mime) => (mime.as_str(), TEXT_LIMIT),
                };
                if mime != wanted {
                    return Step::Pending;
                }
                if self.data.len() + data.len() > limit {
                    return Step::Done(Err("Clipboard content exceeds its size limit".into()));
                }
                self.data.extend(data);
                Step::Pending
            }
            Status::Done => match self.mime.take() {
                None => {
                    let listed = String::from_utf8_lossy_owned(std::mem::take(&mut self.data))
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let Some(mime) = choose(&listed) else {
                        return Step::Done(Err("Clipboard has no supported image or text".into()));
                    };
                    let request = request(&self.id, &mime);
                    self.mime = Some(mime);
                    self.deadline = now + ANSWER_TIMEOUT;
                    Step::Send(request)
                }
                Some(mime) => {
                    let data = std::mem::take(&mut self.data);
                    Step::Done(if mime.starts_with("image/") {
                        images::from_bytes(data).map(Source::Image)
                    } else {
                        String::from_utf8(data)
                            .map(Source::Text)
                            .map_err(|_| "Clipboard text must be UTF-8".into())
                    })
                }
            },
        }
    }
}

/// The type a paste inserts: an attachable image before text, as the native
/// readers do. Files are left out, since their paths name the terminal's
/// machine.
fn choose(listed: &[String]) -> Option<String> {
    let has = |wanted: &str| listed.iter().any(|mime| mime == wanted);
    IMAGE_TYPES
        .into_iter()
        .find(|mime| has(mime))
        .map(str::to_owned)
        .or_else(|| {
            TEXT_TYPES
                .into_iter()
                .find(|mime| has(mime))
                .map(str::to_owned)
        })
}

/// A read request for `mimes` (space separated; `.` lists the types). The
/// name and per-process password let the terminal remember the user's
/// permission for the rest of the run.
fn request(id: &str, mimes: &str) -> Vec<u8> {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let password = PASSWORD.get_or_init(|| uuid::Uuid::new_v4().to_string());
    format!(
        "\x1b]5522;type=read:id={id}:name={}:pw={};{}\x1b\\",
        STANDARD.encode("dim"),
        STANDARD.encode(password),
        STANDARD.encode(mimes)
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(body: &str) -> Reply {
        parse(body.as_bytes()).expect("read reply")
    }

    fn data(id: &str, mime: &str, bytes: &[u8]) -> Reply {
        reply(&format!(
            "5522;type=read:status=DATA:id={id}:mime={};{}",
            STANDARD.encode(mime),
            STANDARD.encode(bytes)
        ))
    }

    fn done(id: &str) -> Reply {
        reply(&format!("5522;type=read:status=DONE:id={id}"))
    }

    fn png() -> Vec<u8> {
        png_of(2, 1)
    }

    fn png_of(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    /// A screenshot wider than the shared edge limit is scaled, not refused.
    #[test]
    fn a_wide_terminal_image_is_scaled() {
        use dim_agent_capabilities::images::MAX_EDGE;
        let now = Instant::now();
        let (mut read, _) = Read::start("dim-5".into(), now);
        read.feed(data("dim-5", ".", b"image/png"), now);
        assert!(matches!(read.feed(done("dim-5"), now), Step::Send(_)));
        let image = png_of(MAX_EDGE * 2, 2);
        read.feed(data("dim-5", "image/png", &image), now);
        let Step::Done(Ok(Source::Image(image))) = read.feed(done("dim-5"), now) else {
            panic!("expected a scaled image")
        };
        assert!(image.scaled);
        assert_eq!(image.source.media_type, "image/png");
        assert_eq!((image.sent.width, image.sent.height), (MAX_EDGE, 1));
    }

    #[test]
    fn listing_then_reading_inserts_the_image_before_text() {
        let now = Instant::now();
        let (mut read, first) = Read::start("dim-1".into(), now);
        let first = String::from_utf8(first).unwrap();
        assert!(first.starts_with("\x1b]5522;type=read:id=dim-1:name=ZGlt:pw="));
        assert!(first.ends_with(";Lg==\x1b\\"), "{first}");
        assert!(matches!(
            read.feed(reply("5522;type=read:status=OK:id=dim-1"), now),
            Step::Pending
        ));
        read.feed(data("dim-1", ".", b"text/plain image/png"), now);
        let Step::Send(second) = read.feed(done("dim-1"), now) else {
            panic!("expected the image request")
        };
        assert!(
            String::from_utf8(second)
                .unwrap()
                .ends_with(&format!(";{}\x1b\\", STANDARD.encode("image/png")))
        );
        let image = png();
        let (head, tail) = image.split_at(image.len() / 2);
        read.feed(data("dim-1", "image/png", head), now);
        read.feed(data("other", "image/png", b"stale"), now);
        read.feed(data("dim-1", "image/png", tail), now);
        let Step::Done(Ok(Source::Image(source))) = read.feed(done("dim-1"), now) else {
            panic!("expected an image")
        };
        assert_eq!(source.source.media_type, "image/png");
        assert_eq!(source.source.data, Some(STANDARD.encode(&image)));
    }

    #[test]
    fn text_wins_over_an_unattachable_image_from_a_terminal_that_drops_the_id() {
        let now = Instant::now();
        let (mut read, _) = Read::start("dim-2".into(), now);
        read.feed(
            reply(&format!(
                "5522;type=read:status=DATA:mime=Lg==;{}",
                STANDARD.encode("image/tiff text/plain")
            )),
            now,
        );
        assert!(matches!(
            read.feed(reply("5522;type=read:status=DONE"), now),
            Step::Send(_)
        ));
        read.feed(
            reply(&format!(
                "5522;type=read:status=DATA:mime={};{}",
                STANDARD.encode("text/plain"),
                STANDARD.encode("中\n")
            )),
            now,
        );
        let Step::Done(Ok(Source::Text(text))) =
            read.feed(reply("5522;type=read:status=DONE"), now)
        else {
            panic!("expected text")
        };
        assert_eq!(text, "中\n");
    }

    #[test]
    fn denial_empty_clipboard_and_silence_end_the_read() {
        let now = Instant::now();
        let (mut read, _) = Read::start("dim-3".into(), now);
        let Step::Done(Err(error)) = read.feed(reply("5522;type=read:status=EPERM:id=dim-3"), now)
        else {
            panic!("expected a denial")
        };
        assert!(error.to_string().contains("denied"));
        let (mut read, _) = Read::start("dim-4".into(), now);
        read.feed(data("dim-4", ".", b"text/uri-list"), now);
        assert!(matches!(read.feed(done("dim-4"), now), Step::Done(Err(_))));
        assert!(!read.expired(now) && read.expired(now + ANSWER_TIMEOUT));
    }

    #[test]
    fn malformed_and_foreign_replies_are_not_read_replies() {
        assert_eq!(parse(b"52;c;aGk="), None);
        assert_eq!(parse(b"5522;type=write:status=DONE"), None);
        assert_eq!(
            parse(b"5522;type=read:status=DATA:mime=Lg==;not base64!"),
            None
        );
        assert_eq!(parse(b"5522;type=read:status=DATA;aGk="), None);
    }
}
