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
fn home_browsing_and_exit_need_no_session_in_both_tui_modes() {
    for fullscreen in [false, true] {
        for leaf in [false, true] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(if leaf { &["ui.leaf.2"] } else { &[] });
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                assert_eq!(
                    wire.trailing_request(),
                    None,
                    "Home must not issue Session or UI attachment RPCs"
                );
            });
            let mut command = command(&home);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("Welcome to DIM");
            terminal.write(b"/permissions\r");
            terminal.until("Full access");
            terminal.write(b"\x1b[27u");
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
        }
    }
}

#[test]
fn first_shell_command_creates_once_without_submitting_a_model_turn() {
    for fullscreen in [false, true] {
        for private in [false, true] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(&[]);
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                wire.created();
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
                for text in ["printf first-shell", "printf second-shell"] {
                    let run = wire.read("shell.run");
                    assert_eq!(run["params"]["sessionId"], "session");
                    assert_eq!(run["params"]["branchId"], "main");
                    assert_eq!(run["params"]["command"], text);
                    assert_eq!(run["params"]["mode"], if private { "private" } else { "record" });
                    wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":format!("{text}-output\n"),"truncated":false,"durationMs":1}));
                    if text == "printf first-shell" {
                        wire.no_context();
                    }
                }
                let submit = wire.read("session.submit");
                assert_eq!(submit["params"]["sessionId"], "session");
                assert_eq!(submit["params"]["text"], "next message");
                wire.reply(&submit, json!({"event":{}}));
                wire.event(9, "text", json!({"turn_id":"next","request_id":"model","delta":"message accepted"}));
                assert_eq!(wire.trailing_request(), None);
            });
            let mut command = command(&home);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("Welcome to DIM");
            terminal.write(if private {
                b"!!printf first-shell\r"
            } else {
                b"!printf first-shell\r"
            });
            terminal.until("first-shell-output");
            terminal.write(if private {
                b"!!printf second-shell\r"
            } else {
                b"!printf second-shell\r"
            });
            terminal.until("second-shell-output");
            terminal.write(b"next message\r");
            terminal.until("message accepted");
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
            assert!(!terminal.transcript.contains("Sign in with /login"));
        }
    }
}

#[test]
fn home_previews_the_model_without_creating_a_session_and_survives_preview_errors() {
    for fullscreen in [false, true] {
        for outcome in ["ready", "rejected", "preview-timeout"] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(&["session.model.preview"]);
                let preview = wire.read("session.model.preview");
                assert!(preview["params"].get("model").is_none());
                assert!(preview["params"].get("permissionPolicy").is_none());
                assert!(
                    preview["params"].get("modelPreferences").is_none(),
                    "The server applies the model's saved preferences; the client sends none: {}",
                    preview["params"]
                );
                if outcome == "rejected" {
                    reject(wire, &preview);
                } else if outcome == "ready" {
                    wire.reply(&preview, json!({"model":{"providerId":"preview-provider","modelId":"preview-model"},"modelPreferences":{"reasoningEffort":"high"}}));
                }
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                assert_eq!(wire.trailing_request(), None, "Preview and browsing must not create or attach a Session");
            });
            let mut command = command(&home);
            command.args(["--permission", "read-only"]);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until(if outcome == "ready" {
                "preview-provider / preview-model"
            } else {
                "Cannot preview startup model"
            });
            if outcome == "ready" {
                terminal.until("high");
            }
            terminal.write(b"/permissions\r");
            terminal.until("Full access");
            terminal.write(b"\x1b[27u");
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
        }
    }
}

/// The `provider.catalog` reply for a model that advertises `low`, `high`
/// and reasoning off, and both service tiers.
fn preview_catalog() -> Value {
    json!({"catalog":{"version":1,"disabledModelIds":[],"models":[{
        "id":"preview-model","name":"Preview model",
        "capabilities":{"modalities":["text"],"toolCalling":true,"streaming":true,
            "structuredOutput":true,"vision":false,"audio":false,"reasoning":true,
            "contextWindow":128000,"maxOutputTokens":16384},
        "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"],
                "off":{"kind":"thinking_type","value":"disabled"}},
            "serviceTier":{"supported":true,"options":["default","priority"]}}}]}})
}

/// Bare `/effort` at Home lists the preview's advertised efforts, with the
/// preview's own preference marked current.
#[test]
fn bare_effort_at_home_lists_the_preview_models_choices() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.model.preview"]);
            let preview = wire.read("session.model.preview");
            wire.reply(
                &preview,
                json!({"model":{"providerId":"preview-provider","modelId":"preview-model"},
                "modelPreferences":{"reasoningEffort":"high"}}),
            );
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let catalog = wire.read("provider.catalog");
            assert_eq!(
                catalog["params"],
                json!({"providerId":"preview-provider","modelId":"preview-model"})
            );
            wire.reply(&catalog, preview_catalog());
            assert_eq!(
                wire.trailing_request(),
                None,
                "listing choices writes nothing"
            );
        });
        let mut command = command(&home);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("preview-provider / preview-model · high");
        terminal.write(b"/effort\r");
        terminal.until("Current: high");
        terminal.until("low");
        terminal.write(b"\x1b[27u");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(!terminal.transcript.contains("adopted model"));
    }
}

