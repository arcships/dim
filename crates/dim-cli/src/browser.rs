//! Opening the system browser for sign-in links.

use std::process::{Command, Stdio};

/// The command that opens `url` on `platform`; `None` when the user asked for
/// manual sign-in (`DIMCODE_OAUTH_MANUAL=1`) or the platform has no opener.
pub fn open_command(
    platform: &str,
    url: &str,
    manual: bool,
) -> Option<(&'static str, Vec<String>)> {
    if manual {
        return None;
    }
    match platform {
        "macos" => Some(("open", vec![url.to_owned()])),
        "linux" => Some(("xdg-open", vec![url.to_owned()])),
        "windows" => Some((
            "powershell.exe",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "Start-Process -FilePath $args[0]".into(),
                url.to_owned(),
            ],
        )),
        _ => None,
    }
}

/// Starts the browser detached; `false` when it could not be launched.
pub fn open(url: &str) -> bool {
    let manual = std::env::var("DIMCODE_OAUTH_MANUAL").is_ok_and(|v| v == "1");
    let Some((program, args)) = open_command(std::env::consts::OS, url, manual) else {
        return false;
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_mode_never_opens() {
        assert!(open_command("macos", "https://x", true).is_none());
        assert_eq!(open_command("macos", "https://x", false).unwrap().0, "open");
        assert!(open_command("plan9", "https://x", false).is_none());
    }
}
