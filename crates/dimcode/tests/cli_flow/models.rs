//! `dimcode models ...`: one Provider's models through its adapter, over the
//! public wire (cli-commands.md "The Models area").
use super::*;

fn hello(wire: &mut Wire) {
    wire.hello(&[
        "provider.management",
        "provider.models.set_enabled",
        "provider.models.edit",
        "provider.models.create",
        "provider.models.delete",
        "provider.models.reset",
        "provider.refresh",
    ]);
}

/// One model as `provider.management` reports it.
fn model(id: &str, name: &str, source: &str, enabled: bool, actions: &[&str]) -> Value {
    json!({
        "model": {
            "modelId": id,
            "displayName": name,
            "status": "active",
            "capabilities": {
                "modalities": ["text"],
                "toolCalling": true,
                "streaming": true,
                "structuredOutput": true,
                "vision": false,
                "audio": false,
                "reasoning": false,
                "contextWindow": 128000,
                "maxOutputTokens": 16384
            }
        },
        "source": source,
        "enabled": enabled,
        "endpoint": "openai-compatible",
        "actions": actions,
    })
}

/// The Provider as `provider.management` reports it.
fn provider(models: Value, version: u64) -> Value {
    json!({"providers": [{
        "providerId": "fixture",
        "displayName": "Fixture",
        "version": version,
        "actions": ["models.create", "models.edit", "models.delete", "models.reset",
                    "models.set_enabled", "preferences.update", "refresh"],
        "connection": {"driverKind": "new-api-router"},
        "models": models,
    }]})
}

/// The models a read reports: a catalog model, a user model, and an
/// override with its catalog entry.
fn models(flash_enabled: bool) -> Value {
    json!([
        model(
            "flash",
            "Flash",
            "default",
            flash_enabled,
            &["set_enabled", "edit"]
        ),
        model(
            "custom",
            "Custom",
            "user",
            true,
            &["set_enabled", "edit", "delete"]
        ),
        model(
            "tuned",
            "Tuned",
            "override",
            true,
            &["set_enabled", "edit", "reset"]
        ),
    ])
}

#[test]
fn models_area_verbs_write_through_the_management_methods() {
    // enable: the set is the complete enabled set with the model flipped.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        assert_eq!(request["params"], json!({"providerId": "fixture"}));
        wire.reply(&request, provider(models(false), 3));
        let request = wire.read("provider.models.set_enabled");
        assert_eq!(
            request["params"],
            json!({"providerId": "fixture", "expectedVersion": 3,
                "modelIds": ["flash", "custom", "tuned"]})
        );
        wire.reply(&request, json!({"models": []}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "enable", "flash"])
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
        "Enabled Flash.\n"
    );

    // disable names the set without the model.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
        let request = wire.read("provider.models.set_enabled");
        assert_eq!(
            request["params"],
            json!({"providerId": "fixture", "expectedVersion": 3,
                "modelIds": ["custom", "tuned"]})
        );
        wire.reply(&request, json!({"models": []}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(false), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "disable", "flash"])
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
        "Disabled Flash.\n"
    );

    // remove deletes a user model.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
        let request = wire.read("provider.models.delete");
        assert_eq!(
            request["params"],
            json!({"providerId": "fixture", "expectedVersion": 3, "modelId": "custom"})
        );
        wire.reply(&request, json!({"models": []}));
        // The deleted model is gone from the read after it, so the line names
        // the id the user typed.
        let request = wire.read("provider.management");
        let mut after = models(true);
        after
            .as_array_mut()
            .unwrap()
            .retain(|model| model["model"]["modelId"] != json!("custom"));
        wire.reply(&request, provider(after, 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "remove", "custom"])
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
        "Removed custom.\n"
    );

    // remove resets an override to its catalog entry.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
        let request = wire.read("provider.models.reset");
        assert_eq!(
            request["params"],
            json!({"providerId": "fixture", "expectedVersion": 3, "modelId": "tuned"})
        );
        wire.reply(&request, json!({"models": []}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "remove", "tuned"])
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
        "Reset Tuned to its catalog entry.\n"
    );

    // add sends the document at the version read first.
    let home = Home::new();
    let path = home.0.join("added.json");
    fs::write(
        &path,
        r#"{"modelId":"new","displayName":"New model","status":"active","capabilities":{
            "modalities":["text"],"toolCalling":true,"streaming":true,
            "structuredOutput":true,"vision":false,"audio":false,"reasoning":false}}"#,
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
        let request = wire.read("provider.models.create");
        assert_eq!(request["params"]["providerId"], "fixture");
        assert_eq!(request["params"]["expectedVersion"], 3);
        assert_eq!(request["params"]["model"]["modelId"], "new");
        assert_eq!(request["params"]["model"]["displayName"], "New model");
        wire.reply(&request, json!({"models": []}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "add", path.to_str().unwrap()])
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
        "Added New model.\n"
    );
}