/// A Home with neither a preview nor a model says what to do instead of
/// waiting for choices from a model it does not show.
#[test]
fn bare_effort_at_home_without_a_model_says_what_to_do() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        assert_eq!(wire.trailing_request(), None, "no model to ask for choices");
    });
    let mut terminal = Terminal::launch_tui(command(&home));
    terminal.until("Welcome to DIM");
    terminal.write(b"/effort\r");
    terminal.until("No model to configure");
    terminal.until("choose a model with /models");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("adopted model"));
}

/// A choice made at Home reaches the Session the first message creates, and
/// writes no saved preference.
#[test]
fn a_choice_at_home_reaches_the_first_session_and_saves_nothing() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let original = fs::read(home.0.join("config.json")).unwrap();
        let (created, creations) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.model.preview"]);
            let preview = wire.read("session.model.preview");
            wire.reply(
                &preview,
                json!({"model":{"providerId":"preview-provider","modelId":"preview-model"},
                "modelPreferences":{"reasoningEffort":"high"}}),
            );
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                if wire.2 && wire.empty_state(&request) {
                    continue;
                }
                match request["method"].as_str().unwrap() {
                    "account.providers" => wire.reply(&request, json!([])),
                    "provider.catalog" => wire.reply(&request, preview_catalog()),
                    "session.create" => {
                        created.send(request["params"].clone()).ok();
                        wire.reply(
                            &request,
                            json!({"sessionId":"session","sequence":8,
                            "model":{"providerId":"preview-provider","modelId":"preview-model"}}),
                        );
                    }
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["model"] =
                            json!({"providerId":"preview-provider","modelId":"preview-model"});
                        record["modelPreferences"] = json!({"reasoningEffort":"low"});
                        wire.reply(&request, record);
                    }
                    "session.history" => wire.reply(
                        &request,
                        json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                    ),
                    "session.submit" => wire.reply(&request, json!({"event":{}})),
                    "session.detach" => wire.reply(&request, json!({})),
                    "provider.select" | "provider.selection" | "provider.preferences.update" => {
                        panic!("Home's choice must not write a saved preference")
                    }
                    method => wire.reply(&request, json!({"method":method})),
                }
            }
        });
        let mut command = command(&home);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("preview-provider / preview-model · high");
        terminal.write(b"/effort\r");
        terminal.until("Current: high");
        // The dialog paints `low` above the current `high`.
        terminal.write(b"\x1b[A\r");
        terminal.until("Reasoning effort for the first Session: low");
        terminal.until("preview-provider / preview-model · low");
        terminal.write(b"after the choice\r");
        let params = creations
            .recv_timeout(PATIENCE)
            .expect("the first message created a Session");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
        assert_eq!(
            params["modelPreferences"],
            json!({"reasoningEffort":"low"}),
            "the effort chosen at Home is the creation's preference"
        );
        assert_eq!(fs::read(home.0.join("config.json")).unwrap(), original);
    }
}

/// `/models` at Home drops the effort Home chose, because it was chosen for
/// the model the picker replaced.
#[test]
fn a_model_picked_at_home_drops_the_effort_home_chose() {
    let home = Home::new();
    let (created, creations) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.model.preview"]);
        let preview = wire.read("session.model.preview");
        wire.reply(
            &preview,
            json!({"model":{"providerId":"preview-provider","modelId":"preview-model"},
            "modelPreferences":{"reasoningEffort":"high"}}),
        );
        let mut picked = preview_catalog();
        picked["catalog"]["models"][0]["id"] = json!("other-model");
        picked["catalog"]["models"][0]["name"] = json!("Other model");
        // The preview's own catalog names the model Home shows, until
        // `/models` opens and the picker's provider lists another.
        let previewed = preview_catalog();
        let mut picking = false;
        let mut read = false;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|count| count > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            if wire.2 && wire.empty_state(&request) {
                continue;
            }
            match request["method"].as_str().unwrap() {
                "account.providers" => wire.reply(&request, json!([])),
                "provider.models.query" => {
                    picking = true;
                    wire.reply(
                        &request,
                        super::models::picker_page("preview-provider", "Preview", picked.clone()),
                    );
                }
                "provider.catalog" => {
                    let listed = if picking {
                        picked.clone()
                    } else {
                        previewed.clone()
                    };
                    wire.reply(&request, listed);
                }
                "provider.selection" => {
                    read = !read;
                    wire.reply(
                        &request,
                        json!({"selection": if read { json!({"version":1}) } else { Value::Null }}),
                    );
                }
                "provider.select" | "provider.preferences.update" => {
                    wire.reply(&request, json!({}))
                }
                "session.create" => {
                    created.send(request["params"].clone()).unwrap();
                    wire.reply(
                        &request,
                        json!({"sessionId":"session","sequence":8,
                        "model":{"providerId":"preview-provider","modelId":"other-model"}}),
                    );
                }
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["model"] =
                        json!({"providerId":"preview-provider","modelId":"other-model"});
                    wire.reply(&request, record);
                }
                "session.history" => wire.reply(
                    &request,
                    json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                ),
                "session.submit" => wire.reply(&request, json!({"event":{}})),
                "session.detach" => wire.reply(&request, json!({})),
                method => wire.reply(&request, json!({"method":method})),
            }
        }
    });
    let mut terminal = Terminal::launch_tui(command(&home));
    terminal.until("preview-provider / preview-model · high");
    terminal.write(b"/effort\r");
    terminal.until("Current: high");
    terminal.write(b"\x1b[A\r");
    terminal.until("Reasoning effort for the first Session: low");
    terminal.write(b"\x1b[27u/models\r");
    terminal.until("Preview / other-model");
    terminal.write(b"\r");
    terminal.write(b"first message\r");
    let params = creations
        .recv_timeout(PATIENCE)
        .expect("the first message created a Session");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    assert_eq!(
        params["model"],
        json!({"provider":"preview-provider","modelId":"other-model"}),
        "the model the picker chose is the creation's model"
    );
    assert!(
        params.get("modelPreferences").is_none(),
        "the effort chosen for the previous model is gone: {params}"
    );
}

