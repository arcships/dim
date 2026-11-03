use super::*;

const STARTUP_ENV: &[&str] = &[
    "DIM_MODEL",
    "DIM_EFFORT_LEVEL",
    "DIM_BASE_URL",
    "DIM_API_FORMAT",
    "DIM_API_KEY",
    "DIM_AUTH_TOKEN",
    "DIM_CUSTOM_HEADERS",
    "DIM_MAX_CONTEXT_TOKENS",
    "DIM_MAX_OUTPUT_TOKENS",
    "DIM_AUTO_COMPACT_WINDOW",
];

fn command(home: &Home) -> Command {
    let mut command = home.command();
    for name in STARTUP_ENV {
        command.env_remove(name);
    }
    command
}

fn reject(wire: &mut Wire, request: &Value) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
        "code":-32000,"message":"startup fixture rejection","data":{"appError":"provider_failed"}}}));
}

#[test]
fn direct_startup_reaches_each_creation_entry_without_persisting_secrets() {
    for format in [
        "anthropic",
        "openai-chat",
        "openai-responses",
        "openai-compatible",
        "gemini",
        "xai",
        "xai-responses",
    ] {
        for (tui, leaf) in [(false, false), (true, false), (true, true)] {
            let home = Home::new();
            let original = fs::read(home.0.join("config.json")).unwrap();
            let daemon = home.daemon(move |wire| {
                wire.hello(if leaf { &["ui.leaf.2"] } else { &[] });
                let create = wire.read("session.create");
                assert_eq!(create["params"]["model"], json!({"modelId":"flag-model"}));
                assert_eq!(
                    create["params"]["modelPreferences"],
                    json!({"reasoningEffort":"high"})
                );
                assert_eq!(
                    create["params"]["startup"],
                    json!({
                    "connection":{"apiFormat":format,"baseUrl":"https://gateway.example/prefix",
                        "authToken":"secret-token","headers":{"x-gateway":"secret:header:value"}},
                    "contextWindow":256000,"maxOutputTokens":16000,"autoCompactWindow":128000})
                );
                reject(wire, &create);
            });
            let mut command = command(&home);
            command
                .envs([
                    ("DIM_BASE_URL", "https://gateway.example/prefix"),
                    ("DIM_API_FORMAT", format),
                    ("DIM_AUTH_TOKEN", "secret-token"),
                    (
                        "DIM_CUSTOM_HEADERS",
                        "X-GaTeWaY:\t secret:header:value \t\n",
                    ),
                    ("DIM_MODEL", "environment-model"),
                    ("DIM_EFFORT_LEVEL", "low"),
                    ("DIM_AUTO_COMPACT_WINDOW", "1m"),
                    ("DIM_MAX_CONTEXT_TOKENS", "256000"),
                    ("DIM_MAX_OUTPUT_TOKENS", "16000"),
                ])
                .args([
                    "--model",
                    "flag-model",
                    "--effort",
                    "high",
                    "--autocompact",
                    "128k",
                ]);
            let text = if tui {
                let mut terminal = Terminal::launch_tui(command);
                terminal.until("startup fixture rejection");
                if !leaf {
                    terminal.signal(libc::SIGKILL);
                }
                assert!(!terminal.wait().success());
                terminal.transcript.clone()
            } else {
                let output = command.args(["exec", "-p", "Hello"]).output().unwrap();
                assert!(!output.status.success());
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            };
            daemon.join().unwrap();
            assert!(text.contains("startup fixture rejection"), "{text}");
            assert!(!text.contains("secret-token"), "{text}");
            assert!(!text.contains("secret:header:value"), "{text}");
            assert_eq!(fs::read(home.0.join("config.json")).unwrap(), original);
        }
    }
}

