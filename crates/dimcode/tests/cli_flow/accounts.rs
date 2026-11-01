//! `dimcode login|logout|account` and the `/connect` sign-in follow the
//! account descriptor `provider.management` reports.
use super::*;

/// A `provider.management` reply with the given providers.
fn management(providers: Value) -> Value {
    json!({"providers": providers, "errors": []})
}

/// One described provider, with the connection fields the panel reads.
fn described(id: &str, name: &str, namespace: &str, methods: &[&str], signed_in: bool) -> Value {
    json!({
        "providerId": id,
        "displayName": name,
        "source": "builtin",
        "version": 1,
        "enabled": true,
        "preferences": {},
        "connection": {
            "driverKind": "openai-codex",
            "auth": {"type": "bearer"},
            "baseUrl": "https://fixture.invalid/v1",
            "credentialConfigured": signed_in,
        },
        "account": {
            "namespace": namespace,
            "loginMethods": methods,
            "state": {"authenticated": signed_in},
        },
    })
}

fn hello(wire: &mut Wire, extra: &[&str]) {
    let hello = wire.read("hello");
    let mut features = vec!["session.create", "session.history", "provider.list"];
    features.extend(extra);
    wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
}

#[test]
fn a_described_browser_provider_signs_in_from_the_cli() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(
            wire,
            &[
                "provider.management",
                "account.codex.login.browser.begin",
                "account.codex.login.browser.complete",
            ],
        );
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([
                described(
                    "openai-codex",
                    "OpenAI Codex",
                    "account.codex",
                    &["browser"],
                    false
                ),
                described("grok-build", "Grok Build", "account.grok", &["device"], false),
            ])),
        );
        let begin = wire.read("account.codex.login.browser.begin");
        assert_eq!(begin["params"], json!({}));
        wire.reply(&begin,json!({"attemptId":"codex-attempt","authorizationUrl":"urn:fixture:codex","expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
        let complete = wire.read("account.codex.login.browser.complete");
        assert_eq!(
            complete["params"],
            json!({"attemptId":"codex-attempt","callbackUrl":"http://localhost:1455/auth/callback?code=a&state=b"})
        );
        wire.reply(&complete, json!({}));
    });
    let mut command = home.command();
    command.env("DIMCODE_OAUTH_MANUAL", "1");
    command.args(["login", "openai-codex"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("Open: urn:fixture:codex");
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1455/auth/callback?code=a&state=b\n");
    terminal.until("Signed in.");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
}

#[test]
fn a_described_device_provider_signs_in_and_cancels_from_the_cli() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(
            wire,
            &[
                "provider.management",
                "account.grok.login.begin",
                "account.grok.login.status",
                "account.grok.login.cancel",
            ],
        );
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([described(
                "grok-build",
                "Grok Build",
                "account.grok",
                &["device"],
                false
            )])),
        );
        let begin = wire.read("account.grok.login.begin");
        assert_eq!(begin["params"], json!({}));
        wire.reply(&begin,json!({"attemptId":"grok-attempt","verificationUri":"https://fixture.invalid/device","userCode":"GROK-CODE","expiresAt":999999}));
        let status = wire.read("account.grok.login.status");
        assert_eq!(status["params"], json!({"attemptId":"grok-attempt"}));
        wire.reply(&status, json!({"status":"pending"}));
        let cancel = wire.read("account.grok.login.cancel");
        assert_eq!(cancel["params"], json!({"attemptId":"grok-attempt"}));
        wire.reply(&cancel, json!({}));
    });
    let mut command = home.command();
    command.args(["login", "grok-build"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("GROK-CODE");
    thread::sleep(Duration::from_millis(100));
    terminal.signal(libc::SIGINT);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(130));
}

#[test]
fn login_without_a_provider_lists_the_described_accounts() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire, &["provider.management"]);
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([
                described(
                    "openai-codex",
                    "OpenAI Codex",
                    "account.codex",
                    &["browser"],
                    false
                ),
                described(
                    "grok-build",
                    "Grok Build",
                    "account.grok",
                    &["device"],
                    false
                ),
            ])),
        );
    });
    let output = home.command().arg("login").output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("openai-codex (OpenAI Codex): browser"),
        "{stdout}"
    );
    assert!(
        stdout.contains("grok-build (Grok Build): device"),
        "{stdout}"
    );
    assert!(stdout.contains("Run dimcode login PROVIDER."), "{stdout}");
}