/// A Home reached by `/new` from a Session inherits that Session's model
/// through `modelFromSession`, which a creation's preferences cannot
/// accompany, so a value says where it applies instead of never arriving.
#[test]
fn home_that_inherits_a_model_refuses_a_preference_choice() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.model.preview", "session.create.modelFromSession"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|count| count > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            if wire.2 && wire.empty_state(&request) {
                continue;
            }
            match request["method"].as_str().unwrap() {
                "account.providers" => wire.reply(&request, json!([])),
                "provider.catalog" => {
                    let mut reply = preview_catalog();
                    reply["catalog"]["models"][0]["id"] = json!("model");
                    wire.reply(&request, reply);
                }
                "session.detach" => wire.reply(&request, json!({})),
                method @ ("session.create" | "session.submit" | "provider.select") => {
                    panic!("a Home that inherits its model writes nothing: {method}")
                }
                method => wire.reply(&request, json!({"method":method})),
            }
        }
    });
    let mut terminal = Terminal::resume_tui(command(&home));
    terminal.until("test / model");
    terminal.write(b"/new\r");
    terminal.until("─ new session ─");
    terminal.write(b"/effort high\r");
    terminal.until("The next Session inherits its model");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Bare `/fast` at Home lists the tiers the preview advertises; a valued form
/// is held for the first Session, and refused when the model advertises no
/// tiers.
#[test]
fn fast_at_home_lists_tiers_and_holds_a_choice_for_the_first_session() {
    for tiers in [true, false] {
        let home = Home::new();
        let (created, creations) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.model.preview"]);
            let preview = wire.read("session.model.preview");
            wire.reply(
                &preview,
                json!({"model":{"providerId":"preview-provider","modelId":"preview-model"}}),
            );
            let mut reply = preview_catalog();
            if !tiers {
                reply["catalog"]["models"][0]["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("serviceTier");
            }
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|count| count > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                if wire.2 && wire.empty_state(&request) {
                    continue;
                }
                match request["method"].as_str().unwrap() {
                    "account.providers" => wire.reply(&request, json!([])),
                    "provider.catalog" => wire.reply(&request, reply.clone()),
                    "session.create" => {
                        created.send(request["params"].clone()).unwrap();
                        wire.reply(
                            &request,
                            json!({"sessionId":"session","sequence":8,
                            "model":{"providerId":"preview-provider","modelId":"preview-model"}}),
                        );
                    }
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["model"] =
                            json!({"providerId":"preview-provider","modelId":"preview-model"});
                        wire.reply(&request, record);
                    }
                    "session.history" => wire.reply(
                        &request,
                        json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                    ),
                    "session.submit" => wire.reply(&request, json!({"event":{}})),
                    "session.detach" => wire.reply(&request, json!({})),
                    "provider.select" | "provider.selection" | "provider.preferences.update" => {
                        panic!("Home's choice must not write a saved preference")
                    }
                    method => wire.reply(&request, json!({"method":method})),
                }
            }
        });
        let mut terminal = Terminal::launch_tui(command(&home));
        terminal.until("preview-provider / preview-model");
        if tiers {
            terminal.write(b"/fast\r");
            terminal.until("Service tier choices: on (priority), off (default)");
            terminal.write(b"/fast on\r");
            terminal.until("Service tier for the first Session: on");
            terminal.write(b"first message\r");
        } else {
            terminal.write(b"/fast\r");
            terminal.until("This model does not advertise priority/default service tiers");
        }
        if tiers {
            let params = creations
                .recv_timeout(PATIENCE)
                .expect("the first message created a Session");
            assert_eq!(
                params["modelPreferences"],
                json!({"serviceTier":"priority"}),
                "the tier chosen at Home is the creation's preference"
            );
        }
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
        if !tiers {
            assert!(
                creations.try_recv().is_err(),
                "a refused value starts no Session"
            );
        }
    }
}

