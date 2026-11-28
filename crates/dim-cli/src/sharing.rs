use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize, Default)]
pub(crate) struct Status {
    pub name: String,
    pub enabled: bool,
    pub token: Option<String>,
    pub addresses: Vec<String>,
    pub error: Option<String>,
}

pub(crate) fn installed() -> bool {
    std::env::current_exe().is_ok_and(|exe| {
        exe.with_file_name(format!("dim-sharing{}", std::env::consts::EXE_SUFFIX))
            .is_file()
    })
}

pub(crate) fn command(home: &Path) -> crate::Result<Command> {
    let path = std::env::current_exe()?
        .with_file_name(format!("dim-sharing{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = Command::new(path);
    cmd.arg("--home").arg(home.join("rust"));
    Ok(cmd)
}
pub(crate) fn run(home: &Path, args: &[String]) -> crate::Result<i32> {
    if args.is_empty()
        || !matches!(
            args[0].as_str(),
            "status" | "enable" | "disable" | "reset" | "rename" | "discover" | "connect"
        )
    {
        return Err(
            "Use share status, enable, disable, rename NAME, reset, discover or connect ADDRESS"
                .into(),
        );
    }
    Ok(command(home)?.args(args).status()?.code().unwrap_or(1))
}
pub(crate) fn setting(home: &Path, args: &[String]) -> crate::Result<Status> {
    let mut owner = Command::new(std::env::current_exe()?);
    let result = owner
        .env_remove("DIMCODE_TERMINAL_PROXY")
        .arg("--home")
        .arg(home)
        .arg("share")
        .args(args)
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr)
            .trim()
            .to_owned()
            .into());
    }
    Ok(serde_json::from_slice(&result.stdout)?)
}
pub(crate) fn unavailable(command: &str) -> String {
    format!(
        "{command} is unavailable through a shared connection. \
         Run it directly on the intended computer."
    )
}

pub(crate) fn proxy() -> Option<PathBuf> {
    std::env::var_os("DIMCODE_TERMINAL_PROXY").map(PathBuf::from)
}
pub(crate) async fn leaf_address(proxy: Option<&Path>, local: &str) -> crate::Result<String> {
    let Some(proxy) = proxy else {
        return Ok(local.to_owned());
    };
    let path = proxy.with_extension("leaf");
    #[cfg(unix)]
    let mut control = tokio::net::UnixStream::connect(path).await?;
    #[cfg(windows)]
    let mut control = dim_client::pipe::connect(path.to_str().ok_or("invalid leaf pipe")?)
        .await
        .map_err(|(_, err)| err)?;
    control.write_all(local.as_bytes()).await?;
    control.write_all(b"\n").await?;
    let mut address = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        BufReader::new(control.take(4097)).read_line(&mut address),
    )
    .await??;
    Ok(address
        .strip_suffix('\n')
        .ok_or("shared computer did not prepare its leaf listener")?
        .to_owned())
}
