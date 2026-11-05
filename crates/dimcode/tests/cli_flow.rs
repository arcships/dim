#![cfg(unix)]

#[path = "cli_flow/accounts.rs"]
mod accounts;

#[path = "cli_flow/acp.rs"]
mod acp;

#[path = "cli_flow/catalog.rs"]
mod catalog;

#[path = "cli_flow/context.rs"]
mod context;

#[path = "cli_flow/exit.rs"]
mod exit;

#[path = "cli_flow/goal.rs"]
mod goal;

#[path = "cli_flow/hooks.rs"]
mod hooks;

#[path = "cli_flow/mcp.rs"]
mod mcp;

#[path = "cli_flow/migration.rs"]
mod migration;
#[path = "cli_flow/models.rs"]
mod models;
#[path = "cli_flow/permission.rs"]
mod permission;
#[path = "cli_flow/prompt_suggestion.rs"]
mod prompt_suggestion;
#[path = "cli_flow/question.rs"]
mod question;
#[path = "cli_flow/scheduled.rs"]
mod scheduled;
#[path = "cli_flow/server.rs"]
mod server;
#[path = "cli_flow/session_management.rs"]
mod session_management;
#[path = "cli_flow/skills.rs"]
mod skills;
#[path = "cli_flow/startup.rs"]
mod startup;
#[path = "cli_flow/tasks.rs"]
mod tasks;
#[path = "cli_flow/title.rs"]
mod title;
#[path = "cli_flow/workflow.rs"]
mod workflow;

#[path = "support/daemon.rs"]
mod daemon_cleanup;

use dim_agent_capabilities::images::{MAX_BASE64_BYTES, MAX_EDGE, base64_len};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Home(PathBuf, Arc<AtomicBool>);
impl Home {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("dim-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("rust")).unwrap();
        fs::set_permissions(path.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(path.join("config.json"), r#"{"locale":"en"}"#).unwrap();
        Self::at(path)
    }
    fn at(path: PathBuf) -> Self {
        Self(path, Arc::new(AtomicBool::new(false)))
    }
    /// Ends the CLI itself, as the last step of a flow. From here on a closed
    /// connection is the end the flow expects; before it, `Wire` fails on one.
    fn kill(&self, terminal: &Terminal) {
        self.1.store(true, Ordering::SeqCst);
        terminal.signal(libc::SIGKILL);
    }
    fn daemon(&self, flow: impl FnOnce(&mut Wire) + Send + 'static) -> thread::JoinHandle<()> {
        // Each flow owns the deliberate-kill mark. A prior flow's `kill` must
        // not make this one accept an unexpected close, and a flow's own kill
        // is only set while it runs: the tests join an earlier daemon before
        // starting the next, so nothing is pending here.
        self.1.store(false, Ordering::SeqCst);
        let path = self.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let ending = Arc::clone(&self.1);
        thread::spawn(move || {
            // A client that fails before connecting must fail the test, not
            // hang the run.
            let deadline = Instant::now() + HANG_GUARD;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "no client connected within {HANG_GUARD:?}"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire(BufReader::new(stream), ending);
            if let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| flow(&mut wire)))
                && !payload.is::<PeerGone>()
            {
                std::panic::resume_unwind(payload);
            }
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command.args(["--home", self.0.to_str().unwrap()]);
        command.env("DIMCODE_UPDATE_CHECK", "0");
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        daemon_cleanup::stop_daemon(&self.0);
        daemon_cleanup::remove_directory(&self.0);
    }
}

/// How long a step that should succeed may take before the test fails. CI
/// shares one machine between jobs, and a debug CLI starting under that load
/// can take several seconds; a passing step never waits this long.
const PATIENCE: Duration = Duration::from_secs(30);

/// Bounds a peer frame that never arrives, so a broken path fails instead of
/// hanging; it is not a step's budget. A frame is due when the CLI and the
/// fixture's daemon reach it, and a debug CLI starting on a loaded machine can
/// take tens of seconds, so ten times the 30 s step budget keeps machine load
/// out of the result: the scripted daemon's socket reads, the deadline
/// `Wire::frame` retries within, and the terminal waits for what the CLI draws
/// or accepts (#807).
const HANG_GUARD: Duration = Duration::from_secs(300);

/// The test ended the CLI itself: the flow stops where it would answer a CLI
/// that is gone. `Home::kill` sets that expectation before the signal; without
/// it a closed connection fails the flow, so a CLI that exits on its own still
/// fails the daemon thread. A CLI that stays connected and silent is not gone;
/// that still fails at `HANG_GUARD`.
struct PeerGone;

struct Wire(BufReader<UnixStream>, Arc<AtomicBool>);
impl Wire {
    fn new(stream: UnixStream) -> Self {
        Self(BufReader::new(stream), Arc::new(AtomicBool::new(false)))
    }
    fn read(&mut self, method: &str) -> Value {
        let value = self.frame();
        assert_eq!(value["method"], method);
        value
    }
    /// The next request, whichever it is: for requests whose order the
    /// flow does not fix.
    fn read_any(&mut self) -> Value {
        self.frame()
    }
    fn frame(&mut self) -> Value {
        let deadline = Instant::now() + HANG_GUARD;
        let mut line = String::new();
        loop {
            match self.0.read_line(&mut line) {
                Ok(0) => self.closed(format!("the CLI closed before a frame: {line:?}")),
                Ok(_) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "no frame within {HANG_GUARD:?}: {line:?}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                    self.closed(format!("the CLI reset before a frame: {line:?}"));
                }
                Err(error) => panic!("{error}"),
            }
        }
        serde_json::from_str(&line).unwrap()
    }
    /// The request that arrives after the flow settled, or `None` when the CLI
    /// closed first. A client killed with a reply still unread resets the
    /// connection instead of ending it; both mean no request arrived.
    fn trailing_request(&mut self) -> Option<String> {
        let deadline = Instant::now() + HANG_GUARD;
        let mut line = String::new();
        loop {
            match self.0.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => return Some(line),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "the CLI neither closed nor sent a request: {line:?}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return None,
                Err(error) => panic!("{error}"),
            }
        }
    }
    fn send(&mut self, value: Value) {
        let written = writeln!(self.0.get_mut(), "{value}").and_then(|()| self.0.get_mut().flush());
        match written {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                self.closed(format!("the CLI closed while the flow replied: {value}"));
            }
            Err(error) => panic!("{error}"),
        }
    }
    /// A closed connection while the flow waits: the end of the flow only when
    /// the test ended the CLI with `Home::kill`.
    fn closed(&self, message: String) -> ! {
        if self.1.load(Ordering::SeqCst) {
            std::panic::panic_any(PeerGone);
        }
        panic!("{message}");
    }
    fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    }
    fn notice(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    fn quiet(&mut self, duration: Duration) {
        let previous = self.0.get_ref().read_timeout().unwrap();
        self.0.get_ref().set_read_timeout(Some(duration)).unwrap();
        let mut line = String::new();
        let result = self.0.read_line(&mut line);
        self.0.get_ref().set_read_timeout(previous).unwrap();
        match result {
            Err(ref error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(0) => self.closed(format!("the CLI closed during the quiet window: {line:?}")),
            Err(ref error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                self.closed(format!("the CLI reset during the quiet window: {line:?}"));
            }
            _ => panic!("Unexpected request or closure: {line:?}, {result:?}"),
        }
    }
    fn start(&mut self, resume: bool) {
        self.hello(&[]);
        if !resume {
            self.created();
        }
        self.attached();
    }
    /// Answers `hello`, advertising `extra` methods besides the Session basics.
    fn hello(&mut self, extra: &[&str]) {
        let hello = self.read("hello");
        assert_eq!(hello["params"]["protocolVersion"], 3);
        let mut features = vec!["session.create", "session.history", "provider.list"];
        features.extend(extra);
        self.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
    }
    fn created(&mut self) -> Value {
        let create = self.read("session.create");
        self.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        create
    }
    fn attached(&mut self) {
        let attach = self.read("session.attach");
        self.reply(&attach, attach_record(&attach));
        let history = self.read("session.history");
        self.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let idle = self.read("session.idle");
        self.reply(&idle, json!({"sequence":8}));
    }
    /// Answers the context reading the interface asks for when it enters a
    /// Session: this fixture reports the method as unknown, as a Session
    /// that does not contribute it would.
    fn no_context(&mut self) {
        let request = self.read("session.context.usage");
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32601,"message":"Unknown method: session.context.usage",
            "data":{"appError":"unknown_method"}}}));
    }
    fn event(&mut self, sequence: u64, name: &str, data: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,"type":"step","name":name,"data":data}}));
    }
    fn end(&mut self, outcome: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"end","sequence":25,"create_at":1,"type":"turn","name":"end","data":outcome}}));
    }
    fn detach(&mut self) {
        let detach = self.read("session.detach");
        self.reply(&detach, json!({}));
    }
    /// Holds the connection open until the client leaves, answering nothing:
    /// a flow the test drives with notices instead of replies.
    fn hold(&mut self) {
        let mut line = String::new();
        loop {
            line.clear();
            match self.0.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }
}

/// The record `session.attach` replies for the Session it names: that
/// Session's `SessionInfo`, whose model follows its latest adopted
/// configuration rather than the creation model `session.summaries` reports until
/// #795 lands.
fn attach_record(request: &Value) -> Value {
    json!({
        "sessionId": request["params"]["sessionId"],
        "cwd": "/workspace",
        "model": {"providerId":"test","modelId":"model"},
        "createdAt": 1,
        "running": false,
        "sequence": 8,
        "attachedClients": 1,
    })
}

/// The public `context/configured` event: the adopted model, and the
/// preferences the configuration carries when it carries any.
fn configured_event(sequence: u64, model_id: &str, preferences: Value) -> Value {
    let mut data = json!({
        "timeline":"main","revision":"revision-1",
        "provider_id":"test","model_id":model_id
    });
    if preferences
        .as_object()
        .is_some_and(|preferences| !preferences.is_empty())
    {
        data["model_preferences"] = preferences;
    }
    json!({"id":format!("configured-{sequence}"),"sequence":sequence,"create_at":1,"type":"context","name":"configured","data":data})
}

fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + HANG_GUARD;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("CLI did not settle");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn empty_management_lists_explain_the_result() {
    for (area, method, reply, message) in [
        (
            "scheduled",
            "schedule.list",
            json!({"jobs":[]}),
            "No job is scheduled.\n",
        ),
        (
            "skills",
            "skills.catalog",
            json!({"version":1,"entries":[],"issues":[]}),
            "No Skill is configured.\n",
        ),
        (
            "hooks",
            "hooks.catalog",
            json!({"userRevision":"user:1","projectsRevision":"user:1",
                "disableAllHooks":false,"sources":[],"hooks":[]}),
            "No hook is configured.\n",
        ),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[method]);
            let request = wire.read(method);
            wire.reply(&request, reply);
        });
        let output = home.command().args([area, "list"]).output().unwrap();
        daemon.join().unwrap();
        assert!(output.status.success(), "{area}: {output:?}");
        assert!(output.stdout.is_empty(), "{area}: {output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), message, "{area}");
    }
}

#[test]
fn help_and_version_do_not_create_product_state() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for flag in ["--help", "-h", "--version"] {
        let output = home
            .command()
            .env("DIM_EFFORT_LEVEL", " invalid ")
            .arg(flag)
            .output()
            .unwrap();
        assert!(output.status.success());
        if flag != "--version" {
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("--effort LEVEL"));
            assert!(help.contains("DIM_EFFORT_LEVEL"));
        }
    }
    assert!(!home.0.join("rust").exists());
}

#[test]
fn a_build_from_source_reports_its_version_as_source() {
    let home = Home::new();
    let output = home.command().arg("--version").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("dimcode {}-source\n", env!("CARGO_PKG_VERSION")),
        "a build the release flow did not stamp reports the committed version marked -source"
    );
}

#[test]
fn model_catalog_lists_safe_ids_and_availability_and_refreshes_explicitly() {
    for refresh in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let request = wire.read(if refresh { "provider.refresh" } else { "provider.catalog" });
            assert_eq!(request["params"], json!({"providerId":"fixture"}));
            let catalog = json!({"account":{"accessToken":"must-not-print"},"version":1,"disabledModelIds":["disabled"],"models":[
                {"id":"model\u{1b}[2J-id","name":"Model\nName\tvalue","unavailableReason":null,"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"disabled","name":"Disabled","unavailableReason":null,"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"missing","name":"Missing","unavailableReason":"private diagnostic must-not-print","capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]});
            wire.reply(&request, if refresh { catalog } else { json!({"catalog":catalog}) });
        });
        let mut command = home.command();
        command.args(["models", "fixture"]);
        if refresh {
            command.arg("--refresh");
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "model-id\tModel Name value\tavailable\ndisabled\tDisabled\tdisabled\nmissing\tMissing\tunavailable\n"
        );
        assert!(output.stderr.is_empty());
    }
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--refresh"));
}

/// A provider whose catalog holds no models is named on standard error with
/// the command that lists them, instead of printing nothing at all.
#[test]
fn model_catalog_names_an_empty_catalog() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "No models. Run dimcode models fixture --refresh to list them.\n"
    );
}

/// `--refresh` against a provider that lists nothing says the listing came
/// back empty, instead of naming the refresh command the user just ran.
#[test]
fn model_catalog_refresh_names_an_empty_listing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.refresh");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"version":2,"disabledModelIds":[],"models":[]}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture", "--refresh"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "No models. The provider listed none.\n"
    );
}

/// A model no source declares limits for is marked `undeclared` and the
/// list names the command that declares them, instead of hiding it among
/// the unavailable models.
#[test]
fn model_catalog_marks_undeclared_limits_and_names_the_declaring_command() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tundeclared\nready\tReady\tavailable\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models fixture --set-missing-limits --context-window N --max-output-tokens N.\n"
    );
}

/// A model refused for its undeclared limits names the command that
/// declares them, with its provider and model, when the explicit start
/// model cannot be adopted.
#[test]
fn model_adoption_names_the_declaring_command_for_undeclared_limits() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let create = wire.read("session.create");
        assert_eq!(
            create["params"]["model"],
            json!({"provider":"fixture","modelId":"narrow"})
        );
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"Explicit model is unavailable","data":{"appError":"provider_invalid_config"}}}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "--provider",
            "fixture",
            "--model",
            "narrow",
            "--prompt",
            "hello",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Cannot create session: "), "{stderr}");
    assert!(
        stderr.contains("Declare them with dimcode models fixture --set-limits narrow --context-window N --max-output-tokens N."),
        "{stderr}"
    );
}

/// `--set-limits` declares both limits of the named model at the record
/// version, then prints the catalog the way `models PROVIDER` does.
#[test]
fn model_catalog_declares_limits_for_one_model() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]}}),
        );
        let edit = wire.read("provider.models.edit");
        assert_eq!(
            edit["params"],
            json!({"providerId":"fixture","expectedVersion":1,"modelId":"narrow",
                "edit":{"contextWindow":64000,"maxOutputTokens":8000}})
        );
        wire.reply(&edit, json!({"version":2,"disabledModelIds":[],"models":[
            {"id":"narrow","name":"Narrow","unavailableReason":null,"missingLimits":[]}
        ]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":2,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000, 8_000)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "models",
            "fixture",
            "--set-limits",
            "narrow",
            "--context-window",
            "64000",
            "--max-output-tokens",
            "8000",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tavailable\n"
    );
    assert!(output.stderr.is_empty());
}

/// `--set-missing-limits` reads the catalog, declares the limits of every
/// model the list marks `undeclared`, and chains each reply's version into
/// the next write. A disabled model and one unavailable for another reason
/// keep that state even when they report missing limits.
#[test]
fn model_catalog_declares_missing_limits_for_every_model() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":["gone"],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"wide","name":"Wide","unavailableReason":"limits_undeclared","missingLimits":["maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities { context_window: Some(128_000), ..Default::default() }},
                {"id":"gone","name":"Gone","unavailableReason":null,"missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"blocked","name":"Blocked","unavailableReason":"provider_unavailable","missingLimits":["contextWindow"],"capabilities":dim_agent_types::CatalogCapabilities { max_output_tokens: Some(16_384), ..Default::default() }}
            ]}}),
        );
        let first = wire.read("provider.models.edit");
        assert_eq!(
            first["params"],
            json!({"providerId":"fixture","expectedVersion":1,"modelId":"narrow",
                "edit":{"contextWindow":64000,"maxOutputTokens":8000}})
        );
        wire.reply(&first, json!({"version":2,"disabledModelIds":[],"models":[]}));
        let second = wire.read("provider.models.edit");
        assert_eq!(
            second["params"],
            json!({"providerId":"fixture","expectedVersion":2,"modelId":"wide",
                "edit":{"maxOutputTokens":8000}})
        );
        wire.reply(&second, json!({"version":3,"disabledModelIds":[],"models":[]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(
            &catalog,
            json!({"catalog":{"version":3,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000, 8_000)},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"wide","name":"Wide","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 8_000)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "models",
            "fixture",
            "--set-missing-limits",
            "--context-window",
            "64000",
            "--max-output-tokens",
            "8000",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tavailable\nready\tReady\tavailable\nwide\tWide\tavailable\n"
    );
    assert!(output.stderr.is_empty());
}

/// The declaration flags require the models command, one form and both
/// values, and fail before the CLI connects to a daemon.
#[test]
fn model_limit_declaration_flags_are_validated_before_connecting() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for (args, reason) in [
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--context-window",
                "64000",
            ],
            "--max-output-tokens",
        ),
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--context-window",
                "0",
                "--max-output-tokens",
                "8000",
            ],
            "positive integer",
        ),
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--set-missing-limits",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "not both",
        ),
        (
            vec![
                "models",
                "fixture",
                "--refresh",
                "--set-missing-limits",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "cannot be combined",
        ),
        (
            vec!["models", "fixture", "--context-window", "1"],
            "--set-limits or --set-missing-limits",
        ),
        (
            vec![
                "exec",
                "--set-limits",
                "narrow",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "models PROVIDER",
        ),
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!home.0.join("rust").exists(), "{args:?}");
    }
}

#[test]
fn daemon_stop_without_an_endpoint_never_creates_state_or_spawns_a_server() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let output = home
        .command()
        .args([
            "--server",
            "/missing-app-server",
            "--model-config",
            "/missing-model-config",
            "daemon",
            "stop",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.0.join("rust").exists());
    fs::create_dir(home.0.join("rust")).unwrap();
    fs::set_permissions(home.0.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        home.command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn daemon_stop_waits_for_completion_and_closure_without_touching_a_successor() {
    let home = Home::new();
    fs::write(home.0.join("account-marker"), "retained").unwrap();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"configured","modelConfigSha256":"0".repeat(64)},"features":["session.create","session.history","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({}));
        thread::sleep(Duration::from_millis(100));
        fs::remove_file(&socket).unwrap();
        let _successor = UnixListener::bind(&socket).unwrap();
        wire.reply(&shutdown, json!({}));
        thread::sleep(Duration::from_millis(100));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
    });
    let started = Instant::now();
    let output = home.command().args(["daemon", "stop"]).output().unwrap();
    let elapsed = started.elapsed();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed >= Duration::from_millis(200));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert_eq!(
        fs::read_to_string(home.0.join("account-marker")).unwrap(),
        "retained"
    );
}

#[test]
fn a_daemon_of_another_version_is_stopped_and_replaced() {
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"old","serverVersion":"0.0.1-old","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        fs::remove_file(&socket).unwrap();
        let successor = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        successor.set_nonblocking(true).unwrap();
        wire.reply(&shutdown, json!({}));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            let stream = match successor.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the CLI never reconnected");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire::new(stream);
            let Some(line) = wire.trailing_request() else {
                continue;
            };
            let hello: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(hello["method"], "hello");
            wire.reply(&hello,json!({"instanceId":"new","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog":null}));
            break;
        }
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The daemon the CLI started for a home is gone once the home drops.
///
/// The harness finds it with `ps`, so a missing `ps` fails the test instead
/// of reading as no daemon.
#[test]
fn a_home_drop_leaves_no_daemon_behind() {
    let home = Home::new();
    let path = home.0.clone();
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pattern = path.to_str().unwrap();
    assert_eq!(
        daemon_cleanup::pids_naming(pattern).len(),
        1,
        "the CLI did not start its daemon for {}",
        path.display()
    );
    drop(home);
    assert!(
        daemon_cleanup::pids_naming(pattern).is_empty(),
        "the test left its daemon running for {}",
        path.display()
    );
    assert!(!path.exists(), "the test left {} on disk", path.display());
}

/// The daemon closes the descriptors it inherited from the CLI.
///
/// The sweep and this test use the same listing: `/dev/fd` on macOS and the
/// other Unix systems, and on Linux `close_range` or `/proc/self/fd`. The
/// bounded fallback closes every number below the soft `RLIMIT_NOFILE`,
/// which includes the pipe's inherited write end.
#[test]
fn a_daemon_keeps_no_descriptor_the_cli_inherited() {
    let home = Home::new();
    let (reader, writer) = std::io::pipe().unwrap();
    let mut command = home.command();
    command.args(["models", "fixture"]);
    let inherited = writer.as_raw_fd();
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, move || {
            if libc::fcntl(inherited, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command.status().unwrap();
    drop(writer);
    assert!(
        status.success(),
        "the CLI did not start the daemon for {}",
        home.0.display()
    );
    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut descriptor, 1, PATIENCE.as_millis() as libc::c_int) };
    assert!(
        ready > 0,
        "the daemon kept the pipe's write end for {}",
        home.0.display()
    );
    let mut byte = [0u8; 1];
    let read = unsafe {
        libc::read(
            reader.as_raw_fd(),
            byte.as_mut_ptr().cast::<libc::c_void>(),
            byte.len(),
        )
    };
    assert_eq!(read, 0, "the pipe carried data instead of EOF");
}

/// A server a test starts on a home itself is stopped even before it binds:
/// the CLI's `daemon.log` and the server's `daemon.sock.lock` do not exist
/// yet, so the marker the test writes is what tells the drop to stop it.
#[test]
fn a_home_stops_a_marked_server_that_never_binds() {
    let home = Home::new();
    let pattern = home.0.to_str().unwrap().to_owned();
    daemon_cleanup::mark_test_server(&home.0);
    // A stand-in for a server that is still starting when the test ends: it
    // names the home in its command line, never binds, and is a grandchild,
    // so the test holds no handle on it and the drop has to find it.
    let status = Command::new("sh")
        .arg("-c")
        .arg("sh -c \"$2\" sh \"$1\" & exit 0")
        .arg("sh")
        .arg(&pattern)
        .arg("while :; do sleep 1; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the stand-in server did not start");
    let deadline = Instant::now() + PATIENCE;
    while daemon_cleanup::pids_naming(&pattern).is_empty() {
        assert!(Instant::now() < deadline, "the stand-in server never ran");
        thread::sleep(Duration::from_millis(10));
    }
    drop(home);
    assert!(
        daemon_cleanup::pids_naming(&pattern).is_empty(),
        "the drop left the marked server for {pattern} running"
    );
}

/// A stop re-reads a pid's command line before it signals, so a pid that now
/// runs something else is left alone. The process here starts with the home
/// in its command line and then replaces it, the shape a reused pid has.
#[test]
fn a_stop_never_signals_a_process_that_no_longer_names_the_home() {
    let home = Home::new();
    let pattern = home.0.to_str().unwrap();
    let mut process = Command::new("sh")
        .arg("-c")
        .arg("exec sleep 30")
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = process.id() as libc::pid_t;
    let deadline = Instant::now() + PATIENCE;
    while daemon_cleanup::command_of(pid).is_some_and(|command| command.contains(pattern)) {
        assert!(
            Instant::now() < deadline,
            "the stand-in never replaced its command line"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !daemon_cleanup::signal_if_naming(pid, pattern, libc::SIGTERM),
        "the stop signalled a process that no longer names the home"
    );
    assert!(
        process.try_wait().unwrap().is_none(),
        "the signal reached a process that no longer names the home"
    );
    process.kill().unwrap();
    process.wait().unwrap();
}

#[test]
fn daemon_stop_never_treats_old_protocol_eof_or_cleanup_failure_as_completion() {
    for case in ["old", "eof", "failed"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            let mut features = vec!["session.create", "session.history"];
            if case != "old" { features.push("daemon.shutdown.completed"); }
            wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
            if case == "old" {
                assert_eq!(wire.trailing_request(), None, "must not stop an incompatible daemon");
            } else {
                let request = wire.read("daemon.shutdown");
                if case == "failed" { wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"Daemon cleanup failed"}})); }
            }
        });
        let output = home.command().args(["daemon", "stop"]).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Daemon stopped."));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn headless_streams_unicode_once_and_waits_for_the_delivery_barrier() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "/fast on");
        assert!(!submit["params"]["submissionId"].as_str().unwrap().is_empty());
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"你\u{1b}[2J好","thinking":"","thinkingOffset":0}));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"你\u{1b}[2J好"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":6,"text":"世界","thinking":"","thinkingOffset":0}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"世界"}));
        wire.event(22,"model-output-end",json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "/fast on"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "你好世界\n");
}

fn image_fixture(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

/// Deterministic RGB noise of `width`×`height` pixels.
fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut step = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for _ in 0..width as usize * height as usize {
        let (red, green, blue) = (step(), step(), step());
        pixels.extend([(red >> 24) as u8, (green >> 24) as u8, (blue >> 24) as u8]);
    }
    pixels
}

/// Deterministic noise: JPEG quality 100 keeps it over 5 MiB, and its long
/// edge is over the shared edge limit, so an entry has to scale it.
fn noise_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 100);
    encoder
        .encode(
            &noise_rgb(width, height),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    bytes
}

/// `noise_jpeg`, with a minimal EXIF APP1 segment whose one IFD0 entry is the
/// orientation: `3` turns the image 180° and `6` 90° clockwise.
fn oriented_jpeg(width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let mut bytes = noise_jpeg(width, height);
    let mut exif = vec![0xFF, 0xE1, 0x00, 0x22];
    exif.extend(b"Exif\0\0");
    exif.extend(b"II\x2A\x00");
    exif.extend(8u32.to_le_bytes());
    exif.extend(1u16.to_le_bytes());
    exif.extend(0x0112u16.to_le_bytes());
    exif.extend(3u16.to_le_bytes());
    exif.extend(1u32.to_le_bytes());
    exif.extend(orientation.to_le_bytes());
    exif.extend([0, 0]);
    exif.extend(0u32.to_le_bytes());
    bytes.splice(2..2, exif);
    bytes
}

/// Deterministic noise as PNG: within the shared edge limit while its base64
/// is over the byte limit, so it is sent as a JPEG at the same size.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut bytes),
        &noise_rgb(width, height),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    bytes
}

#[test]
fn exec_scales_a_large_photo_and_reports_both_sizes() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    // Quality-100 noise over the raw bytes that fit the base64 limit, while a
    // debug-build decode and scale stay well inside the harness patience.
    let bytes = noise_jpeg(2100, 700);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        let sent = STANDARD
            .decode(image["source"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&sent[..3], b"\xff\xd8\xff");
        assert!(
            base64_len(sent.len() as u64) <= MAX_BASE64_BYTES,
            "{}",
            sent.len()
        );
        let image = image::load_from_memory(&sent).unwrap();
        assert!(
            image.width() <= MAX_EDGE && image.height() <= MAX_EDGE,
            "{}×{}",
            image.width(),
            image.height()
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args([
                "exec",
                "--image",
                photo.to_str().unwrap(),
                "-p",
                "Describe this",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Scaled photo.jpg from 2100×700 to "),
        "{stderr}"
    );
    assert!(stderr.contains("px (image/jpeg)"), "{stderr}");
}

/// A PNG within the edge limit whose base64 is over the byte limit is
/// re-encoded as JPEG at the same size: the size alone would hide that the
/// screenshot is now lossy, so exec says the type changed.
#[test]
fn exec_reports_a_reencode_that_keeps_the_size() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let bytes = noise_png(1500, 1500);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let shot = home.0.join("shot.png");
    fs::write(&shot, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        let sent = STANDARD
            .decode(image["source"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&sent[..3], b"\xff\xd8\xff");
        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (1500, 1500));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", shot.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded shot.png as image/jpeg (1500×1500 px)"),
        "{stderr}"
    );
}

/// A JPEG within the edge and byte limits whose EXIF orientation turns it
/// upright is re-encoded as JPEG at the same size: the file differs from what
/// is sent only in its encoding, and exec says so.
#[test]
fn exec_reports_a_photo_turned_upright_at_the_same_size() {
    let home = Home::new();
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, oriented_jpeg(40, 20, 3)).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (40×20 px)"),
        "{stderr}"
    );
}

/// The 90° turn swaps the axes before the sizes are compared, so a square
/// result keeps the size too: exec still says the encoding changed.
#[test]
fn exec_reports_a_quarter_turn_at_the_same_size() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, oriented_jpeg(40, 20, 6)).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        let sent = STANDARD
            .decode(image["source"]["data"].as_str().unwrap())
            .unwrap();
        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (20, 40));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (20×40 px)"),
        "{stderr}"
    );
}

/// A JPEG within the edge limit whose base64 is over the byte limit fits once
/// it is re-encoded at quality 85, again without a size or type change.
#[test]
fn exec_reports_a_jpeg_that_fits_only_after_the_reencode() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let bytes = noise_jpeg(1500, 1500);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        let sent = STANDARD
            .decode(image["source"]["data"].as_str().unwrap())
            .unwrap();
        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (1500, 1500));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (1500×1500 px)"),
        "{stderr}"
    );
}

#[test]
fn exec_reports_the_first_frame_of_an_animation() {
    let home = Home::new();
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder
            .encode_frames([
                image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([255, 0, 0, 255]),
                )),
                image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([0, 0, 255, 255]),
                )),
            ])
            .unwrap();
    }
    let animation = home.0.join("anim.gif");
    fs::write(&animation, &gif).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", animation.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Only the first frame of anim.gif is sent (image/jpeg)"),
        "{stderr}"
    );
}