#[test]
fn first_text_or_draft_creates_once_with_startup_options_in_both_tui_modes() {
    for fullscreen in [false, true] {
        for draft in [false, true] {
            let home = Home::new();
            let original = fs::read(home.0.join("config.json")).unwrap();
            let long = "x".repeat(16 * 1024 + 1);
            let expected_long = long.clone();
            let daemon = home.daemon(move |wire| {
                wire.hello(&["session.model.preview"]);
                let preview = wire.read("session.model.preview");
                wire.reply(&preview, json!({"model":{"providerId":"temporary-preview","modelId":"first-model"},"modelPreferences":{"reasoningEffort":"high"}}));
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                let create = wire.created();
                let mut model_params = create["params"].clone();
                model_params.as_object_mut().unwrap().remove("permissionPolicy");
                assert_eq!(preview["params"], model_params, "Preview must not replace launch options with its resolved identity");
                assert_eq!(create["params"]["model"], json!({"modelId":"first-model"}));
                assert_eq!(create["params"]["modelPreferences"], json!({"reasoningEffort":"high"}));
                assert_eq!(create["params"]["permissionPolicy"], json!({"preset":"read-only"}));
                assert_eq!(create["params"]["startup"], json!({"connection":{
                    "baseUrl":"https://gateway.example/prefix","apiFormat":"openai-compatible",
                    "apiKey":"first-secret"},"autoCompactWindow":128000}));
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
                if draft {
                    let put = wire.read("attachment.put");
                    assert_eq!(put["params"], json!({"sessionId":"session","text":expected_long}));
                    wire.reply(&put, json!({"attachmentId":"first-paste","path":"/files/paste.txt","bytes":16385}));
                }
                let submit = wire.read("session.submit");
                assert_eq!(submit["params"]["sessionId"], "session");
                assert_eq!(submit["params"]["text"], if draft { "[Pasted text: /files/paste.txt (1 lines)]" } else { "first text" });
                wire.reply(&submit, json!({"event":{}}));
                wire.event(9, "text", json!({"turn_id":"first","request_id":"first-model","delta":"first input accepted"}));
                wire.no_context();
                let submit = wire.read("session.submit");
                assert_eq!(submit["params"]["sessionId"], "session");
                assert_eq!(submit["params"]["text"], "second text");
                wire.reply(&submit, json!({"event":{}}));
                wire.event(10, "text", json!({"turn_id":"second","request_id":"second-model","delta":"second input accepted"}));
                assert_eq!(wire.trailing_request(), None, "subsequent input must reuse the Session");
            });
            let mut command = command(&home);
            command
                .envs([
                    ("DIM_BASE_URL", "https://gateway.example/prefix"),
                    ("DIM_API_FORMAT", "openai-compatible"),
                    ("DIM_API_KEY", "first-secret"),
                ])
                .args([
                    "--model",
                    "first-model",
                    "--effort",
                    "high",
                    "--autocompact",
                    "128k",
                    "--permission",
                    "read-only",
                ]);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("temporary-preview / first-model");
            if draft {
                terminal.paste(&long);
                terminal.until("16385 chars");
                terminal.write(b"\r");
            } else {
                terminal.write(b"first text\r");
            }
            terminal.until("first input accepted");
            terminal.write(b"second text\r");
            terminal.until("second input accepted");
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
            assert_eq!(fs::read(home.0.join("config.json")).unwrap(), original);
            assert!(!terminal.transcript.contains("first-secret"));
        }
    }
}

