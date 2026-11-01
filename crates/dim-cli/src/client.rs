use crate::{
    Result,
    args::{Options, SERVER_REQUIREMENT},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub use dim_client::{
    Client, ConnectionConfig, DaemonProfile, Message, RequestTooLarge, RpcError, Server, app_error,
};

/// The App Server the CLI runs: `--server`, or this executable's own.
pub fn server(options: &Options) -> Result<Server> {
    let Some(path) = &options.server else {
        return Ok(Server {
            program: std::env::current_exe()?,
            embedded: true,
        });
    };
    if is_this_executable(path) {
        return Err(format!(
            "{SERVER_REQUIREMENT}, not this executable ({}); without --server, this \
             executable's app-server subcommand runs",
            path.display()
        )
        .into());
    }
    Ok(Server {
        program: path.clone(),
        embedded: false,
    })
}

fn is_this_executable(path: &Path) -> bool {
    std::env::current_exe().is_ok_and(|executable| same_file(path, &executable))
}

/// Whether `a` and `b` name the same file: links and symlinks included on
/// Unix, symlinks included elsewhere.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let (Ok(a), Ok(b)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) else {
            return false;
        };
        a == b
    }
}

/// Where the Web workspace's files are, when they are installed: the
/// `DIMCODE_WEB_ASSETS` directory (the npm wrapper names the installed
/// `dimcode-web` package's, `./dev` and frontend development name their
/// own), else `web/` beside the executable's `bin/` (a native archive
/// unpacked next to it), else `web/` in the product home. A directory
/// counts when it holds `index.html`.
pub fn web_assets(executable: &Path, home: &Path) -> Option<PathBuf> {
    web_assets_in(std::env::var_os("DIMCODE_WEB_ASSETS"), executable, home)
}

fn web_assets_in(configured: Option<OsString>, executable: &Path, home: &Path) -> Option<PathBuf> {
    let candidates = [
        configured.map(PathBuf::from),
        executable
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("web")),
        Some(home.join("web")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|directory| directory.join("index.html").is_file())
}

/// The default plugin configuration file (PluginManager §2.3):
/// `DIMCODE_DEFAULT_PLUGINS`, else `default-plugins.json` beside the
/// executable's `bin/`, next to the `plugins/` directory it lists.
fn default_plugins(executable: &Path) -> Option<PathBuf> {
    [
        std::env::var_os("DIMCODE_DEFAULT_PLUGINS").map(PathBuf::from),
        executable
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("default-plugins.json")),
    ]
    .into_iter()
    .flatten()
    .find(|file| file.is_file())
}

/// What to tell the user when the daemon cannot serve the Web workspace, as
/// two sentences without their full stops; `None` for any other failure. A
/// daemon started with no frontend while the CLI now finds one beside the
/// executable or in `DIMCODE_WEB_ASSETS` keeps serving without it until it
/// restarts; otherwise a released build installs this version's with
/// `dimcode web install`, while a build from source, which has none
/// published, is pointed at `DIMCODE_WEB_ASSETS` or the embedded-web build.
pub fn web_hint(error: &crate::Error, executable: &Path, home: &Path) -> Option<(String, String)> {
    let install = if crate::version::RELEASE {
        "Install it with `dimcode web install`".to_owned()
    } else {
        "Set DIMCODE_WEB_ASSETS to a frontend built from this source, or build the App Server with the embedded-web feature".to_owned()
    };
    match app_error(error)? {
        "web_assets_missing" if web_assets(executable, home).is_some() => Some((
            "The Web workspace was installed after the daemon started".into(),
            "Restart the daemon with `dimcode daemon stop`, then try again".into(),
        )),
        "web_assets_missing" => Some(("The Web workspace is not installed".into(), install)),
        "web_assets_incompatible" => Some((
            format!(
                "The installed Web workspace does not fit dimcode {}",
                crate::version::current()
            ),
            install,
        )),
        _ => None,
    }
}

/// The name the daemon shows in process lists.
pub const SERVER_NAME: &str = if cfg!(windows) {
    "dimcode-server.exe"
} else {
    "dimcode-server"
};