#[test]
fn resume_startup_is_adopted_or_explicitly_rejected_before_input_in_every_client() {
    for (tui, leaf) in [(false, false), (true, false), (true, true)] {
        for model in [None, Some("replacement")] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(if leaf { &["ui.leaf.2"] } else { &[] });
                let request = wire.read_any();
                let update = if request["method"] == "session.attach" {
                    wire.reply(&request, attach_record(&request));
                    wire.read("session.model.update")
                } else {
                    assert_eq!(request["method"], "session.model.update");
                    request
                };
                let mut expected = json!({"sessionId":"session","branchId":"main",
                    "modelPreferences":{"reasoningEffort":"low"},
                    "startup":{"connection":{"baseUrl":"https://gateway.example/prefix",
                        "apiFormat":"anthropic","apiKey":"resume-secret"},"autoCompactWindow":"auto"}});
                if let Some(model) = model {
                    expected["modelId"] = json!(model);
                }
                assert_eq!(update["params"], expected);
                reject(wire, &update);
            });
            let mut command = command(&home);
            command
                .envs([
                    ("DIM_BASE_URL", "https://gateway.example/prefix"),
                    ("DIM_API_FORMAT", "anthropic"),
                    ("DIM_API_KEY", "resume-secret"),
                    ("DIM_EFFORT_LEVEL", "low"),
                    ("DIM_AUTO_COMPACT_WINDOW", "auto"),
                ])
                .args(["--resume", "session"]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            if tui {
                let mut terminal = Terminal::launch_tui(command);
                terminal.until("Cannot adopt startup configuration");
                if !leaf {
                    terminal.signal(libc::SIGKILL);
                }
                assert!(!terminal.wait().success());
                assert!(!terminal.transcript.contains("resume-secret"));
            } else {
                let output = command.args(["exec", "-p", "Hello"]).output().unwrap();
                assert!(!output.status.success());
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(
                    error.contains("Cannot adopt startup configuration"),
                    "{error}"
                );
                assert!(!error.contains("resume-secret"), "{error}");
            }
            daemon.join().unwrap();
        }
    }
}

