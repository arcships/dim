//! `/mcp` and the MCP area through its adapter, over the public wire.
use super::*;
use std::sync::mpsc;

#[test]
fn resumed_mcp_line_commands_use_the_session_workspace_in_both_clients() {
    for tui in [false, true] {
        let home = Home::new();
        let launch_cwd = fs::canonicalize(&home.0).unwrap();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["sessionId"], "session");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            if tui {
                wire.no_context();
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
            } else {
                let idle = wire.read("session.idle");
                wire.reply(&idle, json!({"sequence":8}));
            }
            let list = wire.read("mcp.catalog");
            assert_eq!(list["params"]["cwd"], "/workspace");
            wire.reply(&list, catalog(true, None));
            let state = wire.read("session.mcp");
            wire.reply(&state, json!({"sessionId":"session","servers":[]}));
            let resolve = wire.read("mcp.catalog");
            assert_eq!(resolve["params"]["cwd"], "/workspace");
            wire.reply(&resolve, catalog(true, None));
            let disable = wire.read("mcp.set_enabled");
            assert_eq!(disable["params"], json!({"cwd":"/workspace","id":"metrics","source":"project","expectedVersion":"project:1","enabled":false}));
            wire.reply(&disable, json!({}));
            if tui {
                let list = wire.read("mcp.catalog");
                assert_eq!(list["params"]["cwd"], json!(launch_cwd));
                let mut catalog = catalog(true, None);
                catalog["servers"][0]["id"] = json!("launch-only");
                wire.reply(&list, catalog);
            }
            wire.hold();
        });
        let mut launch = home.command();
        launch.args(["--resume", "session", "--cwd", home.0.to_str().unwrap()]);
        let mut terminal = if tui {
            Terminal::launch_tui(launch)
        } else {
            Terminal::launch(launch)
        };
        if tui {
            terminal.until("test / model");
        } else {
            terminal.editor();
        }
        terminal.write(b"/mcp list\r");
        terminal.until("Columns: ID, source");
        if !tui {
            terminal.editor();
        }
        terminal.write(b"/mcp disable metrics --source project\r");
        terminal.until("Disabled");
        if tui {
            terminal.write(b"/new\r");
            // The palette's own row ("start a new session") shows the words
            // before `/new` runs; the boundary rule is painted once the
            // switch cleared the composer.
            terminal.until("─ new session ─");
            terminal.write(b"/mcp list\r");
            terminal.until("launch-only");
        }
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

fn hello(wire: &mut Wire) {
    wire.hello(&[
        "mcp.catalog",
        "mcp.configuration",
        "mcp.inspect",
        "mcp.test",
        "mcp.update",
        "mcp.save",
        "mcp.remove",
        "mcp.set_enabled",
        "mcp.import.sources",
        "mcp.import.preview",
        "mcp.import.apply",
        "mcp.disconnect",
        "mcp.auth.start",
        "mcp.auth.complete",
        "mcp.auth.logout",
    ]);
}

/// The effective catalog: a user entry, a project entry and a plugin entry.
fn catalog(docs_enabled: bool, project_error: Option<&str>) -> Value {
    json!({
        "userRevision": "user:1",
        "projectRevision": "project:1",
        "userApplication": {"appliedRevision": "user:1", "error": null, "cleanupError": null},
        "pluginApplication": {"appliedRevision": null, "error": null, "cleanupError": null},
        "projectApplication": {
            "appliedRevision": "project:1",
            "error": project_error,
            "cleanupError": null,
        },
        "servers": [
            {"id": "docs", "source": "user", "enabled": docs_enabled, "transport": "stdio",
             "auth": {"status": "authorized", "missingScopes": ["repo"]}},
            {"id": "metrics", "source": "project", "enabled": false, "transport": "http",
             "auth": {"status": "required"}},
            {"id": "notes", "source": "plugin", "enabled": true, "transport": "stdio"},
        ],
    })
}

fn docs(enabled: bool) -> Value {
    json!({"revision": "user:1", "enabled": enabled, "configuration": {
        "type": "stdio", "command": "npx", "argsCount": 2, "cwd": null,
        "envNames": ["DOCS_TOKEN"],
    }})
}

fn metrics() -> Value {
    json!({"revision": "project:1", "enabled": false, "configuration": {
        "type": "http", "endpoint": "https://metrics.example.com",
        "headerNames": ["X-Key"], "authConfigured": false, "oauthConfigured": true,
        "oauth": {"clientId": "client-1", "callbackPort": 1456, "scopes": "read",
                  "metadataConfigured": false},
    }})
}

/// The same entry after a patch removed its OAuth settings: the stored
/// header is still reported.
fn metrics_without_oauth() -> Value {
    json!({"revision": "project:2", "enabled": false, "configuration": {
        "type": "http", "endpoint": "https://metrics.example.com",
        "headerNames": ["X-Key"], "authConfigured": false, "oauthConfigured": false,
    }})
}

fn notes() -> Value {
    json!({"enabled": true, "configuration": {
        "type": "stdio", "command": "notes", "argsCount": 0, "cwd": "/w", "envNames": [],
    }})
}

/// One entry's configuration read, answered in the order the catalog lists
/// its source.
fn configuration(wire: &mut Wire, id: &str, source: &str, reply: Value) {
    let request = wire.read("mcp.configuration");
    assert_eq!(request["params"]["id"], id);
    assert_eq!(request["params"]["source"], source);
    // The panel reads the Session's workspace, and a command its own cwd.
    assert!(request["params"]["cwd"].is_string());
    wire.reply(&request, reply);
}

/// The whole view read: the catalog, then one configuration per entry.
fn area(wire: &mut Wire, built: Value, docs: Value) {
    let request = wire.read("mcp.catalog");
    wire.reply(&request, built);
    configuration(wire, "docs", "user", docs);
    configuration(wire, "metrics", "project", metrics());
    configuration(wire, "notes", "plugin", notes());
}

/// A write resolves the entry's source from a catalog read of its own.
fn source(wire: &mut Wire) {
    let request = wire.read("mcp.catalog");
    wire.reply(&request, catalog(true, None));
}

/// The Session handshake every terminal flow performs before its own
/// requests.
fn started(wire: &mut Wire) {
    hello(wire);
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
            Some("session.context.usage") => wire
                .send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
            method => panic!("unexpected request {method:?}"),
        }
    }
}