#[test]
fn a_refused_leaf_start_shows_the_error_restores_the_terminal_and_exits() {
    for fullscreen in [false, true] {
        for queued in ["", "second message\r", "/new\rqueued after new\r"] {
            let home = Home::new();
            let (opening, opened) = std::sync::mpsc::channel();
            let (reject, rejected) = std::sync::mpsc::channel();
            let daemon = home.daemon(move |wire| {
                wire.hello(&["ui.leaf.2"]);
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                wire.created();
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let open = wire.read("ui.open");
                opening.send(()).unwrap();
                rejected.recv_timeout(PATIENCE).unwrap();
                wire.send(json!({"jsonrpc":"2.0","id":open["id"],"error":{
                    "code":-32000,"message":"UI startup refused","data":{"appError":"ui_leaf_failed"}}}));
                assert_eq!(wire.trailing_request(), None, "a failed UI must not execute queued commands");
            });
            let mut command = command(&home);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("Welcome to DIM");
            terminal.write(b"first message\r");
            opened.recv_timeout(PATIENCE).unwrap();
            terminal.write(queued.as_bytes());
            terminal.write(b"/permissions\r");
            terminal.until("Full access");
            reject.send(()).unwrap();
            terminal.until("UI startup refused");
            assert!(!terminal.wait().success());
            assert_ne!(terminal.local_mode() & libc::ICANON, 0);
            assert_ne!(terminal.local_mode() & libc::ECHO, 0);
            assert!(!terminal.transcript.contains("panicked"));
            daemon.join().unwrap();
        }
    }
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
        let (params, observed) = std::sync::mpsc::channel();
        for (tui, leaf) in [(false, false), (true, false), (true, true)] {
            let home = Home::new();
            let original = fs::read(home.0.join("config.json")).unwrap();
            let params = params.clone();
            let daemon = home.daemon(move |wire| {
                wire.hello(if leaf { &["ui.leaf.2"] } else { &[] });
                if tui {
                    let providers = wire.read("account.providers");
                    wire.reply(&providers, json!([]));
                }
                let create = wire.read("session.create");
                params
                    .send(serde_json::to_vec(&create["params"]).unwrap())
                    .unwrap();
                assert_eq!(
                    create["params"]["permissionPolicy"],
                    json!({"preset":"read-only"})
                );
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
                if tui {
                    let retry = wire.read("session.create");
                    assert_eq!(
                        retry["params"], create["params"],
                        "retry must preserve startup options"
                    );
                    wire.send(json!({"jsonrpc":"2.0","id":retry["id"],"error":{
                        "code":-32000,"message":"startup retry fixture rejection","data":{"appError":"provider_failed"}}}));
                    assert_eq!(wire.trailing_request(), None, "a rejected creation must not attach or submit");
                }
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
                    "--permission",
                    "read-only",
                ]);
            let text = if tui {
                let mut terminal = Terminal::launch_tui(command);
                terminal.until("Ask anything...");
                terminal.write(b"Hello\r");
                terminal.until("startup fixture rejection");
                assert!(terminal.child.try_wait().unwrap().is_none());
                terminal.write(b"retry\r");
                terminal.until("startup retry fixture rejection");
                assert!(terminal.child.try_wait().unwrap().is_none());
                terminal.signal(libc::SIGKILL);
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
        let line = observed.recv().unwrap();
        assert_eq!(
            line,
            observed.recv().unwrap(),
            "line and TUI creation params"
        );
        assert_eq!(
            line,
            observed.recv().unwrap(),
            "line and leaf creation params"
        );
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
                    // The launch attaches before it adopts, and the attach
                    // opens the UI and the conversation's window first.
                    loop {
                        let next = wire.read_any();
                        match next["method"].as_str().unwrap_or_default() {
                            "ui.open" => {
                                wire.reply(&next, json!({"uiId":"ui-1","token":"token-1"}))
                            }
                            "session.history" => wire.reply(
                                &next,
                                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                            ),
                            "session.model.update" => break next,
                            other => panic!("unexpected {other}: {next}"),
                        }
                    }
                } else {
                    assert_eq!(request["method"], "session.model.update");
                    request
                };
                let mut expected = json!({"sessionId":"session",
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
                assert_eq!(terminal.wait().code(), Some(1));
                assert_ne!(terminal.local_mode() & libc::ICANON, 0);
                assert_ne!(terminal.local_mode() & libc::ECHO, 0);
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
            json!({"sessionId":"session",
            "modelPreferences":{"reasoningEffort":"high"},"startup":{}})
        );
        wire.reply(&update, json!({"outcome":"adopted"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_pending_questions(8);
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
        wire.2 = true;
        let mut creates = 0;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            if wire.2 && wire.empty_state(&request) { continue; }
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                    "protocolVersion":4,"daemonProfile":{"kind":"product"},
                    "features":["session.create","session.create.modelFromSession","session.history","session.history.latestTurn","session.input.pending.blockPresence","session.methods","provider.list"]}),
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
                    if switched { record["model"]=json!({"providerId":"configured-provider","modelId":"selected-model"}); record["modelPreferences"]=json!({"reasoningEffort":"low"}); }
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
                "session.submit" => {
                    assert!(!resume, "the resumed fixture starts without submitting input");
                    assert_eq!(request["params"]["text"], "initial message");
                    json!({"event":{}})
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
        if !resume {
            terminal.until("Ask anything...");
            terminal.write(b"initial message\r");
        }
        terminal.until(if switched {
            "configured-provider / selected-model"
        } else {
            "session-source / model"
        });
        terminal.write(b"/new\r");
        // The palette row ("start a new session") carries the words before
        // `/new` runs; the boundary rule follows the switch's composer clear.
        terminal.until("─ new session ─");
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
fn an_output_limit_at_or_above_the_context_limit_reaches_the_daemon() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let create = wire.read("session.create");
        assert_eq!(
            create["params"]["startup"],
            json!({"connection":{
            "baseUrl":"https://gateway.example/prefix","apiFormat":"openai-compatible"},
            "contextWindow":4,"maxOutputTokens":5})
        );
        reject(wire, &create);
    });
    let output = command(&home)
        .envs([
            ("DIM_BASE_URL", "https://gateway.example/prefix"),
            ("DIM_API_FORMAT", "openai-compatible"),
            ("DIM_MODEL", "model"),
            ("DIM_MAX_CONTEXT_TOKENS", "4"),
            ("DIM_MAX_OUTPUT_TOKENS", "5"),
        ])
        .args(["exec", "-p", "Hello"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("startup fixture rejection"), "{text}");
    assert!(!text.contains("cannot exceed"), "{text}");
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
            record["capabilitySnapshot"] = snapshot.clone();
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
            assert!(text.contains("--autocompact"));
            assert!(text.contains("--model"));
            let (_, entries) = text.split_once("\nCommands:\n").unwrap();
            let (entries, _) = entries.split_once("\nModel startup:\n").unwrap();
            for line in entries.lines().filter(|line| !line.is_empty()) {
                assert!(
                    line == "Options:" || line.starts_with("  "),
                    "help entries and descriptions must be indented: {line:?}"
                );
            }
            assert!(text.contains("\n                                  -p sends TEXT"));
            assert!(text.contains("\n  dimcode exec --model model-id --effort high"));
            for line in text.lines() {
                assert!(line.len() <= 80, "help exceeds 80 columns: {line:?}");
            }
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
        wire.2 = true;
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
            "protocolVersion":4,"daemonProfile":{"kind":"configured","modelConfigSha256":digest},
            "features":["session.create","session.history","session.history.latestTurn","session.methods"]}),
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
fn home_creation_uses_the_advertised_product_or_configured_model() {
    use sha2::{Digest, Sha256};

    for (inheritance, configured, management) in [
        (true, false, false),
        (true, false, true),
        (true, true, true),
        (false, true, false),
    ] {
        let home = Home::new();
        let provider = if configured {
            "9f556780-8461-4cda-8108-22560729b8ba"
        } else {
            "temporary-source"
        };
        let config = home.0.join("model.json");
        let profile = if inheritance {
            json!({"kind":"product"})
        } else {
            fs::write(&config, "{}").unwrap();
            json!({"kind":"configured","modelConfigSha256":format!("{:x}", Sha256::digest(b"{}"))})
        };
        let daemon = home.daemon(move |wire| {
            wire.2 = true;
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                if wire.2 && wire.empty_state(&request) {
                    continue;
                }
                let result = match request["method"].as_str().unwrap() {
                    "hello" => {
                        let mut features = vec![
                            "session.create",
                            "session.history",
                            "session.history.latestTurn",
                            "session.input.pending.blockPresence",
                            "session.methods",
                            "provider.list",
                        ];
                        if inheritance {
                            features.push("session.create.modelFromSession");
                        }
                        if management {
                            features.push("provider.management");
                        }
                        json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),
                            "protocolVersion":4,"daemonProfile":profile,"features":features})
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
                        panic!("Session inheritance must not read Provider management")
                    }
                    "session.create" => {
                        if inheritance {
                            assert_eq!(request["params"]["modelFromSession"], "session");
                            assert!(request["params"].get("model").is_none());
                        } else {
                            assert_eq!(
                                request["params"]["model"],
                                json!({"provider":provider,"modelId":"model"})
                            );
                            assert!(request["params"].get("modelFromSession").is_none());
                        }
                        assert!(request["params"].get("modelPreferences").is_none());
                        reject(wire, &request);
                        continue;
                    }
                    "provider.select" | "provider.preferences.update" => {
                        panic!("Startup must not mutate shared preferences")
                    }
                    _ => json!({}),
                };
                wire.reply(&request, result);
            }
        });
        let mut command = command(&home);
        command.args(["--resume", "session"]);
        if !inheritance {
            command.args(["--model-config", config.to_str().unwrap()]);
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.resize(160, 32);
        terminal.until(&format!("{provider} / model"));
        terminal.write(b"/new\r");
        // The palette row ("start a new session") carries the words before
        // `/new` runs; the boundary rule follows the switch's composer clear.
        terminal.until("─ new session ─");
        terminal.write(b"hello\r");
        terminal.until("startup fixture rejection");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `-p` on `tui` is the interface's first message: one submission, and the
/// interface stays open for the next one.
#[test]
fn a_first_prompt_is_submitted_once_and_the_interface_stays() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (submitted, seen) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "hello");
            assert_eq!(submit["params"]["mode"], "queue");
            wire.reply(&submit, json!({"event":{}}));
            submitted.send(()).unwrap();
            wire.hold();
        });
        let mut command = command(&home);
        command.args([
            "tui",
            if fullscreen {
                "--fullscreen"
            } else {
                "--scrollback"
            },
            "-p",
            "hello",
        ]);
        let mut terminal = Terminal::launch_tui(command);
        seen.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        let status = terminal.wait();
        daemon.join().unwrap();
        assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
    }
}

