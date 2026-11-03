//! The process boundary the sandbox switch applies, before ordinary spawn/PTY.
use super::{HostPlatform, Isolation, ProcessSpec, ProcessSpecError, TemporaryDirectory};
use std::path::Path;
use std::{ffi::OsString, path::PathBuf};

#[cfg(not(target_os = "macos"))]
pub(super) const BWRAP_UNAVAILABLE: &str = "bubblewrap (bwrap) is not installed on the captured host PATH. Install bubblewrap, or select Off in /settings > Process isolation";

/// The bubblewrap executable preparation would run, from the captured host
/// `PATH`. The availability report and preparation share this resolution.
#[cfg(not(target_os = "macos"))]
pub(super) fn bubblewrap(env: &[(OsString, OsString)]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    super::path_entries(env)
        .into_iter()
        .map(|path| path.join("bwrap"))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Resolves the system roots once at the sandbox boundary, each with its
/// requested path and canonical source. Returns the directories, parents
/// before children, and the regular files, which only read-only roots
/// (`files`) may name.
pub(super) async fn roots(
    paths: Vec<PathBuf>,
    files: bool,
) -> Result<(Vec<(PathBuf, PathBuf)>, Vec<(PathBuf, PathBuf)>), String> {
    let mut directories = Vec::with_capacity(paths.len());
    let mut regular = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(format!(
                "Isolation root must be absolute: {}",
                path.display()
            ));
        }
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|error| format!("Isolation root {}: {error}", path.display()))?;
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            directories.push((path, canonical));
        } else if files && metadata.is_file() {
            regular.push((path, canonical));
        } else if files {
            return Err(format!(
                "Read-only isolation root must be a directory or a regular file: {}",
                path.display()
            ));
        } else {
            return Err(format!(
                "Writable isolation root must be a directory: {}",
                path.display()
            ));
        }
    }
    directories.sort_by_key(|(path, _)| path.components().count());
    Ok((directories, regular))
}

pub(super) fn validate_overlap(
    read_only: &[(PathBuf, PathBuf)],
    read_only_files: &[(PathBuf, PathBuf)],
    writable: &[(PathBuf, PathBuf)],
) -> Result<(), String> {
    if read_only
        .iter()
        .chain(read_only_files)
        .any(|(read_path, read)| {
            writable.iter().any(|(write_path, write)| {
                read.starts_with(write) || read_path.starts_with(write_path)
            })
        })
    {
        return Err("A read-only isolation root cannot equal or be inside a writable root".into());
    }
    Ok(())
}