/// `/mcp` lists the effective catalog, and every kind calls the method its
/// design names: Enter toggles, Ctrl+T tests, Tab inspects, Ctrl+X removes;
/// a plugin entry offers no write.
#[test]
fn tui_mcp_panel_lists_manages_and_tests_entries() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, catalog(true, None), docs(true));
        // Enter disables the selected user entry at the catalog's revision.
        source(wire);
        let request = wire.read("mcp.set_enabled");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "docs", "source": "user",
                   "expectedVersion": "user:1", "enabled": false})
        );
        wire.reply(&request, json!({}));
        area(wire, catalog(false, None), docs(false));
        // Ctrl+T tests the stored entry; the answer is the tool detail and
        // the area is not read again.
        source(wire);
        let request = wire.read("mcp.test");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "docs", "source": "user"})
        );
        wire.reply(
            &request,
            json!({"status": "connected", "latencyMs": 5, "tools": [
                {"name": "search", "description": "Search docs",
                 "inputSchema": {"type": "object"}},
            ]}),
        );
        // Tab inspects the effective entry and its connections.
        source(wire);
        let request = wire.read("mcp.inspect");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "docs", "source": "user"})
        );
        wire.reply(
            &request,
            json!({
                "enabled": false,
                "configuration": {
                    "type": "stdio", "command": "npx", "argsCount": 2, "cwd": null,
                    "envNames": ["DOCS_TOKEN"],
                },
                "connections": [{"state": {"status": "connected"}, "tools": [
                    {"name": "search", "description": "Search docs",
                     "inputSchema": {"type": "object"}},
                ]}],
            }),
        );
        // Ctrl+X asks, Enter removes at the source revision.
        source(wire);
        let request = wire.read("mcp.remove");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "docs", "source": "user",
                   "expectedVersion": "user:1"})
        );
        wire.reply(&request, json!({}));
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1", "projectRevision": "project:1",
                "userApplication": {"error": null, "cleanupError": null},
                "pluginApplication": {"error": null, "cleanupError": null},
                "projectApplication": {"error": null, "cleanupError": null},
                "servers": [
                    {"id": "metrics", "source": "project", "enabled": false,
                     "transport": "http", "auth": {"status": "required"}},
                    {"id": "notes", "source": "plugin", "enabled": true,
                     "transport": "stdio"},
                ],
            }),
        );
        configuration(wire, "metrics", "project", metrics());
        configuration(wire, "notes", "plugin", notes());
        // /mcp list keeps the report the bare command had.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(false, None));
        let state = wire.read("session.mcp");
        wire.reply(
            &state,
            json!({"sessionId": "session", "servers": [], "catalogError": null, "closed": false}),
        );
        // Nothing offered a remove for the plugin entry: no mcp.remove may
        // arrive after it.
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                assert_ne!(value["method"], "mcp.remove", "the plugin entry wrote");
            }
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("user · stdio · signed in (missing scopes: repo)");
    terminal.until("Environment names: DOCS_TOKEN");
    terminal.raw(b"\r");
    terminal.until("Disabled \"docs\" (user).");
    terminal.raw(b"\x14");
    terminal.until("Tools of docs");
    terminal.until("search — Search docs");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\t");
    terminal.until("MCP entry docs");
    terminal.until("Connection 1 · connected");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x18");
    terminal.until("docs? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("Removed docs.");
    // The plugin entry is selected next; it shows its state and its owner,
    // and Ctrl+X sends nothing.
    terminal.raw(b"\x1b[B");
    terminal.until("read-only");
    terminal.until("Owned by a plugin; change it through Plugins.");
    terminal.raw(b"\x18");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/mcp list\r");
    terminal.until("Columns: ID, source, transport, enablement, authorization.");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// An editor that writes a patch for a seed and a new entry for the create
/// template.
fn editor(home: &Home, name: &str, script: &str) -> PathBuf {
    let path = home.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn edit_command(home: &Home, program: &std::path::Path, fullscreen: bool) -> Command {
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", program);
    if fullscreen {
        command.arg("--fullscreen");
    }
    command
}

const EDITS_THE_ENTRY: &str = "case \"$(cat \"$1\")\" in\n\
    *'\"id\"'*) printf '%s' '{\"id\":\"example\",\"source\":\"user\",\"configuration\":{\"type\":\"stdio\",\"command\":\"npx\",\"args\":[\"-y\",\"server\"],\"env\":{},\"enabled\":false}}' > \"$1\";;\n\
    *) printf '%s' '{\"command\":\"npx\",\"cwd\":\"/workspace\",\"env\":{\"DOCS_TOKEN\":\"value\"}}' > \"$1\";;\n\
    esac";

/// Ctrl+E opens the patch without a stored value, Ctrl+N the create
/// document, and each writes with the method its kind names, in both
/// terminal modes.
#[test]
fn tui_mcp_panel_edits_and_creates_entries() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", EDITS_THE_ENTRY);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            started(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            area(wire, catalog(true, None), docs(true));
            // Ctrl+E writes the patch at the revision the view read.
            source(wire);
            let request = wire.read("mcp.update");
            assert_eq!(
                request["params"],
                json!({"cwd": "/workspace", "id": "docs", "source": "user",
                       "expectedVersion": "user:1",
                       "patch": {"command": "npx", "cwd": "/workspace",
                                 "env": {"DOCS_TOKEN": "value"}}})
            );
            wire.reply(&request, json!({}));
            area(wire, catalog(true, None), docs(true));
            // Ctrl+N writes the entry with the source revision, not a
            // revision of its own.
            source(wire);
            let request = wire.read("mcp.save");
            assert_eq!(
                request["params"],
                json!({"cwd": "/workspace", "id": "example", "source": "user",
                       "expectedVersion": "user:1",
                       "configuration": {"type": "stdio", "command": "npx",
                                         "args": ["-y", "server"], "env": {},
                                         "enabled": false}})
            );
            wire.reply(&request, json!({}));
            area(wire, catalog(true, None), docs(true));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut terminal = Terminal::resume_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/mcp\r");
        terminal.until("Executable: npx");
        terminal.raw(b"\x05");
        terminal.until("Saved docs.");
        terminal.raw(b"\x0e");
        terminal.until("Added example.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A remote entry's edit seed names no stored value: the endpoint, the
/// header names and the stored values stay out of it. A patch that changes
/// another field keeps the stored header, which the next read reports.
#[test]
fn tui_mcp_panel_seeds_a_remote_patch_without_stored_values() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "seed=\"$(cat \"$1\")\"\n\
         case \"$seed\" in *metrics.example.com*|*X-Key*) exit 1;; esac\n\
         case \"$seed\" in *'\"headers\": {}'*) ;; *) exit 1;; esac\n\
         case \"$seed\" in *'\"oauth\"'*) ;; *) exit 1;; esac\n\
         printf '%s' '{\"oauth\":{\"action\":\"remove\"}}' > \"$1\"",
    );
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, catalog(true, None), docs(true));
        // Ctrl+E writes the patch the seed produced at the read's revision.
        source(wire);
        let request = wire.read("mcp.update");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "metrics", "source": "project",
                   "expectedVersion": "project:1",
                   "patch": {"oauth": {"action": "remove"}}})
        );
        wire.reply(&request, json!({}));
        // The read after the write reports the settings the patch removed
        // and the stored header it left alone.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true, None));
        configuration(wire, "docs", "user", docs(true));
        configuration(wire, "metrics", "project", metrics_without_oauth());
        configuration(wire, "notes", "plugin", notes());
        wire.hold();
    });
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", &script);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("Executable: npx");
    terminal.raw(b"\x1b[B");
    terminal.until("OAuth: configured (client ID client-1)");
    terminal.raw(b"\x05");
    // The read after the write reports the OAuth settings the patch removed.
    terminal.until("OAuth: not configured");
    // The nearest header line before the entry's OAuth line is the one this
    // read reported: the stored header the patch kept.
    let at = terminal.transcript.rfind("OAuth: not configured").unwrap();
    let header = terminal.transcript[..at].rfind("Header names:").unwrap();
    assert!(
        terminal.transcript[header..].starts_with("Header names: X-Key"),
        "{}",
        &terminal.transcript[header..]
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// One entry's configuration read fails: that entry shows the error, and the
/// entries beside it keep their detail.
#[test]
fn tui_mcp_panel_shows_one_entries_failed_configuration_read() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true, None));
        let request = wire.read("mcp.configuration");
        assert_eq!(request["params"]["id"], "docs");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"MCP settings are unavailable",
            "data":{"appError":"mcp_unavailable"}}}));
        configuration(wire, "metrics", "project", metrics());
        configuration(wire, "notes", "plugin", notes());
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    // The failed entry is marked, and its error stands under it.
    terminal.until("invalid");
    terminal.until("MCP settings are unavailable");
    // The entry after it kept the detail its own read supplied.
    terminal.raw(b"\x1b[B");
    terminal.until("Endpoint: https://metrics.example.com");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A daemon older than `mcp.configuration` lists its entries from the