/// `-p` with `--image` submits the text and the image together through the
/// same composer a typed message uses.
#[test]
fn a_first_prompt_submits_its_image_with_the_text() {
    let home = Home::new();
    let image = home.0.join("shot.png");
    let bytes = image_fixture(image::ImageFormat::Png, 3, 2);
    fs::write(&image, &bytes).unwrap();
    let expected = json!([
        {"type":"text","text":"hello "},
        {"type":"image","source":{
            "type":"file","mediaType":"image/png","fileId":"image-file"}},
    ]);
    let (submitted, seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let import = wire.read("attachment.import");
        assert_eq!(import["params"]["sessionId"], "session");
        assert_eq!(import["params"]["image"], true);
        assert_eq!(fs::read(import["params"]["path"].as_str().unwrap()).unwrap(), bytes);
        wire.reply(&import, json!({"attachmentId":"image-file","path":"/owned/image","bytes":bytes.len(),"image":{"type":"file","mediaType":"image/png","fileId":"image-file"}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["mode"], "queue");
        assert!(
            submit["params"].get("text").is_none(),
            "the image must travel as user content: {submit}"
        );
        assert_eq!(submit["params"]["user"]["content"], expected);
        wire.reply(&submit, json!({"event":{}}));
        submitted.send(()).unwrap();
        wire.hold();
    });
    let mut command = command(&home);
    command.args([
        "tui",
        "--scrollback",
        "-p",
        "hello",
        "--image",
        image.to_str().unwrap(),
    ]);
    let mut terminal = Terminal::launch_tui(command);
    seen.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A first prompt on a resumed Session attaches first and submits into it.
#[test]
fn a_first_prompt_on_a_resumed_session_submits_into_it() {
    let home = Home::new();
    let (submitted, seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
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
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        assert_eq!(submit["params"]["text"], "continue");
        wire.reply(&submit, json!({"event":{}}));
        submitted.send(()).unwrap();
        wire.hold();
    });
    let mut command = command(&home);
    command.args([
        "tui",
        "--scrollback",
        "--resume",
        "session",
        "-p",
        "continue",
    ]);
    let mut terminal = Terminal::launch_tui(command);
    seen.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A first prompt that names a command runs it instead of sending it.
#[test]
fn a_first_prompt_that_names_a_command_runs_it() {
    let home = Home::new();
    let (settled, seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        settled.send(wire.trailing_request()).unwrap();
    });
    let mut command = command(&home);
    command.args(["tui", "--scrollback", "-p", "/permissions"]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Full access");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    let trailing = seen.recv_timeout(PATIENCE).unwrap();
    assert!(
        trailing.as_deref().is_none_or(
            |frame| !frame.contains("session.create") && !frame.contains("session.submit")
        ),
        "a command must run instead of starting a Session: {trailing:?}"
    );
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A first message no Session can take stays in the composer.
#[test]
fn a_first_prompt_without_a_session_stays_in_the_draft() {
    let home = Home::new();
    let (settled, seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let create = wire.read("session.create");
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{
            "code":-32000,"message":"No available model","data":{"appError":"provider_failed"}}}));
        settled.send(wire.trailing_request()).unwrap();
    });
    let mut command = command(&home);
    command.args(["tui", "--scrollback", "-p", "hello"]);
    let mut terminal = Terminal::launch_tui(command);
    // The Home hint arrives with the first frame, before the message is
    // submitted; the draft follows the refusal.
    terminal.until("Sign in with /login");
    terminal.until("hello");
    // The unsent message is in the composer, where a typed command would
    // join it; clear it before naming the exit command.
    terminal.write(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    let trailing = seen.recv_timeout(PATIENCE).unwrap();
    assert!(
        trailing
            .as_deref()
            .is_none_or(|frame| !frame.contains("session.submit")),
        "a message no Session can take was submitted anyway: {trailing:?}"
    );
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A first message stays in the composer when the resumed Session cannot
/// attach, the other way no Session takes it.
#[test]
fn a_first_prompt_is_kept_when_the_resumed_session_cannot_attach() {
    let home = Home::new();
    let (settled, seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.send(json!({"jsonrpc":"2.0","id":attach["id"],"error":{
            "code":-32000,"message":"Session not found","data":{"appError":"session_not_found"}}}));
        settled.send(wire.trailing_request()).unwrap();
    });
    let mut command = command(&home);
    command.args(["tui", "--scrollback", "--resume", "gone", "-p", "hello"]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("hello");
    // A failed session ends on Ctrl+C, which keeps the draft on screen
    // instead of taking an exit command's text.
    terminal.write(b"\x03");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    let trailing = seen.recv_timeout(PATIENCE).unwrap();
    assert!(
        trailing
            .as_deref()
            .is_none_or(|frame| !frame.contains("session.submit")),
        "a message no Session can take was submitted anyway: {trailing:?}"
    );
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A draft the user starts before the refusal returns keeps the composer,
/// and the refused first message is reported instead of silently dropped.
#[test]
fn a_first_prompt_a_new_draft_keeps_out_is_reported() {
    let home = Home::new();
    let (created, refusing) = std::sync::mpsc::channel();
    let (release, answered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let create = wire.read("session.create");
        created.send(()).unwrap();
        // The user starts another draft while the creation is in flight.
        answered.recv_timeout(PATIENCE).unwrap();
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{
            "code":-32000,"message":"No available model","data":{"appError":"provider_failed"}}}));
        wire.hold();
    });
    let mut command = command(&home);
    command.args(["tui", "--scrollback", "-p", "hello"]);
    let mut terminal = Terminal::launch_tui(command);
    refusing.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"later draft");
    terminal.until("later draft");
    release.send(()).unwrap();
    terminal.until("The first message was not sent; your new draft was kept.");
    // The newer draft still holds the composer; the refused message was
    // reported, not restored over it.
    terminal.until("later draft");
    terminal.write(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(0), "{}", terminal.transcript);
}