impl HostPlatform {
    pub(crate) async fn prepare_isolated_process(
        &self,
        mut spec: ProcessSpec,
        workspaces: &[PathBuf],
        output_dir: Option<&Path>,
    ) -> Result<(ProcessSpec, Option<TemporaryDirectory>, Option<Isolation>), ProcessSpecError>
    {
        let invalid = |reason| ProcessSpecError {
            code: "isolation_config_invalid",
            reason,
        };
        let settings = self.execution_settings().await?;
        if !settings.sandbox {
            return Ok((spec, None, None));
        }
        let environment: Vec<String> = [
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "NO_COLOR",
            "PAGER",
            "GIT_PAGER",
            "GH_PAGER",
            #[cfg(target_os = "macos")]
            "DEVELOPER_DIR",
            #[cfg(target_os = "macos")]
            "XDG_CONFIG_HOME",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let (read_only_paths, default_files) = {
            let mut installed = Vec::new();
            for path in &self.platform_roots {
                match tokio::fs::metadata(path).await {
                    Ok(_) => installed.push(path.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(invalid(format!(
                            "System isolation root {}: {error}",
                            path.display()
                        )));
                    }
                }
            }
            #[cfg(target_os = "macos")]
            {
                let developer = received(&environment, &spec.env, "DEVELOPER_DIR")
                    .unwrap_or_else(|| "/var/select/developer_dir".into());
                if developer.is_absolute()
                    && tokio::fs::metadata(&developer)
                        .await
                        .is_ok_and(|metadata| metadata.is_dir())
                {
                    installed.push(developer);
                }
            }
            #[cfg(target_os = "macos")]
            let files = git_configuration(&environment, &spec.env).await;
            #[cfg(not(target_os = "macos"))]
            let files = Vec::new();
            (installed, files)
        };
        let (read_only, mut read_only_files) =
            roots(read_only_paths, true).await.map_err(invalid)?;
        let writable_paths: Vec<PathBuf> = workspaces
            .iter()
            .map(PathBuf::as_path)
            .chain(output_dir)
            .map(Path::to_path_buf)
            .collect();
        let (mut writable, _) = roots(writable_paths, false).await.map_err(invalid)?;
        validate_overlap(&read_only, &read_only_files, &writable).map_err(invalid)?;
        read_only_files.extend(default_files);
        spec.env
            .retain(|(name, _)| environment.iter().any(|allowed| name == allowed.as_str()));
        let temporary = if cfg!(target_os = "macos") {
            let temporary = temporary_directory()
                .await
                .map_err(|error| ProcessSpecError {
                    code: "isolation_unavailable",
                    reason: format!(
                        "Isolated temporary directory: {error}; select Off in /settings > Process isolation"
                    ),
                })?;
            writable.push((temporary.0.clone(), temporary.0.clone()));
            spec.env.retain(|(name, _)| name != "TMPDIR");
            spec.env
                .push(("TMPDIR".into(), temporary.0.clone().into_os_string()));
            Some(temporary)
        } else {
            None
        };
        let spec = prepare(
            self,
            spec,
            &read_only,
            &read_only_files,
            &writable,
            settings.network,
        )?;
        Ok((spec, temporary, Some(Isolation)))
    }
}

/// A nonempty value that `environment` passes to the command from `env`.
#[cfg(target_os = "macos")]
fn received(environment: &[String], env: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    if !environment.iter().any(|allowed| allowed == name) {
        return None;
    }
    env.iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| PathBuf::from(value))
        .filter(|value| !value.as_os_str().is_empty())
}

/// The files git reads for its global configuration, excludes and attributes,
/// where they exist, with their canonical paths. git exits when an existing
/// configuration file cannot be read. The XDG git directory is not granted
/// whole because git's credential store also reads it.
#[cfg(target_os = "macos")]
async fn git_configuration(
    environment: &[String],
    env: &[(OsString, OsString)],
) -> Vec<(PathBuf, PathBuf)> {
    let home = received(environment, env, "HOME");
    let config = received(environment, env, "XDG_CONFIG_HOME")
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let mut files = Vec::new();
    for path in home
        .map(|home| home.join(".gitconfig"))
        .into_iter()
        .chain(config.into_iter().flat_map(|config| {
            ["config", "ignore", "attributes"].map(|name| config.join("git").join(name))
        }))
        .filter(|path| path.is_absolute())
    {
        if let Ok(canonical) = tokio::fs::canonicalize(&path).await
            && tokio::fs::metadata(&canonical)
                .await
                .is_ok_and(|metadata| metadata.is_file())
        {
            files.push((path, canonical));
        }
    }
    files
}