/// catalog: no detail, no error, and no entry marked invalid.
#[test]
fn tui_mcp_panel_lists_entries_when_the_read_is_absent() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["mcp.catalog", "mcp.set_enabled", "mcp.save", "mcp.remove"]);
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
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true, None));
        // A client that asks anyway is answered as this daemon would.
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(request) = serde_json::from_str::<Value>(&line)
                && request["method"] == "mcp.configuration"
            {
                wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method",
                    "data":{"appError":"unknown_method"}}}));
            }
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    // The catalog's own line stands for the entry, with nothing under it.
    terminal.until("user · stdio · signed in");
    assert!(
        !terminal.transcript.contains("invalid"),
        "an absent read marked an entry invalid: {}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("Executable:"),
        "a detail appeared without a read: {}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A stale revision writes nothing, says so, reads the area again, and the
/// next edit opens the stored entry, in both terminal modes.
#[test]
fn tui_mcp_panel_keeps_the_users_text_when_the_revision_is_stale() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", EDITS_THE_ENTRY);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            started(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            area(wire, catalog(true, None), docs(true));
            source(wire);
            let request = wire.read("mcp.update");
            assert_eq!(request["params"]["expectedVersion"], "user:1");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"The MCP settings changed; read them before updating",
                "data":{"appError":"mcp_settings_conflict"}}}));
            // Nothing was written: the area is read again, and the panel
            // moves to the newer revision.
            let request = wire.read("mcp.catalog");
            wire.reply(&request, catalog(true, None));
            let request = wire.read("mcp.configuration");
            wire.reply(
                &request,
                json!({"revision": "user:2", "enabled": true, "configuration": {
                    "type": "stdio", "command": "npx", "argsCount": 2, "cwd": null,
                    "envNames": ["DOCS_TOKEN"],
                }}),
            );
            configuration(wire, "metrics", "project", metrics());
            configuration(wire, "notes", "plugin", notes());
            // The next edit opens the stored entry at that revision.
            source(wire);
            let request = wire.read("mcp.update");
            assert_eq!(request["params"]["expectedVersion"], "user:2");
            assert_eq!(
                request["params"]["patch"],
                json!({"command": "npx", "cwd": "/workspace",
                       "env": {"DOCS_TOKEN": "value"}})
            );
            wire.reply(&request, json!({}));
            area(wire, catalog(true, None), docs(true));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut terminal = Terminal::resume_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/mcp\r");
        terminal.until("Executable: npx");
        terminal.raw(b"\x05");
        terminal.until("The MCP entry changed since it was read; nothing was written.");
        terminal.until("text is kept in");
        terminal.raw(b"\x05");
        terminal.until("Saved docs.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A write the owner refuses keeps the draft's revision, so the next edit
/// opens the user's text and writes it again.
#[test]
fn tui_mcp_panel_reports_a_refused_write() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", EDITS_THE_ENTRY);
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, catalog(true, None), docs(true));
        source(wire);
        let request = wire.read("mcp.update");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"invalid MCP transport configuration",
            "data":{"appError":"mcp_invalid_config"}}}));
        // The same revision writes the same text again.
        source(wire);
        let request = wire.read("mcp.update");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert_eq!(
            request["params"]["patch"],
            json!({"command": "npx", "cwd": "/workspace",
                   "env": {"DOCS_TOKEN": "value"}})
        );
        wire.reply(&request, json!({}));
        area(wire, catalog(true, None), docs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(edit_command(&home, &script, false));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("Executable: npx");
    terminal.raw(b"\x05");
    terminal.until("invalid MCP transport configuration");
    terminal.until("text is kept in");
    terminal.raw(b"\x05");
    terminal.until("Saved docs.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Tool text is third-party: an escape sequence and a line break in a
/// description are drawn sanitized, on one row.
#[test]
fn tui_mcp_panel_draws_tool_text_sanitized() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, catalog(true, None), docs(true));
        source(wire);
        let request = wire.read("mcp.test");
        wire.reply(
            &request,
            json!({"status": "connected", "latencyMs": 3, "tools": [
                {"name": "search", "description": "Search\u{1b}[2J\ndocs",
                 "inputSchema": {"type": "object"}},
            ]}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("Executable: npx");
    terminal.raw(b"\x14");
    terminal.until("search — Search docs");
    assert!(
        !terminal.transcript.contains("[2J"),
        "the escape sequence reaches the screen: {}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A failed catalog read on opening `/mcp` is not an applied reload: the
/// panel shows the error where the list would be, and no `Reloaded.`.
#[test]
fn tui_mcp_panel_shows_a_failed_catalog_read() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("mcp.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"MCP settings are unavailable",
            "data":{"appError":"mcp_unavailable"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("MCP settings are unavailable");
    assert!(
        !terminal.transcript.contains("Reloaded."),
        "a failed read was reported as an applied reload: {}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A `mcp.catalog` reply for the standalone commands: the same entries, and
/// a source revision the tests write at.
fn cli_catalog() -> Value {
    catalog(true, None)
}

/// The create document `mcp add` takes, as the panel's template has it.
fn new_entry(source: &str) -> Value {
    json!({
        "id": "example",
        "source": source,
        "configuration": {
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-everything"],
            "env": {},
            "enabled": false,
        },
    })
}

/// `mcp add` reads the catalog once for the id check and the source
/// revision, writes `mcp.save`, and reports the area again.
#[test]
fn mcp_add_creates_an_entry_with_the_sources_revision() {
    for (stdin, source, expected) in [(false, "user", "user:1"), (true, "project", "project:1")] {
        let home = Home::new();
        let path = home.0.join("entry.json");
        let document = new_entry(source);
        fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();
        let saved = document.clone();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("mcp.catalog");
            assert!(request["params"]["cwd"].is_string());
            wire.reply(&request, cli_catalog());
            let request = wire.read("mcp.save");
            assert_eq!(request["params"]["id"], "example");
            assert_eq!(request["params"]["source"], source);
            assert_eq!(request["params"]["expectedVersion"], expected);
            assert_eq!(request["params"]["configuration"], saved["configuration"]);
            assert!(request["params"]["cwd"].is_string());
            wire.reply(&request, json!({}));
            area(wire, cli_catalog(), docs(true));
        });
        let mut command = home.command();
        command.args(["mcp", "add"]);
        if stdin {
            command.arg("-");
        } else {
            command.arg(&path);
        }
        let mut child = command
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(serde_json::to_string(&document).unwrap().as_bytes())
                .unwrap();
        }
        let output = wait(child);
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "Added example.\n"
        );
        assert!(output.stderr.is_empty());
    }
}

/// An id the catalog already lists is refused before anything is written:
/// `mcp.save` would replace that entry. The refusal names the step the
/// existing entry leaves open, which for a plugin entry is another id.
#[test]
fn mcp_add_refuses_an_id_the_catalog_lists() {
    for (id, sentence) in [
        ("docs", "remove it first or choose another id"),
        (
            "notes",
            "choose another id, or change it through its plugin",
        ),
    ] {
        let home = Home::new();
        let path = home.0.join("entry.json");
        let mut document = new_entry("user");
        document["id"] = json!(id);
        fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("mcp.catalog");
            wire.reply(&request, cli_catalog());
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                assert!(
                    !line.contains("mcp.save"),
                    "the refused add wrote: {line:?}"
                );
                line.clear();
            }
        });
        let output = home
            .command()
            .args(["mcp", "add"])
            .arg(&path)
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(&format!("{id} is already in mcp list")),
            "{error}"
        );
        assert!(error.contains(sentence), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
}

/// A literal value in the imported document. The client sends it to the
/// owner and never prints it.
const IMPORT_TOKEN: &str = "sk-import-fixture-never-print";

/// A standard `mcpServers` document whose entries cover every preview status.
fn import_document() -> String {
    json!({
        "mcpServers": {
            "keeps": {"type": "stdio", "command": "npx", "args": ["server"], "env": {}},
            "docs": {"type": "stdio", "command": "npx", "args": ["-y", "docs"],
                     "env": {"DOCS_TOKEN": IMPORT_TOKEN}},
            "taken": {"type": "http", "url": "https://taken.example.com/mcp"},
            "broken": {"type": "ws", "url": "wss://broken.example.com"},
        },
        "otherTopLevel": true,
    })
    .to_string()
}

/// The owner's preview of [`import_document`] against the user source: an
/// identical, a changed, a conflict and an invalid entry, plus one unknown
/// top-level member.
fn import_preview() -> Value {
    json!({
        "revision": "user:1",
        "entries": [
            {"id": "keeps", "transport": "stdio", "status": "identical",
             "changes": [], "unsupported": []},
            {"id": "docs", "transport": "stdio", "status": "changed",
             "changes": [{"field": "args", "kind": "array"},
                         {"field": "enabled", "kind": "boolean"}],
             "unsupported": [{"field": "x-extra", "effect": "kept-unused"}]},
            {"id": "taken", "transport": "http", "status": "conflict",
             "changes": [{"field": "url", "kind": "string"}],
             "unsupported": []},
            {"id": "broken", "status": "invalid", "changes": [],
             "unsupported": [{"field": "auth.audience", "effect": "dropped"}],
             "error": "broken: type is stdio, http or sse"},
        ],
        "unknownTopLevel": ["otherTopLevel"],
    })
}

/// The owner's preview against the project source, where `docs` carries
/// literal fields the workspace file does not record.
fn import_preview_project() -> Value {
    json!({
        "revision": "project:1",
        "entries": [
            {"id": "keeps", "transport": "stdio", "status": "identical",
             "changes": [], "unsupported": []},
            {"id": "docs", "transport": "stdio", "status": "changed",
             "changes": [{"field": "args", "kind": "array"},
                         {"field": "env", "kind": "object"}],
             "unsupported": []},
        ],
        "unknownTopLevel": [],
    })
}

/// The rows [`import_preview`] prints, one per entry in the document's order.
/// `--yes` prints them too, before the applied rows.
const IMPORT_PREVIEW_ROWS: &str = "\"keeps\"\tidentical\tstdio\t—\t—\t—\n\
    \"docs\"\tchanged\tstdio\targs:array, enabled:boolean\tx-extra:kept-unused\t—\n\
    \"taken\"\tconflict\thttp\turl:string\t—\t—\n\
    \"broken\"\tinvalid\t—\t—\tauth.audience:dropped\tbroken: type is stdio, http or sse\n";

/// The rows [`import_preview_project`] prints.
const IMPORT_PREVIEW_PROJECT_ROWS: &str = "\"keeps\"\tidentical\tstdio\t—\t—\t—\n\
    \"docs\"\tchanged\tstdio\targs:array, env:object\t—\t—\n";

/// `mcp import` previews through the owner: one row per entry with its
/// status, changed and unsupported field names and error, then the unknown
/// top-level members and the revision. The document's literal value goes to
/// the owner and never appears in the output.
#[test]
fn mcp_import_previews_every_status_without_printing_a_value() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    let document = import_document();
    fs::write(&path, &document).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"]["cwd"].is_string());
        assert_eq!(request["params"]["document"], document);
        wire.reply(&request, import_preview());
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, IMPORT_PREVIEW_ROWS);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr
            .contains("Columns: ID, status, transport, changed fields, unsupported fields, error."),
        "{stderr}"
    );
    assert!(
        stderr.contains("Top-level members the import does not carry: otherTopLevel."),
        "{stderr}"
    );
    assert!(
        stderr.contains("The user source was read at revision user:1."),
        "{stderr}"
    );
    assert!(!stdout.contains(IMPORT_TOKEN), "{stdout}");
    assert!(!stderr.contains(IMPORT_TOKEN), "{stderr}");
}