/// A model server that records the User-Agent of every chat request it
/// receives, answers the model list, and answers each turn with "ok". A test
/// stops it with [`IdentityFixture::stop`] once the CLI has exited.
struct IdentityFixture {
    address: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    model: thread::JoinHandle<()>,
}

impl IdentityFixture {
    fn start() -> Self {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (recorded, stopping) = (seen.clone(), stop.clone());
        let model = thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let deadline = Instant::now() + PATIENCE;
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stopping.load(std::sync::atomic::Ordering::SeqCst) {
                                return;
                            }
                            assert!(
                                Instant::now() < deadline,
                                "the model was not requested for {PATIENCE:?}"
                            );
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(PATIENCE)).unwrap();
                let mut stream = std::io::BufReader::new(stream);
                let mut request_line = String::new();
                assert!(stream.read_line(&mut request_line).unwrap() > 0);
                let mut length = 0;
                let mut user_agent = String::new();
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("user-agent:") {
                        user_agent = value.trim().to_owned();
                    }
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let path = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                if path.ends_with("/models") {
                    let list = json!({"data":[{"id":"fixture-model"}]}).to_string();
                    write!(
                        stream.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{list}",
                        list.len()
                    )
                    .unwrap();
                    stream.get_mut().flush().unwrap();
                    continue;
                }
                recorded.lock().unwrap().push(user_agent);
                let frame = |delta, finish| {
                    format!(
                        "data: {}\n\n",
                        json!({"id":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                    )
                };
                let data = frame(json!({"content":"ok"}), Value::Null)
                    + &frame(json!({}), json!("stop"))
                    + "data: [DONE]\n\n";
                write!(
                    stream.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
                    data.len()
                )
                .unwrap();
                stream.get_mut().flush().unwrap();
            }
        });
        Self {
            address,
            seen,
            stop,
            model,
        }
    }

    /// The User-Agent of every chat request so far.
    fn user_agents(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// Ends the server and joins it.
    fn stop(self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.model.join().unwrap();
    }
}