#[test]
fn resume_effort_only_sends_forced_inheritance_before_submitting() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let update = wire.read("session.model.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main",
            "modelPreferences":{"reasoningEffort":"high"},"startup":{}})
        );
        wire.reply(&update, json!({"outcome":"adopted"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":8}));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let output = command(&home)
        .args([
            "exec", "resume", "session", "--effort", "high", "-p", "Hello",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn home_first_message_inherits_the_adopted_model_without_replaying_startup() {
    for (resume, switched) in [(false, false), (true, false), (false, true)] {
        let home = Home::new();
        let (calls, observed) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
        let mut creates = 0;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                    "protocolVersion":3,"daemonProfile":{"kind":"product"},
                    "features":["session.create","session.create.modelFromSession","session.history","provider.list"]}),
                "session.create" => {
                    creates += 1;
                    calls.send(request["params"].clone()).unwrap();
                    if resume || creates == 2 {
                        reject(wire, &request);
                        continue;
                    }
                    json!({"sessionId":"session","sequence":8,"model":{"providerId":"session-source","modelId":"model"}})
                }
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["model"] = json!({"providerId":"session-source","modelId":"model"});
                    record
                }
                "session.history" => {
                    let events = if switched {
                        json!([{"id":"switch","sequence":8,"create_at":1000,
                            "type":"context","name":"configured","data":{
                                "provider_id":"configured-provider","model_id":"selected-model",
                                "model_preferences":{"reasoningEffort":"low"}}}])
                    } else { json!([]) };
                    json!({"events":events,"through":8,"nextAfter":8,"done":true})
                }
                "session.idle" => json!({"sequence":8}),
                "account.providers" => json!([]),
                "provider.catalog" => json!({"catalog":null}),
                "provider.management" => panic!("The adopted model must not be looked up as a saved Provider"),
                "provider.select" | "provider.preferences.update" => panic!("Startup must not write shared model settings"),
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
        let mut command = command(&home);
        if resume {
            command.args(["--resume", "session"]);
        } else {
            command.envs([
                ("DIM_BASE_URL", "https://gateway.example/prefix"),
                ("DIM_API_FORMAT", "gemini"),
                ("DIM_MODEL", "model"),
                ("DIM_API_KEY", "home-secret"),
                ("DIM_AUTO_COMPACT_WINDOW", "1m"),
            ]);
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until(if switched {
            "configured-provider / selected-model"
        } else {
            "session-source / model"
        });
        terminal.write(b"/new\r");
        terminal.until("new session");
        terminal.write(b"hello\r");
        terminal.until("startup fixture rejection");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
        let calls: Vec<Value> = observed.try_iter().collect();
        assert_eq!(calls.len(), if resume { 1 } else { 2 });
        if !resume {
            let params = &calls[0];
            assert_eq!(params["model"], json!({"modelId":"model"}));
            assert_eq!(
                params["startup"],
                json!({"connection":{"baseUrl":"https://gateway.example/prefix",
            "apiFormat":"gemini","apiKey":"home-secret"},"autoCompactWindow":1000000})
            );
        }
        let inherited = calls.last().unwrap();
        assert_eq!(inherited["modelFromSession"], "session");
        for key in ["model", "startup", "modelPreferences"] {
            assert!(inherited.get(key).is_none(), "{inherited}");
        }
        assert!(!terminal.transcript.contains("home-secret"));
    }
}

#[test]
fn anonymous_direct_startup_sends_no_credentials() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let create = wire.read("session.create");
        assert_eq!(create["params"]["model"], json!({"modelId":"model"}));
        assert_eq!(
            create["params"]["startup"],
            json!({"connection":{
            "baseUrl":"https://gateway.example/prefix","apiFormat":"openai-compatible"}})
        );
        reject(wire, &create);
    });
    let output = command(&home)
        .envs([
            ("DIM_BASE_URL", "https://gateway.example/prefix"),
            ("DIM_API_FORMAT", "openai-compatible"),
            ("DIM_MODEL", "model"),
        ])
        .args(["exec", "-p", "Hello"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("startup fixture rejection"));
}

#[test]
fn temporary_model_preferences_use_adopted_choices_without_a_provider_catalog() {
    for from_history in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let mut capabilities = dim_agent_types::ModelCapabilities::with_limits(64000, 2048);
            capabilities.reasoning = true;
            let snapshot = json!({"target":{"providerId":"temporary","modelId":"model"},
                "capabilities":capabilities,
                "reasoning":{"supported":true,"effortOptions":["low","high"]},
                "serviceTier":{"supported":true,"options":["default","priority"]}});
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["model"]["providerId"] = json!("temporary");
            record["modelPreferences"] = json!({"reasoningEffort":"high"});
            if !from_history {
                record["capabilitySnapshot"] = snapshot.clone();
            }
            wire.reply(&attach, record);
            let history = wire.read("session.history");
            let events = if from_history {
                let mut event = configured_event(4, "model", json!({"reasoningEffort":"high"}));
                event["data"]["provider_id"] = json!("temporary");
                event["data"]["capability_snapshot"] = snapshot;
                vec![event]
            } else {
                Vec::new()
            };
            wire.reply(
                &history,
                json!({"events":events,"through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            for patch in [
                json!({"reasoningEffort":"low"}),
                json!({"serviceTier":"priority"}),
            ] {
                let updated = wire.read("session.model.update");
                assert_eq!(
                    updated["params"],
                    json!({"sessionId":"session","branchId":"main",
                    "modelPreferences":patch})
                );
                wire.reply(&updated, json!({"outcome":"adopted"}));
            }
            wire.hold();
        });
        let mut command = command(&home);
        command.args(["--resume", "session"]);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("temporary / model · high");
        terminal.write(b"/effort\r");
        terminal.until("Current: high");
        terminal.write(b"1\r");
        terminal.until("Reasoning effort configuration adopted: low");
        terminal.write(b"/fast on\r");
        terminal.until("Service tier configuration adopted: on");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn invalid_startup_inputs_fail_before_daemon_creation_without_echoing_values() {
    let cases = [
        (
            vec![("DIM_BASE_URL", "https://example.test")],
            vec![],
            "requires both",
        ),
        (
            vec![("DIM_API_FORMAT", "anthropic")],
            vec![],
            "requires both",
        ),
        (
            vec![("DIM_API_KEY", "hidden-secret")],
            vec![],
            "require DIM_BASE_URL",
        ),
        (
            vec![("DIM_AUTH_TOKEN", "hidden-secret")],
            vec![],
            "require DIM_BASE_URL",
        ),
        (
            vec![("DIM_CUSTOM_HEADERS", "X-Test: hidden-secret")],
            vec![],
            "require DIM_BASE_URL",
        ),
        (
            vec![("DIM_MAX_CONTEXT_TOKENS", "0")],
            vec![],
            "positive integer",
        ),
        (
            vec![("DIM_MAX_OUTPUT_TOKENS", "9007199254740992")],
            vec![],
            "positive integer",
        ),
        (
            vec![("DIM_MAX_OUTPUT_TOKENS", "1.2")],
            vec![],
            "positive integer",
        ),
        (
            vec![
                ("DIM_MAX_CONTEXT_TOKENS", "4"),
                ("DIM_MAX_OUTPUT_TOKENS", "5"),
            ],
            vec![],
            "cannot exceed",
        ),
        (
            vec![("DIM_AUTO_COMPACT_WINDOW", "0k")],
            vec![],
            "positive integer",
        ),
        (
            vec![("DIM_AUTO_COMPACT_WINDOW", "9007199254740991m")],
            vec![],
            "too large",
        ),
        (vec![("DIM_MODEL", "")], vec![], "must not be empty"),
        (vec![], vec!["--provider", "fixture"], "requires --model"),
        (
            vec![],
            vec!["--autocompact", "auto", "--autocompact", "1k"],
            "only once",
        ),
    ];
    for (env, args, expected) in cases {
        let home = Home::new();
        fs::remove_dir_all(home.0.join("rust")).unwrap();
        let output = command(&home)
            .envs(env)
            .args(args)
            .args(["exec", "-p", "Hello"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("hidden-secret"), "{error}");
        assert!(!home.0.join("rust").exists());
    }
    for (name, value, expected) in [
        (
            "DIM_BASE_URL",
            "https://hidden-secret@example.test",
            "without credentials",
        ),
        ("DIM_BASE_URL", "hidden-secret", "HTTP(S)"),
        ("DIM_API_FORMAT", "hidden-secret", "DIM_API_FORMAT must be"),
        ("DIM_AUTH_TOKEN", "hidden-secret", "mutually exclusive"),
        (
            "DIM_CUSTOM_HEADERS",
            "X-Test: hidden-secret\nx-test: other",
            "repeated header",
        ),
        (
            "DIM_CUSTOM_HEADERS",
            "Bad Name: hidden-secret",
            "Invalid DIM_CUSTOM_HEADERS name",
        ),
        (
            "DIM_CUSTOM_HEADERS",
            "X-Test: hidden-secret\u{1}",
            "Invalid DIM_CUSTOM_HEADERS value",
        ),
        (
            "DIM_CUSTOM_HEADERS",
            "Authorization: hidden-secret",
            "generated authentication",
        ),
    ] {
        let home = Home::new();
        fs::remove_dir_all(home.0.join("rust")).unwrap();
        let output = command(&home)
            .envs([
                ("DIM_BASE_URL", "https://gateway.example/prefix"),
                ("DIM_API_FORMAT", "openai-chat"),
                ("DIM_API_KEY", "key-secret"),
                ("DIM_MODEL", "model"),
            ])
            .env(name, value)
            .args(["exec", "-p", "Hello"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("hidden-secret"), "{error}");
        assert!(!error.contains("key-secret"), "{error}");
        assert!(!home.0.join("rust").exists());
    }
    for args in [
        vec!["--provider", "fixture", "--model", "model"],
        vec!["--model-config", "missing.json", "--model", "model"],
        vec![],
    ] {
        let home = Home::new();
        fs::remove_dir_all(home.0.join("rust")).unwrap();
        let output = command(&home)
            .envs([
                ("DIM_BASE_URL", "https://gateway.example/prefix"),
                ("DIM_API_FORMAT", "anthropic"),
            ])
            .args(&args)
            .args(["exec", "-p", "Hello"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(if args.is_empty() {
                "requires --model"
            } else {
                "conflicts with"
            }),
            "{error}"
        );
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn launching_a_daemon_does_not_retain_the_sessions_startup_environment() {
    for (credential, management, configured) in [
        ("DIM_API_KEY", false, false),
        ("DIM_AUTH_TOKEN", false, false),
        ("DIM_API_KEY", true, false),
        ("DIM_AUTH_TOKEN", true, false),
        ("DIM_API_KEY", false, true),
    ] {
        let home = Home::new();
        let server = home.0.join("server.sh");
        let leaked = home.0.join("leaked-names");
        let retained = home.0.join("retained-env");
        fs::write(&server, format!(
            "#!/bin/sh\nfor name in {}; do\n  if printenv \"$name\" >/dev/null; then printf '%s\\n' \"$name\" >> '{}'; fi\ndone\nprintf '%s' \"$LEGACY_API_KEY\" > '{}'\nexit 1\n",
            STARTUP_ENV.join(" "), leaked.display(), retained.display()
        )).unwrap();
        fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = command(&home);
        command
            .envs([
                ("DIM_MODEL", "model"),
                ("DIM_EFFORT_LEVEL", "high"),
                ("DIM_BASE_URL", "https://gateway.example/prefix"),
                ("DIM_API_FORMAT", "anthropic"),
                ("DIM_CUSTOM_HEADERS", "X-Gateway: spawn-secret"),
                ("DIM_MAX_CONTEXT_TOKENS", "256000"),
                ("DIM_MAX_OUTPUT_TOKENS", "16000"),
                ("DIM_AUTO_COMPACT_WINDOW", "128k"),
                ("LEGACY_API_KEY", "retained"),
            ])
            .env(credential, "spawn-secret")
            .args(["--server", server.to_str().unwrap()]);
        if configured {
            for name in STARTUP_ENV {
                command.env_remove(name);
            }
            let config = home.0.join("model.json");
            fs::write(
                &config,
                r#"{"providers":[{"id":"fixture","apiKeyEnv":"LEGACY_API_KEY"}]}"#,
            )
            .unwrap();
            command.env("DIM_MODEL", "model").args([
                "--model-config",
                config.to_str().unwrap(),
                "--provider",
                "fixture",
            ]);
        }
        if management {
            command.args(["models", "fixture"]);
        } else {
            command.args(["exec", "-p", "Hello"]);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read_to_string(retained).unwrap(), "retained");
        assert!(
            !leaked.exists(),
            "The daemon inherited startup environment names"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("spawn-secret"));
        assert!(
            !fs::read_to_string(home.0.join("rust/daemon.log"))
                .unwrap()
                .contains("spawn-secret")
        );
    }
}

#[test]
fn startup_environment_is_not_read_by_help_version_or_management() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["daemon", "stop"],
    ] {
        let output = command(&home)
            .envs(STARTUP_ENV.iter().map(|name| (*name, "invalid-secret")))
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        if matches!(args[0], "--help" | "-h") {
            for name in STARTUP_ENV {
                assert!(text.contains(name), "help missing {name}");
            }
            assert!(text.contains("--autocompact"));
            assert!(text.contains("--model"));
        }
        assert!(!text.contains("invalid-secret"));
    }
    assert!(!home.0.join("rust").exists());
}

#[test]
fn configured_model_selection_preserves_legacy_creation() {
    use sha2::{Digest, Sha256};

    let home = Home::new();
    let config = home.0.join("model.json");
    fs::write(&config, "{}").unwrap();
    let digest = format!("{:x}", Sha256::digest(b"{}"));
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
            "protocolVersion":3,"daemonProfile":{"kind":"configured","modelConfigSha256":digest},
            "features":["session.create","session.history"]}),
        );
        let create = wire.read("session.create");
        assert_eq!(
            create["params"]["model"],
            json!({"provider":"fixture","modelId":"selected"})
        );
        assert!(create["params"].get("startup").is_none(), "{create}");
        reject(wire, &create);
    });
    let output = command(&home)
        .args([
            "--model-config",
            config.to_str().unwrap(),
            "--provider",
            "fixture",
            "--model",
            "selected",
            "exec",
            "-p",
            "hello",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("startup fixture rejection"));
}

#[test]
fn model_config_rejects_unsupported_effort_before_create_or_resume() {
    for resume in [false, true] {
        for environment in [false, true] {
            let home = Home::new();
            fs::remove_dir_all(home.0.join("rust")).unwrap();
            let config = home.0.join("model.json");
            fs::write(&config, "{}").unwrap();
            let mut command = command(&home);
            command.args(["--model-config", config.to_str().unwrap()]);
            if environment {
                command.env("DIM_EFFORT_LEVEL", "high");
            } else {
                command.args(["--effort", "high"]);
            }
            if resume {
                command.args(["--resume", "session"]);
            }
            let output = command.args(["exec", "-p", "Hello"]).output().unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                error.contains("catalog with capability declarations"),
                "{error}"
            );
            assert!(
                error.contains("configure the API-key model's generation settings"),
                "{error}"
            );
            assert!(!error.contains("reloading"), "{error}");
            assert!(!home.0.join("rust").exists());
        }
    }
}