/// A fresh, private directory under the service's system temporary directory.
async fn temporary_directory() -> std::io::Result<TemporaryDirectory> {
    let parent = tokio::fs::canonicalize(std::env::temp_dir()).await?;
    let path = parent.join(format!("dim-isolated-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .await?;
    Ok(TemporaryDirectory(path))
}

#[cfg(target_os = "macos")]
fn prepare(
    _host: &HostPlatform,
    mut spec: ProcessSpec,
    read_only: &[(PathBuf, PathBuf)],
    read_only_files: &[(PathBuf, PathBuf)],
    writable: &[(PathBuf, PathBuf)],
    network: bool,
) -> Result<ProcessSpec, ProcessSpecError> {
    let mut profile = String::from(
        "(version 1)(deny default)(allow process*)(allow sysctl-read)\
         (allow file-read* (literal \"/\") (literal \"/dev/random\") (literal \"/dev/urandom\"))\
         (allow file-read* file-write* (literal \"/dev/null\"))",
    );
    for command in [
        libc::TIOCGETD,
        libc::TIOCGETA,
        libc::TIOCSETA,
        libc::TIOCSETAW,
        libc::TIOCSETAF,
        libc::TIOCGWINSZ,
        libc::TIOCSWINSZ,
        libc::TIOCGPGRP,
        libc::TIOCSPGRP,
    ] {
        profile.push_str(&format!(
            "(allow file-ioctl (require-all (regex #\"^/dev/(ttys[0-9]+|tty)$\") (ioctl-command {command})))"
        ));
    }
    let mut args: Vec<OsString> = Vec::new();
    for (index, ((destination, source), write, filter)) in read_only
        .iter()
        .map(|root| (root, false, "subpath"))
        .chain(read_only_files.iter().map(|file| (file, false, "literal")))
        .chain(writable.iter().map(|root| (root, true, "subpath")))
        .enumerate()
    {
        for (alias, path) in std::iter::once(source)
            .chain((destination != source).then_some(destination))
            .enumerate()
        {
            let name = format!("ROOT_{index}_{alias}");
            let mut parameter = OsString::from(format!("{name}="));
            parameter.push(path);
            args.extend(["-D".into(), parameter]);
            profile.push_str(&format!(
                "(allow file-read* {}({filter} (param \"{name}\")))\
                 (allow file-read-metadata (path-ancestors (param \"{name}\")))",
                if write { "file-write* " } else { "" }
            ));
            if network && filter == "subpath" {
                profile.push_str(&format!(
                    "(allow network-outbound (remote unix-socket (subpath (param \"{name}\"))))"
                ));
                if write {
                    profile.push_str(&format!(
                        "(allow network-bind network-inbound (local unix-socket (subpath (param \"{name}\"))))"
                    ));
                }
            }
        }
    }
    if network {
        profile.push_str(
            "(allow network* (local ip) (remote ip))\
             (allow file-read-metadata (literal \"/var\"))\
             (allow network-outbound (literal \"/private/var/run/mDNSResponder\"))",
        );
    }
    args.extend([
        "-p".into(),
        profile.into(),
        spec.executable.into_os_string(),
    ]);
    args.append(&mut spec.args);
    spec.executable = "/usr/bin/sandbox-exec".into();
    spec.args = args;
    Ok(spec)
}

#[cfg(not(target_os = "macos"))]
fn prepare(
    host: &HostPlatform,
    mut spec: ProcessSpec,
    read_only: &[(PathBuf, PathBuf)],
    read_only_files: &[(PathBuf, PathBuf)],
    writable: &[(PathBuf, PathBuf)],
    network: bool,
) -> Result<ProcessSpec, ProcessSpecError> {
    let executable = bubblewrap(&host.env).ok_or_else(|| ProcessSpecError {
        code: "isolation_unavailable",
        reason: BWRAP_UNAVAILABLE.into(),
    })?;
    let mut args: Vec<OsString> = [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if !network {
        args.push("--unshare-net".into());
    }
    args.extend(binds(read_only, read_only_files, writable));
    args.extend([
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--remount-ro".into(),
        "/".into(),
        "--chdir".into(),
        spec.cwd.as_os_str().into(),
        "--".into(),
        spec.executable.into_os_string(),
    ]);
    args.append(&mut spec.args);
    spec.executable = executable;
    spec.args = args;
    Ok(spec)
}

/// bubblewrap's bind arguments, each from a root's canonical source at its
/// requested destination. bubblewrap applies them in order, so a bind inside
/// an earlier one covers that part of it, and creates a missing destination
/// with its missing parents. Read-only directories come first, parents before
/// children; then read-only files, which may lie inside those directories
/// but never inside a writable root; then writable directories, parents
/// before children, which may lie inside read-only directories.
#[cfg(any(test, not(target_os = "macos")))]
fn binds(
    read_only: &[(PathBuf, PathBuf)],
    read_only_files: &[(PathBuf, PathBuf)],
    writable: &[(PathBuf, PathBuf)],
) -> Vec<OsString> {
    let mut args = Vec::new();
    for (flag, paths) in [
        ("--ro-bind", read_only),
        ("--ro-bind", read_only_files),
        ("--bind", writable),
    ] {
        for (destination, source) in paths {
            args.extend([
                flag.into(),
                source.as_os_str().into(),
                destination.as_os_str().into(),
            ]);
        }
    }
    args
}

#[cfg(test)]
#[tokio::test]
async fn bubblewrap_binds_file_roots_after_the_directories_that_contain_them() {
    let base = tokio::fs::canonicalize(std::env::temp_dir())
        .await
        .unwrap()
        .join(format!(
            "dim-isolation-binds-{}",
            uuid::Uuid::new_v4().simple()
        ));
    for directory in ["home/tools", "home/work", "dotfiles"] {
        std::fs::create_dir_all(base.join(directory)).unwrap();
    }
    for file in ["home/tools/config", "dotfiles/gitconfig"] {
        std::fs::write(base.join(file), "").unwrap();
    }
    std::os::unix::fs::symlink(
        base.join("dotfiles/gitconfig"),
        base.join("home/.gitconfig"),
    )
    .unwrap();
    let (read_only, read_only_files) = roots(
        ["home/tools/config", "home/tools", "home/.gitconfig", "home"]
            .map(|path| base.join(path))
            .into(),
        true,
    )
    .await
    .unwrap();
    let (writable, _) = roots(vec![base.join("home/work")], false).await.unwrap();
    let expected: Vec<OsString> = [
        ["--ro-bind", "home", "home"],
        ["--ro-bind", "home/tools", "home/tools"],
        ["--ro-bind", "home/tools/config", "home/tools/config"],
        ["--ro-bind", "dotfiles/gitconfig", "home/.gitconfig"],
        ["--bind", "home/work", "home/work"],
    ]
    .into_iter()
    .flat_map(|[flag, source, destination]| {
        [
            flag.into(),
            base.join(source).into_os_string(),
            base.join(destination).into_os_string(),
        ]
    })
    .collect();
    assert_eq!(binds(&read_only, &read_only_files, &writable), expected);
    std::fs::remove_dir_all(base).unwrap();
}

#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn bubblewrap_unshares_the_network_only_when_the_switch_is_off() {
    use std::os::unix::fs::PermissionsExt;
    let base = tokio::fs::canonicalize(std::env::temp_dir())
        .await
        .unwrap()
        .join(format!(
            "dim-isolation-net-{}",
            uuid::Uuid::new_v4().simple()
        ));
    std::fs::create_dir_all(&base).unwrap();
    let bwrap = base.join("bwrap");
    std::fs::write(&bwrap, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = HostPlatform::current(super::ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap();
    host.env = vec![(OsString::from("PATH"), base.clone().into_os_string())];
    let spec = || ProcessSpec {
        executable: "/bin/true".into(),
        args: Vec::new(),
        cwd: "/".into(),
        env: Vec::new(),
    };
    let isolated = prepare(&host, spec(), &[], &[], &[], false).unwrap();
    assert!(
        isolated
            .args
            .iter()
            .any(|arg| arg.to_str() == Some("--unshare-net"))
    );
    let networked = prepare(&host, spec(), &[], &[], &[], true).unwrap();
    assert!(
        !networked
            .args
            .iter()
            .any(|arg| arg.to_str() == Some("--unshare-net"))
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_allows_network_only_when_the_switch_is_on() {
    let host = HostPlatform::current(super::ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap();
    let spec = || ProcessSpec {
        executable: "/bin/true".into(),
        args: Vec::new(),
        cwd: "/".into(),
        env: Vec::new(),
    };
    let isolated = prepare(&host, spec(), &[], &[], &[], false).unwrap();
    assert!(!isolated.args.iter().any(|arg| {
        arg.to_str()
            .is_some_and(|arg| arg.contains("(allow network*"))
    }));
    let networked = prepare(&host, spec(), &[], &[], &[], true).unwrap();
    assert!(networked.args.iter().any(|arg| {
        arg.to_str()
            .is_some_and(|arg| arg.contains("(allow network*"))
    }));
}