/// `--yes` prints the preview first, then applies the default selection at
/// the preview's revision: changed and identical entries import, conflict and
/// invalid entries skip.
#[test]
fn mcp_import_yes_applies_the_default_selection() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    fs::write(&path, import_document()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        wire.reply(&request, import_preview());
        let request = wire.read("mcp.import.apply");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert_eq!(
            request["params"]["resolutions"],
            json!([
                {"id": "broken", "action": "skip"},
                {"id": "docs", "action": "import"},
                {"id": "keeps", "action": "import"},
                {"id": "taken", "action": "skip"},
            ])
        );
        wire.reply(&request, json!({"revision": "user:2"}));
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .arg("--yes")
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "{IMPORT_PREVIEW_ROWS}\"keeps\"\tunchanged\tkeeps\n\
             \"docs\"\timported\tdocs\n\
             \"taken\"\tskipped\t—\n\
             \"broken\"\tskipped\t—\n"
        )
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Applying the selection at this revision."),
        "{stderr}"
    );
    assert!(
        stderr.contains("Imported 1, replaced 0, renamed 0, skipped 2, unchanged 1."),
        "{stderr}"
    );
    assert!(
        stderr.contains("Imported and replaced entries start disabled"),
        "{stderr}"
    );
    assert!(
        stderr.contains("An unchanged entry was already in the source and was not written."),
        "{stderr}"
    );
    assert!(
        stderr.contains("The source is now at revision user:2."),
        "{stderr}"
    );
}

/// `--replace` replaces the target's entry and `--rename OLD=NEW` writes
/// under a new id; each resolution carries the action the flag names.
#[test]
fn mcp_import_yes_applies_replace_and_rename() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    fs::write(&path, import_document()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        wire.reply(&request, import_preview());
        let request = wire.read("mcp.import.apply");
        assert_eq!(
            request["params"]["resolutions"],
            json!([
                {"id": "broken", "action": "skip"},
                {"id": "docs", "action": "rename", "newId": "docs2"},
                {"id": "keeps", "action": "import"},
                {"id": "taken", "action": "replace"},
            ])
        );
        wire.reply(&request, json!({"revision": "user:2"}));
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .args(["--yes", "--replace", "taken", "--rename", "docs=docs2"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "{IMPORT_PREVIEW_ROWS}\"keeps\"\tunchanged\tkeeps\n\
             \"docs\"\trenamed\tdocs2\n\
             \"taken\"\treplaced\ttaken\n\
             \"broken\"\tskipped\t—\n"
        )
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Imported 0, replaced 1, renamed 1, skipped 1, unchanged 1."),
        "{stderr}"
    );
}

/// A project import takes no confirmation: the preview says the workspace
/// file receives the public skeleton and the withheld values go to the
/// user-scope record, and the apply sends the plain resolutions.
#[test]
fn mcp_import_project_sends_withheld_values_to_the_user_record() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    fs::write(&path, import_document()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        assert_eq!(request["params"]["source"], "project");
        wire.reply(&request, import_preview_project());
        let request = wire.read("mcp.import.apply");
        assert_eq!(request["params"]["expectedVersion"], "project:1");
        assert_eq!(
            request["params"]["resolutions"],
            json!([
                {"id": "docs", "action": "import"},
                {"id": "keeps", "action": "import"},
            ])
        );
        wire.reply(&request, json!({"revision": "project:2"}));
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .args(["--source", "project", "--yes"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "{IMPORT_PREVIEW_PROJECT_ROWS}\"keeps\"\tunchanged\tkeeps\n\
             \"docs\"\timported\tdocs\n"
        )
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(
            "Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file."
        ),
        "{stderr}"
    );
}

/// A source that changed since the preview refuses the apply; the preview it
/// printed stays, no entry is written and the command says to preview again.
#[test]
fn mcp_import_reports_a_stale_revision() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    fs::write(&path, import_document()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        wire.reply(&request, import_preview());
        let request = wire.read("mcp.import.apply");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"mcp_settings_conflict"}}}));
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .arg("--yes")
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again."
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        IMPORT_PREVIEW_ROWS
    );
}