/// `<native_home>/dimcode-server`, a link to `exe`, kept pointing at the
/// executable that starts the daemon: a hard link, whose name the kernel
/// takes as the process name, else a symbolic one, which still names the
/// command line; `exe` itself when neither can be made (the name is
/// cosmetic, the daemon runs either way).
fn server_link(native_home: &Path, exe: &Path) -> PathBuf {
    let link = native_home.join(SERVER_NAME);
    if same_file(&link, exe) {
        return link;
    }
    let _ = std::fs::remove_file(&link);
    if std::fs::hard_link(exe, &link).is_ok() {
        return link;
    }
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(exe, &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(exe, &link);
    match made {
        Ok(()) => link,
        Err(_) => exe.to_path_buf(),
    }
}

/// Stops the daemon `client` is connected to and waits for it to close;
/// `None` means no daemon runs.
pub async fn stop(client: Option<Client>) -> Result<i32> {
    let Some(client) = client else {
        println!("Daemon is not running.");
        return Ok(0);
    };
    shutdown(client).await?;
    println!("Daemon stopped.");
    Ok(0)
}

/// Stops the daemon behind `client` and waits until it has settled.
pub async fn shutdown(mut client: Client) -> Result<()> {
    if !client.offers("daemon.shutdown.completed") {
        return Err("Daemon does not support verified shutdown completion; stop the older daemon with its matching CLI before restarting".into());
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        client.rpc("daemon.shutdown", json!({})).await?;
        client.wait_closed().await?;
        Ok::<_, crate::Error>(())
    })
    .await
    .map_err(|_| "Daemon shutdown timed out; it is still settling and was not force-killed")??;
    Ok(())
}

