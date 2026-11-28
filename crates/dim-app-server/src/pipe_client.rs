//! Authenticated Windows pipe transport for clients without native handle access.
use std::io::{self, Write};

pub fn run(name: &str) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let pipe = match dim_client::pipe::connect(name).await {
            Ok(pipe) => pipe,
            Err((code, error)) => {
                writeln!(
                    io::stderr(),
                    "{}",
                    serde_json::json!({"status":"error", "code":code, "message":error.to_string()})
                )?;
                return Err(error);
            }
        };
        writeln!(io::stderr(), "{}", serde_json::json!({"status":"ready"}))?;
        io::stderr().flush()?;
        let (mut reader, mut writer) = tokio::io::split(pipe);
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        tokio::select! {
            result = tokio::io::copy(&mut stdin, &mut writer) => { result?; },
            result = tokio::io::copy(&mut reader, &mut stdout) => { result?; },
        }
        use tokio::io::AsyncWriteExt;
        stdout.flush().await
    });
    runtime.shutdown_background();
    result
}