/// The models a read reports, with one the owner marked unavailable: the set
/// keeps its enablement, so the owner offers no toggle for it.
fn unavailable_models() -> Value {
    let mut models = models(true);
    models[0]["unavailableReason"] = json!("limits_undeclared");
    models[0]["actions"] = json!(["edit"]);
    models
}

/// A toggle on a model the set does not govern is refused before the write,
/// naming the owner's reason: no `provider.models.set_enabled` is sent.
#[test]
fn models_area_refuses_a_toggle_the_set_does_not_govern() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(unavailable_models(), 3));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["models", "fixture", "enable", "flash"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout).unwrap().is_empty(),
        "a refused toggle reports no applied line"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr
            .contains("flash is unavailable (limits_undeclared); its enablement is kept as it is."),
        "{stderr}"
    );
}

/// A Provider record the owner could not read shows the owner's message,
/// not the not-found fallback.
#[test]
fn models_area_shows_the_owner_error_for_an_unreadable_provider() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(
            &request,
            json!({"providers": [], "errors": [{"providerId": "fixture",
                "message": "The Provider record could not be read: checksum mismatch"}]}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["models", "fixture", "inspect", "flash"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("checksum mismatch"), "{stderr}");
    assert!(!stderr.contains("No provider has that id."), "{stderr}");
}

#[test]
fn models_uses_each_saved_interface_language() {
    for (locale, expected) in [
        ("en", "Disabled flash."),
        ("zh-CN", "已禁用 flash。"),
        ("ja", "flash を無効にしました。"),
        ("ko", "flash을(를) 비활성화했습니다."),
        ("zh-Hant", "已停用 flash。"),
        ("id", "flash dinonaktifkan."),
        ("de", "flash deaktiviert."),
        ("fr", "flash désactivé."),
        ("es", "flash desactivado."),
        ("pt-BR", "flash desativado."),
        ("pt-PT", "flash desativado."),
    ] {
        let home = Home::new();
        fs::write(
            home.0.join("config.json"),
            json!({"locale":locale}).to_string(),
        )
        .unwrap();
        let daemon = home.daemon(|wire| {
            hello(wire);
            let request = wire.read("provider.management");
            wire.reply(&request, provider(models(true), 3));
            let request = wire.read("provider.models.set_enabled");
            wire.reply(&request, json!({}));
            let request = wire.read("provider.management");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,"message":"the provider is unavailable"}}));
        });
        let output = home
            .command()
            .args(["models", "fixture", "disable", "flash"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(output.status.success(), "{locale}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{expected}\n"),
            "{locale}"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("the provider is unavailable"),
            "{locale}: {stderr}"
        );
    }
}

/// A daemon that serves the models catalog without the toggle's method
/// refuses the change with the kind's sentence, which names models.
#[test]
fn models_names_the_kind_the_daemon_cannot_serve() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["provider.management"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["models", "fixture", "disable", "flash"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("This daemon lists models but cannot toggle them."),
        "{stderr}"
    );
}