/// A daemon that does not serve the import pair is refused before the file
/// is read.
#[test]
fn mcp_import_refuses_a_daemon_without_the_import_methods() {
    let home = Home::new();
    let path = home.0.join("mcp.json");
    fs::write(&path, import_document()).unwrap();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.hold();
    });
    let output = home
        .command()
        .args(["mcp", "import"])
        .arg(&path)
        .arg("--yes")
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("This daemon cannot import MCP servers."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--discover` lists every known store with its state and no value. The
/// client names the stores; the owner reads them.
#[test]
fn mcp_import_discover_lists_known_stores_without_a_value() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.sources");
        assert!(request["params"]["cwd"].is_string(), "{request}");
        wire.reply(
            &request,
            json!({"sources": [
                {"id": "claude-code:user", "client": "claude-code", "scope": "user",
                 "path": "~/.claude.json", "state": "ready", "entries": 2},
                {"id": "codex:user", "client": "codex", "scope": "user",
                 "path": "~/.codex/config.toml", "state": "malformed",
                 "error": "the file is not valid TOML at line 2 column 1"},
                {"id": "omp:user", "client": "omp", "scope": "user",
                 "path": "~/.omp/agent/mcp.json", "state": "absent"},
            ]}),
        );
    });
    let output = home
        .command()
        .args(["mcp", "import", "--discover"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "claude-code:user\tclaude-code\tuser\t~/.claude.json\tready\t2\n\
         codex:user\tcodex\tuser\t~/.codex/config.toml\tmalformed\tthe file is not valid TOML at line 2 column 1\n\
         omp:user\tomp\tuser\t~/.omp/agent/mcp.json\tabsent\t—\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Columns: ID, client, scope, path, state, entries or error."),
        "{stderr}"
    );
    assert!(
        stderr.contains("No value from a store is read out or printed"),
        "{stderr}"
    );

    // A daemon without the discovery read is refused before a file or a
    // store is named.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.hold();
    });
    let output = home
        .command()
        .args(["mcp", "import", "--discover"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
}

/// `--from` names one known store: the preview and the apply carry its id
/// and the digest the preview answered, never its text.
#[test]
fn mcp_import_from_previews_and_applies_a_known_store() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        assert_eq!(request["params"]["from"], "codex:user");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"].get("document").is_none(), "{request}");
        let mut preview = import_preview();
        preview["sourceRevision"] = json!("9f2c");
        wire.reply(&request, preview);
        let request = wire.read("mcp.import.apply");
        assert_eq!(request["params"]["from"], "codex:user");
        assert_eq!(request["params"]["expectedSourceRevision"], "9f2c");
        assert!(request["params"].get("document").is_none(), "{request}");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        wire.reply(&request, json!({"revision": "user:2"}));
    });
    let output = home
        .command()
        .args(["mcp", "import", "--from", "codex:user", "--yes"])
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
        format!(
            "{IMPORT_PREVIEW_ROWS}\"keeps\"\tunchanged\tkeeps\n\
             \"docs\"\timported\tdocs\n\
             \"taken\"\tskipped\t—\n\
             \"broken\"\tskipped\t—\n"
        )
    );
}

/// `mcp import` reads one document: a path, `--from` or `--discover`; a
/// discovery takes no selection flags.
#[test]
fn mcp_import_takes_one_source() {
    let home = Home::new();
    for (arguments, message) in [
        (
            vec!["mcp", "import"],
            "Use one of PATH, --from ID or --discover",
        ),
        (
            vec!["mcp", "import", "document.json", "--from", "codex:user"],
            "Use one of PATH, --from ID or --discover",
        ),
        (
            vec!["mcp", "import", "document.json", "--discover"],
            "Use one of PATH, --from ID and --discover",
        ),
        (
            vec!["mcp", "import", "--discover", "--yes"],
            "--discover takes no --yes",
        ),
    ] {
        let output = home.command().args(&arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{arguments:?}: {stderr}");
    }
}

/// `mcp edit` opens the patch the configuration read seeds, without a
/// stored argument or value, and writes it at that revision.
#[test]
fn mcp_edit_opens_the_patch_without_stored_values() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "expected='{\n  \"command\": \"npx\",\n  \"cwd\": \"\",\n  \"env\": {}\n}'\n\
         [ \"$(cat \"$1\")\" = \"$expected\" ] || exit 1\n\
         printf '%s' '{\"command\":\"npx\",\"cwd\":\"/workspace\",\"env\":{\"DOCS_TOKEN\":\"value\"}}' > \"$1\"",
    );
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        configuration(wire, "docs", "user", docs(true));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.update");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert_eq!(
            request["params"]["patch"],
            json!({"command": "npx", "cwd": "/workspace",
                   "env": {"DOCS_TOKEN": "value"}})
        );
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        area(wire, cli_catalog(), docs(true));
    });
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", &script)
        .args(["mcp", "edit", "docs"]);
    let mut terminal = Terminal::launch_tui(command);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("Saved docs."),
        "{}",
        terminal.transcript
    );
}

/// `mcp set` reads the entry's revision immediately before the write, and
/// text that says what is stored writes nothing.
#[test]
fn mcp_set_reads_the_revision_immediately_before_the_write() {
    let home = Home::new();
    let path = home.0.join("patch.json");
    fs::write(
        &path,
        json!({"command": "npx", "cwd": "/workspace", "env": {"DOCS_TOKEN": "value"}}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        configuration(wire, "docs", "user", docs(true));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.update");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        area(wire, cli_catalog(), docs(true));
    });
    let output = home
        .command()
        .args(["mcp", "set", "docs"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Saved docs.\n");

    // The seed's own text is no change, and no write.
    let home = Home::new();
    let path = home.0.join("patch.json");
    fs::write(
        &path,
        json!({"command": "npx", "cwd": "", "env": {}}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        configuration(wire, "docs", "user", docs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("mcp.update"), "the unchanged set wrote");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "set", "docs"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "No change.\n");
}

/// A stale revision writes nothing, reads the area again, and names the
/// file that keeps the user's text.
#[test]
fn mcp_set_keeps_the_users_text_on_a_conflict() {
    let home = Home::new();
    let path = home.0.join("patch.json");
    fs::write(
        &path,
        json!({"command": "npx", "cwd": "/workspace", "env": {}}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        configuration(wire, "docs", "user", docs(true));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.update");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The MCP settings changed; read them before updating",
            "data":{"appError":"mcp_settings_conflict"}}}));
        area(wire, cli_catalog(), docs(true));
    });
    let output = home
        .command()
        .args(["mcp", "set", "docs"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("The MCP entry changed since it was read; nothing was written."),
        "{error}"
    );
    assert!(error.contains("Your text is kept in"), "{error}");
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}

/// A conflict whose re-read fails is still a conflict: nothing was written,
/// the command says so with the read error that followed, and the user's
/// text is kept.
#[test]
fn mcp_set_reports_a_conflict_when_the_read_after_it_fails() {
    let home = Home::new();
    let path = home.0.join("patch.json");
    fs::write(
        &path,
        json!({"command": "npx", "cwd": "/workspace", "env": {}}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        configuration(wire, "docs", "user", docs(true));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.update");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The MCP settings changed; read them before updating",
            "data":{"appError":"mcp_settings_conflict"}}}));
        // The conflict wrote nothing, and the read of the area as it now is
        // fails.
        let request = wire.read("mcp.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"MCP settings are unavailable",
            "data":{"appError":"mcp_unavailable"}}}));
    });
    let output = home
        .command()
        .args(["mcp", "set", "docs"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("The MCP entry changed since it was read; nothing was written."),
        "{error}"
    );
    assert!(error.contains("MCP settings are unavailable"), "{error}");
    assert!(error.contains("Your text is kept in"), "{error}");
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}

/// `mcp remove` deletes at the source revision; when the read after it
/// fails, the removal stands and the error goes to stderr with exit 0.
#[test]
fn mcp_remove_removes_at_the_sources_revision() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        // The command prints the removal; the read that reports the area
        // after the write is the adapter's, and its view belongs to the
        // panel, so no configuration is read here.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    daemon.join().unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Removed docs.\n");

    // The entry was removed; the read that reports the area failed.
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.remove");
        wire.reply(&request, json!({}));
        let request = wire.read("mcp.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"MCP settings are unavailable",
            "data":{"appError":"mcp_unavailable"}}}));
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Removed docs.\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("MCP settings are unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A plugin entry's write is refused before the request is formed: its
/// source has no Settings revision.
#[test]
fn mcp_remove_refuses_a_plugin_entry() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("mcp.remove"), "the plugin entry wrote");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "remove", "notes"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("plugin MCP entries change through their plugin")
    );
}

/// `mcp test` prints one row per tool, and a test that finds none says so
/// without a row.
#[test]
fn mcp_test_prints_the_tools() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.test");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(
            &request,
            json!({"status": "connected", "latencyMs": 4, "tools": [
                {"name": "search", "description": "Search docs",
                 "inputSchema": {"type": "object"}},
                {"name": "fetch", "description": null,
                 "inputSchema": {"type": "object"}},
            ]}),
        );
    });
    let output = home
        .command()
        .args(["mcp", "test", "docs"])
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
        "search\tSearch docs\nfetch\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Columns: tool, description.\nThe connection was temporary; no configuration was saved.\n"
    );

    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.test");
        wire.reply(
            &request,
            json!({"status": "connected", "latencyMs": 1, "tools": []}),
        );
    });
    let output = home
        .command()
        .args(["mcp", "test", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "The server exposes no tools.\nThe connection was temporary; no configuration was saved.\n"
    );
}

/// A failed test is the owner's refusal, with its message and exit 1.
#[test]
fn mcp_test_reports_a_failed_probe() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.test");
        wire.reply(
            &request,
            json!({"status": "authRequired", "latencyMs": 2,
            "expired": true, "tools": null}),
        );
    });
    let output = home
        .command()
        .args(["mcp", "test", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("The server rejected the stored credential"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `mcp inspect` prints the effective entry and its connections, and
/// writes nothing back.
#[test]
fn mcp_inspect_prints_the_effective_entry_and_connections() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.inspect");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(
            &request,
            json!({
                "enabled": true,
                "configuration": {
                    "type": "stdio", "command": "npx", "argsCount": 2, "cwd": null,
                    "envNames": ["DOCS_TOKEN"],
                },
                "connections": [{"state": {"status": "connected"}, "tools": [
                    {"name": "search", "description": "Search docs",
                     "inputSchema": {"type": "object"}},
                ]}, {"state": {"status": "disconnected"}, "tools": null,
                    "failure": "MCP server exited (exit status: 7): fixture diagnostic"}],
            }),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("mcp.update"), "the inspect wrote back");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "inspect", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("user · stdio · enabled · signed in (missing scopes: repo)"),
        "{stdout}"
    );
    assert!(stdout.contains("Executable: npx"), "{stdout}");
    assert!(stdout.contains("Environment names: DOCS_TOKEN"), "{stdout}");
    assert!(stdout.contains("Connection 1 · connected"), "{stdout}");
    assert!(stdout.contains("  search — Search docs"), "{stdout}");
    assert!(stdout.contains("Connection 2 · disconnected"), "{stdout}");
    assert!(
        stdout.contains("exit status: 7): fixture diagnostic"),
        "{stdout}"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Inspection does not connect or contact the server.\n"
    );
}

/// #953: `mcp add` takes `timeout` as a member of the definition document.
#[test]
fn mcp_add_passes_a_tool_call_timeout_through() {
    let home = Home::new();
    let path = home.0.join("entry.json");
    let document = json!({
        "id": "example",
        "source": "user",
        "configuration": {
            "type": "stdio", "command": "npx", "args": [], "env": {},
            "enabled": false, "timeout": 1000,
        },
    });
    fs::write(&path, document.to_string()).unwrap();
    let saved = document["configuration"].clone();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.save");
        assert_eq!(request["params"]["configuration"], saved);
        wire.reply(&request, json!({}));
        area(wire, cli_catalog(), docs(true));
    });
    let output = home
        .command()
        .args(["mcp", "add"])
        .arg(&path)
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
        "Added example.\n"
    );
}

/// #953: `mcp set` writes a `timeout` and clears one with `null`.
#[test]
fn mcp_set_writes_and_clears_a_tool_call_timeout() {
    for (text, patch) in [
        (json!({"timeout": 1000}), json!({"timeout": 1000})),
        (json!({"timeout": null}), json!({"timeout": null})),
    ] {
        let home = Home::new();
        let path = home.0.join("patch.json");
        fs::write(&path, text.to_string()).unwrap();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("mcp.catalog");
            wire.reply(&request, cli_catalog());
            configuration(wire, "docs", "user", docs(true));
            let request = wire.read("mcp.catalog");
            wire.reply(&request, cli_catalog());
            let request = wire.read("mcp.update");
            assert_eq!(request["params"]["patch"], patch);
            wire.reply(&request, json!({}));
            area(wire, cli_catalog(), docs(true));
        });
        let output = home
            .command()
            .args(["mcp", "set", "docs"])
            .arg(&path)
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "Saved docs.\n");
    }
}

/// #953: an entry's stated limit shows in `mcp inspect`.
#[test]
fn mcp_inspect_shows_a_stated_tool_call_timeout() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.inspect");
        let mut configuration = docs(true)["configuration"].clone();
        configuration["timeout"] = json!(1000);
        wire.reply(
            &request,
            json!({"enabled": true, "configuration": configuration, "connections": []}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("mcp.update"), "the inspect wrote back");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "inspect", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Tool call timeout: 1000 ms"), "{stdout}");
}

/// A daemon without the namespace refuses each new verb before any read.
#[test]
fn mcp_commands_refuse_a_daemon_without_the_namespace() {
    for verb in ["add", "set", "remove", "test", "inspect"] {
        let home = Home::new();
        let file = home.0.join("document.json");
        fs::write(&file, json!({}).to_string()).unwrap();
        let mut args = vec!["mcp".to_owned(), verb.to_owned()];
        if verb == "set" {
            args.push("docs".to_owned());
        }
        match verb {
            "add" | "set" => args.push(file.to_str().unwrap().to_owned()),
            _ => args.push("docs".to_owned()),
        }
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            wire.hold();
        });
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("This daemon cannot manage MCP servers."),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A daemon that serves the catalog but not a verb's method names that verb.
#[test]
fn mcp_commands_name_the_verb_the_daemon_cannot_serve() {
    for (verb, sentence) in [
        ("add", "This daemon lists MCP servers but cannot add them."),
        ("set", "This daemon lists MCP servers but cannot edit them."),
        (
            "remove",
            "This daemon lists MCP servers but cannot remove them.",
        ),
        ("test", "This daemon lists MCP servers but cannot run them."),
        (
            "inspect",
            "This daemon lists MCP servers but cannot inspect them.",
        ),
    ] {
        let home = Home::new();
        let file = home.0.join("document.json");
        fs::write(&file, json!({}).to_string()).unwrap();
        let mut args = vec!["mcp".to_owned(), verb.to_owned()];
        if verb == "set" {
            args.push("docs".to_owned());
        }
        match verb {
            "add" | "set" => args.push(file.to_str().unwrap().to_owned()),
            _ => args.push("docs".to_owned()),
        }
        let daemon = home.daemon(|wire| {
            wire.hello(&["mcp.catalog"]);
            wire.hold();
        });
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(sentence),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// `--help` names every MCP management verb.
#[test]
fn mcp_help_names_every_management_verb() {
    let home = Home::new();
    let output = home.command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for verb in [
        "mcp add PATH",
        "mcp import PATH",
        "mcp edit ID",
        "mcp set ID PATH",
        "mcp remove ID",
        "mcp test ID",
        "mcp inspect ID",
        "mcp disconnect ID",
    ] {
        assert!(help.contains(verb), "{verb} is missing:\n{help}");
    }
}

/// `mcp list` prints each source application's error, the plugin source
/// included.
#[test]
fn mcp_list_prints_every_sources_errors() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        let mut listed = catalog(false, Some("project file is not JSON"));
        listed["pluginApplication"]["error"] = json!("plugin entry refused");
        wire.reply(&request, listed);
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("plugin MCP configuration: plugin entry refused"),
        "{stderr}"
    );
    assert!(
        stderr.contains("project MCP configuration: project file is not JSON"),
        "{stderr}"
    );
}

/// A source, a transport, an auth status or a scope name that holds a
/// break stays on its row, and a source's error stays on its note:
/// `mcp list` prints each server as one line.
#[test]
fn mcp_list_keeps_a_fields_breaks_on_its_row() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["mcp.catalog"]);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1",
                "projectRevision": null,
                "userApplication": {"error": "first\nsecond", "cleanupError": null},
                "pluginApplication": null,
                "projectApplication": null,
                "servers": [
                    {"id": "docs", "source": "user\nuser", "enabled": true,
                     "transport": "stdio\nstdio",
                     "auth": {"status": "custom\nstatus",
                              "missingScopes": ["repo\nread"]}},
                ],
            }),
        );
        wire.hold();
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout:?}");
    for field in ["user user", "stdio stdio", "custom status"] {
        assert!(
            lines[0].contains(field),
            "{field:?} missing from {stdout:?}"
        );
    }
    assert!(lines[1].contains("Missing scopes: repo read"), "{stdout:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("first second"), "{stderr:?}");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("MCP configuration:"))
            .count(),
        1,
        "{stderr:?}"
    );
}

/// The Session's report keeps each server's state, message and refusal on
/// its own line, with the Session's catalog error after them.
#[test]
fn tui_mcp_list_keeps_a_session_state_on_one_line() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1", "projectRevision": null,
                "userApplication": {}, "projectApplication": null,
                "servers": [],
            }),
        );
        let state = wire.read("session.mcp");
        wire.reply(
            &state,
            json!({
                "sessionId": "session",
                "servers": [{"id": "docs", "state": "failed\nstate",
                             "message": "message\nline",
                             "rejection": {"version": "v1\nx", "reason": "reason\ny"}}],
                "catalogError": "catalog\nerror",
            }),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp list\r");
    terminal.until("failed state");
    terminal.until("message line");
    terminal.until("Version v1 x rejected: reason y");
    terminal.until("MCP catalog: catalog error");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A source that holds a break stays on the enablement sentence's line.
#[test]
fn mcp_enable_keeps_the_source_on_one_line() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1", "projectRevision": null,
                "userApplication": {}, "projectApplication": null,
                "servers": [
                    {"id": "docs", "source": "user\nsource", "enabled": false,
                     "transport": "stdio"},
                ],
            }),
        );
        let request = wire.read("mcp.set_enabled");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user\nsource");
        wire.reply(&request, json!({}));
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
        "Enabled \"docs\" (user source).\n"
    );
}

/// A disable that withdraws the record without a readable catalog keeps
/// the read's error on the note's line.
#[test]
fn mcp_disable_keeps_the_catalog_error_on_one_line() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"catalog\nunavailable",
            "data":{"appError":"mcp_unavailable"}}}));
        let request = wire.read("mcp.set_enabled");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "project");
        assert_eq!(request["params"]["expectedVersion"], "");
        assert_eq!(request["params"]["enabled"], false);
        wire.reply(&request, json!({}));
    });
    let output = home
        .command()
        .args(["mcp", "disable", "docs"])
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
        "Disabled \"docs\" (project).\n"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("catalog unavailable"), "{stderr:?}");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("could not be read"))
            .count(),
        1,
        "{stderr:?}"
    );
}

/// `mcp enable` and `mcp disable` keep printing what they printed before
/// the panel shared their implementation.
#[test]
fn mcp_enable_keeps_its_existing_output() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1", "projectRevision": "project:1",
                "userApplication": {}, "projectApplication": null,
                "servers": [
                    {"id": "docs", "source": "user", "enabled": false, "transport": "stdio"},
                ],
            }),
        );
        let request = wire.read("mcp.set_enabled");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert_eq!(request["params"]["enabled"], true);
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
    });
    let output = home
        .command()
        .args(["mcp", "enable", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Enabled \"docs\" (user).\n"
    );
    assert!(output.stderr.is_empty());

    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1", "projectRevision": "project:1",
                "userApplication": {}, "projectApplication": null,
                "servers": [],
            }),
        );
        let request = wire.read("mcp.set_enabled");
        assert_eq!(request["params"]["id"], "gone");
        assert_eq!(request["params"]["source"], "project");
        assert_eq!(request["params"]["expectedVersion"], "project:1");
        assert_eq!(request["params"]["enabled"], false);
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
    });
    let output = home
        .command()
        .args(["mcp", "disable", "gone"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Disabled \"gone\" (project).\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "\"gone\" is not in mcp list; withdrew any stored enablement of it in this workspace.\n"
    );
}

/// `mcp disconnect` closes the entry's live connections over the owner's
/// method and reports them closed; the line command does the same, and an
/// unknown id is refused before any request.
#[test]
fn mcp_disconnect_closes_the_entry_connections() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.disconnect");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
    });
    let output = home
        .command()
        .args(["mcp", "disconnect", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Disconnected docs.\n"
    );

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(
                !line.contains("mcp.disconnect"),
                "the unknown id wrote: {line:?}"
            );
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "disconnect", "missing"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No MCP server has that id."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        started(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.disconnect");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "user");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/mcp disconnect docs\r");
    terminal.until("Disconnected docs.");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A create the owner refuses over a newer document keeps the user's text
/// and reads the area again; the next add writes it at the newer revision.
#[test]
fn tui_mcp_panel_keeps_the_users_text_when_a_create_finds_a_conflict() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", EDITS_THE_ENTRY);
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, catalog(true, None), docs(true));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog(true, None));
        let request = wire.read("mcp.save");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The MCP settings changed; read them before updating",
            "data":{"appError":"mcp_settings_conflict"}}}));
        // The conflict writes nothing: the area is read again.
        area(wire, catalog(true, None), docs(true));
        // The next add reopens the user's text and writes it at the
        // revision the catalog reports now.
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:2", "projectRevision": "project:1",
                "userApplication": {"error": null, "cleanupError": null},
                "pluginApplication": {"error": null, "cleanupError": null},
                "projectApplication": {"error": null, "cleanupError": null},
                "servers": [
                    {"id": "docs", "source": "user", "enabled": true,
                     "transport": "stdio"},
                ],
            }),
        );
        let request = wire.read("mcp.save");
        assert_eq!(request["params"]["expectedVersion"], "user:2");
        assert_eq!(request["params"]["id"], "example");
        wire.reply(&request, json!({}));
        area(wire, catalog(true, None), docs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(edit_command(&home, &script, false));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("Executable: npx");
    terminal.raw(b"\x0e");
    terminal.until("The MCP entries changed since they were read; nothing was written.");
    terminal.until("text is kept in");
    terminal.raw(b"\x0e");
    terminal.until("Added example.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Tool text from a third-party server is drawn on one line, without the
/// escape sequences it carries.
#[test]
fn mcp_test_sanitizes_tool_text() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.test");
        wire.reply(
            &request,
            json!({"status": "connected", "latencyMs": 1, "tools": [
                {"name": "search", "description": "Search\u{1b}[2J\ndocs",
                 "inputSchema": {"type": "object"}},
            ]}),
        );
    });
    let output = home
        .command()
        .args(["mcp", "test", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "search\tSearch docs\n");
    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains("[2J"),
        "{stdout:?}"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("[2J"));
}

/// The typed patch and the owner's decoder agree, through the real App
/// Server: add with the panel's template, patch one env key and `cwd`,
/// inspect what the store holds, remove, and list again.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn mcp_entries_round_trip_through_the_real_app_server() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir_all(&project).unwrap();
    let command = |args: &[&str]| {
        let mut command = home.command();
        command
            .arg("--server")
            .arg(&binary)
            .arg("--model-config")
            .arg(&config)
            .arg("--cwd")
            .arg(&project);
        command.args(args);
        command
    };
    let run = |args: &[&str], input: Option<String>| {
        let mut command = command(args);
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        wait(child)
    };
    let document = json!({
        "id": "round-trip",
        "source": "user",
        "configuration": {
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-everything"],
            "env": {},
            "enabled": false,
        },
    });
    let output = run(&["mcp", "add", "-"], Some(document.to_string()));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Added round-trip.\n"
    );
    let patch = json!({"cwd": "/tmp", "env": {"ROUND_TRIP": "value"}});
    let output = run(&["mcp", "set", "round-trip", "-"], Some(patch.to_string()));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Saved round-trip.\n"
    );
    let output = run(&["mcp", "inspect", "round-trip"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Working directory: /tmp"), "{stdout}");
    assert!(stdout.contains("Environment names: ROUND_TRIP"), "{stdout}");
    let output = run(&["mcp", "remove", "round-trip"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Removed round-trip.\n"
    );
    let output = run(&["mcp", "list"], None);
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("round-trip"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The catalog with one entry that did not decode beside its valid entries.
fn broken_catalog() -> Value {
    let mut listed = catalog(true, None);
    listed["invalid"] = json!([{
        "id": "broken",
        "source": "user",
        "error": "MCP server broken: field type must be http or sse; ws is not a supported transport",
    }]);
    listed
}

/// `mcp list` prints a broken entry on its own row, with its source and its
/// error, after the valid rows.
#[test]
fn mcp_list_prints_a_broken_entry_with_its_error() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let broken = stdout
        .lines()
        .find(|line| line.contains("\"broken\""))
        .unwrap_or_else(|| panic!("no broken row in {stdout:?}"));
    assert!(broken.contains("user"), "{broken:?}");
    assert!(broken.contains("invalid"), "{broken:?}");
    assert!(broken.contains("must be http or sse"), "{broken:?}");
    assert!(
        stdout.lines().any(|line| line.contains("\"docs\"")),
        "{stdout:?}"
    );
}

/// `mcp remove` removes a broken entry at its source revision.
#[test]
fn mcp_remove_removes_a_broken_entry_at_its_sources_revision() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(request["params"]["id"], "broken");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
    });
    let output = home
        .command()
        .args(["mcp", "remove", "broken"])
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
        "Removed broken.\n"
    );
}

/// An add whose id names a broken entry of the same source replaces it and
/// says so; it is not refused as a duplicate.
#[test]
fn mcp_add_replaces_a_broken_entry() {
    let home = Home::new();
    let path = home.0.join("entry.json");
    let mut document = new_entry("user");
    document["id"] = json!("broken");
    fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();
    let saved = document.clone();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
        let request = wire.read("mcp.save");
        assert_eq!(request["params"]["id"], "broken");
        assert_eq!(request["params"]["source"], "user");
        assert_eq!(request["params"]["expectedVersion"], "user:1");
        assert_eq!(request["params"]["configuration"], saved["configuration"]);
        wire.reply(&request, json!({}));
        area(wire, broken_catalog(), docs(true));
    });
    let output = home
        .command()
        .args(["mcp", "add"])
        .arg(&path)
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
        "Replaced broken.\n"
    );
}

/// A broken entry is not enableable: the command refuses it with its own
/// error before any write.
#[test]
fn mcp_enable_refuses_a_broken_entry_with_its_error() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                assert_ne!(
                    value["method"], "mcp.set_enabled",
                    "a broken entry was toggled"
                );
            }
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "enable", "broken"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("field type must be http or sse"),
        "{stderr:?}"
    );
}

/// `/mcp` marks a broken entry invalid, shows its error, and offers only
/// removal: Enter and Ctrl+T send nothing, and Ctrl+X removes it.
#[test]
fn tui_mcp_panel_marks_a_broken_entry_invalid_and_offers_removal() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        area(wire, broken_catalog(), docs(true));
        // The removal reads the entry's source, then writes at its revision
        // and reports the area again.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, broken_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "broken", "source": "user",
                   "expectedVersion": "user:1"})
        );
        wire.reply(&request, json!({}));
        area(wire, catalog(true, None), docs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("user · stdio · signed in (missing scopes: repo)");
    // The broken row follows the valid user entry.
    terminal.raw(b"\x1b[B");
    terminal.until("broken");
    terminal.until("user · invalid");
    terminal.until("must be http or sse");
    // Enter and Ctrl+T are not offered; removal is.
    terminal.raw(b"\r");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x14");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x18");
    terminal.until("broken? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("Removed broken.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// The catalog of the reviewer's scenario: a valid user entry and a broken
/// project entry of one id.
fn two_sources_catalog() -> Value {
    let mut listed = catalog(true, None);
    listed["servers"] = json!([
        {"id": "docs", "source": "user", "enabled": true, "transport": "stdio",
         "auth": {"status": "authorized", "missingScopes": []}},
    ]);
    listed["invalid"] = json!([{
        "id": "docs",
        "source": "project",
        "error": "MCP server docs: field type must be http or sse; ws is not a supported transport",
    }]);
    listed
}

/// Two sources' valid entries of one id: the user entry enabled, the project
/// entry disabled.
fn two_valid_sources_catalog() -> Value {
    let mut listed = catalog(true, None);
    listed["servers"] = json!([
        {"id": "docs", "source": "user", "enabled": true, "transport": "stdio",
         "auth": {"status": "authorized", "missingScopes": []}},
        {"id": "docs", "source": "project", "enabled": false, "transport": "http",
         "auth": {"status": "required"}},
    ]);
    listed
}

/// A valid plugin entry and a broken project entry of one id: the plugin
/// first in catalog order, as the catalog reports it.
fn plugin_and_broken_catalog() -> Value {
    let mut listed = catalog(true, None);
    listed["servers"] = json!([
        {"id": "docs", "source": "plugin", "enabled": true, "transport": "stdio"},
    ]);
    listed["invalid"] = json!([{
        "id": "docs",
        "source": "project",
        "error": "MCP server docs: field type must be http or sse; ws is not a supported transport",
    }]);
    listed
}

/// `/mcp` lists two sources' entries of one id as two rows, and removing the
/// broken project row removes the project entry: the user entry of the same
/// id is untouched.
#[test]
fn tui_mcp_panel_removes_the_row_of_the_source_it_names() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        // The area read: the catalog, then the valid entry's configuration.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_sources_catalog());
        configuration(wire, "docs", "user", docs(true));
        // The removal resolves the selected row's source, then writes at the
        // project revision: the user entry is not the one it names.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_sources_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(
            request["params"],
            json!({"cwd": "/workspace", "id": "docs", "source": "project",
                   "expectedVersion": "project:1"})
        );
        wire.reply(&request, json!({}));
        // The area after it keeps the user entry, which the panel reads.
        area(wire, catalog(true, None), docs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                assert_ne!(value["method"], "mcp.remove", "a second remove was sent");
            }
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp\r");
    terminal.until("user · stdio · signed in");
    terminal.until("Environment names: DOCS_TOKEN");
    // The broken project row is the second row of the id.
    terminal.raw(b"\x1b[B");
    terminal.until("project · invalid");
    terminal.until("must be http or sse");
    terminal.raw(b"\x18");
    terminal.until("docs? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("Removed docs.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `mcp remove` refuses an id more than one source lists, naming every
/// source and the flag that settles one, and writes nothing.
#[test]
fn mcp_remove_refuses_an_id_that_more_than_one_source_lists() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_sources_catalog());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                assert_ne!(
                    value["method"], "mcp.remove",
                    "an ambiguous id was removed: {line:?}"
                );
            }
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"docs\" is in more than one MCP source: user, project."),
        "{stderr:?}"
    );
    assert!(stderr.contains("--source"), "{stderr:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

/// `mcp remove ID --source SOURCE` removes that source's entry and leaves
/// the other source's entry of the id alone.
#[test]
fn mcp_remove_takes_the_source_that_picks_the_entry() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_sources_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "project");
        assert_eq!(request["params"]["expectedVersion"], "project:1");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_sources_catalog());
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs", "--source", "project"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Removed docs.\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

/// `--source` picks one source: an id that source does not list is refused
/// with the source named, and nothing is written.
#[test]
fn mcp_remove_refuses_a_source_that_does_not_list_the_id() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                assert_ne!(value["method"], "mcp.remove", "an unlisted source wrote");
            }
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs", "--source", "project"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No MCP server \"docs\" is in source \"project\"."),
        "{stderr:?}"
    );
}

/// The in-session `/mcp` commands take the source their refusal asks for: an
/// id more than one source lists is refused without it, and with it the
/// named source's entry is the one written.
#[test]
fn tui_mcp_line_commands_take_the_source_that_picks_the_entry() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        started(wire);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        // `/mcp disable docs` reads the catalog and refuses: two sources list it.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_valid_sources_catalog());
        // `/mcp enable docs --source project` resolves the project entry and
        // writes at the project revision.
        let request = wire.read("mcp.catalog");
        wire.reply(&request, two_valid_sources_catalog());
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
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/mcp disable docs\r");
    terminal.until("\"docs\" is in more than one MCP source: user, project.");
    terminal.write(b"/mcp enable docs --source project\r");
    terminal.until("Enabled \"docs\" (project).");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// A valid plugin entry and a broken project entry of one id: the broken
/// row is removable by its source. The id-first lookup found the plugin
/// first and refused the write, so this row could not be removed at all.
#[test]
fn mcp_remove_reaches_a_broken_row_beside_a_valid_plugin_entry() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, plugin_and_broken_catalog());
        let request = wire.read("mcp.remove");
        assert_eq!(request["params"]["id"], "docs");
        assert_eq!(request["params"]["source"], "project");
        assert_eq!(request["params"]["expectedVersion"], "project:1");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, json!({}));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, plugin_and_broken_catalog());
    });
    let output = home
        .command()
        .args(["mcp", "remove", "docs", "--source", "project"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Removed docs.\n");
}

/// `mcp list` prints the referenced and unset names the daemon reports.
#[test]
fn mcp_list_names_referenced_and_unset_variables() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["mcp.catalog"]);
        let request = wire.read("mcp.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1",
                "projectRevision": null,
                "userApplication": {"error": null, "cleanupError": null},
                "pluginApplication": null,
                "projectApplication": null,
                "servers": [{
                    "id": "docs", "source": "user", "enabled": true,
                    "transport": "http",
                    "referencedVariables": ["MCP_HOST", "MCP_TOKEN"],
                    "unsetVariables": ["MCP_TOKEN"],
                }],
            }),
        );
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Variables: MCP_HOST, MCP_TOKEN"),
        "{stdout}"
    );
    assert!(stdout.contains("Unset variables: MCP_TOKEN"), "{stdout}");
    assert!(!stdout.contains("made-up-value"), "{stdout}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Variables and unset variables are names only, never values."),
        "{stderr}"
    );
}

/// `mcp inspect` keeps an endpoint reference as written and prints the names.
#[test]
fn mcp_configuration_keeps_a_reference() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, cli_catalog());
        let request = wire.read("mcp.inspect");
        wire.reply(
            &request,
            json!({
                "enabled": true,
                "configuration": {
                    "type": "http",
                    "endpoint": "https://${MCP_HOST}",
                    "headerNames": ["Authorization"],
                    "authConfigured": true,
                    "oauthConfigured": false,
                    "referencedVariables": ["MCP_HOST", "MCP_TOKEN"],
                    "unsetVariables": ["MCP_TOKEN"],
                },
                "connections": [],
            }),
        );
        wire.hold();
    });
    let output = home
        .command()
        .args(["mcp", "inspect", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Endpoint: https://${MCP_HOST}"), "{stdout}");
    assert!(
        stdout.contains("Variables: MCP_HOST, MCP_TOKEN"),
        "{stdout}"
    );
    assert!(stdout.contains("Unset variables: MCP_TOKEN"), "{stdout}");
}

/// `--from` prints the store the preview read, in the same words
/// `mcp.import.sources` reports it.
#[test]
fn mcp_import_names_the_store_it_read() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("mcp.import.preview");
        assert_eq!(request["params"]["from"], "claude-code:user");
        assert!(request["params"].get("document").is_none());
        wire.reply(
            &request,
            json!({
                "revision": "user:1",
                "sourceRevision": "digest:1",
                "store": {
                    "id": "claude-code:user",
                    "client": "claude-code",
                    "scope": "user",
                    "path": "~/.claude.json",
                },
                "entries": [],
                "unknownTopLevel": [],
            }),
        );
    });
    let output = home
        .command()
        .args(["mcp", "import", "--from", "claude-code:user"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("claude-code:user · claude-code · user · ~/.claude.json"),
        "{stderr}"
    );
}