#[test]
fn exec_images_preserve_format_bytes_order_and_invoking_directory() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for source in [
        "images",
        "prompt",
        "empty",
        "stdin",
        "whitespace",
        "whitespace_prompt",
        "symlink",
        "twenty",
        "terminal",
    ] {
        let home = Home::new();
        let workspace = home.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for (index, (format, media_type)) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Gif, "image/gif"),
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = image_fixture(format, 1, if index == 0 { MAX_EDGE } else { 1 });
            let path = format!("image {index}.txt");
            fs::write(home.0.join(&path), &bytes).unwrap();
            images.push(json!({"type":"image","source":{"type":"base64","mediaType":media_type,"data":STANDARD.encode(bytes)}}));
            paths.push(path);
        }
        if source == "symlink" {
            let link = home.0.join("linked.png");
            std::os::unix::fs::symlink(home.0.join(&paths[0]), &link).unwrap();
            paths[0] = link.to_str().unwrap().into();
        }
        if source == "twenty" {
            images = images.iter().cycle().take(20).cloned().collect();
            paths = paths.iter().cycle().take(20).cloned().collect();
        }
        let text = match source {
            "prompt" => Some("compare /exit literally"),
            "empty" => Some(""),
            "stdin" => Some("stdin\r\nwith trailing newline\n"),
            "whitespace" | "whitespace_prompt" => Some(" \t\n"),
            _ => None,
        };
        let mut expected = Vec::new();
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            expected.push(json!({"type":"text","text":text}));
        }
        expected.extend(images);
        let daemon = home.daemon(move |wire| {
            wire.start(source == "prompt");
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["user"]["content"], json!(expected));
            assert!(submit["params"].get("text").is_none());
            assert_eq!(submit["params"]["mode"], "queue");
            uuid::Uuid::parse_str(submit["params"]["submissionId"].as_str().unwrap()).unwrap();
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        command
            .current_dir(&home.0)
            .args(["--cwd", workspace.to_str().unwrap(), "exec"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if source == "prompt" {
            command.args(["resume", "session", "--json"]);
        }
        if matches!(source, "prompt" | "empty" | "whitespace_prompt") {
            command.args(["-p", text.unwrap()]);
        }
        if matches!(source, "stdin" | "whitespace") {
            command.stdin(Stdio::piped());
        }
        for path in paths {
            command.arg("--image").arg(path);
        }
        let output = if source == "terminal" {
            Output {
                status: Terminal::launch(command).wait(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        } else {
            let mut child = command.spawn().unwrap();
            if matches!(source, "stdin" | "whitespace") {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(text.unwrap().as_bytes())
                    .unwrap();
            }
            wait(child)
        };
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn invalid_exec_images_fail_before_connecting_or_creating_home() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    let mut bad_crc = png.clone();
    bad_crc[29] ^= 1;
    let mut zero = image_fixture(image::ImageFormat::Gif, 1, 1);
    zero[6..8].fill(0);
    let mut guard = b"GIF89a".to_vec();
    guard.extend(12000u16.to_le_bytes());
    guard.extend(9000u16.to_le_bytes());
    guard.extend([0, 0, 0, 0]);
    guard.push(b';');
    for (name, bytes, reason) in [
        ("text", b"not an image".to_vec(), "PNG, JPEG, WebP or GIF"),
        ("truncated", png[..8].to_vec(), "Invalid or truncated"),
        (
            "png_header_only",
            png[..33].to_vec(),
            "Invalid or truncated",
        ),
        ("jpeg", b"\xff\xd8\xff".to_vec(), "Invalid or truncated"),
        ("gif", b"GIF89a".to_vec(), "Invalid or truncated"),
        ("webp", b"RIFF\0\0\0\0WEBP".to_vec(), "Invalid or truncated"),
        ("zero", zero, "dimensions"),
        ("crc", bad_crc, "Invalid or truncated"),
        ("guard", guard, "12000×9000"),
    ] {
        let file = home.0.join(name);
        fs::write(&file, bytes).unwrap();
        let output = home
            .command()
            .arg("exec")
            .arg("--image")
            .arg(file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
    let fifo = home.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for args in [
        vec!["--image", "missing.png"],
        vec!["tui", "--image", "missing.png"],
        vec!["exec", "--image"],
        vec!["exec", "--image", "missing.png"],
        vec!["exec", "--image", "http://127.0.0.1:9/missing.png"],
        vec!["exec", "--image", home.0.to_str().unwrap()],
        vec!["exec", "--image", fifo.to_str().unwrap()],
        std::iter::once("exec")
            .chain(["--image", "missing.png"].repeat(21))
            .collect(),
    ] {
        let output = home
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn exec_images_check_the_combined_encoded_frame_before_sending() {
    // A kept image encodes to at most MAX_BASE64_BYTES, so four of them
    // exceed the 16 MiB request frame; one raw byte over the fit is
    // re-encoded and fits.
    let fits_raw = (MAX_BASE64_BYTES / 4 * 3) as usize;
    assert_eq!(base64_len(fits_raw as u64), MAX_BASE64_BYTES);
    for (size, refused) in [(fits_raw, true), (fits_raw + 1024 * 1024, false)] {
        let home = Home::new();
        let mut png = image_fixture(image::ImageFormat::Png, 1, 1);
        png.resize(size, 0);
        let image = home.0.join("large.png");
        fs::write(&image, png).unwrap();
        let daemon = if refused {
            fs::remove_dir_all(home.0.join("rust")).unwrap();
            None
        } else {
            Some(home.daemon(|wire| {
                wire.start(false);
                let submit = wire.read("session.submit");
                wire.reply(&submit, json!({"event":{}}));
                let idle = wire.read("session.idle");
                wire.reply(&idle, json!({"sequence":30}));
                wire.detach();
            }))
        };
        let mut command = home.command();
        command.arg("exec").stdin(Stdio::null());
        for _ in 0..4 {
            command.arg("--image").arg(&image);
        }
        let output = command.output().unwrap();
        if let Some(daemon) = daemon {
            daemon.join().unwrap();
        } else {
            assert!(!home.0.join("rust").exists());
        }
        if refused {
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("16 MiB"));
            assert!(output.stdout.is_empty());
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn submit_errors_on_a_live_connection_exit_without_resending() {
    for (app_error, uncertain) in [
        (Some("submission_unknown"), true),
        (Some("publication_failed"), true),
        (None, true),
        (Some("session_invalid_input"), false),
        (Some("invalid_params"), false),
        (Some("input_blocked"), false),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            if uncertain {
                wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"committed","sequence":9,"create_at":1,"type":"input","name":"message","data":{"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"once"}]},"mode":"queue"}}}));
            }
            wire.send(json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,"message":"Submission failed","data":app_error.map(|code| json!({"appError":code}))}}));
            wire.detach();
        });
        let output = home
            .command()
            .args(["exec", "-p", "once"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("Submission failed"));
        if uncertain {
            assert!(diagnostic.contains("session: session"));
            assert!(diagnostic.contains("input may already be queued"));
            assert!(diagnostic.contains("instead of sending it again"));
        } else {
            assert!(!diagnostic.contains("input may already be queued"));
            assert!(!diagnostic.contains("instead of sending it again"));
        }
    }
}

/// Appendix D 5: a blocked prompt is reported as the block, never as an uncertain
/// submission that may have been queued.
#[test]
fn a_blocked_prompt_reports_the_hooks_message() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.send(
            json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,
            "message":"Hook user: no prompts today","data":{"appError":"input_blocked"}}}),
        );
        wire.detach();
    });
    let output = home
        .command()
        .args(["exec", "-p", "once"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("Hook user: no prompts today"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("input may already be queued"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("instead of sending it again"),
        "{diagnostic}"
    );
}

#[test]
fn noninteractive_approval_denies_and_drains_outstanding_responses_before_detach() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"do something","token":"never print me"}},"reason":"shell command"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--json", "--prompt", "exec"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never print me"));
    let notices: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["method"], "approval.request");
    assert_eq!(notices[1]["method"], "session.event");
}

#[test]
fn noninteractive_outside_read_is_denied_by_the_ordinary_approval_answer() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"read-approval","source":{"kind":"model"},"call":{"name":"read","input":{"path":"/etc/hosts"}},"reason":"Reading outside the session workspace requires approval: /etc/hosts"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--prompt", "read /etc/hosts"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("Interactive approval required"), "{stderr}");
}