#[test]
fn models_area_set_writes_the_revision_it_read_and_only_the_changed_fields() {
    let home = Home::new();
    let path = home.0.join("flash.json");
    // The template the command builds from the read, with one field edited:
    // the write carries that field alone.
    fs::write(
        &path,
        json!({
            "displayName": "Renamed",
            "contextWindow": 128000,
            "maxOutputTokens": 16384,
            "toolCalling": true,
            "vision": false,
            "reasoning": {"supported": false}
        })
        .to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        // The command reads the document, then the adapter reads the entry
        // it diffs the draft against.
        for _ in 0..2 {
            let request = wire.read("provider.management");
            wire.reply(&request, provider(models(true), 3));
        }
        let request = wire.read("provider.models.edit");
        assert_eq!(
            request["params"],
            json!({"providerId": "fixture", "expectedVersion": 3, "modelId": "flash",
                "edit": {"displayName": "Renamed"}})
        );
        wire.reply(&request, json!({"models": []}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "set", "flash", path.to_str().unwrap()])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Saved Flash.\n");

    // A conflict writes nothing, and the user's text keeps its file.
    let home = Home::new();
    let path = home.0.join("flash.json");
    let kept = path.display().to_string();
    fs::write(
        &path,
        json!({
            "displayName": "Renamed",
            "contextWindow": 128000,
            "maxOutputTokens": 16384,
            "toolCalling": true,
            "vision": false,
            "reasoning": {"supported": false}
        })
        .to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        for _ in 0..2 {
            let request = wire.read("provider.management");
            wire.reply(&request, provider(models(true), 3));
        }
        let request = wire.read("provider.models.edit");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Provider version changed; reread before retrying",
            "data":{"appError":"provider_conflict"}}}));
        // The conflict is classified by the version the fresh read reports.
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 4));
    });
    let output = home
        .command()
        .args(["models", "fixture", "set", "flash", path.to_str().unwrap()])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The models changed since they were read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains(&kept), "{stderr}");
}

/// A conflict whose re-read fails is still a conflict: the command prints
/// the owner's sentence, then the read error, and says where the text is.
#[test]
fn models_area_reports_a_conflict_whose_reread_fails() {
    let home = Home::new();
    let path = home.0.join("flash.json");
    let kept = path.display().to_string();
    fs::write(
        &path,
        json!({
            "displayName": "Renamed",
            "contextWindow": 128000,
            "maxOutputTokens": 16384,
            "toolCalling": true,
            "vision": false,
            "reasoning": {"supported": false}
        })
        .to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        for _ in 0..2 {
            let request = wire.read("provider.management");
            wire.reply(&request, provider(models(true), 3));
        }
        let request = wire.read("provider.models.edit");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Provider version changed; reread before retrying",
            "data":{"appError":"provider_conflict"}}}));
        // The read that would classify the conflict fails.
        let request = wire.read("provider.management");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"the provider is unavailable"}}));
    });
    let output = home
        .command()
        .args(["models", "fixture", "set", "flash", path.to_str().unwrap()])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The models changed since they were read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("the provider is unavailable"), "{stderr}");
    assert!(stderr.contains(&kept), "{stderr}");
}

#[test]
fn models_area_add_keeps_the_text_when_the_id_exists() {
    let home = Home::new();
    let path = home.0.join("added.json");
    let kept = path.display().to_string();
    fs::write(
        &path,
        r#"{"modelId":"flash","displayName":"Flash again","status":"active","capabilities":{
            "modalities":["text"],"toolCalling":true,"streaming":true,
            "structuredOutput":true,"vision":false,"audio":false,"reasoning":false}}"#,
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
        // A duplicate id is the owner's own refusal: the version did not
        // move, so this is not a stale-revision conflict.
        let request = wire.read("provider.models.create");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Model already exists",
            "data":{"appError":"provider_conflict"}}}));
        let request = wire.read("provider.management");
        wire.reply(&request, provider(models(true), 3));
    });
    let output = home
        .command()
        .args(["models", "fixture", "add", path.to_str().unwrap()])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Model already exists"), "{stderr}");
    assert!(stderr.contains(&kept), "{stderr}");
    assert!(output.stdout.is_empty());
}