/// The connection to the daemon of `options.home` after `hello`. Without a
/// daemon, starts one when `start` and waits for it; else `None`.
pub async fn connect_inner(options: &Options, start: bool) -> Result<Option<Client>> {
    let profile = if !start {
        None
    } else {
        Some(match &options.model_config {
            Some(path) => DaemonProfile::Configured {
                model_config_sha256: format!("{:x}", Sha256::digest(std::fs::read(path)?)),
            },
            None => DaemonProfile::Product,
        })
    };
    let native_home = options.home.join("rust");
    if start {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&native_home)?;
    }
    let native_home = match std::fs::canonicalize(native_home) {
        Ok(path) => path,
        Err(error) if !start && error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&native_home)?;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Rust home must be private (0700) and owned by the current user".into());
        }
    }
    let startup_note = native_home.join("dimcode.sqlite.startup");
    #[cfg(unix)]
    let endpoint = native_home.join("daemon.sock");
    #[cfg(windows)]
    let endpoint = {
        let key = format!(
            "{:x}",
            Sha256::digest(format!("dimcode:{}", native_home.display()))
        );
        std::path::PathBuf::from(format!(r"\\.\pipe\dim-rust-{}", &key[..32]))
    };
    let server = server(options)?;
    let binary = &server.program;
    let connection = ConnectionConfig {
        endpoint: endpoint.clone(),
        server: server.clone(),
        profile,
        client_kind: if matches!(options.action, crate::args::Action::Acp { .. }) {
            "other"
        } else {
            "cli"
        },
        client_name: if matches!(options.action, crate::args::Action::Acp { .. }) {
            "AcpGateway"
        } else {
            "dimcode"
        },
    };
    match Client::open(&connection, false).await {
        Ok(mut client) => {
            let version = client.hello().await?;
            if !(start && server.embedded && version != env!("CARGO_PKG_VERSION")) {
                let _ = std::fs::remove_file(&startup_note);
                return Ok(Some(client));
            }
            shutdown(client).await?;
        }
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error.into()),
    }
    if !start {
        return Ok(None);
    }
    if !binary.is_file() {
        return Err(if server.embedded {
            format!(
                "App Server binary is missing at {}; supply --server PATH",
                binary.display()
            )
        } else {
            format!("{SERVER_REQUIREMENT}; {} is missing", binary.display())
        }
        .into());
    }
    let log_path = native_home.join("daemon.log");
    let mut log = std::fs::OpenOptions::new();
    log.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let log = log.open(&log_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = log.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("Daemon log must be a private file owned by the current user".into());
        }
    }
    let program = if server.embedded {
        server_link(&native_home, binary)
    } else {
        binary.clone()
    };
    let mut command = Command::new(&program);
    command.args(server.subcommand());
    command
        .current_dir(&options.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    command
        .arg(if cfg!(windows) { "--pipe" } else { "--unix" })
        .arg(&endpoint)
        .args(["--no-ws", "--home"])
        .arg(&native_home);
    if let Some(path) = &options.model_config {
        command.arg("--model-config").arg(path);
    } else {
        let product_home = std::fs::canonicalize(&options.home)?;
        command.arg("--product-home").arg(&product_home);
    }
    // The Web workspace is a separate artifact the daemon serves from a
    // directory (web-workspace.md §2.4).
    if let Some(path) = web_assets(binary, &options.home) {
        command.arg("--web-assets").arg(path);
    }
    if let Some(path) = default_plugins(binary) {
        command.arg("--defaults").arg(path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    let _ = std::fs::remove_file(&startup_note);
    let mut child = command.spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match Client::open(&connection, true).await {
            Ok(mut client) => {
                client.hello().await?;
                let _ = std::fs::remove_file(&startup_note);
                return Ok(Some(client));
            }
            Err(error)
                if missing(&error) || cfg!(unix) && error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = child.try_wait()? {
            let cause = startup_failure(&startup_note, child.id());
            let _ = std::fs::remove_file(&startup_note);
            return Err(match cause {
                Some(cause) => format!("{cause}\n  Details: {}", log_path.display()).into(),
                None if server.embedded => {
                    format!("App Server exited ({status}); see {}", log_path.display()).into()
                }
                None => format!(
                    "{SERVER_REQUIREMENT}; it exited before serving ({status}); see {}",
                    log_path.display()
                )
                .into(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(if server.embedded {
                format!("App Server startup timed out; see {}", log_path.display())
            } else {
                format!(
                    "{SERVER_REQUIREMENT}; it did not start in time; see {}",
                    log_path.display()
                )
            }
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The storage startup note this child left, if it is a private file owned by
/// the current user and the header names this child.
fn startup_failure(path: &std::path::Path, pid: u32) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    let (header, body) = text.split_once('\n')?;
    if header != format!("dimcode-startup-failure v1 pid={pid}") {
        return None;
    }
    let body: String = body
        .chars()
        .filter(|character| *character == '\n' || *character == '\t' || !character.is_control())
        .collect();
    let body = body.trim_end();
    (!body.is_empty()).then(|| body.to_owned())
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && error.raw_os_error() == Some(231)
}

#[cfg(test)]
mod web_assets_tests {
    use super::web_assets_in;

    /// The configured directory wins, then the one beside `bin/`, then the
    /// product home's; a directory without `index.html` does not count.
    #[test]
    fn the_frontend_is_found_in_order() {
        let root = std::env::temp_dir().join(format!("dim-web-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (configured, beside, home) = (root.join("dev"), root.join("pkg"), root.join("home"));
        let exe = beside.join("bin").join("dimcode");
        for dir in [&configured, &beside.join("web"), &home.join("web")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            None
        );
        std::fs::write(home.join("web/index.html"), "").unwrap();
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            Some(home.join("web"))
        );
        std::fs::write(beside.join("web/index.html"), "").unwrap();
        assert_eq!(web_assets_in(None, &exe, &home), Some(beside.join("web")));
        std::fs::write(configured.join("index.html"), "").unwrap();
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            Some(configured.clone())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, unix))]
mod same_file_tests {
    use super::same_file;
    use std::fs;

    /// A file, a hard link and a symlink to it are the same file; a copy
    /// and a missing path are not.
    #[test]
    fn links_name_the_same_file() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("dimcode");
        fs::write(&file, "binary").unwrap();
        let hard = root.path().join("dimcode-server");
        fs::hard_link(&file, &hard).unwrap();
        let symlink = root.path().join("dimcode-link");
        std::os::unix::fs::symlink(&file, &symlink).unwrap();
        let copy = root.path().join("copy");
        fs::copy(&file, &copy).unwrap();

        assert!(same_file(&file, &hard), "hard link");
        assert!(same_file(&file, &symlink), "symlink");
        assert!(same_file(&hard, &symlink), "link and symlink");
        assert!(!same_file(&file, &copy), "copy");
        assert!(
            !same_file(&file, &root.path().join("missing")),
            "missing path"
        );
    }
}