#[test]
fn resume_advances_filtered_history_positions_and_does_not_reprint_old_output() {
    let home = Home::new();
    let launch = {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            home.0
                .join(std::ffi::OsString::from_vec(b"launch-\xff".to_vec()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            home.0.join("launch")
        }
    };
    fs::create_dir(&launch).unwrap();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let first = wire.read("session.history"); assert_eq!(first["params"]["after"],0);
        wire.reply(&first,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old-request","delta":"old output"}}],"through":10,"nextAfter":7,"done":false}));
        let next = wire.read("session.history");
        assert_eq!(next["params"]["after"],7); assert_eq!(next["params"]["through"],10);
        wire.reply(&next,json!({"events":[{"sequence":8,"create_at":2,"type":"step","name":"model-output-end","data":{"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}}],"through":10,"nextAfter":10,"done":true}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"old-request","textOffset":0,"text":"old output","thinking":"","thinkingOffset":0}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":10}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"new output"}));
        wire.end(json!({"turn_id":"new-turn","status":"completed","reason":"done"}));
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .current_dir(&launch)
            .args(["exec", "resume", "session", "--prompt", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "new output\n");
}

#[cfg(target_os = "linux")]
#[test]
fn exec_refuses_a_non_utf8_workspace_before_connecting() {
    use std::os::unix::ffi::OsStringExt;
    let home = Home::new();
    let launch = home
        .0
        .join(std::ffi::OsString::from_vec(b"launch-\xff".to_vec()));
    fs::create_dir(&launch).unwrap();
    let output = home
        .command()
        .current_dir(&launch)
        .args(["exec", "-p", "start"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be valid UTF-8"));
    assert!(!home.0.join("rust/daemon.sock").exists());
}

#[test]
fn exec_json_preserves_public_notifications_and_only_observes_the_selected_session() {
    let home = Home::new();
    let notices = vec![
        json!({"jsonrpc":"2.0","method":"session.mcp.state","params":{"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}}),
        json!({"jsonrpc":"2.0","method":"session.plugin.notice","params":{"sessionId":"session","branchId":"child","source":"fixture","future":{"control":"\u{1b}[2J\n"}}}),
        json!({"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"Hello","newField":true}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":12,"create_at":1,"type":"step","name":"text","data":{"turn_id":"turn","request_id":"request","delta":"Hello","newField":"preserved"}}}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":25,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":"failed","error":"fixture"}}}}),
    ];
    let emitted = notices.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old","delta":"history-only","newField":"preserved"}}],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "continue"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.closed",json!({"sessionId":"other"}));
        wire.notice("session.model.progress",json!({"sessionId":"other","text":"other-session"}));
        for notice in emitted { wire.send(notice); }
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--json", "-p", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let received: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(received, notices);
    assert!(!stdout.contains("history-only"));
    assert!(!stdout.contains("workspace:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: session"));
}

#[test]
fn f6_argument_errors_do_not_connect_or_create_product_state() {
    let home = Home::new();
    for args in [
        vec!["exec", "--prompt", "one", "-p", "two"],
        vec!["exec", "resume", "one", "--resume", "two", "-p", "text"],
        vec!["--resume", "one", "exec", "resume", "two", "-p", "text"],
        vec!["--json", "-p", "text"],
        vec!["session", "export", "session"],
        vec!["session", "export", "session", "--format", "xml"],
        vec!["tui"],
        vec!["exec"],
        vec!["exec", "--permission", "admin", "-p", "text"],
        vec!["exec", "--permission", "-p", "text"],
        vec![
            "--permission",
            "read-only",
            "--permission",
            "full-access",
            "-p",
            "text",
        ],
        vec!["mcp", "list", "--permission", "read-only"],
        vec!["export"],
        vec!["export", "session", "--format", "json"],
        vec!["--database", "store.sqlite", "export", "session"],
        vec!["export", "session", "--prompt", "text"],
        vec![
            "session",
            "export",
            "session",
            "--format",
            "json",
            "--permission",
            "full-access",
        ],
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(!home.0.join("rust/daemon.sock").exists());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn exec_json_interrupt_drains_the_stop_barrier_without_human_stdout() {
    let home = Home::new();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        entered.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "wait", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_entered.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["method"], "session.event");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Waiting for resource settlement"));
}

#[test]
fn session_export_preserves_the_fixed_main_prefix_and_does_not_replace_files() {
    for mode in ["stdout-json", "markdown", "existing", "history-error"] {
        let home = Home::new();
        let cli_dir = home.0.join("client");
        let workspace = home.0.join("workspace");
        fs::create_dir(&cli_dir).unwrap();
        fs::create_dir(&workspace).unwrap();
        let path = cli_dir.join("Session notes.md");
        if mode == "existing" {
            fs::write(&path, b"keep original bytes").unwrap();
        }
        let events = vec![
            json!({"sequence":1,"create_at":1,"type":"extension","name":"future","data":{"unknown":[1,2]},"extra":"preserved"}),
            json!({"sequence":2,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"Keep ```code``` intact"},{"type":"image","source":{"type":"url","mediaType":"image/png","url":"https://invalid.example/not-fetched"}}]}}}),
            json!({"sequence":3,"create_at":1,"type":"step","name":"thinking","data":{"turn_id":"t","request_id":"r","delta":"A reason"}}),
            json!({"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"Hello "}}),
            json!({"sequence":5,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"world"}}),
            json!({"sequence":6,"create_at":1,"type":"step","name":"tool-use","data":{"turn_id":"t","request_id":"r","call":{"id":"call","name":"read","input":{"path":"file"}}}}),
            json!({"sequence":7,"create_at":1,"type":"step","name":"tool-completed","data":{"turn_id":"t","request_id":"r","call_event_id":"dispatch","call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}}),
            json!({"sequence":8,"create_at":1,"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","results":[{"call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}]}}),
            json!({"sequence":9,"create_at":1,"type":"branch","name":"create","data":{"branch":"child","at":{"kind":"create"},"source":{"kind":"host","operation_id":"export-fixture"},"prompt":"child contents are elsewhere"}}),
            json!({"sequence":10,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"failed","branch":"child","createEvent":9,"error":"fixture"}}),
            json!({"sequence":15,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"cancelled","branch":"cancelled-child","createEvent":14,"reason":"fixture"}}),
        ];
        let returned = events.clone();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            if mode == "existing" {
                assert!(wire.trailing_request().is_none(),"an existing output path is refused before the history read");
                return;
            }
            let first = wire.read("session.history");
            assert_eq!(first["params"],json!({"sessionId":"session","branchId":"main","after":0,"limit":256}));
            wire.reply(&first,json!({"events":&returned[..10],"through":20,"nextAfter":12,"done":false}));
            let second = wire.read("session.history");
            assert_eq!(second["params"]["after"],12); assert_eq!(second["params"]["through"],20);
            if mode == "history-error" {
                wire.send(json!({"jsonrpc":"2.0","id":second["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
            } else {
                wire.reply(&second,json!({"events":&returned[10..],"through":20,"nextAfter":20,"done":true}));
            }
        });
        let mut command = home.command();
        command.current_dir(&cli_dir).args([
            "session",
            "export",
            "session",
            "--format",
            if mode == "stdout-json" {
                "json"
            } else {
                "markdown"
            },
            "--cwd",
            workspace.to_str().unwrap(),
        ]);
        if mode != "stdout-json" {
            command.args(["--output", "Session notes.md"]);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        match mode {
            "stdout-json" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let document = serde_json::to_string_pretty(
                    &json!({"sessionId":"session","branchId":"main","through":20,"events":events}),
                )
                .unwrap();
                assert_eq!(String::from_utf8(output.stdout).unwrap(), document + "\n");
            }
            "markdown" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stdout.is_empty());
                let markdown = fs::read_to_string(&path).unwrap();
                let expected = r#"# Session session

Branch: main


### User

Keep ```code``` intact
[Image attachment: image/png]


### Reasoning

A reason

### Assistant

Hello world

### Tool call: read (call)

```json
{
  "path": "file"
}
```


### Tool result: call

```````json
{
  "content": "```\n# must remain inside fence\n``````"
}
```````


Child branch child (contents not exported).


Child branch child was not created.


Child branch cancelled-child was not created.

"#;
                assert_eq!(markdown, expected);
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            "existing" => {
                assert!(!output.status.success());
                assert_eq!(fs::read(&path).unwrap(), b"keep original bytes");
                assert!(!String::from_utf8_lossy(&output.stderr).contains("Exported"));
            }
            _ => {
                assert!(!output.status.success());
                assert!(!path.exists());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("fixture history failure")
                );
            }
        }
        assert!(!workspace.join("Session notes.md").exists());
    }
}

#[test]
fn interactive_exports_keep_literal_paths_and_errors_out_of_model_input() {
    let home = Home::new();
    let cli_dir = home.0.join("client");
    let workspace = home.0.join("workspace");
    fs::create_dir_all(cli_dir.join("~")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let history = wire.read("session.history");
        entered.send(()).unwrap();
        wait_release.recv_timeout(PATIENCE).unwrap();
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let history = wire.read("session.history");
        wire.send(json!({"jsonrpc":"2.0","id":history["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
        wire.detach();
    });
    let mut command = home.command();
    command
        .current_dir(&cli_dir)
        .args(["tui", "--cwd", workspace.to_str().unwrap()]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/export markdown\n");
    terminal.until("~ is literal.");
    terminal.until("\n> ");
    terminal.write(b"/export yaml bad.md\n");
    terminal.until("Export format must be markdown or json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    wait_entered.recv_timeout(PATIENCE).unwrap();
    for _ in 0..2 {
        terminal.signal(libc::SIGINT);
        terminal.until("Waiting for export result");
    }
    release.send(()).unwrap();
    terminal.until("Exported JSON: ~/Session notes.json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    terminal.until("Error:");
    terminal.until("\n> ");
    terminal.write(b"/export markdown failure.md\n");
    terminal.until("fixture history failure");
    terminal.until("\n> ");
    terminal.write(b"/exit extra\n");
    terminal.until("Usage: /exit");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let path = cli_dir.join("~/Session notes.json");
    let text = fs::read_to_string(&path).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(data["events"], json!([]));
    assert_eq!(
        text,
        serde_json::to_string_pretty(&json!({
            "sessionId": data["sessionId"],
            "branchId": "main",
            "through": data["through"],
            "events": [],
        }))
        .unwrap()
            + "\n"
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!cli_dir.join("failure.md").exists());
    assert!(!workspace.join("~").exists());
}

#[test]
fn incompatible_daemon_is_rejected_without_removing_its_endpoint() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"other","serverVersion":"1","protocolVersion":2,"features":[]}),
        );
    });
    let output = home.command().args(["--prompt", "hello"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn explicit_model_config_rejects_mock_and_mismatched_configured_profiles() {
    for profile in [
        json!({"kind":"mock"}),
        json!({"kind":"configured","modelConfigSha256":"0".repeat(64)}),
    ] {
        let home = Home::new();
        let config = home.0.join("model.json");
        fs::write(&config, "{}").unwrap();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":profile,"features":["session.create","session.history"]}));
        });
        let output = home
            .command()
            .args([
                "--model-config",
                config.to_str().unwrap(),
                "--prompt",
                "hello",
            ])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(home.0.join("rust/daemon.sock").exists());
        assert!(!home.0.join("rust/daemon.log").exists());
    }
}

#[test]
fn piped_prompt_runs_once_and_model_failure_has_a_distinct_exit_code() {
    for (explicit_exec, input) in [
        (false, "from stdin\n"),
        (true, "from stdin\n"),
        (false, "/mcp login docs\n"),
        (true, "/mcp login docs\n"),
        (false, "!!literal prompt\n"),
        (true, "!!literal prompt\n"),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], input);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.end(
                json!({"turn_id":"turn","status":"failed","error":"credential should not print"}),
            );
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("credential should not print"));
    }
    for explicit_exec in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'"'; 8 * 1024 * 1024])
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("App Server request exceeds 16 MiB after JSON encoding")
        );
    }
}

/// `text` without its ANSI escape sequences, so a painted row reads as the
/// text the user sees.
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if c == '\u{07}' || (previous == '\u{1b}' && c == '\\') {
                        break;
                    }
                    previous = c;
                }
            }
            _ => {}
        }
    }
    plain
}

/// A CLI running on its own pseudo-terminal. Dropping it before the CLI has
/// exited, as a failed flow does, ends the CLI's whole process group.
struct Terminal {
    master: Option<fs::File>,
    child: Child,
    transcript: String,
    consumed: usize,
}
impl Terminal {
    fn editor(&mut self) {
        self.until("\u{1b}[?2004h");
        self.until("Ctrl+J: newline");
    }
    /// Launches under a pty with the line client; a dimcode chat in a pty
    /// otherwise opens the terminal UI.
    fn launch(mut command: Command) -> Self {
        if command.get_program() == env!("CARGO_BIN_EXE_dimcode") {
            command.arg("--no-tui");
        }
        Self::launch_tui(command)
    }
    fn launch_tui(mut command: Command) -> Self {
        let master = unsafe {
            libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        };
        assert!(master >= 0, "{}", std::io::Error::last_os_error());
        let master = unsafe { fs::File::from_raw_fd(master) };
        let fd = master.as_raw_fd();
        assert_eq!(unsafe { libc::grantpt(fd) }, 0);
        assert_eq!(unsafe { libc::unlockpt(fd) }, 0);
        let mut name = [0; 128];
        #[cfg(target_os = "macos")]
        let named = unsafe { libc::ioctl(fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) };
        #[cfg(not(target_os = "macos"))]
        let named = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
        assert_eq!(named, 0);
        let slave = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        assert!(slave >= 0, "{}", std::io::Error::last_os_error());
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            master: Some(master),
            child: command.spawn().unwrap(),
            transcript: String::new(),
            consumed: 0,
        }
    }
    fn signal(&self, signal: libc::c_int) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    /// Bytes the child wrote that the terminal has not consumed yet. A pty
    /// master reports the queue on its other side through `TIOCOUTQ` on
    /// macOS, while `FIONREAD` there always answers zero; Linux has no
    /// `TIOCOUTQ` for terminals and answers through `FIONREAD`.
    fn unread(&self) -> libc::c_int {
        #[cfg(target_os = "macos")]
        const WAITING_BYTES: libc::c_ulong = 0x4004_7473; // TIOCOUTQ
        #[cfg(not(target_os = "macos"))]
        const WAITING_BYTES: libc::c_ulong = libc::FIONREAD as libc::c_ulong;
        let mut waiting: libc::c_int = 0;
        let fd = self.master.as_ref().unwrap().as_raw_fd();
        assert_eq!(unsafe { libc::ioctl(fd, WAITING_BYTES, &mut waiting) }, 0);
        waiting
    }
    /// The terminal's local mode. Both ends of a pty share one termios, so
    /// the master reads what raw mode set on the slave.
    fn local_mode(&self) -> libc::tcflag_t {
        let mut mode = unsafe { std::mem::zeroed::<libc::termios>() };
        let fd = self.master.as_ref().unwrap().as_raw_fd();
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut mode) }, 0);
        mode.c_lflag
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            assert!(Instant::now() < deadline, "CLI did not settle");
            let mut bytes = [0u8; 4096];
            match self.master.as_mut().unwrap().read(&mut bytes) {
                Ok(size) if size > 0 => {
                    self.transcript
                        .push_str(&String::from_utf8_lossy(&bytes[..size]));
                    continue;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error)
                    if cfg!(target_os = "linux") && error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("{error}"),
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    /// Waits for the child without reading its output: the terminal stopped
    /// consuming it, as a suspended application's or a stalled remote
    /// session's terminal does, and stays that way until the child is gone.
    fn wait_without_reading(&mut self) -> ExitStatus {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI did not exit with its terminal unread: {}",
                self.transcript
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    /// Waits until the terminal has shown `expected` anywhere, for a frame
    /// whose place among the frames already consumed the flow does not fix.
    fn until_anywhere(&mut self, expected: &str) {
        let consumed = std::mem::take(&mut self.consumed);
        self.until(expected);
        self.consumed = consumed;
    }
    fn until(&mut self, expected: &str) {
        self.wait_for(expected, |tail| {
            tail.find(expected).map(|index| index + expected.len())
        });
    }
    /// Waits until the row labelled `label` shows `value`: a panel paints a
    /// row's value in the same write as its label, so the text that follows
    /// the label is the row's loaded state.
    fn until_row(&mut self, label: &str, value: &str) {
        let expected = format!("{label:?} showing {value:?}");
        self.wait_for(&expected, |tail| {
            let mut from = 0;
            while let Some(index) = tail[from..].find(label) {
                let end = from + index + label.len();
                if strip_ansi(&tail[end..]).trim_start().starts_with(value) {
                    return Some(end);
                }
                from = end;
            }
            None
        });
    }
    /// Reads until `found` names how far into the unconsumed transcript the
    /// wait is satisfied.
    fn wait_for(&mut self, expected: &str, found: impl Fn(&str) -> Option<usize>) {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            if let Some(end) = found(&self.transcript[self.consumed..]) {
                self.consumed += end;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Terminal did not show {expected:?}: {}",
                self.transcript
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_ref().unwrap().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } == 0 {
                continue;
            }
            let mut bytes = [0u8; 4096];
            let size = match self.master.as_mut().unwrap().read(&mut bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                result => result.unwrap(),
            };
            assert!(
                size > 0,
                "Terminal closed before {expected}: {}",
                self.transcript
            );
            self.transcript
                .push_str(&String::from_utf8_lossy(&bytes[..size]));
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        self.raw(
            &bytes
                .iter()
                .map(|&byte| if byte == b'\n' { b'\r' } else { byte })
                .collect::<Vec<_>>(),
        );
    }
    fn raw(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + HANG_GUARD;
        while !bytes.is_empty() {
            assert!(Instant::now() < deadline, "Terminal did not accept input");
            match self.master.as_mut().unwrap().write(bytes) {
                Ok(0) => panic!("Terminal input closed"),
                Ok(size) => bytes = &bytes[size..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut output = [0u8; 4096];
                    match self.master.as_mut().unwrap().read(&mut output) {
                        Ok(0) => panic!("Terminal output closed while writing"),
                        Ok(size) => self
                            .transcript
                            .push_str(&String::from_utf8_lossy(&output[..size])),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("{error}"),
                    }
                    let mut poll = libc::pollfd {
                        fd: self.master.as_ref().unwrap().as_raw_fd(),
                        events: libc::POLLIN | libc::POLLOUT,
                        revents: 0,
                    };
                    assert!(unsafe { libc::poll(&mut poll, 1, 100) } >= 0);
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    fn paste(&mut self, text: &str) {
        self.raw(b"\x1b[200~");
        self.raw(text.as_bytes());
        self.raw(b"\x1b[201~");
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            drop(self.master.take());
            let _ = self.child.wait();
        }
    }
}

#[test]
fn a_failed_terminal_flow_leaves_no_cli_running() {
    for pending_output in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        wire.start(false);
        if pending_output {
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            let _idle = wire.read("session.idle");
            wire.event(12, "text", json!({"turn_id":"turn","request_id":"request","delta":format!("pending output {}", "x".repeat(512 * 1024))}));
        }
        let mut rest = Vec::new();
        wire.0.read_to_end(&mut rest).unwrap();
    });
        let flow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut terminal = Terminal::launch(home.command());
            terminal.editor();
            if pending_output {
                terminal.write(b"continue\n");
                terminal.until("pending output");
                thread::sleep(Duration::from_millis(150));
            }
            panic!("the flow fails while its CLI waits for input");
        }));
        assert_eq!(
            flow.unwrap_err().downcast_ref::<&str>(),
            Some(&"the flow fails while its CLI waits for input")
        );
        daemon.join().unwrap();
    }
}

#[test]
#[cfg(target_os = "linux")]
fn clipboard_cancellation_keeps_raw_terminal_input_responsive_and_reaps_the_reader() {
    let home = Home::new();
    let bin = home.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let helper = bin.join("wl-paste");
    fs::write(&helper, "#!/bin/sh\nif [ \"$1\" = \"--list-types\" ]; then\n  printf 'text/plain;charset=utf-8\\n'\n  exit 0\nfi\nprintf '%s\\n' \"$$\" > \"$DIM_CLIPBOARD_TEST_PID\"\nexec /bin/sleep 300\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let pid_file = home.0.join("reader.pid");
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for text in ["after cancel", "retained"] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command
        .env("PATH", &bin)
        .env("WAYLAND_DISPLAY", "fixture")
        .env("DIM_CLIPBOARD_TEST_PID", &pid_file);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"kept\x16");
    terminal.until("Reading clipboard");
    let deadline = Instant::now() + PATIENCE;
    while !pid_file.exists() {
        assert!(
            Instant::now() < deadline,
            "Clipboard reader was not started"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    terminal.write(b"\n");
    terminal.until("Wait for clipboard insertion");
    terminal.raw(b"\x03");
    terminal.until("Ctrl+C: clear");
    let deadline = Instant::now() + PATIENCE;
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "Cancelled clipboard reader was not reaped"
        );
        thread::sleep(Duration::from_millis(10));
    }
    terminal.write(b"after cancel\n");
    terminal.editor();
    fs::remove_file(helper).unwrap();
    terminal.write(b"retained\x16");
    terminal.until("Cannot start wl-paste");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn clipboard_uri_lists_fall_back_to_text_and_reject_mixed_file_groups() {
    let home = Home::new();
    let bin = home.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let helper = bin.join("wl-paste");
    fs::write(
        &helper,
        r#"#!/bin/sh
if [ "$1" = "--list-types" ]; then
  printf 'text/uri-list\ntext/plain;charset=utf-8\n'
elif [ "$3" = "text/uri-list" ]; then
  /bin/cat "$DIM_CLIPBOARD_TEST_URIS"
else
  printf 'https://example.invalid/copied'
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let uris = home.0.join("uris");
    fs::write(&uris, "# copied link\nhttps://example.invalid/copied\n").unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for text in ["https://example.invalid/copied", "kept"] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command
        .env("PATH", &bin)
        .env("WAYLAND_DISPLAY", "fixture")
        .env("DIM_CLIPBOARD_TEST_URIS", &uris);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"\x16");
    terminal.until("https://example.invalid/copied");
    terminal.write(b"\n");
    terminal.editor();
    fs::write(
        &uris,
        "file:///tmp/copied\nhttps://example.invalid/copied\n",
    )
    .unwrap();
    terminal.write(b"kept\x16");
    terminal.until("Clipboard file URL is not local");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_file_sources_preserve_order_bytes_and_the_local_resume_base() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let local = home.0.join("local");
    let invoking = home.0.join("invoking");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&invoking).unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    let jpeg = image_fixture(image::ImageFormat::Jpeg, 1, 1);
    let first = local.join("wrong.txt");
    let second = local.join("other image.bin");
    let note = local.join("space note.md");
    fs::write(&first, &png).unwrap();
    fs::write(&second, &jpeg).unwrap();
    fs::write(&note, "P1 ordinary text reference contents stay local").unwrap();
    let expected_note = note.canonicalize().unwrap();
    let other = second.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(true);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["user"]["content"], json!([
            {"type":"text","text":"Compare "},
            {"type":"image","source":{"type":"base64","mediaType":"image/png","data":STANDARD.encode(png)}},
            {"type":"text","text":expected_note.to_str().unwrap()},
            {"type":"image","source":{"type":"base64","mediaType":"image/jpeg","data":STANDARD.encode(jpeg)}},
            {"type":"text","text":" done"}
        ]));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        for text in [expected_note.to_str().unwrap().to_owned(), "prefix wrong.txt suffix".into(), "https://example.invalid/image.png".into(), "file://remote/tmp/photo.png".into()] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        }
        assert!(other.exists());
        wire.detach();
    });
    let mut command = home.command();
    command
        .arg("--resume")
        .arg("session")
        .arg("--cwd")
        .arg(&local)
        .current_dir(&invoking);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"Compare ");
    terminal.paste("'wrong.txt' space\\ note.md");
    terminal.until("[File: space note.md]");
    fs::write(&first, "changed after capture").unwrap();
    terminal.paste(url::Url::from_file_path(&second).unwrap().as_str());
    terminal.until("[Image 2: other image.bin]");
    terminal.write(b" done\n");
    terminal.editor();
    fs::write(&note, "BM ordinary text is still a path reference").unwrap();
    terminal.paste("space note.md");
    terminal.until("[File: space note.md]");
    terminal.write(b"\n");
    terminal.editor();
    for text in [
        "prefix wrong.txt suffix",
        "https://example.invalid/image.png",
        "file://remote/tmp/photo.png",
    ] {
        terminal.paste(text);
        terminal.write(b"\n");
        terminal.editor();
    }
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("local files:"));
    assert!(first.exists() && note.exists() && second.exists());
}

#[test]
fn interactive_file_groups_reject_atomically_and_marker_removal_releases_capacity() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    fs::write(home.0.join("note.md"), "note").unwrap();
    let broken = format!("broken-{}.png", "x".repeat(100));
    fs::write(home.0.join(&broken), "not png").unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    fs::write(home.0.join("one.png"), &png).unwrap();
    let gif = image_fixture(image::ImageFormat::Gif, 1, 1);
    fs::write(home.0.join("two.gif"), &gif).unwrap();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "kept");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 20);
        for part in &content[..19] {
            assert_eq!(part["source"]["data"], STANDARD.encode(&png));
        }
        assert_eq!(content[19]["source"]["mediaType"], "image/gif");
        assert_eq!(content[19]["source"]["data"], STANDARD.encode(gif));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut command = home.command();
    command.arg("--cwd").arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"kept");
    terminal.paste(&format!("note.md {broken}"));
    terminal.until("PNG, JPEG, WebP or GIF");
    terminal.write(b"\n");
    terminal.editor();
    terminal.paste(&["one.png"; 20].join(" "));
    terminal.until("[Image 20: one.png]");
    terminal.paste("note.md two.gif");
    terminal.until("at most 20 images");
    terminal.raw(b"\x7f");
    terminal.paste("two.gif");
    terminal.until("[Image 20: two.gif]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(home.0.join("one.png").exists());
    assert!(home.0.join("two.gif").exists());
}

#[test]
fn interactive_file_completion_navigates_selects_and_dismisses_without_submitting() {
    let home = Home::new();
    let dir = home.0.join("docs a");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.md"), "a").unwrap();
    fs::write(dir.join("b.md"), "b").unwrap();
    let hostile = "bad\n\u{1b}[31m.txt";
    fs::write(home.0.join(hostile), "safe").unwrap();
    let expected = [
        dir.join("b.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        "literal @docs".into(),
        "kept @missing/file".into(),
        home.0
            .join(hostile)
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        "delayed @docsfragmented\n/exit\n".into(),
    ];
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for text in expected {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command.arg("--cwd").arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"@docs");
    terminal.until("* docs a/");
    terminal.raw(b"\t");
    terminal.until("* a.md");
    terminal.raw(b"\x1b[B");
    terminal.until("* b.md");
    terminal.raw(b"\t");
    terminal.until("[File: b.md]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"literal @docs");
    terminal.until("Esc: close suggestions");
    terminal.raw(b"\x1b");
    terminal.until("Enter: send");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"kept @missing/file");
    terminal.until("No such file or directory");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"@bad");
    terminal.until("bad\\n\\u{1b}[31m.txt");
    terminal.raw(b"\t");
    terminal.until("[File: bad\\n\\u{1b}[31m.txt]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"delayed @docs");
    terminal.until("Esc: close suggestions");
    terminal.raw(b"\x1b");
    terminal.until("Enter: send");
    terminal.raw(b"[200~fragmented\n/exit\n\x1b[201~");
    terminal.until("[Paste 1: 2 lines]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("\u{1b}[31m"));
}

/// The line composer edits graphemes atomically: inserting a combining
/// character can merge both adjacent graphemes, so the cursor snaps to a
/// grapheme boundary and the next delete or backspace removes the cluster.
#[test]
fn interactive_composer_preserves_multiline_pastes_and_edits_atomic_markers() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for expected in [
            "one\ntwo",
            "before start\r\n中\n/exit\nend\u{3}\n after",
            "remove keep [Paste 1: 2 lines]",
            "ABC",
            "first\nZsecond",
            "kept across EOF",
            "shift\nenter\nworks",
            "after malformed UTF-8",
            "standalone\rCR\r",
            "e\u{301}ok",
            "resized draft",
        ] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], expected);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.raw(b"one\ntwo");
    terminal.until("two");
    terminal.raw(b"\r");
    terminal.until("\n> ");
    terminal.until("Ctrl+C: clear");
    terminal.raw(b"before ");
    terminal.until("before ");
    terminal.until("Ctrl+C: clear");
    terminal.raw(b"\x1b[20");
    terminal.raw("0~start\r\n中\n/exit\nend\u{3}\n\x1b[2".as_bytes());
    terminal.raw(b"01~");
    terminal.until("[Paste 1: 4 lines]");
    terminal.raw(b" after\r");
    terminal.until("\n> ");
    terminal.raw("remove \x1b[200~discard\nme\x1b[201~\x7fe\u{301}👩‍💻🇨🇳\x1b[D\x7f\x1b[C\x7f\x7fkeep [Paste 1: 2 lines]\r".as_bytes());
    terminal.until("\n> ");
    terminal
        .raw(b"A\x1b[200~x\n\x1b[201~B\x1b[200~y\n\x1b[201~C\x1b[H\x1b[C\x1b[3~\x1b[F\x1b[D\x7f\r");
    terminal.until("\n> ");
    terminal.raw(b"first\nsecond\x1b[H\x1b[A\x1b[BZ\x1b[F\r");
    terminal.until("\n> ");
    terminal.write(b"kept across EOF");
    terminal.raw(b"\x04");
    terminal.until("Draft retained; clear it before exiting");
    terminal.raw(b"\x1a\r");
    terminal.until("\n> ");
    terminal.raw(b"shift\x1b[13;2uenter\x1b[27;2;13~works\r");
    terminal.until("\n> ");
    terminal.raw(b"\xf0after malformed UTF-8\r");
    terminal.until("\n> ");
    terminal.paste("standalone\rCR\r");
    terminal.until("[Paste 1: 1 lines]");
    terminal.raw(b"\r");
    terminal.until("\n> ");
    terminal.raw(
        "e\u{301}\x1b[200~p\n\x1b[201~👩‍💻🇨🇳\x1b[D\x7f\x1b[D\x1b[3~\x1b[H\x1b[C\x1b[3~ok\r"
            .as_bytes(),
    );
    terminal.until("\n> ");
    let size = libc::winsize {
        ws_row: 6,
        ws_col: 15,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.signal(libc::SIGWINCH);
    terminal.raw(b"resized draft\r");
    terminal.until("\n> ");
    terminal.raw(b"\x04");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let mut mode = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.master.as_ref().unwrap().as_raw_fd(), &mut mode) },
        0
    );
    assert_eq!(
        mode.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG),
        libc::ICANON | libc::ECHO | libc::ISIG,
    );
}

#[test]
fn interactive_output_survives_terminal_backpressure() {
    for interactive in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12, "text", json!({"turn_id":"turn","request_id":"request","delta":format!("{}output complete", "x".repeat(512 * 1024))}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
        let mut command = home.command();
        if !interactive {
            command.args(["exec", "--prompt", "continue"]);
        }
        let mut terminal = Terminal::launch(command);
        if interactive {
            terminal.until("Ctrl+J: newline");
            terminal.write(b"continue\n");
        }
        thread::sleep(Duration::from_millis(150));
        if interactive {
            terminal.until("output complete");
            terminal.until("Ctrl+J: newline");
            terminal.write(b"/exit\n");
        }
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(terminal.transcript.contains(&"x".repeat(512 * 1024)));
        assert!(terminal.transcript.contains("output complete"));
    }
}

#[test]
#[cfg(target_os = "macos")]
fn interactive_input_names_its_terminal_without_listing_dev_while_terminals_close() {
    const SANDBOX: &str = "/usr/bin/sandbox-exec";
    const PROFILE: &str = r#"(version 1)(allow default)(deny file-read-data (literal "/dev"))"#;
    let listing = Command::new(SANDBOX)
        .args(["-p", PROFILE, "/bin/ls", "/dev"])
        .output()
        .unwrap();
    assert!(
        !listing.status.success(),
        "The profile must deny the /dev listing"
    );
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        wire.detach();
    });
    let (ready, started) = std::sync::mpsc::channel();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let churn = thread::spawn(move || {
        let mut ready = Some(ready);
        loop {
            let (mut master, mut slave) = (0, 0);
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            unsafe {
                libc::close(slave);
                libc::close(master);
            }
            if let Some(ready) = ready.take() {
                ready.send(()).unwrap();
            }
            if stopped.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
                break;
            }
        }
    });
    started.recv().unwrap();
    let mut command = Command::new(SANDBOX);
    command
        .args([
            "-p",
            PROFILE,
            env!("CARGO_BIN_EXE_dimcode"),
            "--no-tui",
            "--home",
        ])
        .arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.until("Ctrl+J: newline");
    drop(stop);
    churn.join().unwrap();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_commands_reject_attached_drafts_before_storing_or_executing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "  !literaldo not execute\n");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for prefix in [
        "/exit ",
        "/export markdown file ",
        "!!echo ",
        "/mcp login docs ",
        "/effort high ",
        "/reasoning low ",
        "/fast on ",
        "/agent profile ",
    ] {
        terminal.write(prefix.as_bytes());
        terminal.paste(&"large text\n".repeat(2000));
        terminal.until("[Paste 1: 2000 lines]");
        terminal.write(b"\n");
        terminal.until("Commands require plain single-line input");
        terminal.raw(b"\x03");
    }
    terminal.write(b"  !literal");
    terminal.paste("do not execute\n");
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_put_rejection_retains_confirmed_paths_without_uploading_again() {
    let home = Home::new();
    let first = "first\r\n".repeat(3000);
    let second = "second\n".repeat(3000);
    let expected = first.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], expected);
        wire.reply(&put, json!({"attachmentId":"first","path":"/files/first.txt","bytes":expected.len()}));
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], second);
        wire.send(json!({"jsonrpc":"2.0","id":put["id"],"error":{"code":-32000,"message":"storage failed","data":{"appError":"storage_failed"}}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "[Pasted text: /files/first.txt (3000 lines)]");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        let put = wire.read("attachment.put");
        wire.send(json!({"jsonrpc":"2.0","id":put["id"],"error":{"code":-32601,"message":"unknown_method","data":{"appError":"unknown_method"}}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "clean draft");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.paste(&first);
    terminal.until("[Paste 1: 3000 lines]");
    terminal.paste(&"second\n".repeat(3000));
    terminal.until("[Paste 2: 3000 lines]");
    terminal.write(b"\n");
    terminal.until("Paste 2: storage failed");
    terminal.raw(b"\x7f\r");
    terminal.until("\n> ");
    terminal.paste(&first);
    terminal.until("[Paste 1: 3000 lines]");
    terminal.write(b"\n");
    terminal.until("unknown_method");
    terminal.raw(b"\x03");
    terminal.write(b"clean draft\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_cancellation_drains_the_put_and_reuses_its_confirmed_path() {
    let home = Home::new();
    let text = "cancelled upload\r\n".repeat(1100);
    let (accepted, admitted) = std::sync::mpsc::channel();
    let (release, drain) = std::sync::mpsc::channel();
    let expected = text.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], expected);
        accepted.send(()).unwrap();
        drain.recv_timeout(PATIENCE).unwrap();
        wire.reply(
            &put,
            json!({"attachmentId":"drained","path":"/files/drained.txt","bytes":expected.len()}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            "[Pasted text: /files/drained.txt (1100 lines)]"
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.paste(&text);
    terminal.until("[Paste 1: 1100 lines]");
    terminal.write(b"\n");
    admitted.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for the text attachment to settle");
    terminal.signal(libc::SIGINT);
    terminal.until("Still waiting for the text attachment to settle");
    release.send(()).unwrap();
    terminal.until("Preparation cancelled");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_encoding_rejection_keeps_confirmed_items_editable() {
    let home = Home::new();
    let path = format!("/files/{}.txt", "\"".repeat(4 * 1024 * 1024));
    let expected = path.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for _ in 0..2 {
            let put = wire.read("attachment.put");
            assert_eq!(put["params"]["text"], "x".repeat(16 * 1024 + 1));
            wire.reply(
                &put,
                json!({"attachmentId":"stored","path":expected,"bytes":16*1024+1}),
            );
        }
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            format!("[Pasted text: {expected} (1 lines)]")
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for index in [1, 2] {
        terminal.paste(&"x".repeat(16 * 1024 + 1));
        terminal.until(&format!("[Paste {index}: 1 lines]"));
    }
    terminal.write(b"\n");
    terminal.until("exceeds 16 MiB after JSON encoding");
    terminal.raw(b"\x7f\r");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn a_line_draft_of_images_over_the_frame_is_refused_and_kept() {
    // A kept image encodes to at most MAX_BASE64_BYTES; four exceed the
    // 16 MiB request frame, and deleting one leaves three that fit.
    let fits_raw = (MAX_BASE64_BYTES / 4 * 3) as usize;
    assert_eq!(base64_len(fits_raw as u64), MAX_BASE64_BYTES);
    let home = Home::new();
    let mut paths = Vec::new();
    for index in 0..4 {
        let mut png = image_fixture(image::ImageFormat::Png, 1, 1);
        png.resize(fits_raw, 0);
        let path = home.0.join(format!("large{index}.png"));
        fs::write(&path, &png).unwrap();
        paths.push(path);
    }
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "{content:?}");
        assert!(content.iter().all(|block| block["type"] == "image"));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.editor();
    for path in &paths {
        terminal.paste(path.to_str().unwrap());
    }
    terminal.until("[Image 4: large3.png]");
    terminal.write(b"\n");
    terminal.until("Draft retained");
    terminal.write(b"\x7f\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_boundaries_reject_without_losing_the_existing_draft() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            format!("kept{}", "\0".repeat(16 * 1024))
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        let put = wire.read("attachment.put");
        let text = put["params"]["text"].as_str().unwrap();
        assert_eq!(text.len(), 10 * 1024 * 1024);
        assert!(text.bytes().all(|byte| byte == b'x'));
        wire.reply(
            &put,
            json!({"attachmentId":"maximum","path":"/files/max.txt","bytes":text.len()}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            "[Pasted text: /files/max.txt (1 lines)]"
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "after");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"kept");
    terminal.raw(b"\x1b[200~unfinished\x03");
    terminal.raw(b"\x03");
    terminal.until("Paste cancelled; draft retained");
    terminal.raw(b"\x1b[200~\xff\x1b[201~");
    terminal.until("Paste must be UTF-8");
    terminal.paste(&"x".repeat(10 * 1024 * 1024 + 1));
    terminal.until("Paste exceeds 10 MiB");
    terminal.paste(&"\0".repeat(16 * 1024));
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.paste(&"x".repeat(10 * 1024 * 1024));
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.paste(&"\0".repeat(3 * 1024 * 1024));
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("exceeds 16 MiB after JSON encoding");
    terminal.raw(b"\x7f");
    terminal.write(b"after\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains('\0'));
    assert!(terminal.transcript.contains("\\0"));
}

#[test]
fn interactive_put_uncertainty_exits_without_another_put_or_submission() {
    for disconnect in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let put = wire.read("attachment.put");
            if !disconnect {
                wire.reply(
                    &put,
                    json!({"path":123,"attachmentId":"stored","bytes":20000}),
                );
                wire.detach();
            }
        });
        let mut terminal = Terminal::launch(home.command());
        terminal.until("\n> ");
        terminal.paste(&"x".repeat(20000));
        terminal.until("[Paste 1: 1 lines]");
        terminal.write(b"\n");
        terminal.until(if disconnect {
            "App Server connection closed"
        } else {
            "Attachment outcome could not be confirmed"
        });
        assert_eq!(terminal.wait().code(), Some(1));
        daemon.join().unwrap();
    }
}

#[test]
fn interactive_shell_prefixes_use_session_methods_and_keep_failures_out_of_model_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for _ in 0..2 { let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8})); }
        let mut identities = std::collections::HashSet::new();
        for (mode, command) in [("record", " printf 'recorded' "), ("private", "printf private"), ("record", "broken"), ("record", "busy"), ("record", "unknown"), ("record", "opaque"), ("private", "!nested")] {
            let run = wire.read("shell.run");
            assert_eq!(run["params"]["sessionId"], "session");
            assert_eq!(run["params"]["branchId"], "main");
            assert_eq!(run["params"]["mode"], mode);
            assert_eq!(run["params"]["command"], command);
            let id = run["params"]["runId"].as_str().unwrap();
            assert!(uuid::Uuid::parse_str(id).is_ok());
            assert!(identities.insert(id.to_owned()));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":8}));
            if command == "unknown" || command == "opaque" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32601,"message":"Unavailable","data":if command == "unknown" {json!({"details":"opaque-private"})} else {json!("opaque-private")}}}));
                continue;
            }
            if command == "busy" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Already running","data":{"appError":"shell_busy"}}}));
                continue;
            }
            let mut reply = json!({"runId":id,"status":"completed","exitCode":0,"output":"final-\u{1b}[31mresult\u{1b}[0m\u{202e}","truncated":false,"durationMs":1});
            if mode == "record" {
                reply["recorded"] = json!({"sequence":9});
            }
            if command == "broken" {
                reply["status"] = "failed".into();
                reply["reason"] = "read failed".into();
                reply["truncated"] = true.into();
                reply["recorded"] = json!({"error":{"appError":"branch_closed","message":"Branch closed"}});
            }
            wire.reply(&run, reply);
        }
        let submit=wire.read("session.submit"); assert_eq!(submit["params"]["text"],"  !literal"); wire.reply(&submit,json!({"event":{}}));
        let idle=wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"!!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"! printf 'recorded' \n");
    terminal.until("[shell recorded] sequence: 9");
    terminal.until("\n> ");
    terminal.write(b"!!printf private\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"!broken\n");
    terminal.until("[shell recording failed] Branch closed (branch_closed)");
    terminal.until("\n> ");
    terminal.write(b"!busy\n");
    terminal.until("[shell error] Already running (RPC -32000, shell_busy)");
    terminal.until("\n> ");
    for command in ["!unknown", "!opaque"] {
        terminal.write(format!("{command}\n").as_bytes());
        terminal.until("[shell error] Unavailable (RPC -32601)");
        terminal.until("\n> ");
    }
    terminal.write(b"!!!nested\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"  !literal\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("final-result"));
    assert!(terminal.transcript.contains("(output truncated)"));
    assert_eq!(terminal.transcript.matches("[shell recorded]").count(), 1);
    assert!(!terminal.transcript.contains("opaque-private"));
    assert!(!terminal.transcript.contains("\x1b[31m"));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_runs_during_a_turn_preserve_approval_focus_and_two_stage_interrupts() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "work");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "record");
        assert_eq!(run["params"]["command"], "inspect");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":null}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"li\u{1b}[31mve","skipped":7}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":" out\u{202e}put"}}));
        wire.notice("management.progress", json!({"id":99999,"method":"shell.run","data":{"text":"foreign progress"}}));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"authoritative output","truncated":false,"durationMs":1,"recorded":{"sequence":12}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"late progress must be ignored"}}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"], "deny");
        wire.reply(&answer, json!({}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "private");
        assert_eq!(run["params"]["command"], "wait");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"waiting"}}));
        let shell_stop = wire.read("shell.stop");
        assert_eq!(shell_stop["params"]["runId"], run["params"]["runId"]);
        let stop = wire.read("session.stop");
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":40}));
        wire.reply(&idle, json!({"sequence":40}));
        wire.reply(&shell_stop, json!({"stopped":false}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "next");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model working");
    terminal.write(b"ordinary typeahead\n");
    terminal.until("[busy] Wait for the prompt");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("authoritative output"));
    terminal.write(b"!never-execute-this-answer\n");
    terminal.until("[shell recorded] sequence: 12");
    terminal.write(b"!!wait\n");
    terminal.until("waiting");
    terminal.write(b"!another\n");
    terminal.until("[shell busy]");
    terminal.signal(libc::SIGINT);
    terminal.until("[shell stopping] Waiting for command settlement");
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(b"next\n");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("skipped 7 bytes"));
    assert!(!terminal.transcript.contains("foreign progress"));
    assert_eq!(terminal.transcript.matches("[shell started]").count(), 2);
    assert_eq!(
        terminal
            .transcript
            .matches("[shell progress] (incomplete)")
            .count(),
        2
    );
    assert!(terminal.transcript.contains("live output"));
    assert!(terminal.transcript.contains("[shell progress error]"));
    assert!(
        !terminal
            .transcript
            .contains("late progress must be ignored")
    );
    assert!(!terminal.transcript.contains("\x1b[31m"));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_eof_stops_an_unanswered_run_and_detaches_after_both_replies() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let run = wire.read("shell.run");
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"before admission"}}));
        let stop = wire.read("shell.stop");
        assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.reply(&stop, json!({"stopped":false}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!wait\n");
    terminal.until("before admission");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_default_deny_discards_typeahead_and_ctrl_c_settles_before_next_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"first-request","textOffset":0,"text":"working","thinking":"","thinkingOffset":0}));
        thread::sleep(Duration::from_millis(150));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"description":"Inspect workspace","command":"printf workspace-inspected","cwd":"/tmp/approval-fixture","secret":"never print me"}},"reason":"shell command"}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"],"deny");
        wire.reply(&answer,json!({}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"denied"}));
        wire.reply(&idle,json!({"sequence":30}));
        let submit = wire.read("session.submit"); assert_eq!(submit["params"]["text"],"second");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"second-request","textOffset":0,"text":"partial","thinking":"","thinkingOffset":0}));
        let ended = json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":35,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"second-turn","status":"cancelled","reason":"interrupted"}}}}).to_string();
        wire.0.get_mut().write_all(&ended.as_bytes()[..10]).unwrap();
        wire.0.get_mut().flush().unwrap();
        let stop = wire.read("session.stop");
        writeln!(wire.0.get_mut(),"{}",&ended[10..]).unwrap();
        wire.reply(&stop,json!({"sequence":40}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"first\n");
    terminal.until("working");
    terminal.write(b"yes\n");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"second\n");
    terminal.until("partial");
    thread::sleep(Duration::from_millis(100));
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
    assert!(
        terminal
            .transcript
            .contains("command: printf workspace-inspected"),
        "{}",
        terminal.transcript
    );
    assert!(
        terminal.transcript.contains("cwd: /tmp/approval-fixture"),
        "{}",
        terminal.transcript
    );
    assert!(!terminal.transcript.contains("never print me"));
}

#[test]
fn interactive_configuration_commands_route_and_recover_without_submitting_commands() {
    fn history(wire: &mut Wire) {
        let request = wire.read("session.history");
        let events = if request["params"]["after"].as_u64().unwrap() < 9 {
            json!([{"sequence":9,"create_at":1,"type":"context","name":"configured","data":{
                "timeline":"main","revision":"configured","previous":null,"prompt":null,
                "provider_id":"test","model_id":"model"}}])
        } else {
            json!([])
        };
        wire.reply(
            &request,
            json!({"events":events,"through":9,"nextAfter":9,"done":true}),
        );
    }
    fn catalog(wire: &mut Wire) {
        let request = wire.read("provider.catalog");
        assert_eq!(request["params"], json!({"providerId":"test"}));
        let mut capabilities = serde_json::to_value(
            dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384),
        )
        .unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(
            &request,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{"id":"model","name":"Model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["high","low"]},
                "serviceTier":{"supported":true,"options":["priority","default"]}}}]}}),
        );
    }
    let home = Home::new();
    let exported = home.0.join("configured.json");
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        history(wire);
        catalog(wire);
        history(wire);
        catalog(wire);
        let effort = wire.read("session.model.update");
        assert_eq!(effort["params"], json!({"sessionId":"session","branchId":"main","modelPreferences":{"reasoningEffort":"high"}}));
        entered.send(()).unwrap();
        wait_release.recv_timeout(PATIENCE).unwrap();
        wire.reply(&effort, json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}));
        history(wire);
        history(wire);
        history(wire);
        catalog(wire);
        let fast = wire.read("session.model.update");
        assert_eq!(fast["params"]["modelPreferences"], json!({"serviceTier":"default"}));
        wire.send(json!({"jsonrpc":"2.0","id":fast["id"],"error":{"code":-32000,"message":"fixture preference rejection"}}));
        let agents = wire.read("agent.list");
        wire.reply(&agents, json!([{"agentId":"profile-id","name":"Reviewer","description":"Review changes"}]));
        let agent = wire.read("session.agent.update");
        assert_eq!(agent["params"], json!({"sessionId":"session","branchId":"main","agentId":"profile-id"}));
        wire.reply(&agent, json!({"outcome":"adopted"}));
        history(wire);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "after configuration");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":9}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/effort\n");
    terminal.until("Reasoning effort choices: high, low");
    terminal.until("\n> ");
    terminal.write(b"/reasoning high\n");
    wait_entered.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for configuration result");
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for configuration result");
    release.send(()).unwrap();
    terminal.until("Reasoning effort configuration adopted: high");
    terminal.until("\n> ");
    terminal.write(format!("/export json {}\n", exported.display()).as_bytes());
    terminal.until("Exported JSON:");
    terminal.until("\n> ");
    terminal.write(b"/fast off\n");
    terminal.until("Error: fixture preference rejection");
    terminal.until("\n> ");
    terminal.write(b"/agent\n");
    terminal.until("profile-id\tReviewer\tReview changes");
    terminal.until("\n> ");
    terminal.write(b"/agent profile-id\n");
    terminal.until("Session tools unchanged.");
    terminal.until("\n> ");
    terminal.write(b"after configuration\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let data: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
    assert_eq!(data["through"], 9);
    assert_eq!(data["events"][0]["data"]["model_id"], "model");
}

#[test]
fn explicit_model_selection_uses_protocol_field_names_from_tui_and_line_client() {
    for (tui, leaf) in [(true, true), (true, false), (false, false)] {
        for (environment, argument, expected) in [
            (None, None, "high"),
            (Some("low"), None, "low"),
            (Some("low"), Some("medium"), "medium"),
        ] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            let mut features = vec!["session.create", "session.history", "provider.list", "provider.management"];
            if leaf {
                features.push("ui.leaf.2");
            }
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
            let management = wire.read("provider.management");
            assert_eq!(management["params"], json!({"providerId":"fixture"}));
            wire.reply(&management, json!({"providers":[{"providerId":"fixture","source":"custom","displayName":"Fixture","enabled":true,"version":1,"connection":null,"preferences":{"fixture-model":{"reasoningEffort":"high","serviceTier":"priority"}}}],"errors":[]}));
            let create = wire.read("session.create");
            assert_eq!(create["params"]["model"], json!({"provider":"fixture","modelId":"fixture-model"}));
            assert_eq!(create["params"]["startup"], json!({}));
            assert_eq!(create["params"]["modelPreferences"], json!({"reasoningEffort":expected,"serviceTier":"priority"}));
            wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"fixture stops here","data":{"appError":"provider_failed"}}}));
        });
            let mut command = home.command();
            command.args(["--provider", "fixture", "--model", "fixture-model"]);
            command.env_remove("DIM_EFFORT_LEVEL");
            if let Some(effort) = environment {
                command.env("DIM_EFFORT_LEVEL", effort);
            }
            if let Some(effort) = argument {
                command.args(["--effort", effort]);
            }
            if tui {
                let mut terminal = Terminal::launch_tui(command);
                terminal.until("fixture stops here");
                if !leaf {
                    terminal.signal(libc::SIGKILL);
                }
                assert!(!terminal.wait().success());
                daemon.join().unwrap();
            } else {
                command
                    .args(["--prompt", "hello"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let output = wait(command.spawn().unwrap());
                daemon.join().unwrap();
                assert!(!output.status.success());
            }
        }
    }
}

#[test]
fn startup_effort_on_the_default_model_reports_rejection_without_clearing_saved_preferences() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
            "protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let create = wire.read("session.create");
        assert!(create["params"].get("model").is_none());
        assert_eq!(create["params"]["modelPreferences"], json!({"reasoningEffort":"medium"}));
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,
            "message":"Reasoning effort is not supported by this model","data":{"appError":"provider_failed"}}}));
    });
    let output = home
        .command()
        .env_remove("DIM_EFFORT_LEVEL")
        .args(["exec", "--effort", "medium", "-p", "hello"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("Reasoning effort is not supported by this model"),
        "{error}"
    );
    assert!(!error.contains("--clear-preferences"), "{error}");
}

#[test]
fn invalid_startup_effort_fails_without_starting_a_daemon() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for (args, expected) in [
        (vec!["--effort", ""], "must be a nonempty effort name"),
        (vec!["--effort", " high "], "without surrounding whitespace"),
        (
            vec!["--effort", "high", "--effort", "low"],
            "Supply --effort only once",
        ),
        (
            vec!["models", "fixture", "--effort", "high"],
            "Commands cannot be combined with chat options",
        ),
    ] {
        let output = home
            .command()
            .env_remove("DIM_EFFORT_LEVEL")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn fullscreen_session_attached_before_the_first_frame_shows_one_banner() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert_eq!(
        terminal.transcript.matches("Welcome to DIM").count(),
        1,
        "{}",
        terminal.transcript
    );
}

/// The model label carries the reasoning effort the Session adopted, in both
/// terminal modes, and no suffix when the configuration carries none.
#[test]
fn the_model_label_shows_the_adopted_reasoning_effort_in_both_modes() {
    for fullscreen in [false, true] {
        for (preferences, expected) in [
            (json!({"reasoningEffort":"high"}), "test / model · high"),
            (json!({}), "test / model"),
        ] {
            let home = Home::new();
            let event = configured_event(4, "model", preferences.clone());
            let daemon = home.daemon(move |wire| {
                wire.hello(&[]);
                wire.created();
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(
                    &history,
                    json!({"events":[event],"through":8,"nextAfter":8,"done":true}),
                );
                wire.no_context();
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                wire.hold();
            });
            let mut command = home.command();
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until(expected);
            if preferences
                .as_object()
                .is_some_and(|value| value.is_empty())
            {
                assert!(
                    !terminal.transcript.contains("· high"),
                    "a configuration without a preference drew a suffix: {}",
                    terminal.transcript
                );
            }
            home.kill(&terminal);
            terminal.wait();
            daemon.join().unwrap();
        }
    }
}

/// A Session created with an effort shows it before its first turn, in both
/// terminal modes: the create and attach replies name the adopted
/// configuration's preferences, before any `context/configured` fact exists.
#[test]
fn a_new_session_shows_the_effort_its_creation_adopted() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (calls, observed) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            let mut capabilities =
                serde_json::to_value(dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384))
                    .unwrap();
            capabilities["reasoning"] = json!(true);
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                calls.send(request.clone()).unwrap();
                if request["method"] == "session.context.usage" {
                    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method: session.context.usage",
                        "data":{"appError":"unknown_method"}}}));
                    continue;
                }
                let result = match request["method"].as_str().unwrap() {
                    "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                        "protocolVersion":3,"daemonProfile":{"kind":"product"},
                        "features":["session.create","session.history","provider.list",
                            "provider.catalog","provider.management"]}),
                    "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                    "provider.management" => json!({"providers":[{"providerId":"test","source":"custom",
                        "displayName":"Test","enabled":true,"version":1,"connection":null,
                        "preferences":{"model":{"reasoningEffort":"high"}}}],"errors":[]}),
                    "provider.catalog" => json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{"id":"model","name":"Model",
                        "capabilities":capabilities}]}}),
                    "session.create" => json!({"sessionId":"session","sequence":8,
                        "model":{"providerId":"test","modelId":"model"},
                        "modelPreferences":{"reasoningEffort":"high"}}),
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["modelPreferences"] = json!({"reasoningEffort":"high"});
                        record
                    }
                    "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                    "session.idle" => json!({"sequence":8}),
                    "account.providers" => json!([]),
                    _ => json!({}),
                };
                wire.reply(&request, result);
            }
        });
        let mut command = home.command();
        command.args(["--provider", "test", "--model", "model"]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model · high");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
        let calls: Vec<Value> = observed.try_iter().collect();
        let create = calls
            .iter()
            .find(|call| call["method"] == "session.create")
            .expect("the Session was created");
        assert_eq!(
            create["params"]["modelPreferences"],
            json!({"reasoningEffort":"high"})
        );
    }
}

/// A model-config profile records the protocol its selection adopts, so the
/// configured effort reaches the status line; a profile with no effort shows
/// no suffix.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn the_model_label_shows_a_configured_profile_effort() {
    let binary = std::env::var("DIM_TEST_APP_SERVER")
        .expect("Set DIM_TEST_APP_SERVER to the dim-app-server binary");
    for (suffix, label) in [
        ("effortful", "fixture / fixture-model · low"),
        ("plain", "fixture / fixture-model"),
    ] {
        let home = Home::new();
        let config = home.0.join("model.json");
        let mut model = json!({"id":"fixture-model","contextWindow":32_000,
            "maxOutputTokens":4_096,"config":{"maxTokens":4096}});
        if suffix == "effortful" {
            model["config"]["reasoning"] = json!({"effort":"low"});
        }
        fs::write(
            &config,
            serde_json::to_vec(&json!({
                "defaultModel":{"providerId":"fixture","modelId":"fixture-model"},
                "providers":[{"id":"fixture","kind":"anthropic",
                    "baseUrl":"http://127.0.0.1:9","models":[model]}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut command = home.command();
        command.args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
        ]);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until(label);
        if suffix == "plain" {
            assert!(
                !terminal.transcript.contains("· low"),
                "a profile without an effort drew a suffix: {}",
                terminal.transcript
            );
        }
        home.kill(&terminal);
        terminal.wait();
    }
}

/// `/new` returns home: the status line keeps the provider and model it
/// showed and drops the effort, because nothing has been adopted there yet.
#[test]
fn home_after_new_drops_the_previous_sessions_effort() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "provider.management",
            "ui.open",
            "ui.close",
            "ui.attach",
            "ui.status",
            "ui.shutdown",
            "ui.replace",
        ]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let register = wire.read("ui.attach");
        wire.reply(&register, json!({"uiId":"ui-1"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[configured_event(4, "model", json!({"reasoningEffort":"high"}))],
                "through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));

        // `/effort high` adopts the value on the first Session.
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"test"}));
        let mut capabilities = serde_json::to_value(
            dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384),
        )
        .unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{"id":"model","name":"Model","capabilities":capabilities,
                "metadata":{"reasoning":{"supported":true,"effortOptions":["high","low"]}}}]}}),
        );
        let effort = wire.read("session.model.update");
        assert_eq!(
            effort["params"],
            json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":"high"}})
        );
        wire.reply(&effort, json!({"outcome":"adopted"}));

        // `/new` and its first message create the next Session, which has
        // adopted no preference yet.
        let providers = wire.read("provider.management");
        assert_eq!(providers["params"], json!({}));
        wire.reply(
            &providers,
            json!({"providers":[{"providerId":"test","source":"user",
                "displayName":"Test","preferences":{}}],"errors":[]}),
        );
        let create = wire.read("session.create");
        assert!(
            create["params"].get("modelPreferences").is_none(),
            "{create}"
        );
        wire.reply(
            &create,
            json!({"sessionId":"next","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
        );
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"session"}));
        wire.reply(&detach, json!({}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"next"}));
        wire.reply(&attach, attach_record(&attach));
        let register = wire.read("ui.attach");
        assert_eq!(register["params"], json!({"sessionId":"next","kind":"tui"}));
        wire.reply(&register, json!({"uiId":"ui-2"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "next");
        wire.reply(&submit, json!({"event":{}}));
        wire.no_context();
        wire.notice("session.event", json!({"sessionId":"next","branchId":"main",
            "event":{"id":"next-text","sequence":9,"create_at":1,"type":"step","name":"text",
                "data":{"turn_id":"next-turn","request_id":"next-request","delta":"next reply"}}}));
        wire.hold();
    });
    let mut command = home.command();
    // The waits below close on the end of a synchronized update, which frames
    // every repaint; tmux would turn that framing off for the child.
    command.env_remove("TMUX");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model · high");
    terminal.write(b"/effort high\r");
    terminal.until("Reasoning effort configuration adopted: high");
    terminal.write(b"/new\r");
    // The palette lists `/new` with its description while it is typed, so
    // wait for the rule the boundary commits once the command has run.
    terminal.until("─ new session ─");
    // Widen the terminal so the whole label fits and every row is repainted:
    // the status line at home must carry the model alone.
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 200,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    let resized_at = terminal.transcript.len();
    terminal.signal(libc::SIGWINCH);
    // The repaint of a resize clears the screen first; its status line is
    // the last row of the frame, so wait for the label on that row and for
    // the frame to close behind it before reading either.
    terminal.until("\u{1b}[2J\u{1b}[H\u{1b}[3J");
    terminal.until("test / model");
    terminal.until("\u{1b}[?2026l");
    let screen = &terminal.transcript[resized_at..];
    assert!(
        screen.contains("test / model"),
        "home drew no label: {screen}"
    );
    assert!(
        !screen.contains("· high"),
        "home kept the previous Session's effort: {screen}"
    );
    terminal.write(b"hello\r");
    terminal.until("next reply");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// Re-picking the model the label already shows keeps its effort: an
/// `unchanged` adoption publishes no `context/configured` fact to redraw it.
#[test]
fn repicking_the_current_model_keeps_the_effort() {
    let home = Home::new();
    let (calls, observed) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        let mut capabilities =
            serde_json::to_value(dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384))
                .unwrap();
        capabilities["reasoning"] = json!(true);
        let catalog = json!({"catalog":{"version":4,"disabledModelIds":[],"models":[{
            "id":"model","name":"Model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effort":"high",
                "effortOptions":["low","high"]}}
        }]}});
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            calls.send(request.clone()).unwrap();
            if request["method"] == "session.context.usage" {
                wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method: session.context.usage",
                    "data":{"appError":"unknown_method"}}}));
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                    "protocolVersion":3,"daemonProfile":{"kind":"product"},
                    "features":["session.create","session.history","provider.list",
                        "provider.catalog","provider.management","provider.selection","provider.select"]}),
                "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                "provider.catalog" => catalog.clone(),
                "provider.management" => json!({"providers":[{"providerId":"test","source":"custom",
                    "displayName":"Test","enabled":true,"version":1,"connection":null,
                    "preferences":{"model":{"reasoningEffort":"high"}}}],"errors":[]}),
                "session.create" => json!({"sessionId":"session","sequence":8,
                    "model":{"providerId":"test","modelId":"model"}}),
                "session.attach" => attach_record(&request),
                "session.history" => json!({"events":[
                    configured_event(4, "model", json!({"reasoningEffort":"high"}))],
                    "through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "account.providers" => json!([]),
                "session.model.update" => json!({"outcome":"unchanged",
                    "model":{"providerId":"test","modelId":"model"}}),
                "provider.selection" => json!({"selection":{"version":1}}),
                "provider.select" => json!({}),
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model · high");
    terminal.write(b"/models\r");
    terminal.until("‹ high ›");
    terminal.write(b"\r");
    terminal.until("Ask anything...");
    terminal.until("test / model · high");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let calls: Vec<Value> = observed.try_iter().collect();
    let update = calls
        .iter()
        .find(|call| call["method"] == "session.model.update")
        .unwrap_or_else(|| panic!("the model was re-picked: {calls:?}"));
    assert_eq!(
        update["params"],
        json!({"sessionId":"session","branchId":"main","providerId":"test","modelId":"model",
            "modelPreferences":{"reasoningEffort":"high"}})
    );
}

/// A resumed Session draws the model and effort its history adopted, not the
/// model its `session.attach` record names.
#[test]
fn a_resumed_session_draws_the_adopted_model_and_effort() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[configured_event(4, "second", json!({"reasoningEffort":"high"}))],
                "through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--resume").arg("session");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / second · high");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// An adoption written by another client reaches the label as it arrives.
#[test]
fn another_clients_adoption_redraws_the_model_label() {
    let home = Home::new();
    let (adopt, adopting) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        adopting.recv_timeout(PATIENCE).unwrap();
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main",
                "event":configured_event(9, "model", json!({"reasoningEffort":"low"}))}),
        );
        wire.hold();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    adopt.send(()).unwrap();
    terminal.until("test / model · low");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The command with the terminal already laid out `cols` wide by the time the
/// interface starts, so its first frame is drawn at that width.
fn sized(command: &mut Command, cols: u16, rows: u16) {
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(command, move || {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if libc::ioctl(0, libc::TIOCSWINSZ, &size) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// A composer too narrow for the whole label sheds the effort first and the
/// model second.
#[test]
fn a_narrow_composer_drops_the_effort_before_the_model() {
    for cols in [80, 26, 18] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[configured_event(4, "model", json!({"reasoningEffort":"high"}))],
                    "through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            wire.hold();
        });
        let mut command = home.command();
        command.arg("--fullscreen");
        sized(&mut command, cols, 24);
        let mut terminal = Terminal::launch_tui(command);
        match cols {
            80 => terminal.until("test / model · high"),
            26 => {
                terminal.until("test / model");
                assert!(
                    !terminal.transcript.contains("· high"),
                    "26 columns drew the effort: {}",
                    terminal.transcript
                );
            }
            _ => {
                terminal.until("╭");
                assert!(
                    !terminal.transcript.contains("test / model"),
                    "18 columns drew the model label: {}",
                    terminal.transcript
                );
            }
        }
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_escape_sends_the_reason_that_records_the_interruption() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (settle, settlement) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            wire.notice(
                "session.state",
                json!({"sessionId":"session","branchId":"main","running":true,"stopping":false}),
            );
            let stop = wire.read("session.stop");
            assert_eq!(
                stop["params"],
                json!({
                    "sessionId":"session", "branchId":"main", "reason":"User interrupted the run."
                })
            );
            settlement.recv_timeout(PATIENCE).unwrap();
            wire.end(
                json!({"turn_id":"turn", "status":"cancelled", "reason":stop["params"]["reason"]}),
            );
            wire.notice(
                "session.state",
                json!({"sessionId":"session","branchId":"main","running":false,"stopping":false}),
            );
            wire.reply(&stop, json!({"sequence":25}));
            wire.no_context();
            let mut line = String::new();
            assert_eq!(
                wire.0.read_line(&mut line).unwrap(),
                0,
                "unexpected action: {line}"
            );
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("esc interrupt");
        terminal.write(b"\x1b");
        terminal.until("Stopping");
        assert!(!terminal.transcript.contains("User interrupted the run."));
        settle.send(()).unwrap();
        terminal.until("User interrupted the run.");
        terminal.until("stopped");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_shell_mode_runs_through_session_methods_and_shows_the_recorded_run() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let run = wire.read("shell.run");
        let params = &run["params"];
        assert_eq!((&params["sessionId"], &params["branchId"]), (&json!("session"), &json!("main")));
        assert_eq!((&params["command"], &params["mode"]), (&json!("echo recorded"), &json!("record")));
        let run_id = params["runId"].clone();
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"live-out\u{1b}]52;c;blocked-clipboard\u{7}"}}));
        wait_release.recv_timeout(PATIENCE).unwrap();
        let marker = "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"0\" truncated=\"false\">\n$ echo recorded\nrecorded-out\u{1b}]0;blocked-title\u{7}\u{1b}]8;;file:///tmp/blocked-link\u{7}hidden\u{1b}]8;;\u{7}\n\n</dim-shell-input>";
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"shell","sequence":9,"create_at":1,"type":"input","name":"message","data":{"blocks":[{"type":"raw","text":marker}],"mode":"append"}}}));
        wire.reply(&run, json!({"runId":run_id,"status":"completed","exitCode":0,"output":"recorded-out\n","truncated":false,"durationMs":1,"recorded":{"sequence":9}}));
        let run = wire.read("shell.run");
        assert_eq!((&run["params"]["command"], &run["params"]["mode"]), (&json!("echo private"), &json!("private")));
        let stop = wire.read("shell.stop");
        assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&stop, json!({"stopped":true}));
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"private-out\u{1b}]52;c;blocked-private-clipboard\u{7}\n","truncated":false,"durationMs":1}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"!echo recorded\r");
    terminal.until("live-out");
    terminal.write(b"!echo rejected\r");
    terminal.until("A shell command is already running");
    release.send(()).unwrap();
    terminal.until("recorded-out");
    terminal.write(b"\x15");
    terminal.write(b"!echo private\r");
    terminal.until("Ctrl+C to interrupt");
    terminal.write(b"\x03");
    terminal.until("private-out");
    terminal.until("not sent to the model");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    for payload in [
        "blocked-title",
        "blocked-clipboard",
        "blocked-private-clipboard",
        "file:///tmp/blocked-link",
    ] {
        assert!(
            !terminal.transcript.contains(payload),
            "{payload}: {}",
            terminal.transcript
        );
    }
    assert!(
        !terminal.transcript.contains("<dim-shell-input"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn tui_effort_dialog_selects_the_current_models_effort_and_cancels_without_writing() {
    fn catalog(wire: &mut Wire) {
        let request = wire.read("provider.catalog");
        assert_eq!(request["params"], json!({"providerId":"test"}));
        let mut capabilities = serde_json::to_value(
            dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384),
        )
        .unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(
            &request,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"other","name":"Other","capabilities":capabilities,"metadata":{"reasoning":{
                    "supported":true,"effortOptions":["invalid-neighbor"]}}},
                {"id":"model","name":"Model","capabilities":capabilities,"metadata":{"reasoning":{
                    "supported":true,"effortOptions":["low","high"],
                    "off":{"kind":"enable_thinking","value":false}}}}
            ]}}),
        );
    }
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[configured_event(4, "model", json!({"reasoningEffort":"high"}))],
                "through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            catalog(wire);
            catalog(wire);
            let selected = wire.read("session.model.update");
            assert_eq!(
                selected["params"],
                json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":"high"}})
            );
            wire.reply(&selected, json!({"outcome":"unchanged"}));
            catalog(wire);
            catalog(wire);
            catalog(wire);
            let selected = wire.read("session.model.update");
            assert_eq!(
                selected["params"],
                json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":"none"}})
            );
            wire.reply(&selected, json!({"outcome":"adopted"}));
            wire.hold();
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model · high");
        terminal.write(b"/effort\r");
        terminal.until("Current: high");
        terminal.until("2 (•) high");
        terminal.write(b"\r");
        terminal.until("Reasoning effort configuration unchanged: high");
        terminal.write(b"/effort\r");
        terminal.until("Current: high");
        terminal.write(b"\x1b");
        terminal.until(if fullscreen {
            "test / model · high"
        } else {
            "Ask anything..."
        });
        terminal.write(b"/reasoning\r");
        terminal.until("Current: high");
        terminal.until("3 ( ) none");
        assert!(!terminal.transcript.contains("invalid-neighbor"));
        terminal.write(b"3\r");
        terminal.until("Reasoning effort configuration adopted: none");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_mcp_and_configuration_commands_use_session_methods() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.notice("session.mcp.state", json!({"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}));
        let server = json!({"id":"docs","source":"user","enabled":true,"transport":"http","auth":{"status":"required"}});
        let catalog = json!({"userRevision":"1","projectRevision":null,"servers":[server],"userApplication":{},"projectApplication":null});
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog.clone());
        let state = wire.read("session.mcp");
        assert_eq!(state["params"], json!({"sessionId":"session"}));
        wire.reply(&state, json!({"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog.clone());
        let start = wire.read("mcp.auth.start");
        assert_eq!((&start["params"]["id"], &start["params"]["source"]), (&json!("docs"), &json!("user")));
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 60_000;
        wire.reply(&start, json!({"attemptId":"attempt","authorizationUrl":"https://auth.example/authorize","redirectUri":"http://localhost:1/callback","resource":"https://docs.example/mcp","issuer":"https://auth.example","expiresAt":expires}));
        let complete = wire.read("mcp.auth.complete");
        assert_eq!(complete["params"], json!({"attemptId":"attempt","callbackUrl":"http://localhost:1/callback?code=c"}));
        wire.reply(&complete, json!({}));
        let request = wire.read("provider.catalog");
        assert_eq!(request["params"], json!({"providerId":"test"}));
        let mut capabilities = serde_json::to_value(dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)).unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(&request, json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{"id":"model","name":"Model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["high","low"]}}}]}}));
        let effort = wire.read("session.model.update");
        assert_eq!(effort["params"], json!({"sessionId":"session","branchId":"main","modelPreferences":{"reasoningEffort":"high"}}));
        wire.reply(&effort, json!({"outcome":"adopted"}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env("SSH_TTY", "/dev/fixture");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("sign-in required");
    terminal.write(b"/mcp list\r");
    terminal.until("Columns: ID, source");
    terminal.write(b"/mcp login docs\r");
    terminal.until("auth.example/authorize");
    terminal.write(b"/mcp callback http://localhost:1/callback?code=c\r");
    terminal.until("Signed in to \"docs\"");
    terminal.write(b"/effort high\r");
    terminal.until("Reasoning effort configuration adopted: high");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// `session.create` fails the way it does without a model, so the TUI has
/// no Session: commands that need one must answer on their own.
fn no_session_daemon(home: &Home, quieted: std::sync::mpsc::Sender<()>) -> thread::JoinHandle<()> {
    home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"No model is chosen","data":{"appError":"provider_failed"}}}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.quiet(Duration::from_millis(1200));
        let _ = quieted.send(());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    })
}

#[test]
fn tui_mcp_without_a_session_reports_the_catalog_only() {
    let home = Home::new();
    let (quieted, quiet_done) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"No model is chosen","data":{"appError":"provider_failed"}}}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,json!({"userRevision":"1","projectRevision":null,"servers":[],"userApplication":{},"projectApplication":null}));
        // There is no Session, so there are no Session server states to ask for.
        wire.quiet(Duration::from_millis(1200));
        let _ = quieted.send(());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Sign in with /login");
    terminal.write(b"/mcp list\r");
    terminal.until("No MCP server is configured.");
    quiet_done.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("session_closed"));
}

#[test]
fn tui_agent_without_a_session_answers_the_no_session_notice_and_sends_nothing() {
    let home = Home::new();
    let (quieted, quiet_done) = std::sync::mpsc::channel();
    let daemon = no_session_daemon(&home, quieted);
    let mut terminal = Terminal::launch_tui(home.command());
    // The startup notice is the first; the command's answer is the second.
    terminal.until("Sign in with /login");
    terminal.write(b"/agent ghost\r");
    terminal.until("Sign in with /login");
    quiet_done.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("session_closed"));
}

#[test]
fn tui_effort_without_a_session_answers_the_no_session_notice_and_sends_nothing() {
    let home = Home::new();
    let (quieted, quiet_done) = std::sync::mpsc::channel();
    let daemon = no_session_daemon(&home, quieted);
    let mut terminal = Terminal::launch_tui(home.command());
    // The startup notice is the first; the command's answer is the second.
    terminal.until("Sign in with /login");
    terminal.write(b"/effort high\r");
    terminal.until("Sign in with /login");
    quiet_done.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("session_closed"));
}

#[test]
fn tui_shell_without_a_session_settles_with_the_no_session_notice() {
    let home = Home::new();
    let (quieted, quiet_done) = std::sync::mpsc::channel();
    let daemon = no_session_daemon(&home, quieted);
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Sign in with /login");
    terminal.write(b"!echo hi\r");
    // The settled shell block carries the notice as its status, and the
    // status line repeats it; neither the block nor the row stays running.
    terminal.until("failed: Sign in with /login");
    terminal.until("Shell: Sign in with");
    quiet_done.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("session_closed"));
}

#[test]
fn tui_pasted_images_and_long_text_submit_as_user_content() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let image = home.0.join("shot.png");
    let bytes = image_fixture(image::ImageFormat::Png, 3, 2);
    fs::write(&image, &bytes).unwrap();
    let long = "y".repeat(16 * 1024 + 1);
    let expected_long = long.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let put = wire.read("attachment.put");
        assert_eq!(put["params"], json!({"sessionId":"session","text":expected_long}));
        wire.reply(&put, json!({"attachmentId":"stored","path":"/files/paste.txt","bytes":16*1024+1}));
        let submit = wire.read("session.submit");
        assert!(submit["params"].get("text").is_none());
        assert_eq!(submit["params"]["user"]["content"], json!([
            {"type":"text","text":"look "},
            {"type":"image","source":{"type":"base64","mediaType":"image/png","data":STANDARD.encode(&bytes)}},
            {"type":"text","text":" [Pasted text: /files/paste.txt (1 lines)]"},
        ]));
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"image-input","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":submit["params"]["user"],"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"image-applied","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"image-input","mode":"queue"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"look ");
    terminal.paste(image.to_str().unwrap());
    terminal.until("[Image #1]");
    terminal.paste(&long);
    terminal.until("16385 chars");
    terminal.write(b"\r");
    terminal.until("[Image #1]");
    terminal.until("[Pasted text: /files/paste.txt (1 lines)]");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_at_mentions_pick_files_from_the_working_directory() {
    let home = Home::new();
    let project = home.0.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("notes.txt"), "").unwrap();
    fs::write(project.join("src/main.rs"), "").unwrap();
    let resolved = project.canonicalize().unwrap();
    let expected = format!(
        "read [@notes.txt]({}) [@src/main.rs]({})",
        resolved.join("notes.txt").display(),
        resolved.join("src/main.rs").display()
    );
    let (submitted, received) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], expected);
        wire.reply(&submit, json!({"event":{}}));
        submitted.send(()).unwrap();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.args(["--cwd", project.to_str().unwrap()]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"read @no");
    terminal.until("Files");
    terminal.until("@notes.txt");
    terminal.write(b"\r");
    terminal.write(b"@sr");
    terminal.until("@src/");
    terminal.write(b"\t");
    terminal.until("@src/main.rs");
    terminal.write(b"\t");
    terminal.write(b"\r");
    received.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

fn catalog_entry(name: &str, enabled: bool, shadowed_by: Option<&str>) -> Value {
    json!({
        "id": format!("user:{name}"),
        "source": "user",
        "name": name,
        "description": format!("About {name}"),
        "filePath": format!("/home/.agents/skills/{name}/SKILL.md"),
        "enabled": enabled,
        "shadowedBy": shadowed_by,
    })
}

#[test]
fn tui_skill_rows_come_from_the_catalog_and_complete_without_sending() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&["skills.catalog"]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let catalog = wire.read("skills.catalog");
            wire.reply(
                &catalog,
                json!({
                    "entries": [
                        catalog_entry("commit", true, None),
                        catalog_entry("deploy", false, None),
                        catalog_entry("shadowed", true, Some("user:shadowed")),
                    ],
                    "issues": [],
                }),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            wire.hold();
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");

        // The enabled Skill is listed and its row completes the marker into
        // the draft; no message is sent.
        terminal.write(b"/comm");
        terminal.until("$commit");
        terminal.raw(b"\r");
        terminal.until("$skill: commit ");
        terminal.write(b"\x03");

        // A disabled or shadowed Skill is not offered: Tab completes nothing.
        terminal.write(b"/dep");
        terminal.raw(b"\t");
        terminal.write(b"x");
        terminal.until("depx");
        terminal.write(b"\x03");
        terminal.write(b"/sha");
        terminal.raw(b"\t");
        terminal.write(b"x");
        terminal.until("shax");
        terminal.write(b"\x03");

        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn reloading_skills_changes_the_palette_rows() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["skills.catalog", "skills.refresh"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let catalog = wire.read("skills.catalog");
        wire.reply(
            &catalog,
            json!({"entries":[catalog_entry("commit", true, None)],"issues":[]}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));

        // `/reload-skills` rescans, then reads the area: its view and the
        // palette come from that one read.
        let refresh = wire.read("skills.refresh");
        assert_eq!(refresh["params"]["cwd"], json!("/workspace"));
        wire.reply(&refresh, json!({}));
        let rescan = json!({
            "entries": [
                catalog_entry("commit", true, None),
                catalog_entry("release", true, None),
            ],
            "issues": [],
        });
        let panel = wire.read("skills.catalog");
        wire.reply(&panel, rescan);
        wire.hold();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");

    // Before the rescan only the command matches; the new Skill is not known.
    terminal.write(b"/rel");
    terminal.until("/reload-skills");
    assert!(!terminal.transcript.contains("$release"));
    terminal.write(b"\x03");

    terminal.write(b"/reload-skills\r");
    // The palette feed takes the rescan's list.
    terminal.write(b"/rel");
    terminal.until("$release");
    terminal.write(b"\x03");

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn a_daemon_without_skill_catalog_keeps_a_working_command_palette() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["skills.catalog"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let catalog = wire.read("skills.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":catalog["id"],"error":{
            "code":-32601,"message":"Unknown method: skills.catalog",
            "data":{"appError":"unknown_method"}}}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.hold();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");

    // The failed read shows no error and leaves the command rows untouched.
    terminal.write(b"/ren");
    terminal.until("/rename");
    terminal.raw(b"\t");
    terminal.write(b"x");
    terminal.until("/rename x");
    terminal.write(b"\x03");
    assert!(!terminal.transcript.contains("$skill"));
    assert!(!terminal.transcript.contains("Unknown method"));

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_palette_tab_only_completes_and_enter_still_runs() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            wire.hold();
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");

        // Tab completes `/keys` into the draft; the typed character that
        // follows lands after it, so the draft proves the completion and that
        // the key bindings panel did not open.
        terminal.write(b"/ke");
        terminal.raw(b"\t");
        terminal.write(b"x");
        terminal.until("keysx");
        terminal.write(b"\x03");

        // Repeating Tab completes and never exits: the draft is still here
        // to take the next typed character.
        terminal.write(b"/");
        for _ in 0..3 {
            terminal.raw(b"\t");
        }
        terminal.write(b"x");
        terminal.until("newx");
        terminal.write(b"\x03");

        // Enter still runs the completed command.
        terminal.write(b"/ke\r");
        terminal.until("Application keys");

        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn tui_sends_dropped_files_as_links_and_images_by_model_capability() {
    let home = Home::new();
    let image = home.0.join("shot.png");
    fs::write(&image, image_fixture(image::ImageFormat::Png, 3, 2)).unwrap();
    let notes = home.0.join("my notes.txt");
    fs::write(&notes, "plain").unwrap();
    let expected = format!(
        "look [shot.png]({}) [my notes.txt](<{}>)",
        image.display(),
        notes.display()
    );
    let (submitted, received) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list","provider.catalog"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"test"}));
        let capabilities = json!({"modalities":["text"],"toolCalling":true,"streaming":true,"structuredOutput":true,"vision":false,"audio":false,"reasoning":false,"contextWindow":128000,"maxOutputTokens":16384});
        wire.reply(&catalog, json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{"id":"model","name":"Model","capabilities":capabilities}]}}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], expected);
        assert!(submit["params"].get("user").is_none());
        wire.reply(&submit, json!({"event":{}}));
        submitted.send(()).unwrap();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"look ");
    let dragged = format!(
        "{} {}",
        image.display(),
        notes.display().to_string().replace(' ', "\\ ")
    );
    terminal.paste(&dragged);
    terminal.until("[my notes.txt]");
    terminal.write(b"\r");
    received.recv_timeout(PATIENCE).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn fullscreen_login_releases_mouse_for_the_link_and_restores_the_preference_on_close() {
    const MOUSE_ON: &str = "\x1b[?1002h\x1b[?1006h";
    const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l";
    const URL: &str = "https://fixture.invalid/authorize";

    for mouse in ["on", "off"] {
        let home = Home::new();
        fs::write(
            home.0.join("config.json"),
            json!({"locale":"en","tuiMouseMode":mouse}).to_string(),
        )
        .unwrap();
        let daemon = home.daemon(|wire| {
            wire.hello(&["account.login.browser.begin", "account.login.cancel"]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([{"providerId":"dimcode-api-oauth","displayName":"DimAgent","loginMethods":["browser"]}]));
            let status = wire.read("account.status");
            assert_eq!(status["params"], json!({"providerId":"dimcode-api-oauth"}));
            wire.reply(&status, json!({"account":null}));
            let begin = wire.read("account.login.browser.begin");
            assert_eq!(begin["params"]["providerId"], "dimcode-api-oauth");
            wire.reply(&begin, json!({"attemptId":"attempt","authorizationUrl":URL,"expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
            let cancel = wire.read("account.login.cancel");
            assert_eq!(cancel["params"], json!({"providerId":"dimcode-api-oauth","attemptId":"attempt"}));
            wire.reply(&cancel, json!({}));
            wire.hold();
        });
        let mut command = home.command();
        command.arg("--fullscreen").env("DIMCODE_OAUTH_MANUAL", "1");
        command.env("SSH_CONNECTION", "fixture");
        sized(&mut command, 100, 32);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        assert_eq!(
            terminal.transcript.rfind(MOUSE_ON) > terminal.transcript.rfind(MOUSE_OFF),
            mouse == "on"
        );
        terminal.write(b"/login\r");
        terminal.until("Not signed in");
        let menu_end = terminal.consumed;
        if mouse == "on" {
            assert!(!terminal.transcript[..menu_end].contains(MOUSE_OFF));
        }
        terminal.write(b"\r");
        terminal.until(URL);
        let link_end = terminal.consumed;
        let selection_start = if mouse == "on" {
            menu_end
                + terminal.transcript[menu_end..link_end]
                    .find(MOUSE_OFF)
                    .expect("The OAuth link appeared without releasing mouse reporting")
                + MOUSE_OFF.len()
        } else {
            menu_end
        };
        terminal.write(b"\t");
        terminal.paste("callback-redraw-one");
        terminal.until("callback-redraw-one");
        terminal.paste("-two");
        terminal.until("callback-redraw-one-two");
        let redraw_end = terminal.consumed;
        assert!(
            !terminal.transcript[selection_start..redraw_end].contains(MOUSE_ON),
            "Redrawing the OAuth dialog re-enabled mouse reporting: {}",
            terminal.transcript
        );
        terminal.write(b"\x1b[27u");
        if mouse == "on" {
            terminal.until(MOUSE_ON);
        }
        terminal.write(b"after-login-close");
        terminal.until("after-login-close");
        if mouse == "off" {
            assert!(
                !terminal.transcript[menu_end..].contains(MOUSE_ON),
                "Closing OAuth ignored the configured mouse-off preference: {}",
                terminal.transcript
            );
        }
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn browser_login_hides_callback_and_uses_only_generic_account_rpc() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list","provider.management","account.login.browser.begin","account.login.browser.complete"]}));
        let listed = wire.read("provider.management");
        wire.reply(&listed,json!({"providers":[{"providerId":"fixture","displayName":"Fixture","account":{"namespace":"account","loginMethods":["browser"]}}]}));
        let begin = wire.read("account.login.browser.begin");
        assert_eq!(begin["params"]["directory"],"https://fixture.invalid");
        wire.reply(&begin,json!({"attemptId":"attempt","authorizationUrl":"urn:fixture:authorization","expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
        let complete = wire.read("account.login.browser.complete");
        assert_eq!(complete["params"]["callbackUrl"],"http://localhost:1455/auth/callback?code=secret-code&state=state");
        wire.reply(&complete,json!({"credentialEpoch":"epoch","generation":1,"account":{"issuer":"fixture","subject":"test","grantedScopes":[]}}));
    });
    let mut command = home.command();
    command.env("DIMCODE_OAUTH_MANUAL", "1").args([
        "login",
        "fixture",
        "--directory",
        "https://fixture.invalid",
    ]);
    let mut terminal = Terminal::launch(command);
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1455/auth/callback?code=secret-code&state=state\n");
    terminal.until("Signed in.");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
    assert!(
        !terminal.transcript.contains("secret-code"),
        "{}",
        terminal.transcript
    );
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_ne!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
}

#[test]
fn device_login_polls_and_cancels_the_attempt_on_ctrl_c() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list","provider.management","account.login.begin","account.login.status","account.login.cancel"]}));
        let listed = wire.read("provider.management");
        wire.reply(&listed,json!({"providers":[{"providerId":"fixture","displayName":"Fixture","account":{"namespace":"account","loginMethods":["device"]}}]}));
        let begin = wire.read("account.login.begin");
        wire.reply(&begin,json!({"attemptId":"device","verificationUri":"https://fixture.invalid/device","userCode":"TEST-CODE","expiresAt":999999}));
        let status = wire.read("account.login.status"); wire.reply(&status,json!({"status":"pending"}));
        let cancel = wire.read("account.login.cancel"); assert_eq!(cancel["params"]["attemptId"],"device"); wire.reply(&cancel,json!({}));
    });
    let mut command = home.command();
    command.args([
        "login",
        "fixture",
        "--directory",
        "https://fixture.invalid",
        "--device",
    ]);
    let mut terminal = Terminal::launch(command);
    terminal.until("TEST-CODE");
    thread::sleep(Duration::from_millis(100));
    terminal.signal(libc::SIGINT);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(130));
}

#[test]
#[ignore = "requires DIM_TEST_NPM_WRAPPER pointing to npm/bin/dimcode.mjs"]
fn unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow() {
    let source =
        PathBuf::from(std::env::var("DIM_TEST_NPM_WRAPPER").expect("DIM_TEST_NPM_WRAPPER"));
    let home = Home::new();
    let node_platform = Command::new("node")
        .args(["-p", "process.platform+'-'+process.arch"])
        .output()
        .unwrap();
    assert!(node_platform.status.success());
    let platform = String::from_utf8(node_platform.stdout).unwrap();
    let package = home
        .0
        .join(format!("wrapper/node_modules/dimcode-{}", platform.trim()));
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"dimcode-{}\"}}", platform.trim()),
    )
    .unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dimcode"), package.join("bin/dimcode")).unwrap();
    let wrapper = home.0.join("wrapper");
    let launcher = wrapper.join("bin").join("dimcode.mjs");
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    fs::copy(&source, &launcher).unwrap();
    fs::copy(
        source
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        wrapper.join("package.json"),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"streaming","thinking":"","thinkingOffset":0}));
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&stop,json!({"sequence":30})); wire.reply(&idle,json!({"sequence":30}));
        wire.detach();
    });
    let mut command = Command::new("node");
    command
        .arg(launcher)
        .arg("--no-tui")
        .arg("--home")
        .arg(&home.0)
        .env("DIMCODE_UPDATE_CHECK", "0");
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"hello\n");
    terminal.until("streaming");
    assert_eq!(
        unsafe { libc::kill(-(terminal.child.id() as i32), libc::SIGINT) },
        0
    );
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
}

struct OwnedDaemon(PathBuf);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let Ok(stream) = UnixStream::connect(&self.0) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut stream = BufReader::new(stream);
        for (id, method, params) in [
            (1, "hello", json!({"protocolVersion":3,"clientKind":"cli"})),
            (2, "daemon.shutdown", json!({})),
        ] {
            if writeln!(
                stream.get_mut(),
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            )
            .is_err()
            {
                return;
            }
            loop {
                let mut line = String::new();
                if stream
                    .read_line(&mut line)
                    .ok()
                    .filter(|size| *size > 0)
                    .is_none()
                {
                    return;
                }
                if serde_json::from_str::<Value>(&line)
                    .ok()
                    .is_some_and(|reply| reply["id"] == id)
                {
                    break;
                }
            }
        }
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn fresh_native_startup_uses_workspace_and_ask_without_policy_file() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let output = home
        .command()
        .arg("--server")
        .arg(&binary)
        .arg("--model-config")
        .arg(&config)
        .args(["mcp", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = Wire::new(stream);
    wire.send(json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocolVersion":3,"clientKind":"cli"}}));
    let mut line = String::new();
    wire.0.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    assert!(hello.get("result").is_some(), "{hello}");
    wire.send(json!({"jsonrpc":"2.0","id":2,"method":"settings.read","params":{"source":"execution","scope":"user"}}));
    line.clear();
    wire.0.read_line(&mut line).unwrap();
    let snapshot: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        snapshot["result"]["document"],
        json!({"sandbox": false, "network": false}),
        "{snapshot}"
    );
    assert!(!home.0.join("rust/execution.json").exists());
    assert!(
        home.command()
            .arg("--server")
            .arg(&binary)
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn project_mcp_entries_start_disabled_and_the_user_enables_them() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    fs::write(project.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":["original"],"env":{"SECRET":"must-not-print"},"enabled":true}}}).to_string()).unwrap();
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .arg("--model-config")
            .arg(&config)
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .output()
            .unwrap()
    };
    let list = || {
        let output = run(&["mcp", "list"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.contains("must-not-print"));
        output
    };
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    let written = fs::read(project.join(".mcp.json")).unwrap();
    let enabled = run(&["mcp", "enable", "docs"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        String::from_utf8(enabled.stdout).unwrap(),
        "Enabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&enabled.stderr).contains(".mcp.json"));
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tenabled\t—\n");
    assert_eq!(fs::read(project.join(".mcp.json")).unwrap(), written);
    assert!(run(&["daemon", "stop"]).status.success());
    assert_eq!(
        list(),
        "\"docs\"\tproject\tstdio\tenabled\t—\n",
        "restart lost the choice"
    );
    assert!(run(&["mcp", "disable", "docs"]).status.success());
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{}}).to_string(),
    )
    .unwrap();
    let unlisted = run(&["mcp", "disable", "docs"]);
    assert!(unlisted.status.success());
    assert!(String::from_utf8_lossy(&unlisted.stderr).contains("\"docs\" is not in mcp list"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(project.join(".mcp.json"), "{broken").unwrap();
    assert!(!run(&["mcp", "list"]).status.success());
    let unreadable = run(&["mcp", "disable", "docs"]);
    assert!(
        unreadable.status.success(),
        "{}",
        String::from_utf8_lossy(&unreadable.stderr)
    );
    assert_eq!(
        String::from_utf8(unreadable.stdout).unwrap(),
        "Disabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&unreadable.stderr).contains("could not be read"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{"remote":{"type":"http","url":"not a url"}}}).to_string(),
    )
    .unwrap();
    assert!(run(&["mcp", "enable", "remote"]).status.success());
    let reported = (0..50).any(|_| {
        let output = run(&["mcp", "list"]);
        let reported = String::from_utf8_lossy(&output.stderr)
            .contains("project MCP configuration: MCP configuration is not active");
        if !reported {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        reported
    });
    assert!(
        reported,
        "mcp list did not report the inactive configuration"
    );
    assert!(run(&["mcp", "disable", "remote"]).status.success());
    fs::write(project.join(".mcp.json"), &written).unwrap();
    let missing = run(&["mcp", "enable", "absent"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No MCP server \"absent\""));
    for removed in [
        &["mcp", "approve", "docs", "token"][..],
        &["mcp", "revoke", "docs"],
    ] {
        let output = run(removed);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("mcp enable ID"));
    }
    let id = "untrusted\u{9b}2J\u{202e}\nname";
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{(id):{"command":"missing-mcp-program"}}}).to_string(),
    )
    .unwrap();
    let escaped = list();
    assert!(!escaped.contains(['\u{9b}', '\u{202e}']));
    assert_eq!(escaped.lines().count(), 1);
    assert!(escaped.contains("untrusted\\u{9b}2J\\u{202e}\\nname"));
    assert!(run(&["daemon", "stop"]).status.success());
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_profile_conflicts_preserve_the_running_daemon_and_endpoint() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    for mock in [true, false] {
        let home = Home::new();
        let original = home.0.join("original.json");
        let requested = home.0.join("requested.json");
        let body = json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string();
        fs::write(&original, &body).unwrap();
        fs::write(&requested, format!("{body}\n")).unwrap();
        let socket = home.0.join("rust/daemon.sock");
        daemon_cleanup::mark_test_server(&home.0);
        let mut command = Command::new(&binary);
        command
            .arg("--unix")
            .arg(&socket)
            .arg("--home")
            .arg(home.0.join("rust"))
            .arg("--no-ws")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if mock {
            command.arg("--mock-model");
        } else {
            command.arg("--model-config").arg(&original);
        }
        let mut daemon = command.spawn().unwrap();
        let owned = OwnedDaemon(socket.clone());
        let deadline = Instant::now() + PATIENCE;
        while !socket.exists() {
            assert!(daemon.try_wait().unwrap().is_none(), "Daemon did not start");
            assert!(Instant::now() < deadline, "Daemon did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
        let output = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "--prompt",
                "must not run",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(socket.exists());
        assert!(daemon.try_wait().unwrap().is_none());
        fs::write(&requested, "invalid changed configuration").unwrap();
        let stopped = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "daemon",
                "stop",
            ])
            .output()
            .unwrap();
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        assert!(!socket.exists());
        assert!(home.0.join("rust").exists());
        drop(owned);
        assert!(wait(daemon).status.success());
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn configured_api_key_model_uses_production_tools_and_restores_the_same_session() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let binary = std::env::var("DIM_TEST_APP_SERVER")
        .expect("Set DIM_TEST_APP_SERVER to the dim-app-server binary");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let image = home.0.join("screenshot.dat");
    let bytes = image_fixture(image::ImageFormat::Png, 1, 1);
    fs::write(&image, &bytes).unwrap();
    let image_data = STANDARD.encode(bytes);
    let model_image = image_data.clone();
    let outside = Home::new();
    let reference = outside.0.join("outside reference.txt");
    fs::write(&reference, "outside-attachment-marker\n").unwrap();
    let model_reference = reference.clone();
    let paste = format!(
        "native-paste-reader-marker\r\n{}",
        "中 e\u{301} text\r\n".repeat(2000)
    );
    let model_paste = paste.clone();
    fs::write(home.0.join("marker.txt"), "production-read-marker\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        for phase in 0..7 {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Model request {phase} was not made"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            let mut authorization = false;
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if lower.trim() == "authorization: bearer fixture-secret" {
                    authorization = true;
                }
            }
            assert!(authorization, "API key environment reference was not used");
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            if phase == 2 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                assert_eq!(
                    user["content"],
                    json!([
                        {"type":"text","text":"continue"},
                        {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}}
                    ])
                );
            }
            let mut names: Vec<_> = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect();
            names.sort_unstable();
            assert_eq!(
                names,
                [
                    "agent",
                    "ask_user",
                    "branch",
                    "edit",
                    "exec",
                    "exec_stop",
                    "glob",
                    "grep",
                    "member_approval_allow",
                    "member_approval_reject",
                    "plugin",
                    "read",
                    "recall",
                    "skill",
                    "wait",
                    "web_fetch",
                    "web_search",
                    "write"
                ]
            );
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"fixture-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if phase == 0 {
                frame(
                    json!({"tool_calls":[{"index":0,"id":"read-call","type":"function","function":{"name":"read","arguments":"{\"path\":\"marker.txt\"}"}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else if phase == 3 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                let text = match &user["content"] {
                    Value::String(text) => text.as_str(),
                    Value::Array(parts) => {
                        assert_eq!(parts.len(), 1);
                        assert_eq!(parts[0]["type"], "text");
                        parts[0]["text"].as_str().unwrap()
                    }
                    other => panic!("Unexpected user content: {other}"),
                };
                let path = text
                    .strip_prefix("[Pasted text: ")
                    .unwrap()
                    .split_once(" (")
                    .unwrap()
                    .0;
                assert_eq!(fs::read(path).unwrap(), model_paste.as_bytes());
                frame(
                    json!({"tool_calls":[{"index":0,"id":"paste-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":path})).unwrap()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else if phase == 5 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                let parts = user["content"].as_array().unwrap();
                assert_eq!(parts[0], json!({"type":"text","text":"selected "}));
                assert_eq!(
                    parts[1],
                    json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}})
                );
                assert!(
                    user["content"]
                        .to_string()
                        .contains(model_reference.to_str().unwrap())
                );
                frame(
                    json!({"tool_calls":[{"index":0,"id":"selected-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":model_reference})).unwrap()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else {
                if phase == 1 {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("production-read-marker"))
                        ),
                        "The real read result did not reach the model"
                    );
                } else if phase == 2 {
                    let messages = request["messages"].to_string();
                    assert!(messages.contains("dim-shell-input"));
                    assert!(messages.contains("cli-recorded-marker"));
                    assert!(!messages.contains("cli-private-marker"));
                } else if phase == 4 {
                    assert!(
                        request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|message| {
                                message["role"] == "tool"
                                    && message["content"].as_str().is_some_and(|text| {
                                        text.contains("native-paste-reader-marker")
                                    })
                            }),
                        "The stored paste did not reach the model through read"
                    );
                } else {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("outside-attachment-marker"))
                        )
                    );
                }
                frame(
                    json!({"content":match phase {1 => "read completed", 2 => "session continued", 4 => "paste read completed", _ => "files read completed"}}),
                    json!("stop"),
                )
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,serde_json::to_vec(&json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]})).unwrap()).unwrap();
    let mut first = home.command();
    first
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "exec",
            "--prompt",
            "read marker.txt",
        ])
        .env("DIM_CLI_TEST_API_KEY", "fixture-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(first.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "read completed\n"
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("[done] read"));
    assert!(!diagnostic.contains("fixture-secret"));
    let session = diagnostic
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap();
    let mut interactive = home.command();
    interactive.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--resume",
        session,
    ]);
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.write(b"!printf cli-recorded-marker\n");
    terminal.until("[shell recorded] sequence:");
    terminal.until("\n> ");
    terminal.write(b"!!printf cli-private-marker\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    let mut resumed = home.command();
    resumed
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "exec",
            "resume",
            session,
            "--prompt",
            "continue",
            "--image",
            image.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(resumed.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "session continued\n"
    );
    let mut interactive = home.command();
    interactive.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--resume",
        session,
    ]);
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.paste(&paste);
    terminal.until("[Paste 1: 2001 lines]");
    terminal.write(b"\n");
    terminal.until("paste read completed");
    terminal.editor();
    terminal.write(b"selected ");
    terminal.paste(image.to_str().unwrap());
    terminal.until("[Image 1: screenshot.dat]");
    terminal.paste(url::Url::from_file_path(&reference).unwrap().as_str());
    terminal.until("[File: outside reference.txt]");
    terminal.write(b"\n");
    // The referenced file is outside the Session workspace, so the read asks
    // (ADR 014 D2); the user answers the ordinary approval.
    terminal.until("Allow once? [y/N]");
    terminal.write(b"y\n");
    terminal.until("files read completed");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    let exported = home
        .command()
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "session",
            "export",
            session,
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let conversation: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(conversation["sessionId"], session);
    assert_eq!(conversation["branchId"], "main");
    assert!(conversation["events"].as_array().unwrap().iter().any(|event| {
        event["type"] == "input" && event["name"] == "message"
            && event["data"]["user"]["content"] == json!([
                {"type":"text","text":"continue"},
                {"type":"image","source":{"type":"base64","mediaType":"image/png","data":image_data}}
            ])
    }));
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["user"]["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| {
                            text.starts_with("[Pasted text: ") && text.ends_with(" (2001 lines)]")
                        })
            })
    );
    assert_eq!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<dim-shell-input ")))
            .count(),
        1
    );
    assert!(!conversation.to_string().contains("cli-private-marker"));
    assert!(conversation["events"].as_array().unwrap().iter().any(|event| event["type"] == "input" && event["name"] == "message" && event["data"]["user"]["content"] == json!([
        {"type":"text","text":"selected "},
        {"type":"image","source":{"type":"base64","mediaType":"image/png","data":image_data}},
        {"type":"text","text":reference.to_str().unwrap()}
    ])));
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["name"] == "text" && event["data"]["delta"] == "session continued")
    );
    assert!(
        !String::from_utf8(exported.stdout)
            .unwrap()
            .contains("fixture-secret")
    );
    model.join().unwrap();
}

fn mcp_catalog(id: &str) -> Value {
    json!({"userRevision":"user:1","projectRevision":"project:1",
        "servers":[{"id":id,"source":"project","transport":"http","enabled":true,
            "auth":{"status":"required","missingScopes":["files:read"]}}],
        "userApplication":{"error":null,"cleanupError":null},"projectApplication":null})
}

fn mcp_hello(wire: &mut Wire) {
    let hello = wire.read("hello");
    wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
}

fn mcp_started(wire: &mut Wire, redirect: &str, expired: bool) {
    let request = wire.read("mcp.auth.start");
    let expires = if expired {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 1200
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000
    };
    wire.reply(&request,json!({"attemptId":"fixture-attempt","authorizationUrl":"https://issuer.example/authorize?state=fixture","redirectUri":redirect,"issuer":"https://issuer.example","resource":"https://resource.example/mcp","expiresAt":expires}));
}

#[test]
fn mcp_sign_in_rejects_noninteractive_and_argument_secrets_before_connecting() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for args in [
        vec!["mcp", "login", "docs"],
        vec!["mcp", "login", "docs", "--client-secret=must-not-print"],
        vec!["mcp", "login", "docs", "--client-secret", "must-not-print"],
    ] {
        let output = home
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-print"));
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn mcp_catalog_and_sign_out_use_public_auth_data_and_the_selected_source() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        let mut catalog = mcp_catalog("docs");
        catalog["servers"].as_array_mut().unwrap().extend([
            json!({"id":"authorized","source":"user","transport":"sse","enabled":true,"auth":{"status":"authorized"}}),
            json!({"id":"expired","source":"user","transport":"http","enabled":true,"auth":{"status":"expired"}}),
        ]);
        wire.reply(&request,catalog);
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("sign-in required"));
    assert!(text.contains("signed in"));
    assert!(text.contains("sign-in expired"));
    assert!(text.contains("files:read"));
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, mcp_catalog("docs"));
        let logout = wire.read("mcp.auth.logout");
        assert_eq!(logout["params"]["source"], "project");
        assert_eq!(logout["params"]["id"], "docs");
        assert!(logout["params"]["cwd"].is_string());
        wire.reply(&logout, json!({"revoked":false}));
    });
    let output = home
        .command()
        .args(["mcp", "logout", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Revoked: false"));
}

/// An empty catalog reads as an empty catalog: one note, and no columns
/// describing rows that are not there.
#[test]
fn mcp_list_says_when_no_server_is_configured() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request,json!({"userRevision":"user:1","projectRevision":null,"servers":[],"userApplication":{},"projectApplication":null}));
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "No MCP server is configured.\n"
    );
}

#[test]
fn tui_mcp_says_when_no_server_is_configured() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,json!({"userRevision":"user:1","projectRevision":null,"servers":[],"userApplication":{},"projectApplication":null}));
        let state = wire.read("session.mcp");
        wire.reply(&state,json!({"sessionId":"session","servers":[],"catalogError":null,"closed":false}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/mcp list\r");
    terminal.until("No MCP server is configured.");
    assert!(
        !terminal.transcript.contains("Columns:"),
        "{}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The Session already knows why a server failed and what it refused;
/// `/mcp` shows those under the server's row, sanitized, and the catalog
/// error once. `auth_required` keeps its single sign-in hint.
#[test]
fn tui_mcp_shows_failure_reasons_rejections_and_the_catalog_error() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,json!({"userRevision":"user:1","projectRevision":null,"servers":[
            {"id":"docs","source":"project","transport":"http","enabled":true,"auth":{"status":"required"}},
            {"id":"team","source":"user","transport":"http","enabled":true}
        ],"userApplication":{},"projectApplication":null}));
        let state = wire.read("session.mcp");
        wire.reply(&state,json!({"sessionId":"session","servers":[
            {"id":"docs","state":"auth_required","message":"authorization required"},
            {"id":"team","state":"failed","message":"connection refused\u{1b}[31m"},
            {"id":"wiki","state":"ready","version":"v1","rejection":{"version":"v2","reason":"duplicate tool name"}}
        ],"catalogError":"project MCP configuration is not active","closed":false}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/mcp list\r");
    terminal.until("connection refused");
    terminal.until("Version v2 rejected: duplicate tool name");
    terminal.until("MCP catalog: project MCP configuration is not active");
    assert!(
        terminal
            .transcript
            .contains("Sign in with mcp login \"docs\""),
        "{}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("authorization required"),
        "the sign-in hint already says it: {}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("[31m"),
        "the failure message is sanitized: {}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// `/mcp enable` and `/mcp disable` send what `dimcode mcp enable` /
/// `disable` sends, report the same text, and the next `/mcp` lists the new
/// state.
#[test]
fn tui_mcp_enables_and_disables_a_project_server() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let catalog = |enabled: bool| {
            json!({"userRevision":"user:1","projectRevision":"project:1","servers":[
                {"id":"docs","source":"project","transport":"http","enabled":enabled}
            ],"userApplication":{},"projectApplication":null})
        };
        let session = json!({"sessionId":"session","servers":[],"catalogError":null,"closed":false});
        // `/mcp`: the project entry starts disabled.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(false));
        let state = wire.read("session.mcp");
        wire.reply(&state, session.clone());
        // `/mcp enable docs`
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(false));
        let set = wire.read("mcp.set_enabled");
        assert_eq!(
            (
                &set["params"]["id"],
                &set["params"]["source"],
                &set["params"]["expectedVersion"],
                &set["params"]["enabled"]
            ),
            (
                &json!("docs"),
                &json!("project"),
                &json!("project:1"),
                &json!(true)
            )
        );
        assert!(set["params"]["cwd"].is_string());
        wire.reply(&set, json!({}));
        // The next `/mcp` shows it enabled.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true));
        let state = wire.read("session.mcp");
        wire.reply(&state, session.clone());
        // `/mcp disable docs`
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true));
        let set = wire.read("mcp.set_enabled");
        assert_eq!(
            (&set["params"]["id"], &set["params"]["enabled"]),
            (&json!("docs"), &json!(false))
        );
        wire.reply(&set, json!({}));
        // The next `/mcp` shows it disabled again.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(false));
        let state = wire.read("session.mcp");
        wire.reply(&state, session);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/mcp list\r");
    terminal.until("disabled");
    terminal.write(b"/mcp enable docs\r");
    terminal.until("Enabled \"docs\" (project).");
    terminal.until(".mcp.json");
    terminal.write(b"/mcp list\r");
    terminal.until("enabled");
    terminal.write(b"/mcp disable docs\r");
    terminal.until("Disabled \"docs\" (project).");
    terminal.write(b"/mcp list\r");
    terminal.until("disabled");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The standalone `mcp enable` prints the entry and the source; the TUI's
/// `/mcp enable` shares that function.
#[test]
fn mcp_enable_reports_the_entry_and_the_source() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({"userRevision":"user:1","projectRevision":"project:1","servers":[
            {"id":"docs","source":"project","transport":"http","enabled":false}
        ],"userApplication":{},"projectApplication":null}),
        );
        let set = wire.read("mcp.set_enabled");
        assert_eq!(set["params"]["enabled"], json!(true));
        wire.reply(&set, json!({}));
    });
    let output = home
        .command()
        .args(["mcp", "enable", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Enabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(".mcp.json"));
}

#[test]
fn interactive_mcp_sign_in_hides_secrets_retries_callbacks_and_drains_before_next_command() {
    let home = Home::new();
    let exported = home.0.join("signed-in.json");
    let (release, finish) = std::sync::mpsc::channel();
    let (accepted, admitted) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let state = wire.read("session.mcp");
        wire.reply(&state,json!({"sessionId":"session","servers":[{"id":"docs team","state":"auth_required"}],"catalogError":null,"closed":false}));
        for other in ["connecting", "connected"] { wire.notice("session.mcp.state", json!({"sessionId":"session","servers":[{"id":"docs team","state":"auth_required"},{"id":"other","state":other}]})); }
        let invalid = wire.read("mcp.catalog");
        wire.reply(&invalid,mcp_catalog("docs team"));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let request = wire.read("mcp.auth.start");
        assert_eq!(request["params"]["source"],"project");
        assert_eq!(request["params"]["clientSecret"],"fixture-confidential-secret");
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 60_000;
        wire.reply(&request,json!({"attemptId":"fixture-attempt","authorizationUrl":"https://issuer.example/authorize","redirectUri":"http://localhost:1456/callback","resource":"https://resource.example/mcp","issuer":"https://issuer.example","expiresAt":expires}));
        let callback = wire.read("mcp.auth.complete");
        assert_eq!(callback["params"]["callbackUrl"],"http://localhost:1456/callback?code=wrong-code&state=wrong-state");
        wire.send(json!({"jsonrpc":"2.0","id":callback["id"],"error":{"code":-32000,"message":"invalid callback","data":{"appError":"mcp_auth_callback_invalid"}}}));
        let callback = wire.read("mcp.auth.complete");
        assert_eq!(callback["params"]["attemptId"],"fixture-attempt");
        assert_eq!(callback["params"]["callbackUrl"],"http://localhost:1456/callback?code=private-code&state=right-state");
        accepted.send(()).unwrap();
        finish.recv_timeout(PATIENCE).unwrap();
        wire.reply(&callback,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let logout = wire.read("mcp.auth.logout");
        wire.reply(&logout,json!({"revoked":true}));
        let submit=wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle=wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/mcp\n");
    terminal.until("auth_required");
    terminal.until("\n> ");
    terminal.write(b"/mcp login docs team --no-browser\n");
    terminal.until("Usage: /mcp login");
    terminal.until("\n> ");
    terminal.write(b"/mcp login --client-secret argument-secret docs\n");
    terminal.until("No MCP server has that id.");
    terminal.until("\n> ");
    terminal.write(b"/mcp login --no-browser --client-secret --source project docs team\n");
    terminal.until("Client secret (hidden): ");
    terminal.write(b"fixture-confidential-secret\n");
    terminal.until("Resource: https://resource.example/mcp");
    terminal.until("Authorization server: issuer.example");
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1456/callback?code=wrong-code&state=wrong-state\n");
    terminal.until("try again");
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1456/callback?code=private-code&state=right-state\n");
    admitted.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for the MCP operation to settle");
    release.send(()).unwrap();
    terminal.until("Signed in to");
    terminal.until("\n> ");
    terminal.write(format!("/export json {}\n", exported.display()).as_bytes());
    terminal.until("Exported JSON:");
    terminal.until("\n> ");
    terminal.write(b"/mcp logout docs team\n");
    terminal.until("Revoked: true");
    terminal.until("\n> ");
    terminal.write(b"continue after sign-out\n");
    terminal.until("sign-in required. Run mcp login");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let data: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
    assert_eq!(data["through"], 8);
    assert_eq!(data["events"], json!([]));
    assert!(!terminal.transcript.contains("fixture-confidential-secret"));
    assert_eq!(
        terminal.transcript.matches("argument-secret").count(),
        1,
        "only the terminal echo of the typed command may contain it"
    );
    assert_eq!(
        terminal
            .transcript
            .matches("sign-in required. Run mcp login")
            .count(),
        1
    );
    assert!(!terminal.transcript.contains("wrong-code"));
    assert!(!terminal.transcript.contains("private-code"));
}

/// A loopback port free on both families, for the CLI to bind. It is taken
/// below the ephemeral range: an ephemeral port released for the CLI can be
/// taken meanwhile by an outgoing connection elsewhere on the machine.
fn loopback_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = (std::process::id() % 8_000) as u16;
    (0..8_000)
        .map(|_| 20_000 + (base + NEXT.fetch_add(1, Ordering::Relaxed)) % 8_000)
        .find(|&port| {
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
                && std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).is_ok()
        })
        .expect("a free loopback port")
}

#[test]
fn mcp_loopback_capture_supports_both_families_and_busy_ports_fall_back_to_hidden_input() {
    for mode in ["ipv4", "ipv6", "busy", "paste", "noise", "cancel", "expiry"] {
        let home = Home::new();
        let _reserve = (mode == "busy")
            .then(|| std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap());
        let port = _reserve.as_ref().map_or_else(loopback_port, |listener| {
            listener.local_addr().unwrap().port()
        });
        let redirect = format!("http://localhost:{port}/callback");
        let expected = format!("{redirect}?code=private-callback&state=fixture");
        let daemon = home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog = wire.read("mcp.catalog");
            wire.reply(&catalog, mcp_catalog("docs"));
            mcp_started(wire, &redirect, mode == "expiry");
            if matches!(mode, "cancel" | "expiry") {
                assert_eq!(
                    wire.trailing_request(),
                    None,
                    "nothing follows the cancelled sign-in"
                );
                return;
            }
            let callback = wire.read("mcp.auth.complete");
            assert_eq!(callback["params"]["callbackUrl"], expected);
            wire.reply(&callback, json!({}));
        });
        let bin = home.0.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in ["open", "xdg-open"] {
            let path = bin.join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut command = home.command();
        command
            .args(["mcp", "login", "docs"])
            .env("PATH", &bin)
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY");
        let mut terminal = Terminal::launch(command);
        if matches!(mode, "cancel" | "expiry") {
            terminal.until("Callback URL: ");
            if mode == "cancel" {
                terminal.signal(libc::SIGINT);
            }
            terminal.until(if mode == "expiry" {
                "Sign-in expired"
            } else {
                "Stopped waiting"
            });
            assert_eq!(
                terminal.wait().code(),
                Some(if mode == "expiry" { 1 } else { 130 })
            );
            assert!(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
            assert!(std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).is_err());
            daemon.join().unwrap();
            continue;
        }
        if matches!(mode, "busy" | "paste") {
            if mode == "paste" {
                terminal.until("Waiting for the browser callback");
            }
            terminal.until("Callback URL: ");
            terminal.write(
                format!("http://localhost:{port}/callback?code=private-callback&state=fixture\n")
                    .as_bytes(),
            );
        } else {
            terminal.until("Callback URL: ");
            if mode == "noise" {
                let mut wrong =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                wrong.set_read_timeout(Some(PATIENCE)).unwrap();
                wrong
                    .write_all(b"GET /unrelated HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut response = String::new();
                wrong.read_to_string(&mut response).unwrap();
                assert!(response.contains("404 Not Found"));
                let mut oversized =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                oversized
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                oversized.write_all(&vec![b'x'; 16385]).unwrap();
                let mut response = Vec::new();
                let _ = oversized.read_to_end(&mut response);
                let mut idle =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                idle.set_read_timeout(Some(PATIENCE)).unwrap();
                assert_eq!(idle.read(&mut [0]).unwrap(), 0);
            }
            let mut browser = if mode == "ipv6" {
                std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).unwrap()
            } else {
                std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap()
            };
            browser.set_read_timeout(Some(PATIENCE)).unwrap();
            write!(browser,"GET /callback?code=private-callback&state=fixture HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
            let mut page = String::new();
            browser.read_to_string(&mut page).unwrap();
            assert!(page.contains("Sign-in completed"));
            assert!(!page.contains("private-callback"));
        }
        terminal.until("Signed in to");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(!terminal.transcript.contains("private-callback"));
    }
}

#[test]
fn mcp_sign_in_stops_on_interrupt_or_expiry_without_a_cleanup_request() {
    for expired in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog = wire.read("mcp.catalog");
            wire.reply(&catalog, mcp_catalog("docs"));
            mcp_started(wire, "http://localhost:1456/callback", expired);
            let mut line = String::new();
            assert_eq!(
                wire.0.read_line(&mut line).unwrap(),
                0,
                "no cancellation RPC exists for MCP attempts"
            );
        });
        let mut command = home.command();
        command.args(["mcp", "login", "--no-browser", "docs"]);
        let mut terminal = Terminal::launch(command);
        if !expired {
            terminal.until("Callback URL: ");
            terminal.signal(libc::SIGINT);
        }
        terminal.until(if expired {
            "Sign-in expired"
        } else {
            "Stopped waiting"
        });
        assert_eq!(terminal.wait().code(), Some(if expired { 1 } else { 130 }));
        daemon.join().unwrap();
    }
}

#[test]
fn mcp_submitted_success_stays_successful_after_interrupt_or_expiry() {
    for expires in [false, true] {
        let home = Home::new();
        let (admitted, accepted) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let daemon=home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog=wire.read("mcp.catalog"); wire.reply(&catalog,mcp_catalog("docs"));
            let start=wire.read("mcp.auth.start");
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            wire.reply(&start,json!({"attemptId":"held","authorizationUrl":"https://issuer.example/authorize","redirectUri":"http://localhost:1456/callback","resource":"https://resource.example/mcp","issuer":"https://issuer.example","expiresAt":now+if expires {1200} else {60_000}}));
            let callback=wire.read("mcp.auth.complete"); admitted.send(()).unwrap();
            wait_release.recv_timeout(PATIENCE).unwrap();
            wire.reply(&callback,json!({}));
        });
        let mut command = home.command();
        command.args(["mcp", "login", "--no-browser", "docs"]);
        let mut terminal = Terminal::launch(command);
        terminal.until("Callback URL: ");
        terminal.write(b"http://localhost:1456/callback?code=held-secret&state=held\n");
        accepted.recv_timeout(PATIENCE).unwrap();
        if expires {
            terminal.until("Sign-in expired; waiting for the submitted operation");
        } else {
            terminal.signal(libc::SIGINT);
            terminal.until("Waiting for the MCP operation to settle");
        }
        release.send(()).unwrap();
        terminal.until("Signed in to");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(!terminal.transcript.contains("held-secret"));
    }
}

#[test]
fn daemon_start_waits_for_private_permissions_without_trusting_existing_public_sockets() {
    let home = Home::new();
    let server = home.0.join("server");
    let started = home.0.join("server.started");
    fs::write(
        &server,
        "#!/bin/sh\n: > \"$0.started\"\nwhile test -f \"$0\"; do sleep 0.01; done\n",
    )
    .unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = home.command();
    command
        .arg("--server")
        .arg(&server)
        .args(["models", "fixture"]);

    let socket = home.0.join("rust/daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    let rejected = command.output().unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("private socket"));
    assert!(
        !started.exists(),
        "an existing insecure endpoint must not start a server"
    );
    drop(listener);
    fs::remove_file(&socket).unwrap();

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    while !started.exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "server was not started");
        thread::sleep(Duration::from_millis(10));
    }
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(
        child.try_wait().unwrap().is_none(),
        "startup must wait for a private endpoint"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (stream, _) = loop {
        match listener.accept() {
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            result => break result.unwrap(),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
    let mut wire = Wire::new(stream);
    let hello = wire.read("hello");
    wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
    let catalog = wire.read("provider.catalog");
    wire.reply(
        &catalog,
        json!({"catalog":{"version":1,"models":[],"disabledModelIds":[]}}),
    );
    let output = wait(child);
    fs::remove_file(&server).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn initial_attach_eof_settles_a_shell_started_before_the_first_prompt() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"restored","textOffset":0,"text":"initial model pending","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"initial shell pending"}}));
        let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false}));
        wire.reply(&stop, json!({"stopped":true}));
        wire.detach();
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("initial model pending");
    terminal.write(b"!!wait\n");
    terminal.until("initial shell pending");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interrupted_shell_reply_retains_its_run_for_explicit_stop_and_settlement_feedback() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for malformed in [false, true] {
            let run = wire.read("shell.run");
            let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8}));
            if malformed { wire.reply(&run, json!({"runId":"wrong","status":"completed","output":"","truncated":false})); }
            else { wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}})); }
            let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
            if !malformed { finish.recv_timeout(PATIENCE).unwrap(); }
            if !malformed {
                wire.send(json!({"jsonrpc":"2.0","id":stop["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}}));
                let retry=wire.read("shell.stop"); assert_ne!(retry["id"], stop["id"]); assert_eq!(retry["params"]["runId"], run["params"]["runId"]);
                wire.reply(&retry,json!({"stopped":false}));
            } else { wire.reply(&stop, json!({"stopped":false})); }
        }
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for malformed in [false, true] {
        terminal.write(b"!held\n");
        terminal.until("[shell interrupted] Run");
        terminal.signal(libc::SIGINT);
        terminal.until("[shell stopping] Waiting for command settlement");
        if !malformed {
            terminal.signal(libc::SIGINT);
            terminal.until("Still waiting for command settlement");
            release.send(()).unwrap();
            terminal.until("Stop was not confirmed");
            terminal.signal(libc::SIGINT);
            terminal.until("[shell stopping] Waiting for command settlement");
        }
        terminal.until("\n> ");
    }
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn approval_interrupt_stops_the_turn_instead_of_a_deferred_completed_shell() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"working","textOffset":0,"text":"model running","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        wire.reply(&run,json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"deferred output","truncated":false}));
        let stop=wire.read("session.stop");
        finish.recv_timeout(PATIENCE).unwrap();
        wire.notice("approval.cancelled",json!({"sessionId":"session","branchId":"main","requestId":"approval"}));
        wire.reply(&idle,json!({"sequence":40})); wire.reply(&stop,json!({"sequence":40}));
        let next = wire.read("session.submit");
        assert_eq!(next["params"]["text"], "after interrupts");
        wire.reply(&next,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model running");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("deferred output"));
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Waiting for resource settlement");
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Still waiting for resource settlement");
    release.send(()).unwrap();
    terminal.until("\n> ");
    terminal.write(b"after interrupts\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("[shell stopping]"));
    assert!(
        terminal.transcript.find("deferred output").unwrap()
            > terminal.transcript.find("[stopping]").unwrap()
    );
}

#[test]
fn web_prints_the_entry_and_holds_it_until_interrupted() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.open"]}));
        let open = wire.read("web.open");
        assert_eq!(open["params"], json!({"port": 4567}));
        wire.reply(&open, json!({"url":"http://127.0.0.1:4567/#token=fixture-token","address":"127.0.0.1:4567"}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command
        .args(["web", "--port", "4567", "--no-open"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut panel = String::new();
    for _ in 0..3 {
        stdout.read_line(&mut panel).unwrap();
    }
    assert_eq!(
        panel,
        "Web workspace  http://127.0.0.1:4567/\nOpen this link to sign in: http://127.0.0.1:4567/#token=fixture-token\nPress Ctrl+C to stop the Web workspace.\n"
    );
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "\nWeb workspace stopped.\n");
    let output = wait(child);
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn web_off_clears_the_daemon_switch() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.disable"]}));
        let disable = wire.read("web.disable");
        assert_eq!(disable["params"], json!({}));
        wire.reply(&disable, json!({"enabled":false,"open":false,"connections":0}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home.command().args(["web", "--off"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Web switch off.\n");
    daemon.join().unwrap();
}

#[test]
fn web_without_the_web_workspace_says_how_to_get_it() {
    let source = "Set DIMCODE_WEB_ASSETS to a frontend built from this source, or build the App Server with the embedded-web feature";
    let cases = [
        (
            false,
            "web_assets_missing",
            format!("The Web workspace is not installed.\n{source}.\n"),
        ),
        (
            true,
            "web_assets_missing",
            "The Web workspace was installed after the daemon started.\nRestart the daemon with `dimcode daemon stop`, then try again.\n".to_owned(),
        ),
        (
            false,
            "web_assets_incompatible",
            format!(
                "The installed Web workspace does not fit dimcode {}-source.\n{source}.\n",
                env!("CARGO_PKG_VERSION")
            ),
        ),
    ];
    for (installed, error, expected) in cases {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.open"]}));
            let open = wire.read("web.open");
            wire.send(json!({"jsonrpc":"2.0","id":open["id"],"error":{"code":-32000,"message":"This App Server has no Web workspace","data":{"appError":error}}}));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        command
            .args(["web", "--no-open"])
            .env_remove("DIMCODE_WEB_ASSETS");
        if installed {
            let assets = home.0.join("npm-web");
            fs::create_dir_all(&assets).unwrap();
            fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
            command.env("DIMCODE_WEB_ASSETS", assets);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(output.stdout.is_empty());
        assert_eq!(stderr, expected);
    }
}

/// A registry that records the requests it receives; its base URL.
fn recording_registry() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut wire = BufReader::new(stream.unwrap());
            let mut request = String::new();
            wire.read_line(&mut request).unwrap();
            recorded.lock().unwrap().push(request);
        }
    });
    (base, requests)
}

#[test]
fn web_install_refuses_for_a_build_from_source() {
    let home = Home::new();
    let (registry, requests) = recording_registry();
    let output = home
        .command()
        .args(["web", "install"])
        .env("DIMCODE_NPM_REGISTRY", registry)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("A build from source has no published Web workspace"),
        "{stderr}"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "a build from source sent a registry request"
    );
}

#[test]
fn tui_web_toggles_the_daemon_switch_without_leaving() {
    for manual in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.toggle"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let toggle = wire.read("web.toggle");
        assert_eq!(toggle["params"], json!({}));
        wire.reply(
            &toggle,
            json!({"enabled":true,"open":true,"url":"http://127.0.0.1:4567/#token=fixture-token"}),
        );
        let toggle = wire.read("web.toggle");
        wire.reply(&toggle, json!({"enabled":false,"open":false}));
        let toggle = wire.read("web.toggle");
        wire.send(json!({"jsonrpc":"2.0","id":toggle["id"],"error":{"code":-32000,"message":"This App Server has no Web workspace","data":{"appError":"web_assets_missing"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
        let bin = home.0.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in ["open", "xdg-open"] {
            let path = bin.join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut command = home.command();
        command
            .env("PATH", &bin)
            .env("DIMCODE_OAUTH_MANUAL", if manual { "1" } else { "0" })
            .env_remove("DIMCODE_WEB_ASSETS");
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        terminal.write(b"/we");
        terminal.until("turn the web interface on or off");
        terminal.write(b"\r");
        terminal.until("Web interface on: http://127.0.0.1:4567/chat/session");
        if manual {
            terminal.until("#token=fixture-token");
        }
        terminal.write(b"/web\r");
        terminal.until("Web interface off");
        if !manual {
            assert!(!terminal.transcript.contains("fixture-token"));
        }
        terminal.write(b"/web\r");
        terminal.until("The Web workspace is not installed. Set DIMCODE_WEB_ASSETS");
        assert!(!terminal.transcript.contains("RPC"));
        terminal.write(b"/exit\r");
        let status = terminal.wait();
        daemon.join().unwrap();
        assert!(status.success());
    }
}

#[test]
fn tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wait_release.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"keep this draft");
    terminal.until("keep this draft");
    release.send(()).unwrap();
    daemon.join().unwrap();
    terminal.until("Error: App Server connection closed");
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_eq!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
    terminal.until("dimcode --resume session");
    terminal.write(b"\r");
    terminal.until("Disconnected; draft kept");
    assert!(!home.0.join("input-history.json").exists());
    terminal.write(b"\x04");
    terminal.until("Bye!");
    terminal.until("\x1b[?2004l");
    let status = terminal.wait();
    assert!(status.success());
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_ne!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
}

/// Provider edits use the source-built daemon, actual HTTP and a disposable home.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn tui_provider_management_uses_the_production_daemon_and_survives_restart() {
    fn rpc(wire: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        wire.get_mut().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                wire.read_line(&mut line).unwrap() > 0,
                "{method}: connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == 1 {
                return value;
            }
        }
    }
    fn connect(home: &Home) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut wire = BufReader::new(stream);
        let hello = rpc(
            &mut wire,
            "hello",
            json!({"protocolVersion":3,"clientKind":"cli"}),
        );
        assert!(hello.get("result").is_some(), "{hello}");
        wire
    }
    fn post(events: &std::sync::mpsc::Receiver<(String, Value)>, marker: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let (key, body) = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if body.to_string().contains(marker) {
                assert!(matches!(
                    key.as_str(),
                    "Bearer first-key" | "Bearer second-key"
                ));
                assert_eq!(body["reasoning_effort"], "high");
                return json!({"key":key,"body":body});
            }
        }
    }
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (send, events) = std::sync::mpsc::channel();
    let http = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            let mut wire = BufReader::new(stream);
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            let request_line = line.clone();
            let mut length = 0;
            let mut key = String::new();
            loop {
                line.clear();
                wire.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if name.eq_ignore_ascii_case("authorization") {
                    key = value.trim().to_owned();
                }
                if name.eq_ignore_ascii_case("x-api-key") {
                    key = format!("x-api-key {}", value.trim());
                }
            }
            let mut bytes = vec![0; length];
            wire.read_exact(&mut bytes).unwrap();
            let stop = request_line.starts_with("GET /stop ");
            let (content_type, body) = if stop {
                ("text/plain", "done".to_owned())
            } else if request_line.starts_with("GET /v1/models ") {
                assert!(matches!(
                    key.as_str(),
                    "Bearer first-key" | "Bearer second-key"
                ));
                ("application/json", json!({"data":[{"id":"fixture-model","context_window":32000,"max_output_tokens":256}]}).to_string())
            } else if request_line.starts_with("POST /v1/messages ") {
                assert_eq!(key, "x-api-key first-key");
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let model = body["model"].clone();
                send.send((key, body)).unwrap();
                let frames = [
                    json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"builtin-provider-reply"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
                    json!({"type":"message_stop"}),
                ];
                (
                    "text/event-stream",
                    frames
                        .into_iter()
                        .map(|frame| {
                            format!(
                                "event: {}\ndata: {frame}\n\n",
                                frame["type"].as_str().unwrap()
                            )
                        })
                        .collect::<String>(),
                )
            } else {
                assert!(
                    request_line.starts_with("POST /v1/chat/completions "),
                    "{request_line}"
                );
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                send.send((key.clone(), body)).unwrap();
                let text = if key == "Bearer first-key" {
                    "provider-first-reply"
                } else {
                    "provider-updated-reply"
                };
                (
                    "text/event-stream",
                    format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"id":"provider-flow","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}),
                        json!({"id":"provider-flow","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
                    ),
                )
            };
            write!(wire.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            wire.get_mut().flush().unwrap();
            if stop {
                break;
            }
        }
    });
    let command = || {
        let mut command = home.command();
        command.args(["--server", &binary, "--cwd", home.0.to_str().unwrap()]);
        command
    };
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Sign in with /login");
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Add custom provider\r");
    terminal.until("Name:");
    terminal.write(format!("Provider flow\rhttp://{address}/v1\rfirst-key\r\r").as_bytes());
    terminal.until("Connect ");
    terminal.until("Disconnect");
    terminal.until("Added Provider flow");
    let mut wire = connect(&home);
    let snapshot = rpc(&mut wire, "provider.management", json!({}));
    let provider = snapshot["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["displayName"] == "Provider flow")
        .unwrap();
    let id = provider["providerId"].as_str().unwrap().to_owned();
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().to_string(), id);
    assert_eq!(provider["version"], 1);
    assert_eq!(provider["models"][0]["model"]["modelId"], "fixture-model");
    assert_eq!(provider["models"][0]["source"], "default");
    assert!(!snapshot.to_string().contains("first-key"));
    assert!(rpc(&mut wire, "provider.models.edit", json!({"providerId":id,"expectedVersion":1,"modelId":"fixture-model","edit":{"reasoning":{"supported":true,"effortOptions":["low","high"]}}})).get("result").is_some());
    assert!(rpc(&mut wire, "provider.preferences.update", json!({"providerId":id,"expectedVersion":2,"modelId":"fixture-model","patch":{"reasoningEffort":{"action":"set","value":"high"}}})).get("result").is_some());
    assert!(
        rpc(
            &mut wire,
            "provider.select",
            json!({"providerId":id,"modelId":"fixture-model"})
        )
        .get("result")
        .is_some()
    );
    home.kill(&terminal);
    terminal.wait();
    let selected = || {
        let mut command = command();
        command.args(["--provider", &id, "--model", "fixture-model"]);
        command
    };
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"first-provider-message\r");
    terminal.until("provider-first-reply");
    assert_eq!(
        post(&events, "first-provider-message")["key"],
        "Bearer first-key"
    );
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Provider flow\r");
    terminal.until("Connect ");
    assert!(
        rpc(
            &mut wire,
            "provider.update",
            json!({"providerId":id,"expectedVersion":3,
        "patch":{"network":{"timeoutMs":{"action":"set","value":9000}}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"second-key\r\r");
    terminal.until("Provider changed");
    terminal.write(b"\r");
    terminal.until("Connected Provider flow");
    let updated = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    assert_eq!(updated["result"]["providers"][0]["version"], 5);
    assert_eq!(
        updated["result"]["providers"][0]["preferences"]["fixture-model"]["reasoningEffort"],
        "high"
    );
    home.kill(&terminal);
    terminal.wait();
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"updated-provider-message\r");
    terminal.until("provider-updated-reply");
    assert_eq!(
        post(&events, "updated-provider-message")["key"],
        "Bearer second-key"
    );
    home.kill(&terminal);
    terminal.wait();
    assert!(
        rpc(&mut wire, "daemon.shutdown", json!({}))
            .get("result")
            .is_some()
    );
    drop(wire);
    let deadline = Instant::now() + Duration::from_secs(8);
    while home.0.join("rust/daemon.sock").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"restored-provider-message\r");
    terminal.until("provider-updated-reply");
    assert_eq!(
        post(&events, "restored-provider-message")["key"],
        "Bearer second-key"
    );
    let mut wire = connect(&home);
    assert!(
        rpc(
            &mut wire,
            "provider.models.edit",
            json!({"providerId":id,"expectedVersion":5,
        "modelId":"fixture-model","edit":{"reasoning":{"supported":true,"effortOptions":["low"]}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"/models\r");
    terminal.until("Provider flow /");
    terminal.write(b"Provider flow\r");
    terminal.until("Model adoption with saved preferences");
    let retained = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    assert_eq!(
        retained["result"]["providers"][0]["preferences"]["fixture-model"]["reasoningEffort"],
        "high"
    );
    terminal.write(b"/models\r");
    terminal.until("Provider flow /");
    terminal.write(b"Provider flow");
    assert!(
        rpc(
            &mut wire,
            "provider.update",
            json!({"providerId":id,"expectedVersion":6,
        "patch":{"network":{"timeoutMs":{"action":"set","value":10000}}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"\x18");
    terminal.until("Provider version changed");
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["result"]["providers"][0]["preferences"]
            ["fixture-model"]["reasoningEffort"],
        "high"
    );
    terminal.write(b"\x18");
    terminal.until("Saved model preferences cleared");
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["result"]["providers"][0]["preferences"],
        json!({})
    );
    terminal.write(b"\r");
    terminal.until("Ask anything...");
    let output = home
        .command()
        .args(["models", &id, "--clear-preferences", "fixture-model"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Saved model preferences cleared"));
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Provider flow\r");
    terminal.until("Connect ");
    terminal.write(b"\r\r");
    terminal.until("Disconnected Provider flow");
    home.kill(&terminal);
    terminal.wait();
    let disconnected = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    let provider = &disconnected["result"]["providers"][0];
    assert_eq!(provider["enabled"], false);
    assert_eq!(provider["connection"]["credentialConfigured"], false);
    assert_eq!(provider["version"], 10);
    assert!(
        rpc(
            &mut wire,
            "provider.delete",
            json!({"providerId":id,"expectedVersion":10})
        )
        .get("result")
        .is_some()
    );
    assert_eq!(
        rpc(&mut wire, "provider.selection", json!({}))["result"]["selection"],
        Value::Null
    );
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["error"]["data"]["appError"],
        "provider_not_found"
    );
    assert!(
        rpc(
            &mut wire,
            "session.create",
            json!({"cwd":home.0,"model":{"provider":id,"modelId":"fixture-model"}})
        )
        .get("error")
        .is_some()
    );
    let anonymous_id = "018ec73f-4d19-7f61-bc79-7130618bb455";
    let created = rpc(
        &mut wire,
        "provider.create",
        json!({"providerId":anonymous_id,"displayName":"Anonymous fixture","connection":{"driverKind":"openai-compatible","auth":{"type":"anonymous"},"baseUrl":format!("http://{address}/v1")},"models":[{"modelId":"anonymous-fixture-model","displayName":"Anonymous fixture model","status":"active","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true}),
    );
    assert!(created.get("result").is_some(), "{created}");
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Ask anything...");
    terminal.write(b"/models\r");
    terminal.until("Anonymous fixture /");
    home.kill(&terminal);
    terminal.wait();
    assert!(
        rpc(
            &mut wire,
            "provider.delete",
            json!({"providerId":anonymous_id,"expectedVersion":1})
        )
        .get("result")
        .is_some()
    );
    let before = rpc(
        &mut wire,
        "provider.management",
        json!({"providerId":"kimi-for-coding"}),
    );
    let version = before["result"]["providers"][0]["version"]
        .as_u64()
        .unwrap();
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Sign in with /login");
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Kimi For Coding\r");
    terminal.until("Connect kimi-for-coding");
    terminal.write(b"\x1b[A\x15");
    terminal.write(format!("http://{address}/v1\rfirst-key\r\r").as_bytes());
    terminal.until("Connected Kimi For Coding; saved without testing");
    let builtin = rpc(
        &mut wire,
        "provider.management",
        json!({"providerId":"kimi-for-coding"}),
    );
    let builtin = &builtin["result"]["providers"][0];
    assert_eq!(builtin["version"], version + 1);
    assert_eq!(builtin["enabled"], true);
    assert_eq!(builtin["connection"]["credentialConfigured"], true);
    assert_eq!(
        builtin["connection"]["baseUrl"],
        format!("http://{address}/v1")
    );
    let builtin_model = builtin["models"][0]["model"]["modelId"].as_str().unwrap();
    home.kill(&terminal);
    terminal.wait();
    let mut builtin_command = command();
    builtin_command.args(["--provider", "kimi-for-coding", "--model", builtin_model]);
    let mut terminal = Terminal::launch_tui(builtin_command);
    terminal.until("Ask anything...");
    terminal.write(b"builtin-without-listing-message\r");
    terminal.until("builtin-provider-reply");
    loop {
        let (key, body) = events.recv_timeout(Duration::from_secs(8)).unwrap();
        if body.to_string().contains("builtin-without-listing-message") {
            assert_eq!(key, "x-api-key first-key");
            assert_eq!(body["model"], builtin_model);
            break;
        }
    }
    home.kill(&terminal);
    terminal.wait();
    drop(wire);
    let mut stop = std::net::TcpStream::connect(address).unwrap();
    write!(stop, "GET /stop HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut response = String::new();
    stop.read_to_string(&mut response).unwrap();
    http.join().unwrap();
}

/// The daemon's UI registry (Tui design §3.6): the interface registers
/// after attaching, detaches from a Session it leaves, and leaves with its
/// reason on screen when the daemon asks it to; the Session is not closed.
#[test]
fn tui_registers_with_the_ui_registry_and_leaves_when_asked() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "session.create.modelFromSession",
            "ui.open",
            "ui.close",
            "ui.attach",
            "ui.status",
            "ui.shutdown",
            "ui.replace",
        ]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let register = wire.read("ui.attach");
        assert_eq!(
            register["params"],
            json!({"sessionId":"session","kind":"tui"})
        );
        wire.reply(&register, json!({"uiId":"ui-1"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let create = wire.read("session.create");
        assert_eq!(create["params"]["modelFromSession"], "session");
        wire.reply(&create, json!({"sessionId":"next","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"session"}));
        wire.reply(&detach, json!({}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"next"}));
        wire.reply(&attach, attach_record(&attach));
        let register = wire.read("ui.attach");
        assert_eq!(register["params"], json!({"sessionId":"next","kind":"tui"}));
        wire.reply(&register, json!({"uiId":"ui-2"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "next");
        wire.reply(&submit, json!({"event":{}}));
        wire.no_context();
        wire.notice("ui.shutdown", json!({"uiId":"ui-1","reason":"not you"}));
        wire.notice(
            "ui.shutdown",
            json!({"uiId":"ui-2","reason":"another terminal took over"}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    // `/new` goes home; the message starts the Session the UI moves to.
    terminal.write(b"/new\r");
    terminal.until("─ new session ─");
    terminal.write(b"hello\r");
    terminal.until("The App Server closed this interface: another terminal took over");
    terminal.until("Bye!");
    terminal.until("\x1b[?2004l");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(
        !terminal.transcript.contains("not you"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn tui_queue_shows_held_messages_and_steers_or_withdraws_them() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.submit");
        assert_eq!(first["params"]["text"], "first");
        wire.reply(&first, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"first"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"child","running":true}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"child","running":false}));
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"child","requestId":"child-request","textOffset":0,"text":"child-only answer","thinkingOffset":0,"thinking":"child-only reasoning"}));
        let child_history = wire.read("session.history");
        assert_eq!(child_history["params"]["sessionId"], "session");
        assert_eq!(child_history["params"]["branchId"], "child");
        wire.reply(&child_history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"note-1","sequence":11,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"background task finished"}]},"mode":"append"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-note","sequence":12,"create_at":1,"type":"input","name":"applied","data":{"input_id":"note-1","mode":"append"}}}));
        let second = wire.read("session.submit");
        assert_eq!(second["params"]["text"], "second");
        wire.reply(&second, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-2","sequence":13,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"second"}]},"mode":"queue"}}}));
        let steer = wire.read("session.input.withdraw");
        assert_eq!(steer["params"]["sessionId"], "session");
        assert_eq!(steer["params"]["branchId"], "main");
        assert_eq!(steer["params"]["inputId"], "in-2");
        assert_eq!(steer["params"]["steer"], json!({"content":[{"type":"text","text":"second"}]}));
        wire.reply(&steer, json!({"status":"deleted","sequence":14,"steer":{"inputId":"in-3","sequence":15}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-2","sequence":14,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"in-2"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-3","sequence":15,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"second"}]},"mode":"steer"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-3","sequence":16,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-3","mode":"steer"}}}));
        let third = wire.read("session.submit");
        assert_eq!(third["params"]["text"], "third");
        wire.reply(&third, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-4","sequence":17,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"third"}]},"mode":"queue"}}}));
        let withdraw = wire.read("session.input.withdraw");
        assert_eq!(withdraw["params"]["inputId"], "in-4");
        assert!(withdraw["params"].get("steer").is_none());
        wire.reply(&withdraw, json!({"status":"deleted","sequence":18}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-4","sequence":18,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"in-4"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"first\r");
    terminal.until("first");
    terminal.until("background task finished");
    terminal.write(b"second\r");
    terminal.until("Queue (1)");
    assert!(
        !terminal.transcript.contains("Queue (0)"),
        "{}",
        terminal.transcript
    );
    terminal.until("› second");
    terminal.until("working…");
    assert!(
        !terminal.transcript.contains("┃ second"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"\x13");
    terminal.write(b"third\r");
    terminal.until("Queue (1)");
    terminal.until("› third");
    terminal.write(b"\x1b[1;2D");
    terminal.until("third");
    let plain = {
        let mut out = String::new();
        let mut rest = terminal.transcript.as_str();
        while let Some(start) = rest.find('\u{1b}') {
            out.push_str(&rest[..start]);
            let tail = &rest[start + 1..];
            let skipped = match tail.chars().next() {
                Some('[') => tail
                    .char_indices()
                    .skip(1)
                    .find(|(_, c)| c.is_ascii_alphabetic())
                    .map_or(tail.len(), |(i, c)| i + c.len_utf8()),
                Some(']') => [("\u{7}", 1), ("\u{1b}\\", 2)]
                    .iter()
                    .filter_map(|(end, len)| tail.find(end).map(|i| i + len))
                    .min()
                    .unwrap_or(tail.len()),
                Some(c) => c.len_utf8(),
                None => 0,
            };
            rest = &tail[skipped..];
        }
        out.push_str(rest);
        out
    };
    assert!(!plain.contains("Queue (0)"), "{plain}");
    assert!(plain.contains("┃ first"), "{plain}");
    assert!(plain.contains("┃ second"), "{plain}");
    assert!(!plain.contains("┃ third"), "{plain}");
    assert!(
        !plain.contains("child-only answer")
            && !plain.contains("child-only reasoning")
            && !plain.contains("Thinking…")
            && !plain.contains("thinking…"),
        "{plain}"
    );
    assert!(
        !plain.contains("already left the queue") && !plain.contains("not available"),
        "{plain}"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_ctrl_s_steers_the_draft_while_a_run_is_in_progress() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.submit");
        assert_eq!(first["params"]["text"], "first");
        wire.reply(&first, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"first"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        let second = wire.read("session.submit");
        assert_eq!(second["params"]["text"], "second");
        assert_eq!(second["params"]["mode"], "steer");
        wire.reply(&second, json!({"event":{}}));
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"first\r");
    terminal.until("first");
    terminal.until("working…");
    terminal.write(b"second");
    terminal.write(b"\x13");
    daemon.join().unwrap();
    home.kill(&terminal);
    terminal.wait();
}

#[test]
fn tui_escape_withdraws_the_queue_before_the_run_stops() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.submit");
        assert_eq!(first["params"]["text"], "first");
        wire.reply(&first, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"first"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        let queued = wire.read("session.submit");
        assert_eq!(queued["params"]["text"], "second");
        assert_eq!(queued["params"]["mode"], "queue");
        wire.reply(&queued, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-2","sequence":11,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"second"}]},"mode":"queue"}}}));
        let withdraw = wire.read("session.input.withdraw");
        assert_eq!(withdraw["params"]["inputId"], "in-2");
        assert!(withdraw["params"].get("steer").is_none());
        wire.reply(&withdraw, json!({"status":"deleted","sequence":12}));
        let stop = wire.read("session.stop");
        wire.reply(&stop, json!({"sequence":13}));
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"first\r");
    terminal.until("first");
    terminal.until("working…");
    terminal.write(b"second\r");
    terminal.until("Queue (1)");
    terminal.write(b"\x1b");
    daemon.join().unwrap();
    home.kill(&terminal);
    terminal.wait();
}

#[test]
fn tui_command_output_refreshes_one_read_and_preserves_its_branch_and_terminal_result() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"output-input","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"output-adopted","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"output-input"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"child","event":{"id":"child-start","sequence":1,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"child-call","task_id":"child-task","description":"cargo cx build","output_path":"/daemon/private/output"}}}));
        let child_history = wire.read("session.history");
        assert_eq!(child_history["params"]["branchId"], "child");
        wire.reply(&child_history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        let expected = json!({"sessionId":"session","branchId":"child","taskId":"child-task"});
        let first = wire.read("session.exec.output");
        assert_eq!(first["params"], expected);
        wire.reply(&first, json!({"text":""}));
        let refreshed = wire.read("session.exec.output");
        assert_eq!(refreshed["params"], expected);
        wire.reply(&refreshed, json!({"text":"first \u{1b}]52;c;must-not-copy\u{7}live preview"}));
        let held = wire.read("session.exec.output");
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        control.recv_timeout(PATIENCE).unwrap();
        wire.reply(&held, json!({"text":"late reply after close"}));
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        let reopened = wire.read("session.exec.output");
        assert_eq!(reopened["params"], expected);
        wire.reply(&reopened, json!({"text":format!("retained preview\n{}\nretained tail", "long output ".repeat(20))}));
        let unavailable = wire.read("session.exec.output");
        wire.send(json!({"jsonrpc":"2.0","id":unavailable["id"],"error":{"code":-32000,"message":"No live output is available for this task","data":{"appError":"exec_task_unknown"}}}));
        let stop = wire.read("session.exec.stop");
        assert_eq!(stop["params"], expected);
        wire.reply(&stop, json!({"task_id":"child-task","status":"completed","result":{"content":"final child result"}}));
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        control.recv_timeout(PATIENCE).unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"child","event":{"id":"child-end","sequence":2,"create_at":1,"type":"exec","name":"ended","data":{"task_id":"child-task","status":"completed","result":{"content":"final child result"}}}}));
        let continued = wire.read("session.submit");
        assert_eq!(continued["params"]["text"], "continue after output");
        wire.reply(&continued, json!({"event":{}}));
        wire.event(11, "text", json!({"turn_id":"t","request_id":"r","delta":"main remains usable"}));
        control.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"go\r");
    terminal.until("1 in background");
    terminal.write(b"/tasks\r");
    terminal.until("Background tasks");
    terminal.until("cargo cx build");
    terminal.write(b"cx");
    terminal.until("> cx");
    terminal.write(b"\r");
    terminal.until("No output yet");
    terminal.until("first live preview");
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    proceed.send(()).unwrap();
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\r");
    terminal.until("retained preview");
    terminal.until("Live output unavailable");
    let size = libc::winsize {
        ws_row: 18,
        ws_col: 64,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.until("Live output unavailable");
    terminal.until("retained tail");
    terminal.write(b"x");
    readiness.recv_timeout(PATIENCE).unwrap();
    assert!(
        !terminal.transcript.contains("Completed"),
        "unknown capture or stop reply fabricated an end"
    );
    proceed.send(()).unwrap();
    terminal.until("Completed");
    terminal.until("final child result");
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    terminal.write(b"\r");
    terminal.until("final child result");
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    terminal.write(b"\x1b[27u\x1b[27ucontinue after output\r");
    terminal.until("main remains usable");
    assert!(!terminal.transcript.contains("must-not-copy"));
    proceed.send(()).unwrap();
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_command_output_errors_stop_refresh_and_reopening_retries() {
    for (app_error, expected) in [
        ("unknown_method", "Live output unavailable in this profile"),
        ("service_reloading", "Output read failed"),
    ] {
        let home = Home::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"error-input","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"running","sequence":10,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"call","task_id":"task","description":"command with unavailable output","output_path":"/daemon/private/output"}}}));
            let read = wire.read("session.exec.output");
            wire.reply(&read, json!({"text":"last known preview"}));
            let read = wire.read("session.exec.output");
            wire.send(json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":if app_error == "unknown_method" { -32601 } else { -32000 },"message":"read unavailable","data":{"appError":app_error}}}));
            wire.quiet(Duration::from_millis(1200));
            ready.send(()).unwrap();
            let retried = wire.read("session.exec.output");
            wire.reply(&retried, json!({"text":"fresh preview after reopening"}));
        });
        let mut terminal = Terminal::launch_tui(home.command());
        terminal.until("test / model");
        terminal.write(b"go\r");
        terminal.until("1 in background");
        terminal.write(b"/tasks\r");
        terminal.until("command with unavailable output");
        terminal.write(b"\r");
        terminal.until("last known preview");
        terminal.until(expected);
        readiness.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"\x1b[27u");
        terminal.until("Background tasks");
        terminal.write(b"\r");
        terminal.until("fresh preview after reopening");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_tasks_panel_stops_the_selected_command_or_member() {
    let home = Home::new();
    let (deliver, delivery) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        wire.event(10, "tool-use", json!({"turn_id":"t","request_id":"r","call":{"id":"call-1","name":"exec","input":{"command":"sleep 100","runInBackground":true}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ex-1","sequence":11,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"call-1","task_id":"task-1","description":"sleep 100","output_path":"/tmp/out"}}}));
        wire.event(12, "tool-result", json!({"turn_id":"t","request_id":"r","results":[{"call_id":"call-1","result":{"content":"Command moved to the background (task task-1)."}}]}));
        wire.event(13, "tool-use", json!({"turn_id":"t","request_id":"r2","call":{"id":"call-2","name":"agent","input":{"action":"request","prompt":"Survey the crates\nand report","agentId":"reviewer"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-1","sequence":14,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r2","call_id":"call-2","target":{"kind":"agent","agent_id":"reviewer"},"prompt":"Survey the crates\nand report"}}}));
        wire.event(15, "tool-result", json!({"turn_id":"t","request_id":"r2","results":[{"call_id":"call-2","result":{"content":"{\"status\":\"accepted\"}"}}]}));
        let past_observations = |wire: &mut Wire, method: &str| loop {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("session.attach") => {}
                Some("agent.list") => wire.reply(
                    &request,
                    json!([{"agentId":"reviewer","name":"reviewer"},{"agentId":"writer","name":"writer"}]),
                ),
                _ => {
                    assert_eq!(request["method"], method);
                    break request;
                }
            }
        };
        let stop = past_observations(wire, "session.exec.stop");
        assert_eq!(stop["params"], json!({"sessionId":"session","branchId":"main","taskId":"task-1"}));
        wire.reply(&stop, json!({}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ex-2","sequence":16,"create_at":1,"type":"exec","name":"ended","data":{"task_id":"task-1","status":"cancelled","result":{"content":"stopped by the user"}}}}));
        delivery.recv_timeout(PATIENCE).unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-1","sequence":17,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-1","status":"ok","agent_id":"reviewer","session_id":"member","input_event_id":"member-input"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-failed","sequence":18,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r3","call_id":"call-3","target":{"kind":"agent","agent_id":"missing"},"prompt":"Unavailable delegate"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-failed","sequence":19,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-failed","status":"error","error":"agent_not_found"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-other","sequence":20,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r4","call_id":"call-4","target":{"kind":"agent","agent_id":"writer"},"prompt":"Check the docs"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-other","sequence":21,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-other","status":"ok","agent_id":"writer","session_id":"other-member","input_event_id":"other-input"}}}));
        let stop = past_observations(wire, "session.stop");
        assert_eq!(stop["params"], json!({"sessionId":"member","branchId":"main","reason":"stopped by the user"}));
        wire.reply(&stop, json!({}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"res-1","sequence":22,"create_at":1,"type":"collaboration","name":"result","data":{"request_event_id":"req-1","agent_id":"reviewer","session_id":"member","text":"","status":"cancelled","reason":"stopped by the user"}}}));
        let submit = past_observations(wire, "session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"res-other","sequence":23,"create_at":1,"type":"collaboration","name":"result","data":{"request_event_id":"req-other","agent_id":"writer","session_id":"other-member","text":"Other agent completed normally.","status":"completed"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"go\r");
    terminal.until("Agent: reviewer");
    terminal.write(b"/tasks\r");
    terminal.until("Background tasks");
    terminal.until("sleep 100");
    terminal.until("Survey the crates");
    terminal.write(b"x");
    terminal.until("Cancelled");
    terminal.write(b"\x1b[27u/tasks\r");
    terminal.until("Survey the crates");
    terminal.write(b"\r");
    terminal.until("Agent is starting; no member Session to stop yet");
    deliver.send(()).unwrap();
    terminal.until("Check the docs");
    terminal.write(b"x");
    terminal.until("agent cancelled: stopped by the user");
    terminal.write(b"\x1b[27ucontinue the leader\r");
    terminal.until("agent result: Other agent completed normally.");
    terminal.write(b"/tasks\r");
    terminal.until("Check the docs");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_permissions_keep_pending_file_and_member_questions_and_save_command_rules() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "session.permission.get",
            "session.permission.update",
            "permission.rules",
            "permission.set_rules",
        ]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let read = wire.read("session.permission.get");
        assert_eq!(read["params"], json!({"sessionId":"session","branchId":"main"}));
        wire.reply(&read, json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}));
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"], json!({"sessionId":"session","branchId":"main","expectedVersion":1,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}));
        wire.reply(&update, json!({"version":2}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"perm-1","sequence":9,"create_at":1,"type":"permission","name":"changed","data":{"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}}}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"req-1","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"npm test"}},"reason":"shell command"}));
        let rules = wire.read("permission.rules");
        wire.reply(&rules, json!({"revision":"r1","commandRules":[]}));
        let set = wire.read("permission.set_rules");
        assert_eq!(set["params"], json!({"expectedRevision":"r1","commandRules":[{"command":"npm test","match":"exact","decision":"allow"}]}));
        wire.reply(&set, json!({"revision":"r2"}));
        let respond = wire.read("approval.respond");
        assert_eq!(respond["params"], json!({"sessionId":"session","branchId":"review","requestId":"req-1","decision":"allow"}));
        wire.reply(&respond, json!({}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"req-2","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"npm run lint"}},"reason":"shell command"}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"req-3","source":{"kind":"model"},"call":{"name":"write","input":{"path":"notes.md"}},"reason":"file write"}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"member","call":{"name":"member_approval_allow","input":{"approvalId":"member-route"}},"reason":"Member requires explicit review"}));
        let detail = wire.read("session.collaboration.approval.get");
        let mut allowed = Vec::new();
        for _ in 0..2 {
            let respond = wire.read("approval.respond");
            assert_eq!(respond["params"]["decision"], "deny");
            allowed.push(respond["params"]["requestId"].as_str().unwrap().to_owned());
            wire.reply(&respond, json!({}));
        }
        assert_eq!(allowed, ["req-2", "req-3"]);
        wire.quiet(Duration::from_millis(200));
        let original = json!({"state":"pending","approvalId":"member-route","descriptor":{"delegationId":"delegation","member":{"agentId":"reviewer","sessionId":"member-session","branchId":"main"},"requestId":"original","call":{"name":"exec","input":{"command":"explicit member action"}},"reason":"Member process request"}});
        wire.reply(&detail, original.clone());
        let refresh = wire.read("session.collaboration.approval.get");
        wire.reply(&refresh, original);
        let response = wire.read("approval.respond");
        assert_eq!(response["params"], json!({"sessionId":"session","branchId":"review","requestId":"member","decision":"allow"}));
        wire.reply(&response, json!({}));
        wire.quiet(Duration::from_millis(200));

        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-9","sequence":10,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"both allowed"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-9-applied","sequence":11,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-9","mode":"queue"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/approvals\r");
    terminal.until("Full access");
    terminal.write(b"\x1b[B\r");
    terminal.until("Permission: full-access (also for new sessions)");
    terminal.until("Permission required");
    terminal.until("Allow always");
    terminal.write(b"a");
    terminal.until("Rule saved: allow exact `npm test`");
    terminal.until("npm run lint");
    terminal.write(b"n");
    terminal.until("file write");
    terminal.write(b"n");
    terminal.until("explicit member action");
    terminal.write(b"y");
    terminal.until("both allowed");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let config: Value =
        serde_json::from_str(&fs::read_to_string(home.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["permission"], "full-access");
    assert!(config.get("permissionPreset").is_none());
    let mut saved = config;
    saved["permissionPreset"] = json!("read-only");
    fs::write(
        home.0.join("config.json"),
        serde_json::to_string(&saved).unwrap(),
    )
    .unwrap();
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.permission.get", "session.permission.update"]);
        let create = wire.read("session.create");
        assert_eq!(create["params"]["permissionPolicy"], json!({"preset":"full-access"}));
        wire.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":1,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Full access");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_approval_alias_selects_presets_and_remembers_them() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.permission.get", "session.permission.update"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let read = wire.read("session.permission.get");
        assert_eq!(
            read["params"],
            json!({"sessionId":"session","branchId":"main"})
        );
        wire.reply(
            &read,
            json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let read = wire.read("session.permission.get");
        wire.reply(
            &read,
            json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}),
        );
        let update = wire.read("session.permission.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main","expectedVersion":1,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}})
        );
        wire.reply(&update, json!({"version":2}));
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main","event":{"id":"perm-1","sequence":9,"create_at":1,"type":"permission","name":"changed","data":{"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}}}),
        );
        let read = wire.read("session.permission.get");
        wire.reply(
            &read,
            json!({"version":2,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        );
        let update = wire.read("session.permission.update");
        assert_eq!(
            update["params"]["policy"]["preset"],
            json!("workspace-write")
        );

        wire.reply(&update, json!({"version":3}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/approval\r");
    terminal.until("Full access");
    terminal.write(b"\x1b[B\r");
    terminal.until("Permission: full-access (also for new sessions)");
    terminal.write(b"/approval\r");
    terminal.until("Full access");
    terminal.write(b"\x1b[A\r");
    terminal.until("Permission: workspace-write");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let config: Value =
        serde_json::from_str(&fs::read_to_string(home.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["permission"], "workspace-write");
    assert!(config.get("permissionPreset").is_none());
}

/// The terminal's program status (OSC 7501) follows a run through a
/// permission request and a question to its result, and the record is
/// cleared on `/exit`, in both interface modes. So does the progress bar (OSC 9;4) where it is
/// drawn, which is repeated while it shows and removed on `/exit`.
#[test]
fn tui_reports_the_run_state_to_the_terminal_and_clears_it_on_exit() {
    let report = |pairs: &str| format!("\x1b]7501;{pairs}\x1b\\");
    let (busy, paused, removed) = ("\x1b]9;4;3\x07", "\x1b]9;4;4;100\x07", "\x1b]9;4;0\x07");
    for (fullscreen, drawn) in [(false, true), (true, true), (false, false)] {
        let home = Home::new();
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "inspect");
            wire.reply(&submit, json!({"event":{}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"inspect"}]},"mode":"queue"}}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
            wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
            wait_shown.recv_timeout(PATIENCE).unwrap();
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"ls"}},"reason":"shell command"}));
            let respond = wire.read("approval.respond");
            assert_eq!(respond["params"]["decision"], "allow");
            wire.reply(&respond, json!({}));
            wire.event(11, "tool-call", json!({"turn_id":"turn","request_id":"ask","calls":[{"id":"c1","name":"ask_user","input":{"questions":[{"question":"Where to deploy?","options":[{"label":"production"},{"label":"staging"}]}]}}]}));
            let answer = wire.read("session.question.respond");
            assert_eq!(answer["params"], json!({"sessionId":"session","branchId":"main","requestId":"ask","callId":"c1","answers":[{"questionId":"1","selected":["production"]}]}));
            wire.reply(&answer, json!({}));
            wire.event(12, "tool-result", json!({"turn_id":"turn","request_id":"ask","call_event_id":"event-11","results":[{"call_id":"c1","result":{"content":"production"}}]}));
            wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
            wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":false}));
            wire.no_context();
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        command.env("DIM_TERMINAL_PROGRESS", if drawn { "on" } else { "off" });
        let mut terminal = Terminal::launch_tui(command);
        terminal.until(&report("state=idle:app=dim"));
        terminal.until("test / model");
        terminal.write(b"inspect\r");
        terminal.until(&report("state=working:app=dim"));
        if drawn {
            terminal.until(busy);
        }
        shown.send(()).unwrap();
        terminal.until(&report(
            "state=blocked:kind=permission:app=dim:msg=UGVybWlzc2lvbiBuZWVkZWQ6IGV4ZWM=",
        ));
        if drawn {
            terminal.until(paused);
        }
        terminal.until("Permission required");
        if drawn {
            terminal.until(paused);
        }
        terminal.write(b"y");
        terminal.until(&report("state=blocked:kind=question:app=dim"));
        terminal.until("Where to deploy?");
        terminal.write(b"1\r");
        terminal.until(&report("state=done:app=dim"));
        if drawn {
            terminal.until(removed);
        }
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        if drawn {
            terminal.until(removed);
        }
        terminal.until(&report("state=clear"));
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        let transcript = &terminal.transcript;
        assert_eq!(transcript.contains("\x1b[?1049h"), fullscreen);
        for pairs in ["state=idle:app=dim", "state=done:app=dim", "state=clear"] {
            assert_eq!(
                transcript.matches(&report(pairs)).count(),
                1,
                "{fullscreen} {pairs}: {transcript}"
            );
        }
        assert_eq!(
            transcript.rfind("\x1b]7501;"),
            transcript.rfind(&report("state=clear")),
            "{fullscreen}: {transcript}"
        );
        if drawn {
            assert_eq!(
                transcript.matches(removed).count(),
                2,
                "{fullscreen}: {transcript}"
            );
            assert_eq!(
                transcript.rfind("\x1b]9;4;"),
                transcript.rfind(removed),
                "{fullscreen}: {transcript}"
            );
        } else {
            assert!(!transcript.contains("\x1b]9;4;"), "{transcript}");
        }
    }

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env("DIM_TERMINAL_STATUS", "off");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(
        !terminal.transcript.contains("\x1b]7501;"),
        "{}",
        terminal.transcript
    );

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until(&report("state=idle:app=dim"));
    terminal.until("test / model");
    terminal.signal(libc::SIGTERM);
    terminal.until(&report("state=clear"));
    terminal.wait();
    daemon.join().unwrap();
}

/// The composer's status corner shows the main branch's context use against
/// its window once attached, and reads it again when a run on main ends, in
/// both interface modes.
#[test]
fn tui_composer_shows_context_use_and_refreshes_it_when_a_run_ends() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.context.usage"]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let usage = |input: f64| {
                json!({"contextWindow":256000.0,"outputTokens":32000.0,
                "thresholdTokens":200000.0,"estimatedInputTokens":input,
                "baselineSource":"estimate"})
            };
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => {
                        assert_eq!(
                            request["params"],
                            json!({"sessionId":"session","branchId":"main"})
                        );
                        wire.reply(&request, usage(1849.0));
                    }
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            wire.notice(
                "session.state",
                json!({"sessionId":"session","branchId":"main","running":true}),
            );
            wire.notice(
                "session.state",
                json!({"sessionId":"session","branchId":"main","running":false}),
            );
            let refresh = wire.read("session.context.usage");
            wire.reply(&refresh, usage(64000.0));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                assert!(
                    !line.contains("session.context.usage"),
                    "extra read: {line}"
                );
                line.clear();
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("1.8K / 256K");
        shown.send(()).unwrap();
        terminal.until("64K / 256K");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// The compaction events (§4): one row that runs until its result, then the
/// summary behind the same disclosure a tool group uses.
#[test]
fn tui_compaction_row_runs_then_shows_the_summary_it_recorded() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let (started, wait_started) = std::sync::mpsc::channel();
    let (more, wait_more) = std::sync::mpsc::channel();
    let (fail, wait_fail) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        // The composer's first frame is out before the row is sent: `until`
        // only searches what follows its last match, and a row painted above
        // the status line in the same frame would be behind that cursor.
        // The wait carries no deadline of its own: the main thread's `until`
        // deadlines bound the flow, and a test that fails drops the sender.
        wait_shown.recv().unwrap();
        let budget = json!({"inputTokens":130969.0,"contextWindow":258400.0,
            "requestedMaxTokens":128000.0,"allowedMaxTokens":127431.0,"ratio":1.0});
        wire.event(11, "model-output-end", json!({"turn_id":"turn","request_id":"budget-check",
            "status":"failed","error":"Model input exhausts its context window.","budget":budget}));
        wire.no_context();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction-start","sequence":12,"create_at":1,"type":"context","name":"compaction-start",
             "data":{"through":11,"trigger":"provider_output_budget_exhausted","strategy":"summary"}}}));
        wait_started.recv().unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction","sequence":13,"create_at":1,"type":"context","name":"compaction",
             "data":{"through":11,"start":12,"status":"completed","retain_from":"dim-2","strategy":"summary",
                     "replacement":[dim_agent_capabilities::history::summary_message("The parser is half done.")],
                     "diagnostics":{"reason":"auto","trigger":"provider_output_budget_exhausted","estimatedInputTokens":300.0,
                                    "entriesReplaced":3,"modelCallCount":1,"durationMs":10,"strategy":{"kind":"summary"}}}}}));
        wire.no_context();
        wait_more.recv().unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction-start-2","sequence":14,"create_at":1,"type":"context","name":"compaction-start",
             "data":{"through":11,"trigger":"manual","strategy":"summary"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction-2","sequence":15,"create_at":1,"type":"context","name":"compaction",
             "data":{"through":11,"start":14,"status":"completed","retain_from":"dim-2","strategy":"summary",
                     "replacement":[dim_agent_capabilities::history::summary_message("The cache lock moved into a RwLock."),
                                    {"role":"user","content":"<dim-digest>\nwrote src/cache.rs\n</dim-digest>"},
                                    {"role":"user","content":"Keep the parser race in mind."}],
                     "replacement_parts":{"strategy":1,"digest":1},
                     "diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":300.0,
                                    "entriesReplaced":3,"modelCallCount":1,"durationMs":10,"strategy":{"kind":"summary"}}}}}));
        wire.no_context();
        // A record whose parts cannot meet its bound: the header names them.
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction-start-3","sequence":16,"create_at":1,"type":"context","name":"compaction-start",
             "data":{"through":11,"trigger":"manual","strategy":"summary"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction-3","sequence":17,"create_at":1,"type":"context","name":"compaction",
             "data":{"through":11,"start":16,"status":"completed","retain_from":"dim-2","strategy":"summary",
                     "replacement":[{"role":"user","content":"<dim-summary>\nTrimmed."}],
                     "diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":300.0,
                                    "estimatedOutputTokens":6400.0,"entriesReplaced":3,"modelCallCount":1,
                                    "durationMs":10,"strategy":{"kind":"summary"},
                                    "insufficientSpace":{"parts":[
                                        {"part":"summary","tokens":3200.0},
                                        {"part":"pinnedInputs","tokens":1100.0}]}}}}}));
        wire.no_context();
        wait_fail.recv().unwrap();
        wire.event(18, "model-output-end", json!({"turn_id":"turn","request_id":"recovery",
            "status":"failed","error":"Context recovery could not fit the request.","budget":budget}));
        wire.end(json!({"turn_id":"turn","status":"failed","error":"Context recovery could not fit the request."}));
        wire.no_context();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.until("Compacting context…");
    started.send(()).unwrap();
    terminal.until("Context compacted ▸");
    terminal.write(b"/tools\r");
    terminal.until("Context compacted ▸");
    terminal.until("test / model");
    assert!(
        !terminal.transcript.contains("The parser is half done."),
        "summary tool form keeps the compaction body closed"
    );
    terminal.write(b"/tools\r");
    terminal.until("The parser is half done.");
    terminal.write(b"/tools\r");
    terminal.until("Context compacted ▸");
    terminal.write(b"/tools\r");
    terminal.until("Context compacted ▸");
    terminal.write(b"/tools\r");
    terminal.until("The parser is half done.");
    more.send(()).unwrap();
    terminal.until("The cache lock moved into a RwLock.");
    terminal.until("insufficient space: summary 3.2K, pinned inputs 1.1K");
    fail.send(()).unwrap();
    terminal.until("turn failed: Context recovery could not fit the request.");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    assert!(
        !terminal.transcript.contains("model failed:"),
        "{}",
        terminal.transcript
    );
    assert!(
        !terminal
            .transcript
            .contains("Model input exhausts its context window."),
        "{}",
        terminal.transcript
    );
    daemon.join().unwrap();
    for hidden in [
        "<dim-summary>",
        "</dim-summary>",
        "Background from earlier parts",
        "End of recorded background.",
        "<dim-digest>",
        "Tool effects",
        "wrote src/cache.rs",
        "Kept verbatim",
        "Keep the parser race in mind.",
    ] {
        assert!(!terminal.transcript.contains(hidden), "{hidden}");
    }
}

#[test]
fn tui_compact_displays_committed_outcomes_and_reports_an_unsupported_daemon() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        for attempt in 0..3 {
            let compact = wire.read("session.context.compact");
            assert_eq!(
                compact["params"],
                json!({"sessionId":"session","branchId":"main"})
            );
            match attempt {
                0 => {
                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
                        {"id":"dim-compaction-start-completed","sequence":11,"create_at":1,"type":"context","name":"compaction-start",
                         "data":{"through":10,"trigger":"manual","strategy":"summary"}}}));
                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
                        {"id":"dim-compaction-completed","sequence":12,"create_at":1,"type":"context","name":"compaction",
                         "data":{"through":10,"start":11,"status":"completed","retain_from":"dim-2","strategy":"summary",
                                 "replacement":[{"role":"user","content":"<dim-summary>\nThe parser is half done.\n</dim-summary>"}],
                                 "diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":300.0,
                                                "entriesReplaced":3,"modelCallCount":1,"durationMs":10,"strategy":{"kind":"summary"}}}}}));
                    wire.reply(&compact, json!({"status":"completed","sequence":12}));
                    wire.no_context();
                }
                1 => {
                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
                        {"id":"dim-compaction-start","sequence":14,"create_at":1,"type":"context","name":"compaction-start",
                         "data":{"through":13,"trigger":"manual","strategy":"summary"}}}));
                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":
                        {"id":"dim-compaction","sequence":15,"create_at":1,"type":"context","name":"compaction",
                         "data":{"through":13,"start":14,"status":"skipped","reason":"nothing to cut",
                                 "diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":120.0,
                                                "entriesReplaced":0,"modelCallCount":0,"durationMs":1,"strategy":{"kind":"summary"}}}}}));
                    wire.reply(&compact, json!({"status":"skipped","reason":"nothing to cut"}));
                    wire.no_context();
                }
                _ => wire.send(json!({"jsonrpc":"2.0","id":compact["id"],"error":
                    {"code":-32601,"message":"Unknown method: session.context.compact","data":{"appError":"unknown_method"}}})),
            }
        }
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/compact\r");
    terminal.until("Context compacted");
    terminal.write(b"/compact\r");
    terminal.until("Context compaction skipped · nothing to cut");
    terminal.write(b"/compact\r");
    terminal.until("This daemon does not support context compaction");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// The gauge warns at 90% of the threshold the server returned — not below —
/// and a compaction result outside a run reads the gauge again (§7.5).
#[test]
fn tui_context_warning_starts_at_the_threshold_share_and_a_compaction_refreshes_it() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.context.usage"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let usage = |input: f64| {
            json!({"contextWindow":256000.0,"outputTokens":32000.0,
                "thresholdTokens":200000.0,"estimatedInputTokens":input,
                "baselineSource":"estimate"})
        };
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => {
                    assert_eq!(
                        request["params"],
                        json!({"sessionId":"session","branchId":"main"})
                    );
                    wire.reply(&request, usage(179_999.0));
                }
                method => panic!("unexpected request {method:?}"),
            }
        }
        // No deadline of its own; the main thread's `until` bounds the flow.
        wait_shown.recv().unwrap();
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main","event":
            {"id":"dim-compaction","sequence":12,"create_at":1,"type":"context","name":"compaction",
             "data":{"through":11,"status":"failed","error":"the summarizer returned nothing"}}}),
        );
        let refresh = wire.read("session.context.usage");
        wire.reply(&refresh, usage(180_000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(
                !line.contains("session.context.usage"),
                "extra read: {line}"
            );
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("179.9K / 256K");
    assert!(
        !terminal
            .transcript
            .contains("near the compaction threshold"),
        "{}",
        terminal.transcript
    );
    shown.send(()).unwrap();
    terminal.until("the summarizer returned nothing");
    terminal.until("180K / 256K · near the compaction threshold");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Reads until the CLI closes the connection, failing on any catalog read.
fn no_catalog(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        assert!(
            !line.contains("session.summaries"),
            "the open read the Session catalog: {line}"
        );
        line.clear();
    }
}

/// #556: a cold hosted TUI opens the Session it created without reading the
/// catalog, and the first message does not wait for it.
#[test]
fn cold_hosted_tui_opens_without_reading_the_session_catalog() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"session"}));
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        no_catalog(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// #556: a resume by id takes the model from the attach record, so opening
/// the Session reads no other Session.
#[test]
fn resume_by_id_takes_its_model_from_the_attach_record() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"session"}));
        wire.reply(&attach, json!({"sessionId":"session","cwd":"/workspace",
            "model":{"providerId":"resumed","modelId":"record"},"createdAt":1,
            "running":false,"sequence":8,"attachedClients":1}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("resumed / record");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A resume by id against a server whose attach reply has no record finds
/// the model in the catalog and still becomes ready.
#[test]
fn resume_by_id_finds_the_model_in_the_catalog_when_the_attach_reply_is_empty() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"session"}));
        wire.reply(&attach, json!({}));
        let summaries = wire.read("session.summaries");
        wire.reply(
            &summaries,
            json!([{"sessionId":"other","cwd":"/workspace",
                    "model":{"providerId":"other","modelId":"other"},"updatedAt":2},
                   {"sessionId":"session","cwd":"/workspace",
                    "model":{"providerId":"catalog","modelId":"fallback"},"updatedAt":1}]),
        );
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("catalog / fallback");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A resumed session rebuilds its compaction rows from history.
#[test]
fn tui_rebuilds_compaction_rows_from_history_on_resume() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[
                {"id":"dim-compaction-start","sequence":3,"create_at":1,"type":"context","name":"compaction-start",
                 "data":{"through":2,"trigger":"threshold","strategy":"summary"}},
                {"id":"dim-compaction","sequence":4,"create_at":1,"type":"context","name":"compaction",
                 "data":{"through":2,"start":3,"status":"completed","retain_from":"dim-1","strategy":"summary",
                         "replacement":[dim_agent_capabilities::history::summary_message("Restored summary text.")],
                         "diagnostics":{"reason":"auto","trigger":"threshold","estimatedInputTokens":300.0,
                                        "entriesReplaced":3,"modelCallCount":1,"durationMs":10,"strategy":{"kind":"summary"}}}}],
                "through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        // Replaying the restored compaction marks the reading stale again.
        wire.no_context();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Context compacted ▸");
    terminal.until("test / model");
    terminal.write(b"/tools\r");
    terminal.until("Context compacted ▸");
    terminal.until("test / model");
    assert!(
        !terminal.transcript.contains("Restored summary text."),
        "summary tool form keeps the restored compaction body closed"
    );
    terminal.write(b"/tools\r");
    terminal.until("Restored summary text.");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("<dim-summary>"));
    assert!(!terminal.transcript.contains("</dim-summary>"));
    assert!(
        !terminal
            .transcript
            .contains("Background from earlier parts")
    );
    assert!(!terminal.transcript.contains("End of recorded background."));
}

/// `dimcode export` (Timeline export design §7, §13 item 6): the sibling
/// App Server's read-only export, live and after stop, without a daemon.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn export_writes_the_timeline_without_starting_a_daemon() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let _daemon = OwnedDaemon(socket.clone());
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .args(args)
            .current_dir(&home.0)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let exec = run(&[
        "--model-config",
        config.to_str().unwrap(),
        "exec",
        "-p",
        "hello",
    ]);
    let stderr = String::from_utf8(exec.stderr).unwrap();
    let session = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap_or_else(|| panic!("{stderr}"))
        .to_owned();
    let lines = |text: &[u8]| -> Vec<Value> {
        std::str::from_utf8(text)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };

    let live = run(&["export", &session]);
    assert!(
        live.status.success(),
        "{}",
        String::from_utf8_lossy(&live.stderr)
    );
    assert!(live.stderr.is_empty());
    let exported = lines(&live.stdout);
    assert_eq!(exported[0]["kind"], "header");
    assert_eq!(exported[0]["session_id"], session.as_str());
    assert_eq!(exported[0]["sensitive"], true);
    assert_eq!(exported.last().unwrap()["kind"], "end");
    let output = home.0.join("timeline.jsonl");
    let written = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert!(written.status.success());
    assert!(written.stdout.is_empty());
    assert!(String::from_utf8_lossy(&written.stderr).contains("Exported Timeline (sensitive): "));
    assert_eq!(fs::read(&output).unwrap(), live.stdout);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let refused = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(fs::read(&output).unwrap(), live.stdout);
    let failed = run(&["export", "missing-session", "--output", "failed.jsonl"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Session missing-session is not in"));
    assert!(!home.0.join("failed.jsonl").exists());

    assert!(run(&["daemon", "stop"]).status.success());
    assert!(!socket.exists());
    let stopped = run(&["export", &session, "--database", "rust/dimcode.sqlite"]);
    assert!(stopped.status.success());
    let stopped = lines(&stopped.stdout);
    assert_eq!(stopped[0], exported[0]);
    let events = |lines: &[Value]| -> Vec<Value> {
        lines
            .iter()
            .filter(|line| line["kind"] == "event")
            .cloned()
            .collect()
    };
    let (before, after) = (events(&exported), events(&stopped));
    assert!(!before.is_empty());
    assert_eq!(after[..before.len()], before[..]);
    assert!(!socket.exists());
}

#[test]
fn tui_provider_create_recovery_preserves_changed_draft_until_confirmation() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.management","provider.create","provider.update","provider.probe"]}));
        let create = wire.read("session.create");
        wire.reply(&create, json!({"sessionId":"session","sequence":0,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        wire.no_context();
        let accounts = wire.read("account.providers");
        wire.reply(&accounts, json!([]));
        for _ in 0..2 {
            let listed = wire.read("provider.management");
            wire.reply(&listed, json!({"providers":[],"errors":[]}));
        }
        let probe = wire.read("provider.probe");
        wire.reply(&probe, json!({"models":[]}));
        let create = wire.read("provider.create");
        let id = create["params"]["providerId"].clone();
        let mut entry = json!({"providerId":id,"source":"custom","displayName":"Recovered provider","enabled":true,"version":1,"preferences":{},"connection":{"driverKind":"openai-compatible","auth":{"type":"bearer"},"baseUrl":"https://first.example/v1","credentialConfigured":true}});
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"Create reply lost"}}));
        let listed = wire.read("provider.management");
        wire.send(json!({"jsonrpc":"2.0","id":listed["id"],"error":{"code":-32000,"message":"Snapshot unavailable"}}));
        for _ in 0..2 {
            let listed = wire.read("provider.management");
            wire.reply(&listed, json!({"providers":[entry],"errors":[]}));
        }
        let probe = wire.read("provider.probe");
        assert_eq!(probe["params"]["providerId"], id);
        assert_eq!(probe["params"]["connection"], json!({"baseUrl":"https://second.example/v1","credential":{"action":"set","value":"second-key"}}));
        wire.reply(&probe, json!({"models":[]}));
        let update = wire.read("provider.update");
        assert_eq!(update["params"]["providerId"], id);
        assert_eq!(update["params"]["expectedVersion"], 1);
        assert_eq!(update["params"]["patch"], probe["params"]["connection"]);
        entry["version"] = json!(2);
        entry["connection"]["baseUrl"] = json!("https://second.example/v1");
        entry["modelsListed"] = json!("listed");
        wire.reply(&update, entry.clone());
        // The update stored the listing itself; the CLI never asks again.
        let listed = wire.read("provider.management");
        wire.reply(&listed, json!({"providers":[entry],"errors":[]}));
        wait_release.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Ask anything...");
    terminal.write(b"/connect\r");
    terminal.until("Add custom provider");
    terminal.write(b"\rRecovered provider\rhttps://first.example/v1\rfirst-key\r\r");
    terminal.until("Snapshot unavailable");
    terminal.write(b"\x1b[A\x15second-key\x1b[A\x15https://second.example/v1\x1b[B\x1b[B\r");
    terminal.until("Provider changed; review the retained draft");
    terminal.write(b"\r");
    terminal.until("Connected Recovered provider; listed its models");
    terminal.write(b"\x1b\x1b");
    terminal.until("Ask anything...");
    terminal.write(b"/exit\r");
    let status = terminal.wait();
    release.send(()).unwrap();
    daemon.join().unwrap();
    assert!(status.success());
}

/// `/models` edits the subagent model policy through its own management
/// methods only: a stale save rereads instead of overwriting, cancel writes
/// nothing, and neither the Session's model nor the provider default moves.
#[test]
fn tui_models_picker_edits_only_the_subagent_model_policy() {
    let home = Home::new();
    let (calls, observed) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        let mut revision = 1;
        let mut policy = json!({"mode":"agent"});
        let mut conflicted = false;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            calls.send(request.clone()).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","collaboration.model_policy",
                    "collaboration.set_model_policy"]}),
                "session.create" => json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
                "session.attach" => attach_record(&request),
                "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                "provider.catalog" => json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                    {"id":"model","name":"Model","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                    {"id":"other","name":"Other","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}]}}),
                "account.providers" => json!([]),
                "collaboration.model_policy" => json!({"revision":revision.to_string(),"policy":policy}),
                "collaboration.set_model_policy" => {
                    if !conflicted {
                        conflicted = true;
                        revision += 1;
                    }
                    if request["params"]["expectedRevision"].as_str() != Some(&*revision.to_string()) {
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,
                            "message":"conflict","data":{"appError":"collaboration_model_policy_conflict"}}}));
                        continue;
                    }
                    revision += 1;
                    policy = request["params"]["policy"].clone();
                    json!({"revision":revision.to_string()})
                }
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/models\r");
    terminal.until("Subagent models: Agent choice");
    terminal.write(b"Subagent\r");
    terminal.until("Inherit requesting branch");
    terminal.until("Existing members keep their models.");
    terminal.raw(b"\x1b[B\r");
    terminal.until("Subagent model policy changed elsewhere");
    terminal.write(b"\r");
    thread::sleep(Duration::from_millis(300));
    terminal.write(b"\r");
    terminal.until("Subagent models: Inherit requesting branch");
    terminal.until("Subagent model policy saved");
    terminal.write(b"\r");
    terminal.until("Fixed model");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(300));
    terminal.write(b"\r");
    terminal.until("Fixed model");
    terminal.raw(b"\x1b[B\r");
    terminal.until("Fixed subagent model");
    terminal.write(b"other\r");
    terminal.until("Subagent models: Test / other");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let calls: Vec<Value> = observed.try_iter().collect();
    let methods: Vec<&str> = calls
        .iter()
        .map(|call| call["method"].as_str().unwrap())
        .collect();
    for forbidden in [
        "session.model.update",
        "provider.select",
        "session.agent.update",
    ] {
        assert!(!methods.contains(&forbidden), "{methods:?}");
    }
    assert_eq!(
        methods.iter().filter(|m| **m == "session.create").count(),
        1
    );
    let saves: Vec<&Value> = calls
        .iter()
        .filter(|call| call["method"] == "collaboration.set_model_policy")
        .map(|call| &call["params"])
        .collect();
    assert_eq!(
        saves,
        [
            &json!({"expectedRevision":"1","policy":{"mode":"inherit"}}),
            &json!({"expectedRevision":"2","policy":{"mode":"inherit"}}),
            &json!({"expectedRevision":"3","policy":{"mode":"fixed","model":{"providerId":"test","modelId":"other"}}}),
        ]
    );
}

/// `/models` lists a Provider's models without offering `Edit models` when
/// the daemon does not advertise the area's method.
#[test]
fn tui_models_picker_hides_edit_models_without_management_methods() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","provider.selection","provider.select"]}),
                "session.create" => json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
                "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                "provider.catalog" => json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                    {"id":"model","name":"Model","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}]}}),
                "account.providers" => json!([]),
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/models\r");
    terminal.until("Test / model");
    thread::sleep(Duration::from_millis(300));
    let opened = terminal.transcript.clone();
    assert!(opened.contains("Model"), "{opened}");
    assert!(!opened.contains("Edit models"), "{opened}");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// ←/→ in `/models` cycles advertised reasoning efforts, including `none`
/// when the model can turn reasoning off. Enter saves the cycled effort and
/// adopts the model with that effort plus the other saved preferences.
#[test]
fn tui_models_picker_cycles_reasoning_effort() {
    let home = Home::new();
    let (calls, observed) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        let mut capabilities =
            serde_json::to_value(dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)).unwrap();
        capabilities["reasoning"] = json!(true);
        let catalog = json!({"catalog":{"version":4,"disabledModelIds":[],"models":[{
            "id":"model","name":"Model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effort":"low",
                "effortOptions":["low","high"],
                "off":{"kind":"thinking_type","value":"disabled"}}}
        }]}});
        let mut version = 4;
        let mut preferences = json!({"reasoningEffort":"low","serviceTier":"priority"});
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            calls.send(request.clone()).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","provider.management","provider.selection","provider.select"]}),
                "session.create" => json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
                "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                "provider.catalog" => catalog.clone(),
                "account.providers" => json!([]),
                "provider.management" => json!({"providers":[{"providerId":"test","source":"custom","displayName":"Test","enabled":true,"version":version,"connection":null,"preferences":{"model":preferences.clone()}}],"errors":[]}),
                "provider.preferences.update" => {
                    version += 1;
                    preferences = json!({"reasoningEffort":request["params"]["patch"]["reasoningEffort"]["value"],"serviceTier":"priority"});
                    json!({})
                }
                "session.model.update" => json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}),
                "provider.selection" => json!({"selection":{"version":1}}),
                "provider.select" => json!({}),
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/models\r");
    terminal.until("‹ low ›");
    terminal.raw(b"\x1b[D");
    terminal.until("‹ none ›");
    terminal.write(b"\r");
    terminal.until("Ask anything...");
    let mut calls = Vec::new();
    loop {
        let call = observed
            .recv_timeout(HANG_GUARD)
            .expect("the model was adopted");
        let adopted = call["method"] == "session.model.update";
        calls.push(call);
        if adopted {
            break;
        }
    }
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let update = calls
        .iter()
        .find(|call| call["method"] == "provider.preferences.update")
        .expect("reasoning effort was saved");
    assert_eq!(
        update["params"],
        json!({"providerId":"test","modelId":"model","expectedVersion":4,
            "patch":{"reasoningEffort":{"action":"set","value":"none"}}})
    );
    let adopted = calls
        .iter()
        .find(|call| call["method"] == "session.model.update")
        .expect("the model was adopted");
    assert_eq!(
        adopted["params"],
        json!({"sessionId":"session","branchId":"main","providerId":"test","modelId":"model",
            "modelPreferences":{"reasoningEffort":"none","serviceTier":"priority"}})
    );
}

/// `/models` sends `provider.refresh` for a usable provider whose catalog is
/// empty when the daemon says its models can be listed, draws its rows without
/// waiting for that listing, shows the models a late listing returns in the
/// same open picker, keeps the row of a refresh that fails, and asks an
/// unchanged catalog once in a connection while a written record asks once
/// more.
#[test]
fn tui_models_picker_refills_an_empty_provider_catalog() {
    let home = Home::new();
    let refreshed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = refreshed.clone();
    let listed = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let counted = listed.clone();
    let stored = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        String,
        Value,
    >::new()));
    let kept = stored.clone();
    let changed = stored.clone();
    let (release, wait_release) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        // One writer at a time: the listing a provider is slow to give is
        // answered from another thread once the test releases it, so the
        // daemon keeps serving the requests the picker makes meanwhile.
        let writes = std::sync::Arc::new(std::sync::Mutex::new(()));
        let held_writes = writes.clone();
        // One listing is held in this test; the loop serves the rest.
        let mut wait_release = Some(wait_release);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            let answer = match request["method"].as_str().unwrap() {
                "hello" => Some(json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","provider.refresh"]})),
                "session.create" => Some(json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}})),
                "session.history" => Some(json!({"events":[],"through":8,"nextAfter":8,"done":true})),
                "session.idle" => Some(json!({"sequence":8})),
                "provider.list" => {
                    *counted.lock().unwrap() += 1;
                    Some(json!({"providers":[
                        {"providerId":"fresh","displayName":"Fresh","enabled":true,"remoteModelList":true},
                        {"providerId":"broken","displayName":"Broken","enabled":true,"remoteModelList":true},
                        {"providerId":"unlisted","displayName":"Unlisted","enabled":true,"remoteModelList":false}]}))
                }
                "provider.catalog" => {
                    let provider = request["params"]["providerId"].as_str().unwrap();
                    let catalog = stored.lock().unwrap().get(provider).cloned().unwrap_or_else(|| {
                        json!({"version":1,"disabledModelIds":[],"models":[]})
                    });
                    Some(json!({"catalog":catalog}))
                }
                "provider.refresh" => {
                    let provider = request["params"]["providerId"].as_str().unwrap().to_owned();
                    seen.lock().unwrap().push(provider.clone());
                    if provider == "broken" {
                        let _guard = writes.lock().unwrap();
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"listing failed"}}));
                        None
                    } else {
                        // Hold the listing: the picker may not wait for it.
                        let mut socket = wire.0.get_ref().try_clone().unwrap();
                        let stored = kept.clone();
                        let writes = held_writes.clone();
                        let id = request["id"].clone();
                        let wait = wait_release.take().expect("one held listing");
                        thread::spawn(move || {
                            wait.recv_timeout(PATIENCE).unwrap();
                            let catalog = json!({"version":2,"disabledModelIds":[],"models":[
                                {"id":"vendor/model-a","name":"Model A","unavailableReason":null,
                                    "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                                {"id":"vendor/model-b","name":"Model B","unavailableReason":null,
                                    "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}]});
                            stored.lock().unwrap().insert(provider, catalog.clone());
                            let _guard = writes.lock().unwrap();
                            writeln!(socket, "{}", json!({"jsonrpc":"2.0","id":id,"result":catalog}))
                                .unwrap();
                            socket.flush().unwrap();
                        });
                        None
                    }
                }
                "account.providers" => Some(json!([])),
                _ => Some(json!({})),
            };
            if let Some(result) = answer {
                let _guard = writes.lock().unwrap();
                wire.reply(&request, result);
            }
        }
    });
    let mut command = home.command();
    command.env("LC_ALL", "C");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/models\r");
    // The listing is still held: the picker draws every row it has.
    terminal.until("Fresh: no models · dimcode models fresh --refresh");
    terminal.until("Broken: no models · dimcode models broken --refresh");
    terminal.until("Unlisted: no models · add a model with provider.models.create");
    release.send(()).unwrap();
    // The listing that arrives late fills the picker that is already open.
    terminal.until("Fresh / vendor/model-a");
    terminal.until("Fresh / vendor/model-b");
    // Closing and reopening in the same connection asks no provider again,
    // though the failed refresh kept its row.
    terminal.write(b"\x1b");
    thread::sleep(Duration::from_millis(500));
    terminal.write(b"/models\r");
    let deadline = Instant::now() + PATIENCE;
    while *listed.lock().unwrap() < 3 {
        assert!(Instant::now() < deadline, "the picker did not open again");
        thread::sleep(Duration::from_millis(10));
    }
    terminal.until("Broken: no models · dimcode models broken --refresh");
    terminal.until("Fresh / vendor/model-a");
    // Another client writes the record (an enable or a re-point), which moves
    // its catalog version: the same empty catalog is asked once more.
    changed.lock().unwrap().insert(
        "broken".to_owned(),
        json!({"version":3,"disabledModelIds":[],"models":[]}),
    );
    terminal.write(b"\x1b");
    thread::sleep(Duration::from_millis(500));
    terminal.write(b"/models\r");
    let deadline = Instant::now() + PATIENCE;
    while *listed.lock().unwrap() < 4 {
        assert!(
            Instant::now() < deadline,
            "the picker did not open after the write"
        );
        thread::sleep(Duration::from_millis(10));
    }
    terminal.until("Broken: no models · dimcode models broken --refresh");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert_eq!(
        *refreshed.lock().unwrap(),
        ["fresh".to_owned(), "broken".to_owned(), "broken".to_owned()],
        "one refresh per catalog version that can be listed, none on an unchanged reopen"
    );
}

/// A refill that lands after the picker closed is recorded, so the provider
/// is not asked again, but it starts no re-read: the next open is what reads
/// the catalog the listing stored.
#[test]
fn tui_models_picker_does_not_reread_when_a_refill_lands_closed() {
    let home = Home::new();
    let listed = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let counted = listed.clone();
    let refreshed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = refreshed.clone();
    let stored = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
    let kept = stored.clone();
    let (release, wait_release) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        let mut wait_release = Some(wait_release);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            let answer = match request["method"].as_str().unwrap() {
                "hello" => Some(json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","provider.refresh"]})),
                "session.create" => Some(json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}})),
                "session.history" => Some(json!({"events":[],"through":8,"nextAfter":8,"done":true})),
                "session.idle" => Some(json!({"sequence":8})),
                "provider.list" => {
                    *counted.lock().unwrap() += 1;
                    Some(json!({"providers":[
                        {"providerId":"fresh","displayName":"Fresh","enabled":true,"remoteModelList":true}]}))
                }
                "provider.catalog" => {
                    let catalog = kept.lock().unwrap().clone();
                    Some(json!({"catalog":catalog}))
                }
                "provider.refresh" => {
                    seen.lock()
                        .unwrap()
                        .push(request["params"]["providerId"].as_str().unwrap().to_owned());
                    let mut socket = wire.0.get_ref().try_clone().unwrap();
                    let stored = stored.clone();
                    let id = request["id"].clone();
                    let wait = wait_release.take().expect("one held listing");
                    thread::spawn(move || {
                        wait.recv_timeout(PATIENCE).unwrap();
                        let catalog = json!({"version":2,"disabledModelIds":[],"models":[
                            {"id":"vendor/model-a","name":"Model A","unavailableReason":null,
                                "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}]});
                        *stored.lock().unwrap() = catalog.clone();
                        writeln!(socket, "{}", json!({"jsonrpc":"2.0","id":id,"result":catalog}))
                            .unwrap();
                        socket.flush().unwrap();
                    });
                    None
                }
                "account.providers" => Some(json!([])),
                _ => Some(json!({})),
            };
            if let Some(result) = answer {
                wire.reply(&request, result);
            }
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/models\r");
    terminal.until("Fresh: no models · dimcode models fresh --refresh");
    terminal.write(b"\x1b");
    terminal.until("Ask anything...");
    let opens = *listed.lock().unwrap();
    release.send(()).unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        *listed.lock().unwrap(),
        opens,
        "a refill that lands with the picker closed starts no re-read"
    );
    // The next open reads the catalog the refresh stored, and asks nothing.
    terminal.write(b"/models\r");
    terminal.until("Fresh / vendor/model-a");
    assert_eq!(
        refreshed.lock().unwrap().as_slice(),
        ["fresh".to_owned()],
        "the provider recorded its refill; the reopen asks again"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A provider that offers nothing selectable keeps a row naming it and the
/// reason: no models with the refresh command when the daemon can list them,
/// no models with the add method when it cannot, an unreadable catalog, or
/// the unavailable and disabled counts together. Undeclared limits count as
/// the reason only when declaring them would unblock the model: a model
/// unavailable for another reason keeps that reason, and a disabled one is
/// not counted. The picker refills only the provider whose catalog is empty
/// and can be listed: a provider whose models are its bundled catalog's, one
/// the daemon cannot list and one whose catalog cannot be read are never
/// refreshed.
#[test]
fn tui_models_picker_names_every_provider_with_no_selectable_model() {
    let home = Home::new();
    let refreshed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = refreshed.clone();
    // The daemon reports the re-list that follows its refill: after the
    // second list's last catalog reply the client has nothing in flight, so
    // the kill below cannot interrupt a reply.
    let (settled, wait_settled) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let mut version = 1u32;
        let mut catalogs = 0usize;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            let model = |id: &str, reason: Option<&str>, declared: bool| {
                let reason = reason.map_or(Value::Null, |reason| json!(reason));
                let capabilities = if declared {
                    dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)
                } else {
                    dim_agent_types::CatalogCapabilities::default()
                };
                let missing = if declared {
                    json!([])
                } else {
                    json!(["contextWindow", "maxOutputTokens"])
                };
                json!({"id":id,"name":id,"unavailableReason":reason,
                    "missingLimits":missing,"capabilities":capabilities})
            };
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog"]}),
                "session.create" => json!({"sessionId":"session","sequence":8,"model":{"providerId":"ready","modelId":"vendor/model-a"}}),
                "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "provider.list" => json!({"providers":[
                    {"providerId":"ready","displayName":"Ready","enabled":true,"remoteModelList":true},
                    {"providerId":"empty","displayName":"Empty","enabled":true},
                    {"providerId":"mixed","displayName":"Mixed","enabled":true},
                    {"providerId":"hidden","displayName":"All disabled","enabled":true},
                    {"providerId":"blank","displayName":"Blank","enabled":true,"remoteModelList":true},
                    {"providerId":"manual","displayName":"Manual","enabled":true,"remoteModelList":false},
                    {"providerId":"broken","displayName":"Broken","enabled":true}]}),
                "provider.catalog" => {
                    let provider = request["params"]["providerId"].as_str().unwrap();
                    catalogs += 1;
                    let last = catalogs == 14;
                    if provider == "broken" {
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32000,"message":"catalog read failed"}}));
                        if last {
                            settled.send(()).ok();
                        }
                        continue;
                    }
                    let (models, disabled) = match provider {
                        "empty" => (
                            json!([
                                model("vendor/embed-a", Some("provider_unavailable"), false),
                                model("vendor/embed-b", Some("provider_unavailable"), false)]),
                            json!([]),
                        ),
                        "mixed" => (
                            json!([
                                model("vendor/embed-c", Some("provider_unavailable"), true),
                                model("vendor/hidden-a", None, true),
                                model("vendor/hidden-c", Some("limits_undeclared"), false)]),
                            json!(["vendor/hidden-a", "vendor/hidden-c"]),
                        ),
                        "hidden" => (
                            json!([model("vendor/hidden-b", None, true)]),
                            json!(["vendor/hidden-b"]),
                        ),
                        "blank" | "manual" => (json!([]), json!([])),
                        _ => (json!([model("vendor/model-a", None, true)]), json!([])),
                    };
                    wire.reply(
                        &request,
                        json!({"catalog":{"version":version,"disabledModelIds":disabled,"models":models}}),
                    );
                    if last {
                        settled.send(()).ok();
                    }
                    continue;
                }
                "provider.refresh" => {
                    seen.lock()
                        .unwrap()
                        .push(request["params"]["providerId"].as_str().unwrap().to_owned());
                    // The refresh stores the listed catalog under a new
                    // version, and the read reports it, so the re-list does
                    // not ask for the same refill again.
                    version = 2;
                    json!({"version":version,"disabledModelIds":[],"models":[]})
                }
                "account.providers" => json!([]),
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("ready / vendor/model-a");
    terminal.write(b"/models\r");
    terminal.until("Ready / vendor/model-a");
    terminal.until("Empty: 2 models unavailable · dimcode models empty");
    terminal.until("Mixed: 2 unavailable and 1 disabled · dimcode models mixed");
    terminal.until("All disabled: 1 model disabled · dimcode models hidden");
    terminal.until("Blank: no models · dimcode models blank --refresh");
    terminal.until("Manual: no models · add a model with provider.models.create");
    terminal.until("Broken: catalog unreadable · dimcode models broken");
    assert!(
        !terminal.transcript.contains("Ready / vendor/hidden-b"),
        "a disabled model is not selectable"
    );
    assert!(
        !terminal.transcript.contains("need a window"),
        "a model unavailable for another reason, and a disabled one, raise no limits row"
    );
    wait_settled
        .recv_timeout(PATIENCE)
        .expect("the picker settles after its re-list");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let refreshed = refreshed.lock().unwrap();
    assert_eq!(
        refreshed.as_slice(),
        ["blank".to_owned()],
        "only the empty catalog the daemon can list is refreshed"
    );
}

#[test]
fn member_tool_approval_requires_trusted_details_in_both_terminal_modes() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (checked, wait_checked) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers"); wire.reply(&providers, json!([]));
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"outer","call":{"name":"member_approval_allow","input":{"approvalId":"route"}},"reason":"Leader permission"}));
            let detail = wire.read("session.collaboration.approval.get");
            assert_eq!(detail["params"], json!({"sessionId":"session","branchId":"review","requestId":"outer"}));
            wire.send(json!({"jsonrpc":"2.0","id":detail["id"],"error":{"code":-32000,"message":"detail service reloading"}}));
            wire.quiet(Duration::from_millis(200));
            checked.send(()).unwrap();
            let detail = wire.read("session.collaboration.approval.get");
            let original = json!({"state":"pending","approvalId":"route","descriptor":{"delegationId":"delegation","member":{"agentId":"reviewer","sessionId":"member-session","branchId":"main"},"requestId":"original","call":{"name":"exec","input":{"command":"cargo test trusted action"}},"reason":"Member process request"}});
            wire.reply(&detail, original.clone());
            let detail = wire.read("session.collaboration.approval.get");
            wire.reply(&detail, original);
            let answer = wire.read("approval.respond");
            assert_eq!(answer["params"], json!({"sessionId":"session","branchId":"review","requestId":"outer","decision":"allow"}));
            wire.reply(&answer, json!({}));
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"outer2","call":{"name":"member_approval_allow","input":{"approvalId":"route2"}},"reason":"Member cancellation"}));
            let detail = wire.read("session.collaboration.approval.get");
            wire.reply(&detail, json!({"state":"pending","approvalId":"route2","descriptor":{"delegationId":"delegation","member":{"agentId":"reviewer","sessionId":"member-session","branchId":"main"},"requestId":"original2","call":{"name":"exec","input":{"command":"cancel this member action"}},"reason":"Member process request"}}));
            wire.notice("session.notice", json!({"sessionId":"session","source":"Collaboration","kind":"memberApprovalEnded","data":{"branchId":"review","approvalId":"route2"}}));
            let answer = wire.read("approval.respond");
            assert_eq!(answer["params"]["requestId"], "outer2");
            assert_eq!(answer["params"]["decision"], "deny");
            wire.reply(&answer, json!({}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"done","sequence":1,"create_at":1,"type":"step","name":"text","data":{"turn_id":"turn","request_id":"model","delta":"member approvals finished"}}}));
            let mut line = String::new(); while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) { line.clear(); }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("Cannot review member action");
        terminal.write(b"y");
        wait_checked.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"r");
        terminal.until("member-session");
        terminal.until("cargo test trusted action");
        assert!(terminal.transcript.contains("Leader branch: review"));
        assert!(!terminal.transcript.contains("Full access"));
        assert!(!terminal.transcript.contains("Allow always"));
        terminal.write(b"y");
        terminal.until("member approvals finished");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn member_approval_without_the_detail_method_offers_only_rejection() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (checked, wait_checked) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]); wire.created();
            let attach = wire.read("session.attach"); wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers"); wire.reply(&providers, json!([]));
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"outer","call":{"name":"member_approval_allow","input":{"approvalId":"route"}},"reason":"Member action"}));
            let detail = wire.read("session.collaboration.approval.get");
            wire.send(json!({"jsonrpc":"2.0","id":detail["id"],"error":{"code":-32601,"message":"Unknown method"}}));
            wire.quiet(Duration::from_millis(250)); checked.send(()).unwrap();
            let answer = wire.read("approval.respond");
            assert_eq!(answer["params"]["decision"], "deny"); wire.reply(&answer, json!({}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"done","sequence":1,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"unsupported approval rejected"}}}));
            let mut line = String::new(); while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) { line.clear(); }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("combination cannot review");
        assert!(!terminal.transcript.contains("Retry"));
        terminal.write(b"ry");
        wait_checked.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"n");
        terminal.until("unsupported approval rejected");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `dimcode recording on` switches the `model-log` source and names the file.
#[test]
fn recording_on_switches_the_model_log() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}),
        );
        let read = wire.read("settings.read");
        assert_eq!(read["params"], json!({"source":"model-log","scope":"user"}));
        wire.reply(
            &read,
            json!({"revision":"user/model-log:1","document":{"enabled":false}}),
        );
        let update = wire.read("settings.update");
        assert_eq!(
            update["params"],
            json!({
                "source":"model-log",
                "scope":"user",
                "expectedRevision":"user/model-log:1",
                "document":{"enabled":true},
            })
        );
        wire.reply(
            &update,
            json!({"revision":"user/model-log:2","document":{"enabled":true}}),
        );
    });
    let output = home.command().args(["recording", "on"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "Model request recording is on: {}\n",
            home.0.join("model-log/recordings.jsonl").display()
        )
    );
}

/// `dimcode recording` reads the source without writing it.
#[test]
fn recording_reports_whether_it_is_on() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}),
        );
        let read = wire.read("settings.read");
        wire.reply(
            &read,
            json!({"revision":"user/model-log:1","document":{"enabled":false}}),
        );
    });
    let output = home.command().arg("recording").output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Model request recording is off.\n"
    );
}

#[test]
fn tui_default_theme_respects_background_and_saved_choice() {
    for (program, term, signal, background, saved, expected) in [
        ("ghostty", "xterm-256color", None, None, None, "Dim Dark"),
        (
            "ghostty",
            "xterm-256color",
            Some(("COLORFGBG", "0;15")),
            None,
            None,
            "System",
        ),
        (
            "Apple_Terminal",
            "xterm-256color",
            None,
            None,
            None,
            "System",
        ),
        ("", "nsterm-256color", None, None, None, "System"),
        (
            "Apple_Terminal",
            "xterm-256color",
            None,
            Some("rgb:ffff/ffff/ffff"),
            None,
            "System",
        ),
        (
            "Apple_Terminal",
            "xterm-256color",
            None,
            Some("rgb:0000/0000/0000"),
            None,
            "Dim Dark",
        ),
        (
            "Apple_Terminal",
            "xterm-256color",
            Some(("DIM_TERMINAL_APPEARANCE", "dark")),
            None,
            None,
            "Dim Dark",
        ),
        (
            "Apple_Terminal",
            "xterm-256color",
            Some(("COLORFGBG", "15;0")),
            None,
            None,
            "Dim Dark",
        ),
        ("", "xterm", None, None, None, "System"),
        (
            "ghostty",
            "xterm-256color",
            None,
            None,
            Some("system"),
            "System",
        ),
        (
            "ghostty",
            "xterm-256color",
            None,
            None,
            Some("monokai-dark"),
            "Monokai Dark",
        ),
    ] {
        let home = Home::new();
        if let Some(saved) = saved {
            fs::write(
                home.0.join("config.json"),
                json!({"locale":"en","themeId":saved}).to_string(),
            )
            .unwrap();
        }
        let (quieted, quiet_done) = std::sync::mpsc::channel();
        let daemon = no_session_daemon(&home, quieted);
        let mut command = home.command();
        for key in [
            "COLORTERM",
            "TERMINAL_EMULATOR",
            "WT_SESSION",
            "COLORFGBG",
            "DIM_TERMINAL_APPEARANCE",
            "DIMCODE_TERMINAL_APPEARANCE",
        ] {
            command.env_remove(key);
        }
        command.env("TERM_PROGRAM", program).env("TERM", term);
        if let Some((key, value)) = signal {
            command.env(key, value);
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("\x1b]11;?\x07");
        if let Some(background) = background {
            terminal.raw(format!("\x1b]11;{background}\x07").as_bytes());
        }
        terminal.raw(b"\x1b[?1;2c");
        terminal.until("Sign in with /login");
        terminal.write(b"/settings\r");
        terminal.until(&format!("{expected} ›"));
        quiet_done.recv_timeout(PATIENCE).unwrap();
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
        let config: Value =
            serde_json::from_slice(&fs::read(home.0.join("config.json")).unwrap()).unwrap();
        assert_eq!(config.get("themeId").and_then(Value::as_str), saved);
    }
}

/// `dimcode daemon vacuum` rewrites an offline store through the sibling App
/// Server and reports its size before and after (#600).
#[test]
fn daemon_vacuum_reclaims_an_offline_store() {
    use dim_app_server::storage::SqliteStorage;
    let home = Home::new();
    let database = home.0.join("rust/dimcode.sqlite");
    drop(SqliteStorage::open(database.clone()).unwrap());
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "INSERT INTO agent_sessions(session_id, config, created_at_ms) VALUES ('s', '{}', 0);
             INSERT INTO timeline_branches VALUES ('s', 'main', '{}', 0);",
        )
        .unwrap();
    connection.execute_batch("BEGIN").unwrap();
    for sequence in 1..=2000 {
        connection
            .execute(
                "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body) \
                 VALUES ('s', 'main', ?1, ?1, 'turn/start', ?2)",
                rusqlite::params![sequence, "x".repeat(4000)],
            )
            .unwrap();
    }
    connection
        .execute_batch("COMMIT; DELETE FROM timeline_events; PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);
    let before = fs::metadata(&database).unwrap().len();
    let output = home.command().args(["daemon", "vacuum"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Vacuumed "), "{stdout}");
    let after = fs::metadata(&database).unwrap().len();
    assert!(
        after < before,
        "vacuum reclaims free pages: {after} >= {before}"
    );
}