/// A refresh the owner accepted is applied even when the read that reports
/// the area fails: the panel shows the refresh note and the read error, not
/// a refusal.
#[test]
fn tui_models_area_keeps_a_refresh_whose_read_fails() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(
            &request,
            json!({"catalog":{"version":3,"disabledModelIds":[],"models":[{
                "id":"flash","name":"Flash","capabilities":{
                    "modalities":["text"],"toolCalling":true,"streaming":true,
                    "structuredOutput":true,"vision":false,"audio":false,
                    "reasoning":false,"contextWindow":128000,"maxOutputTokens":16384}}]}}),
        );
        let area = json!({"providers": [{
            "providerId": "test",
            "displayName": "Test",
            "version": 3,
            "actions": ["models.create", "models.edit", "models.delete", "models.reset",
                        "models.set_enabled", "preferences.update", "refresh"],
            "connection": {"driverKind": "new-api-router"},
            "models": [{
                "model": {
                    "modelId": "flash",
                    "displayName": "flash",
                    "status": "active",
                    "capabilities": {
                        "modalities": ["text"], "toolCalling": true, "streaming": true,
                        "structuredOutput": true, "vision": false, "audio": false,
                        "reasoning": false, "contextWindow": 128000, "maxOutputTokens": 16384
                    }
                },
                "source": "default",
                "enabled": true,
                "endpoint": "openai-compatible",
                "actions": ["set_enabled", "edit"]
            }]
        }]});
        // The picker's load, then the panel's own read.
        for _ in 0..2 {
            let request = wire.read("provider.management");
            wire.reply(&request, area.clone());
        }
        // Ctrl+R reads, refreshes, then the read that reports the new
        // catalog fails.
        let request = wire.read("provider.management");
        wire.reply(&request, area);
        let request = wire.read("provider.refresh");
        wire.reply(&request, json!({}));
        let request = wire.read("provider.management");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,"message":"management read failed"}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.raw(b"\x1b[B");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    terminal.raw(b"\x12");
    terminal.until("Refreshed the models.");
    terminal.until("management read failed");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// The capabilities a picker catalog entry and a management entry share.
fn capabilities() -> Value {
    json!({
        "modalities": ["text"],
        "toolCalling": true,
        "streaming": true,
        "structuredOutput": true,
        "vision": false,
        "audio": false,
        "reasoning": false,
        "contextWindow": 128000,
        "maxOutputTokens": 16384
    })
}

/// `provider.catalog` as the picker reads it.
fn picker_catalog() -> Value {
    json!({"catalog": {"version": 3, "disabledModelIds": [], "models": [
        {"id": "flash", "name": "Flash", "capabilities": capabilities()}]}})
}

/// `provider.management` for the picker's Provider: a catalog model, an
/// override and a user model.
fn test_provider(models: Value, version: u64) -> Value {
    json!({"providers": [{
        "providerId": "test",
        "displayName": "Test",
        "source": "static",
        "credentialAuthority": "static",
        "version": version,
        "enabled": true,
        "actions": ["models.create", "models.edit", "models.delete", "models.reset",
                    "models.set_enabled", "preferences.update", "refresh"],
        "connection": {"driverKind": "new-api-router"},
        "models": models,
        "preferences": {},
    }]})
}

/// The three classes of model, with `flash_enabled` for the catalog one.
fn area_models(flash_enabled: bool) -> Value {
    json!([
        model(
            "flash",
            "flash",
            "default",
            flash_enabled,
            &["set_enabled", "edit"]
        ),
        model(
            "tuned",
            "Tuned",
            "override",
            true,
            &["set_enabled", "edit", "reset"]
        ),
        model(
            "custom",
            "Custom",
            "user",
            true,
            &["set_enabled", "edit", "delete"]
        ),
    ])
}

/// [`area_models`] with one catalog model the owner reports unusable: the
/// set keeps its enablement, so the entry offers no toggle.
fn area_models_unavailable() -> Value {
    let mut models = area_models(true);
    let mut limited = model("limited", "Limited", "default", true, &["edit"]);
    limited["unavailableReason"] = json!("limits_undeclared");
    models.as_array_mut().unwrap().insert(1, limited);
    models
}

/// [`area_models_unavailable`] after the toggle wrote the override model
/// disabled.
fn area_models_unavailable_after_toggle() -> Value {
    let mut models = area_models_unavailable();
    let tuned = models
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["model"]["modelId"] == json!("tuned"))
        .unwrap();
    tuned["enabled"] = json!(false);
    models
}

/// `/models` → `Edit models` opens one Provider's area: the title names the
/// Provider, rows are grouped by source, Enter toggles with the complete
/// enabled set at the read's version, Tab shows the descriptor, and Esc
/// closes back to the prompt.
#[test]
fn tui_models_area_opens_and_toggles_a_model() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        // The picker's own load.
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(&request, picker_catalog());
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Edit models opens the Provider's area.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Enter: the toggle reads, writes the complete set, reads again.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        let request = wire.read("provider.models.set_enabled");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 3,
                "modelIds": ["tuned", "custom"]})
        );
        wire.reply(&request, json!({}));
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(false), 4));
        // Tab: the selected model's descriptor.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(false), 4));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    // The three groups and the wrapped hint need more than 80 columns and
    // 24 rows; the panel is the same at any size.
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.until("Edit models");
    // The edit row follows the Provider's model row.
    terminal.raw(b"\x1b[B");
    terminal.until("Edit models");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    terminal.until("catalog");
    terminal.until("overrides");
    terminal.until("added");
    terminal.raw(b"\r");
    terminal.until("Disabled flash.");
    terminal.raw(b"\t");
    terminal.until("ID flash");
    terminal.raw(b"\x1b");
    terminal.raw(b"\x1b");
    terminal.until("Ask anything...");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// The area offers no toggle for a model the set does not govern: Enter on