#[test]
fn a_provider_without_a_descriptor_offers_no_sign_in() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire, &["provider.management"]);
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([
                described(
                    "openai-codex",
                    "OpenAI Codex",
                    "account.codex",
                    &["browser"],
                    false
                ),
                {
                    "providerId": "api",
                    "displayName": "API key",
                    "source": "custom",
                    "version": 1,
                    "enabled": true,
                    "preferences": {},
                    "connection": {
                        "driverKind": "openai-compatible",
                        "auth": {"type": "bearer"},
                        "baseUrl": "https://fixture.invalid/v1",
                        "credentialConfigured": false,
                    },
                },
            ])),
        );
    });
    let output = home.command().args(["login", "api"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Provider api does not offer sign-in"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_unsupported_daemon_reports_the_missing_sign_in_method() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire, &["provider.management"]);
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([described(
                "openai-codex",
                "OpenAI Codex",
                "account.codex",
                &["browser"],
                false
            )])),
        );
    });
    let output = home
        .command()
        .args(["login", "openai-codex"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon does not offer account.codex.login.browser.begin"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn account_and_logout_follow_the_described_namespace() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire, &["provider.management", "account.codex.status"]);
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([described(
                "openai-codex",
                "OpenAI Codex",
                "account.codex",
                &["browser"],
                true
            )])),
        );
        let status = wire.read("account.codex.status");
        assert_eq!(status["params"], json!({}));
        wire.reply(
            &status,
            json!({"authenticated":true,"recoveryRequired":false,"expiresAt":null,"accountIdPreview":"acct-preview"}),
        );
    });
    let output = home
        .command()
        .args(["account", "openai-codex"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Signed in: acct-preview"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire, &["provider.management", "account.codex.logout"]);
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([described(
                "openai-codex",
                "OpenAI Codex",
                "account.codex",
                &["browser"],
                true
            )])),
        );
        let logout = wire.read("account.codex.logout");
        assert_eq!(logout["params"], json!({}));
        wire.reply(&logout, json!({}));
    });
    let output = home
        .command()
        .args(["logout", "openai-codex"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Signed out."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// #825: a build without the Dim account provider says so and names the
/// API-key alternative, from every account command.
#[test]
fn account_commands_name_the_missing_account_provider() {
    const MISSING: &str = "This build has no Dim account provider, so the Dim account is unavailable; configure an API-key provider instead.";
    for command in ["account", "login", "logout"] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            hello(wire, &["provider.management"]);
            let listed = wire.read("provider.management");
            wire.reply(
                &listed,
                management(json!([
                    described(
                        "openai-codex",
                        "OpenAI Codex",
                        "account.codex",
                        &["browser"],
                        false
                    ),
                    described(
                        "grok-build",
                        "Grok Build",
                        "account.grok",
                        &["device"],
                        false
                    ),
                ])),
            );
        });
        let output = home
            .command()
            .args([command, "dimcode-api-oauth"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(MISSING),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn account_help_prints_usage_without_contacting_a_daemon() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let output = home.command().args(["account", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("account PROVIDER"), "{stdout}");
    assert!(
        stdout.contains("A build without the Dim account provider says so"),
        "{stdout}"
    );
    assert!(!home.0.join("rust").exists(), "the help started a daemon");
}

/// The `/connect` panel signs in through each described namespace; a
/// daemon without the method shows the error in the sign-in step.
#[test]
fn the_provider_panel_signs_in_through_each_described_namespace() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            hello(
                wire,
                &[
                    "provider.management",
                    "provider.create",
                    "provider.update",
                    "provider.probe",
                    "account.codex.login.browser.begin",
                    "account.codex.login.cancel",
                    "account.grok.login.begin",
                    "account.grok.login.cancel",
                ],
            );
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
            let listed = wire.read("provider.management");
            wire.reply(
                &listed,
                management(json!([
                    described(
                        "openai-codex",
                        "OpenAI Codex",
                        "account.codex",
                        &["browser"],
                        false
                    ),
                    described("grok-build", "Grok Build", "account.grok", &["device"], false),
                ])),
            );
            let begin = wire.read("account.codex.login.browser.begin");
            assert_eq!(begin["params"], json!({}));
            wire.reply(&begin,json!({"attemptId":"codex-attempt","authorizationUrl":"urn:fixture:codex","expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
            let cancel = wire.read("account.codex.login.cancel");
            assert_eq!(cancel["params"], json!({"attemptId":"codex-attempt"}));
            wire.reply(&cancel, json!({}));
            let begin = wire.read("account.grok.login.begin");
            assert_eq!(begin["params"], json!({}));
            wire.reply(&begin,json!({"attemptId":"grok-attempt","verificationUri":"https://fixture.invalid/device","userCode":"GROK-CODE","expiresAt":999999}));
            let cancel = wire.read("account.grok.login.cancel");
            assert_eq!(cancel["params"], json!({"attemptId":"grok-attempt"}));
            wire.reply(&cancel, json!({}));
            wire.hold();
        });
        let mut command = home.command();
        command.env("DIMCODE_OAUTH_MANUAL", "1");
        if fullscreen {
            command.arg("--fullscreen");
        }
        sized(&mut command, 100, 32);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        terminal.write(b"/connect\r");
        terminal.until("OpenAI Codex");
        terminal.write(b"\r");
        terminal.until("Browser login");
        terminal.until("urn:fixture:codex");
        terminal.write(b"\x1b");
        terminal.until("Grok Build");
        terminal.write(b"\x1b[B\r");
        terminal.until("GROK-CODE");
        terminal.write(b"\x1b");
        terminal.until("Add custom provider");
        terminal.write(b"\x1b");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn a_panel_without_the_sign_in_method_shows_the_unsupported_daemon_error() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(
            wire,
            &[
                "provider.management",
                "provider.create",
                "provider.update",
                "provider.probe",
            ],
        );
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
        let listed = wire.read("provider.management");
        wire.reply(
            &listed,
            management(json!([described(
                "openai-codex",
                "OpenAI Codex",
                "account.codex",
                &["browser"],
                false
            )])),
        );
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/connect\r");
    terminal.until("OpenAI Codex");
    terminal.write(b"\r");
    terminal.until("This daemon does not offer account.codex.login.browser.begin");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// The production App Server describes the Codex account itself, and its
/// browser sign-in builds the authorization URL without network.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn login_reaches_the_codex_account_of_the_production_daemon() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let mut command = home.command();
    command
        .env("DIMCODE_OAUTH_MANUAL", "1")
        .args(["--server", &binary, "login", "openai-codex"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("Sign in to OpenAI Codex");
    terminal.until("Open: https://auth.openai.com/oauth/authorize?");
    terminal.until("Callback URL: ");
    // An empty callback cancels the attempt through the daemon's
    // `account.codex.login.cancel`, the same exit the Ctrl+C path takes. A
    // signal would race the handler's installation just after the prompt.
    terminal.write(b"\r");
    let status = terminal.wait();
    assert_eq!(status.code(), Some(130));
}