#[test]
fn model_config_requires_a_provider_for_model_overrides_before_create_or_resume() {
    for resume in [false, true] {
        for environment in [false, true] {
            let home = Home::new();
            fs::remove_dir_all(home.0.join("rust")).unwrap();
            let config = home.0.join("model.json");
            fs::write(&config, "{}").unwrap();
            let mut command = command(&home);
            command.args(["--model-config", config.to_str().unwrap()]);
            if environment {
                command.env("DIM_MODEL", "selected");
            } else {
                command.args(["--model", "selected"]);
            }
            if resume {
                command.args(["--resume", "session"]);
            }
            let output = command.args(["exec", "-p", "Hello"]).output().unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("requires --provider"), "{error}");
            assert!(error.contains("--model-config"), "{error}");
            assert!(!error.contains("must appear together"), "{error}");
            assert!(!home.0.join("rust").exists());
        }
    }
}

#[test]
fn model_config_rejects_product_only_controls_before_starting_a_daemon() {
    for resume in [false, true] {
        for (args, env) in [
            (vec!["--autocompact", "auto"], vec![]),
            (vec!["--autocompact", "128k"], vec![]),
            (vec![], vec![("DIM_AUTO_COMPACT_WINDOW", "auto")]),
            (vec![], vec![("DIM_MAX_CONTEXT_TOKENS", "256000")]),
            (vec![], vec![("DIM_MAX_OUTPUT_TOKENS", "16000")]),
        ] {
            let home = Home::new();
            fs::remove_dir_all(home.0.join("rust")).unwrap();
            let config = home.0.join("model.json");
            fs::write(&config, "{}").unwrap();
            let mut command = command(&home);
            command
                .args(["--model-config", config.to_str().unwrap()])
                .args(args)
                .envs(env);
            if resume {
                command.args(["--resume", "session"]);
            }
            let output = command.args(["exec", "-p", "Hello"]).output().unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("require the product Provider"), "{error}");
            assert!(
                error.contains("cannot be used with --model-config"),
                "{error}"
            );
            assert!(!error.contains("reloading"), "{error}");
            assert!(!home.0.join("rust").exists());
        }
    }
}