/// it sends nothing, while the model below it still toggles with the
/// complete enabled set.
#[test]
fn tui_models_area_offers_no_toggle_for_an_unavailable_model() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(&request, picker_catalog());
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_unavailable(), 3));
        // Edit models opens the Provider's area.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_unavailable(), 3));
        // Tab on the unavailable model: an inspect reads the area. Had Enter
        // toggled it, the toggle's own read would stand here instead.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_unavailable(), 3));
        // Enter on an available model still toggles.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_unavailable(), 3));
        let request = wire.read("provider.models.set_enabled");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 3,
                "modelIds": ["flash", "limited", "custom"]})
        );
        wire.reply(&request, json!({}));
        let request = wire.read("provider.management");
        wire.reply(
            &request,
            test_provider(area_models_unavailable_after_toggle(), 4),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.until("Edit models");
    terminal.raw(b"\x1b[B");
    terminal.until("Edit models");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    terminal.until("unavailable");
    // The unavailable model is the row below the catalog's first one: Enter
    // on it changes nothing.
    terminal.raw(b"\x1b[B\r");
    // Tab still opens its descriptor, so the selection was on it.
    terminal.raw(b"\t");
    terminal.until("ID limited");
    terminal.raw(b"\x1b");
    // The available override model below it toggles.
    terminal.raw(b"\x1b[B\r");
    terminal.until("Disabled Tuned.");
    terminal.raw(b"\x1b");
    terminal.until("Ask anything...");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// A write the owner refuses keeps the panel and its list, and shows the
/// owner's message.
#[test]
fn tui_models_area_keeps_the_panel_on_a_refused_write() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(&request, picker_catalog());
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Enter on the user model: its delete is refused, nothing is read
        // again, and the list stays as it was.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        let request = wire.read("provider.models.delete");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 3, "modelId": "custom"})
        );
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Only user models can be deleted",
            "data":{"appError":"provider_unsupported"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.raw(b"\x1b[B");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    // Down to the user model, then Ctrl+X and Enter.
    terminal.raw(b"\x1b[B");
    terminal.until("tuned");
    terminal.raw(b"\x1b[B");
    terminal.until("Custom");
    terminal.raw(b"\x18");
    terminal.until("Custom? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("Only user models can be deleted");
    // The panel still has its list: the next key moves within it.
    terminal.raw(b"\x1b[A");
    terminal.until("tuned");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// The area after the owner's own write: the flash model carries a new name.
fn area_models_later() -> Value {
    let mut models = area_models(true);
    models[0]["model"]["displayName"] = json!("Flash v4");
    models
}

/// A conflict on an edit rereads the area, shows the owner's sentence and
/// the fresh rows, keeps the user's text where it is, and drops the link to
/// the stale revision: the next Ctrl+E opens the current document and writes
/// at the version it reports.
#[test]
fn tui_models_area_rereads_after_a_conflict() {
    let home = Home::new();
    let script = home.0.join("editor.sh");
    let count = home.0.join("editor-count");
    let captured = home.0.join("edited-document");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             run=$(( $(cat '{count}' 2>/dev/null || echo 0) + 1 ))\n\
             printf '%s' \"$run\" > '{count}'\n\
             cp \"$1\" '{captured}'\n\
             printf '%s' '{{\"displayName\":\"Edited by hand\"}}' > \"$1\"\n",
            count = count.display(),
            captured = captured.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(&request, picker_catalog());
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Edit models opens the Provider's area.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Ctrl+E reads the fresh template, then writes at the revision the
        // panel read; the owner moved the version in the meantime.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        let request = wire.read("provider.models.edit");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 3, "modelId": "flash",
                "edit": {"displayName": "Edited by hand"}})
        );
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Provider version changed; reread before retrying",
            "data":{"appError":"provider_conflict"}}}));
        // The conflict is classified by a fresh read, and that view is the
        // one the panel shows.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 4));
        // The next Ctrl+E opens the current document and writes at it.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 4));
        let request = wire.read("provider.models.edit");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 4, "modelId": "flash",
                "edit": {"displayName": "Edited by hand"}})
        );
        wire.reply(&request, json!({}));
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 5));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", &script);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.raw(b"\x1b[B");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    terminal.raw(b"\x05");
    // The fresh list is what the panel shows now, with the owner's sentence
    // and the place the user's text is kept.
    terminal.until("Flash v4");
    terminal.until("The models changed since they were read");
    terminal.until("Your text is kept in");
    terminal.raw(b"\x05");
    terminal.until("Saved Flash v4.");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    // The second editor was handed the current document, not the stale
    // draft: the link to the old revision is gone.
    let document = fs::read_to_string(&captured).unwrap();
    assert!(document.contains("\"Flash v4\""), "{document}");
    assert!(!document.contains("Edited by hand"), "{document}");
    // The user's text is still in the file the conflict named.
    let kept: Vec<std::path::PathBuf> = fs::read_dir(home.0.join("edit"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(
        kept.iter().any(|path| fs::read_to_string(path)
            .unwrap_or_default()
            .contains("Edited by hand")),
        "no kept file holds the user's text: {kept:?}"
    );
}

/// A conflict whose re-read fails is still a conflict: the panel shows the
/// owner's sentence and the read error once, drops the view and the stale
/// link, and Ctrl+R reads the area again; the next edit opens the current
/// document.
#[test]
fn tui_models_area_reports_a_conflict_whose_reread_fails() {
    let home = Home::new();
    let script = home.0.join("editor.sh");
    let captured = home.0.join("edited-document");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             cp \"$1\" '{captured}'\n\
             printf '%s' '{{\"displayName\":\"Edited by hand\"}}' > \"$1\"\n",
            captured = captured.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("provider.list");
        wire.reply(
            &request,
            json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
        );
        let request = wire.read("account.providers");
        wire.reply(&request, json!([]));
        let request = wire.read("provider.catalog");
        wire.reply(&request, picker_catalog());
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Edit models opens the Provider's area.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        // Ctrl+E reads the fresh template, then writes at the revision the
        // panel read; the owner moved the version in the meantime.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models(true), 3));
        let request = wire.read("provider.models.edit");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Provider version changed; reread before retrying",
            "data":{"appError":"provider_conflict"}}}));
        // The read that would classify the conflict fails.
        let request = wire.read("provider.management");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"the provider is unavailable"}}));
        // Ctrl+R reads the area again, without the view the conflict lost.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 4));
        // The next Ctrl+E opens the current document and writes at it.
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 4));
        let request = wire.read("provider.models.edit");
        assert_eq!(
            request["params"],
            json!({"providerId": "test", "expectedVersion": 4, "modelId": "flash",
                "edit": {"displayName": "Edited by hand"}})
        );
        wire.reply(&request, json!({}));
        let request = wire.read("provider.management");
        wire.reply(&request, test_provider(area_models_later(), 5));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", &script);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/models\r");
    terminal.until("Test / flash");
    terminal.raw(b"\x1b[B");
    terminal.raw(b"\r");
    terminal.until("Models · Test");
    terminal.raw(b"\x05");
    // The owner's sentence stands, the draft's link to the stale revision
    // is gone, and the failed read is reported once after the message.
    terminal.until("The models changed since they were read");
    // The sentence wraps inside the footer, so its halves are checked as
    // the rows carry them.
    terminal.until("Your next");
    terminal.until("opens the current document.");
    terminal.until("Your text is kept in");
    terminal.until("the provider is unavailable");
    // Ctrl+R reads the area again, without the view the conflict lost.
    terminal.raw(b"\x12");
    terminal.until("Flash v4");
    terminal.raw(b"\x05");
    terminal.until("Saved Flash v4.");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    // The next editor was handed the current document, not the stale draft.
    let document = fs::read_to_string(&captured).unwrap();
    assert!(document.contains("\"Flash v4\""), "{document}");
    assert!(!document.contains("Edited by hand"), "{document}");
    // The user's text is still in a kept file.
    let kept: Vec<std::path::PathBuf> = fs::read_dir(home.0.join("edit"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(
        kept.iter().any(|path| fs::read_to_string(path)
            .unwrap_or_default()
            .contains("Edited by hand")),
        "no kept file holds the user's text: {kept:?}"
    );
}