/// The model config that points a started process at `address`.
fn identity_model(home: &Home, address: std::net::SocketAddr) -> PathBuf {
    let config = home.0.join("model.json");
    fs::write(&config, json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string()).unwrap();
    config
}

/// `DIM_USER_AGENT` reaches the model requests of the process that hosts
/// models, followed by the library's own parts.
#[test]
fn a_started_process_sends_the_dim_user_agent_from_its_environment() {
    let home = Home::new();
    let fixture = IdentityFixture::start();
    let model = identity_model(&home, fixture.address);
    let output = command(&home)
        .env("DIM_USER_AGENT", "private-client/7")
        .args([
            "--model-config",
            model.to_str().unwrap(),
            "exec",
            "-p",
            "hi",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\ndaemon log: {}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    let seen = fixture.user_agents();
    fixture.stop();
    assert!(!seen.is_empty(), "the fixture saw a chat request");
    for user_agent in &seen {
        let library = user_agent
            .strip_prefix("private-client/7 ")
            .unwrap_or_else(|| panic!("the environment value leads the User-Agent: {user_agent}"));
        assert!(
            library.starts_with("ai-sdk-openai-compatible/"),
            "the library's parts follow: {user_agent}"
        );
    }
}

/// A value a request cannot carry fails the process that hosts models: exit
/// code 1 and the validation message, before anything is served. `--help`
/// stays reachable with the same value, because the override is read after
/// the arguments are parsed.
#[test]
fn an_invalid_dim_user_agent_fails_startup() {
    for invalid in ["", "private client\u{1}/1", "dím/1.0"] {
        let home = Home::new();
        // Startup fails before any request, so nothing listens here.
        let model = identity_model(&home, "127.0.0.1:9".parse().unwrap());
        let socket = home.0.join("rust/daemon.sock");
        daemon_cleanup::mark_test_server(&home.0);
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command
            .arg("app-server")
            .arg("--unix")
            .arg(&socket)
            .args(["--no-ws", "--home"])
            .arg(home.0.join("rust"))
            .arg("--model-config")
            .arg(&model)
            .env("DIM_USER_AGENT", invalid)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in STARTUP_ENV {
            command.env_remove(name);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{invalid:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("DIM_USER_AGENT must be 1 to 512 printable ASCII characters"),
            "{invalid:?}: {error}"
        );
        assert!(!error.contains(invalid) || invalid.is_empty(), "{error}");
        assert!(!socket.exists(), "nothing was served: {invalid:?}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_dimcode"))
        .args(["app-server", "--help"])
        .env("DIM_USER_AGENT", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "--help does not validate the override: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let usage = String::from_utf8_lossy(&output.stdout);
    assert!(
        usage.contains("dim-app-server (--product-home PATH"),
        "--help prints usage: {usage}"
    );
}

/// A startup connection's own `User-Agent` wins over dim's identity and over
/// `DIM_USER_AGENT`; the library's parts still follow it.
#[test]
fn a_startup_connection_user_agent_wins_over_the_identity() {
    let home = Home::new();
    let fixture = IdentityFixture::start();
    let base_url = format!("http://{}/v1", fixture.address);
    let output = command(&home)
        .envs([
            ("DIM_BASE_URL", base_url.as_str()),
            ("DIM_API_FORMAT", "openai-compatible"),
            ("DIM_MODEL", "fixture-model"),
            ("DIM_CUSTOM_HEADERS", "User-Agent: gateway-client/9"),
            ("DIM_USER_AGENT", "private-client/7"),
            ("DIM_MAX_CONTEXT_TOKENS", "128000"),
            ("DIM_MAX_OUTPUT_TOKENS", "4096"),
        ])
        .args(["exec", "-p", "hi"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\ndaemon log: {}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    let seen = fixture.user_agents();
    fixture.stop();
    assert!(!seen.is_empty(), "the fixture saw a chat request");
    for user_agent in &seen {
        assert!(
            user_agent.starts_with("gateway-client/9 ai-sdk-"),
            "the connection's own User-Agent leads: {user_agent}"
        );
        assert!(
            !user_agent.contains("private-client/7"),
            "DIM_USER_AGENT does not reach past it: {user_agent}"
        );
    }
}