#[test]
fn old_daemon_home_creation_rejects_temporary_sources_and_keeps_configured_fallback() {
    for (configured, management) in [(false, false), (false, true), (true, true)] {
        let home = Home::new();
        let provider = if configured {
            "9f556780-8461-4cda-8108-22560729b8ba"
        } else {
            "temporary-source"
        };
        let daemon = home.daemon(move |wire| {
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                let result = match request["method"].as_str().unwrap() {
                    "hello" => {
                        let mut features = vec!["session.create", "session.history", "provider.list"];
                        if management {
                            features.push("provider.management");
                        }
                        json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                            "protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features})
                    }
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["model"] = json!({"providerId":provider,"modelId":"model"});
                        record
                    }
                    "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                    "session.idle" => json!({"sequence":8}),
                    "account.providers" => json!([]),
                    "provider.management" => {
                        assert!(management);
                        assert_eq!(request["params"], json!({}), "Do not query a temporary identity as a saved Provider");
                        let providers = if configured { json!([{"providerId":provider,"source":"user",
                            "displayName":"Configured","preferences":{"model":{"reasoningEffort":"low","serviceTier":"priority"}}}]) } else { json!([]) };
                        json!({"providers":providers,"errors":[]})
                    }
                    "session.create" => {
                        assert!(configured, "A temporary source must not reach legacy creation");
                        assert_eq!(request["params"]["model"], json!({"provider":provider,"modelId":"model"}));
                        assert_eq!(request["params"]["modelPreferences"], json!({"reasoningEffort":"low","serviceTier":"priority"}));
                        assert!(request["params"].get("startup").is_none());
                        assert!(request["params"].get("modelFromSession").is_none());
                        reject(wire, &request);
                        continue;
                    }
                    "provider.select" | "provider.preferences.update" => panic!("Startup must not mutate shared preferences"),
                    _ => json!({}),
                };
                wire.reply(&request, result);
            }
        });
        let mut command = command(&home);
        command.args(["--resume", "session"]);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until(&format!("{provider} / model"));
        terminal.write(b"/new\r");
        terminal.until("new session");
        terminal.write(b"hello\r");
        if configured {
            terminal.until("startup fixture rejection");
        } else {
            terminal.until("Restart the daemon");
            terminal.until("DIM_BASE_URL and DIM_API_FORMAT");
            if management {
                terminal.until("before starting a new Session");
            } else {
                assert!(!terminal.transcript.contains("or select a configured"));
            }
            assert!(
                !terminal
                    .transcript
                    .contains("Provider management entry is missing")
            );
        }
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}
