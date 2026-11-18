use crate::feedback::Submission;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use std::{fmt, io::Read, path::Path, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct Intake {
    destination: String,
    authorization: header::HeaderValue,
}

impl fmt::Debug for Intake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Intake")
            .field("destination", &self.destination)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    case_id: String,
}

impl Intake {
    pub(crate) fn configured() -> Option<Self> {
        // The pilot checks POSIX file permissions. Do not silently skip this on Windows.
        if !cfg!(unix) {
            return None;
        }
        let port = std::env::var("DIM_FEEDBACK_PORT").ok()?.parse().ok()?;
        let file = std::env::var_os("DIM_FEEDBACK_TOKEN_FILE")?;
        Self::open(port, Path::new(&file))
    }

    fn open(port: std::num::NonZeroU16, path: &Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > 513 {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid has no parameters or caller preconditions.
            if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
                return None;
            }
        }
        let mut token = String::new();
        file.take(514).read_to_string(&mut token).ok()?;
        let token = token.strip_suffix('\n').unwrap_or(&token);
        if !(32..=512).contains(&token.len())
            || !token
                .trim_end_matches('=')
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._~+/-".contains(&c))
            || token.starts_with('=')
        {
            return None;
        }
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
        authorization.set_sensitive(true);
        Some(Self {
            destination: format!("http://127.0.0.1:{port}/v1/feedback"),
            authorization,
        })
    }

    pub(crate) fn destination(&self) -> &str {
        &self.destination
    }

    pub(crate) async fn send(&self, submission: &Submission) -> Result<Uuid, &'static str> {
        let credential = &self.authorization.as_bytes()[b"Bearer ".len()..];
        if submission
            .body
            .as_bytes()
            .windows(credential.len())
            .any(|s| s == credential)
        {
            return Err("Not sent: the preview contains the intake credential.");
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|_| "Could not initialize the feedback connection.")?;
        let mut response = client
            .post(self.destination())
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(submission.body.clone())
            .send()
            .await
            .map_err(|_| "No receipt; check your intake tunnel and retry.")?;
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err("Feedback was not accepted: check intake authentication.");
            }
            StatusCode::CONFLICT | StatusCode::GONE => {
                return Err("The intake refused this case ID (changed or withdrawn).");
            }
            _ => return Err("The intake did not confirm receipt."),
        }
        let invalid = "Invalid intake receipt; delivery is not confirmed.";
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid)? {
            if chunk.len() > 256 - bytes.len() {
                return Err(invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| invalid)?;
        if receipt.case_id != submission.id.to_string() {
            return Err(invalid);
        }
        Ok(submission.id)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, io::Write as _, net::TcpListener, os::unix::fs::PermissionsExt};

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credential");
        let value = Uuid::new_v4().simple().to_string();
        fs::write(&path, format!("{value}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, value)
    }

    #[test]
    fn rejects_unsafe_token_files_and_redacts_debug() {
        let (dir, path, value) = fixture();
        let port = 8787.try_into().unwrap();
        let intake = Intake::open(port, &path).unwrap();
        assert_eq!(intake.destination(), "http://127.0.0.1:8787/v1/feedback");
        assert!(!format!("{intake:?}").contains(&value));
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Intake::open(port, &link).is_none());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Intake::open(port, &path).is_none());
    }

    #[tokio::test]
    async fn accepts_only_matching_bounded_receipts() {
        let (_dir, path, _) = fixture();
        let id = Uuid::new_v4();
        let submission = Submission {
            id,
            body: "{}".into(),
        };
        for (status, response, ok) in [
            (201, format!(r#"{{"caseId":"{id}"}}"#), true),
            (200, format!(r#"{{"caseId":"{}"}}"#, Uuid::new_v4()), false),
            (201, "x".repeat(257), false),
            (409, String::new(), false),
        ] {
            let sock = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = sock.local_addr().unwrap().port().try_into().unwrap();
            let server = std::thread::spawn(move || {
                use std::io::BufRead as _;
                let (mut stream, _) = sock.accept().unwrap();
                let mut received = std::io::BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    assert_ne!(received.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = [0; 2];
                received.read_exact(&mut body).unwrap();
                assert_eq!(&body, b"{}");
                write!(stream, "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            });
            let result = Intake::open(port, &path).unwrap().send(&submission).await;
            assert_eq!(result.ok(), ok.then_some(id));
            server.join().unwrap();
        }
    }
}
