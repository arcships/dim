use crate::{Result, args::Options, sanitize::safe};
use dim_migration::{Migration, MigrationPlugin, TaskStatus};
use rutis::host::CtxHost;
use serde_json::{Map, Value, json};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(crate) enum Operation {
    Inspect,
    Run,
    Retry(String),
    List,
    Show(String),
    Cancel(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Command {
    pub operation: Operation,
    pub source: Option<PathBuf>,
    pub source_stopped: bool,
}

const USAGE: &str = "Use migration [run|inspect] [--source OLD_HOME], migration retry TASK [--source OLD_HOME], or migration list|show TASK|cancel TASK; scripts must pass --source-stopped";

pub(crate) fn parse(
    args: &mut std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<crate::args::Action> {
    let operation = match args.peek() {
        Some(value) if value.starts_with('-') => None,
        _ => args.next(),
    };
    let operation = match operation.as_deref() {
        None => Operation::Run,
        Some("inspect") => Operation::Inspect,
        Some("run") => Operation::Run,
        Some("retry") => Operation::Retry(args.next().ok_or(USAGE)?),
        Some("list") => Operation::List,
        Some("show") => Operation::Show(args.next().ok_or(USAGE)?),
        Some("cancel") => Operation::Cancel(args.next().ok_or(USAGE)?),
        _ => return Err(USAGE.into()),
    };
    if matches!(&operation,Operation::Retry(id)|Operation::Show(id)|Operation::Cancel(id) if id=="--help" || id=="-h")
    {
        return Ok(crate::args::Action::Help);
    }
    Ok(crate::args::Action::Migration(Command {
        operation,
        source: None,
        source_stopped: false,
    }))
}

pub(crate) async fn run(
    root: &rutis::Ctx,
    options: &Options,
    command: &Command,
    locale: crate::locale::Locale,
) -> Result<i32> {
    let terminal = dim_migration::terminal::Terminal::new(options.json);
    let json_output = !terminal.is_interactive()
        || !matches!(command.operation, Operation::Run | Operation::Retry(_));
    if matches!(
        command.operation,
        Operation::Inspect | Operation::Run | Operation::Retry(_)
    ) && !command.source_stopped
        && !std::io::stdin().is_terminal()
    {
        return Err(
            "Exit legacy CLI and Desktop using the source home, then pass --source-stopped".into(),
        );
    }
    if matches!(
        command.operation,
        Operation::List | Operation::Show(_) | Operation::Cancel(_)
    ) && (command.source.is_some() || command.source_stopped)
    {
        return Err(USAGE.into());
    }
    let importing = matches!(command.operation, Operation::Run | Operation::Inspect);
    let alpha = crate::update::channel(env!("CARGO_PKG_VERSION")) == "alpha";
    let user_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("Cannot locate the distribution homes; pass --home TARGET");
    if options.home_explicit || importing && command.source.is_some() {
        let mut options = options.clone();
        if !options.home_explicit
            && let Some(source) = &command.source
        {
            if source.file_name() != Some(std::ffi::OsStr::new("v2")) {
                return Err("A custom source without a v2 directory requires --home TARGET".into());
            }
            options.home = if alpha {
                user_home?.join(".dimcode-alpha")
            } else {
                let source = std::env::current_dir()?.join(source);
                std::fs::canonicalize(source.parent().ok_or(USAGE)?)?
            };
        }
        let (code, output) = run_home(root, &options, command, &terminal, locale).await?;
        if let Some(output) = output {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        } else if code == 0 {
            return Err("Migration task was not found in the selected home".into());
        }
        return Ok(code);
    }
    let user_home = user_home?;
    let distributions: &[_] = if alpha {
        &[("cn", ".dimcode", ".dimcode-alpha", "dimcode.sqlite")]
    } else {
        &[
            ("global", ".dim", ".dim", "dimglobal.sqlite"),
            ("cn", ".dimcode", ".dimcode", "dimcode.sqlite"),
        ]
    };
    let homes: Vec<_> = distributions
        .iter()
        .filter_map(|&(name, source_directory, target_directory, database)| {
            let home = user_home.join(target_directory);
            let source = user_home.join(source_directory).join("v2");
            let present = if importing {
                source.join(database).is_file()
            } else {
                home.is_dir()
            };
            present.then_some((name, source, home))
        })
        .collect();
    if importing && homes.is_empty() {
        return Err(
            "No default legacy v2 database found; pass --source OLD_HOME for a custom location"
                .into(),
        );
    }
    if importing
        && !command.source_stopped
        && !confirm_sources(
            homes
                .iter()
                .map(|(_, source, home)| (source.as_path(), home.as_path())),
        )?
    {
        return Ok(130);
    }
    let mut results = Map::new();
    let mut code = 0;
    for (name, source, home) in homes {
        let mut options = options.clone();
        options.home = home.clone();
        let mut command = command.clone();
        if importing {
            command.source = Some(source);
            command.source_stopped = true;
        }
        let invocation = rutis::Ctx::root()?;
        let outcome = run_home(&invocation, &options, &command, &terminal, locale).await;
        let closed = invocation.shutdown().await;
        let outcome = match closed {
            Ok(()) => outcome,
            Err(error) => Err(error.into()),
        };
        match outcome {
            Ok((status, output)) => {
                if let Some(output) = output {
                    results.insert(name.into(), json!({"home":home,"result":output}));
                }
                if status == 130 {
                    code = 130;
                    break;
                }
                if status == 1 || code == 0 {
                    code = status;
                }
            }
            Err(error) => {
                let cancelled = matches!(
                    error.downcast_ref::<dim_migration::Error>(),
                    Some(dim_migration::Error::Cancelled)
                );
                if !json_output {
                    terminal.error(&home, &error.to_string())?;
                }
                results.insert(name.into(), json!({"home":home,"error":error.to_string()}));
                code = if cancelled { 130 } else { 1 };
                if cancelled {
                    break;
                }
            }
        }
    }
    if results.is_empty() && code == 130 {
        return Ok(code);
    }
    if results.is_empty() && !matches!(command.operation, Operation::List) {
        return Err("Migration task was not found in the distribution homes; use --home for a custom target".into());
    }
    if json_output {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    Ok(code)
}

async fn run_home(
    root: &rutis::Ctx,
    options: &Options,
    command: &Command,
    terminal: &dim_migration::terminal::Terminal,
    locale: crate::locale::Locale,
) -> Result<(i32, Option<Value>)> {
    let mut command = command.clone();
    if matches!(command.operation, Operation::Inspect | Operation::Run) {
        let source = command
            .source
            .take()
            .unwrap_or_else(|| options.home.join("v2"));
        let source = dim_migration::validate_source_home(&source, &options.home)?;
        if !command.source_stopped
            && !confirm_sources([(source.as_path(), options.home.as_path())])?
        {
            return Ok((130, None));
        }
        command.source = Some(source);
        command.source_stopped = true;
    }
    std::fs::create_dir_all(&options.home)?;
    let themes = std::iter::once("system")
        .chain(
            crate::ui::theme::themes::THEMES
                .iter()
                .map(|theme| theme.id),
        )
        .map(str::to_owned)
        .collect();
    let plugin = root.plugin(MigrationPlugin::new(
        options.home.clone(),
        PathBuf::from("rust"),
        themes,
    ));
    (&plugin).await.map_err(crate::plugin_error)?;
    let service = root
        .get::<Migration>()
        .ok_or("Migration plugin provided no service")?;
    let result = async {
        let task = match &command.operation {
            Operation::Show(id) | Operation::Retry(id) | Operation::Cancel(id) => Some(id),
            _ => None,
        };
        if let Some(id) = task
            && !service.list().await?.iter().any(|task| &task.id == id)
        {
            return Ok((0, None));
        }
        execute(root, &service, options, &command, terminal, locale).await
    }
    .await;
    plugin.dispose().await?;
    result
}

fn confirm_sources<'a>(sources: impl IntoIterator<Item = (&'a Path, &'a Path)>) -> Result<bool> {
    for (source, target) in sources {
        eprintln!("Source: {}", safe(&source.to_string_lossy()));
        eprintln!("Target: {}", safe(&target.to_string_lossy()));
    }
    eprint!("Close legacy and native clients using these homes. Continue? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

async fn execute(
    root: &rutis::Ctx,
    service: &std::sync::Arc<Migration>,
    options: &Options,
    command: &Command,
    terminal: &dim_migration::terminal::Terminal,
    locale: crate::locale::Locale,
) -> Result<(i32, Option<Value>)> {
    match &command.operation {
        Operation::List => Ok((0, Some(serde_json::to_value(service.list().await?)?))),
        Operation::Show(id) => Ok((0, Some(serde_json::to_value(service.report(id).await?)?))),
        Operation::Cancel(id) => Ok((0, Some(serde_json::to_value(service.cancel(id).await?)?))),
        Operation::Inspect => {
            let source = std::fs::canonicalize(command.source.as_ref().ok_or(USAGE)?)?;
            Ok((
                0,
                Some(serde_json::to_value(service.inspect(source, true).await?)?),
            ))
        }
        Operation::Run | Operation::Retry(_) => {
            let retry = match &command.operation {
                Operation::Retry(id) => Some(id.clone()),
                _ => None,
            };
            let source = match &command.source {
                Some(source) => source.clone(),
                None => {
                    service
                        .report(retry.as_deref().ok_or(USAGE)?)
                        .await?
                        .task
                        .source_home
                }
            };
            let source = dim_migration::validate_source_home(&source, &options.home)?;
            if !command.source_stopped
                && !confirm_sources([(source.as_path(), options.home.as_path())])?
            {
                return Ok((130, None));
            }
            let (link, connection) = crate::daemon_link(root, options, false, locale).await?;
            if let Some(client) = connection.take() {
                crate::client::shutdown(client).await?;
            }
            link.dispose().await?;
            let report = terminal.run(service, source, true, retry).await?;
            let code = match report.task.status {
                TaskStatus::Completed => 0,
                TaskStatus::Partial => 2,
                TaskStatus::Cancelled => 130,
                _ => 1,
            };
            Ok((code, Some(serde_json::to_value(report)?)))
        }
    }
}
