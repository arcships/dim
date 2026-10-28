pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter Ausgabe/Mitglied unterbrechen · x Befehl stoppen · Esc schließen"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter Ausgabe/Mitglied unterbrechen · x Befehl stoppen · Ctrl+X alle stoppen · Esc schließen"
        }
        "Loading branch history... Stop-all unavailable" => {
            "Lade Zweigverlauf… Alle-stoppen nicht verfügbar"
        }
        "Branch history failed; reopen to retry" => "Zweigverlauf fehlgeschlagen; neu öffnen",
        "Branch history is incomplete; stop-all unavailable" => {
            "Zweigverlauf unvollständig; Alle-stoppen nicht verfügbar"
        }
        "Running" => "Läuft",
        "Lines" => "Zweige",
        "Enter show the line · x end it · Esc close" => {
            "Enter Zweig anzeigen · x beenden · Esc schließen"
        }
        "This session has no lines." => "Diese Sitzung hat keine Zweige.",
        "No matching lines." => "Keine passenden Zweige.",
        "the session itself" => "die Sitzung selbst",
        "working" => "arbeitet",
        "ended itself" => "hat sich selbst beendet",
        "ended by an operation" => "durch einen Vorgang beendet",
        "ended with {id}" => "mit {id} beendet",
        "creation failed" => "Erstellung fehlgeschlagen",
        "shown" => "angezeigt",
        "line" => "Zweig",
        "line {id}" => "Zweig {id}",
        "follow, talk to and end this session's lines" => {
            "Zweige dieser Sitzung verfolgen, ansprechen und beenden"
        }
        "End line" => "Zweig beenden",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main ist die Sitzung selbst; ein Zweig endet, main nicht"
        }
        "Line has ended:" => "Zweig wurde beendet:",
        "Starting" => "Startet",
        "Delegation result" => "Delegationsergebnis",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter Details · x Auswahl stoppen · Ctrl+X alle stoppen · Esc schließen"
        }
        "x interrupt member · Esc back" => "x Mitglied unterbrechen · Esc zurück",
        "read-only" => "schreibgeschützt",
        "Member Session" => "Mitglieds-Session",
        "Loading conversation..." => "Lade Gespräch…",
        "No conversation yet" => "Noch kein Gespräch",
        "Observing conversation" => "Beobachte Gespräch",
        "Conversation unavailable" => "Gespräch nicht verfügbar",
        "Session closed" => "Session geschlossen",
        "Member conversation" => "Mitglieds-Gespräch",
        "x stop command · Esc back" => "x Befehl stoppen · Esc zurück",
        "Esc back" => "Esc zurück",
        "Tail preview - last 2,000 lines / 50 KiB" => "Endvorschau - letzte 2.000 Zeilen / 50 KiB",
        "Loading output..." => "Lade Ausgabe…",
        "No output yet" => "Noch keine Ausgabe",
        "Live output unavailable" => "Live-Ausgabe nicht verfügbar",
        "Live output unavailable in this profile" => {
            "Live-Ausgabe in diesem Profil nicht verfügbar"
        }
        "Output read failed" => "Ausgabe-Lesefehler",
        "Completed" => "Fertig",
        "Failed" => "Fehler",
        "Cancelled" => "Abgebrochen",
        "workflow" => "Workflow",
        "Stopping" => "Stoppt",
        "Stopped" => "Gestoppt",
        "Interrupted" => "Unterbrochen",
        "last observed" => "zuletzt beobachtet",
        "Loading Workflow runs... Stop-all unavailable" => {
            "Lade Workflow-Läufe… Alle-stoppen nicht verfügbar"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: zuletzt beobachtet; Aktualisierungsfehler, neu öffnen"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Workflow-Läufe nicht verfügbar; neu öffnen"
        }
        "The selected Workflow run is no longer available" => {
            "Gewählter Workflow-Lauf nicht mehr verfügbar"
        }
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Workflow-Liste unvollständig; Alle-stoppen nicht verfügbar"
        }
        "Workflow stop unavailable" => "Workflow-Stopp nicht verfügbar",
        "Run no longer available; last observed record" => {
            "Lauf nicht mehr verfügbar; letzter Stand"
        }
        "x stop workflow · Esc back" => "x Workflow stoppen · Esc zurück",
        "Run" => "Lauf",
        "Source" => "Quelle",
        "Stages" => "Phasen",
        "result" => "Ergebnis",
        "Log" => "Log",
        "Report" => "Bericht",
        "waiting" => "wartet",
        "launching" => "startet",
        "completed" => "fertig",
        "failed" => "Fehler",
        "stopped" => "gestoppt",
        "skipped" => "übersprungen",
        "lost" => "verloren",
        "none" => "keine",
        "pending" => "ausstehend",
        "delivered" => "zugestellt",
        "undeliverable" => "unzustellbar",
        "Agent is starting; no member Session to stop yet" => {
            "Agent startet; noch keine Mitglieds-Session zum Stoppen"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "Provider-Mitglieder steuert der Leiter; hier kein Stopp"
        }
        "Provider members have no conversation to open here" => {
            "Provider-Mitglieder haben hier kein Gespräch"
        }
        "That delegation is no longer in this transcript" => "Delegation nicht mehr im Verlauf",
        "Member model" => "Mitgliedsmodell",
        "No model observed" => "Kein Modell beobachtet",
        "Observing member..." => "Beobachte Mitglied…",
        "{model} (observation unavailable)" => "{model} (Beobachtung nicht verfügbar)",
        "{model} (Session closed)" => "{model} (Session geschlossen)",
        "{model} (last observed)" => "{model} (zuletzt beobachtet)",
        "Agent: {name}" => "Agent: {name}",
        "Provider member: {name}" => "Provider-Mitglied: {name}",
        "Task: {task}" => "Aufgabe: {task}",
        "Activity: {activity}" => "Aktivität: {activity}",
        "Activity: not observed on this attachment" => {
            "Aktivität: in dieser Verbindung nicht beobachtet"
        }
        "Result: {result}" => "Ergebnis: {result}",
        "Controlled by the leader" => "Vom Leiter gesteuert",
        "› Open member conversation" => "› Mitglieds-Gespräch öffnen",
        "Copied to clipboard" => "In Zwischenablage kopiert",
        "Opened link" => "Link geöffnet",
        "Link: {url}" => "Link: {url}",
        "Not available" => "Nicht verfügbar",
        "files changed  {count}" => "Dateien geändert  {count}",
        "active session" => "aktive Session",
        "new-session draft" => "Entwurf für neue Session",
        "Tool approvals: {title} ({target})" => "Tool-Freigaben: {title} ({target})",
        "Disconnected · draft kept" => "Getrennt · Entwurf behalten",
        "The App Server closed this interface: {reason}" => {
            "App Server hat Oberfläche geschlossen: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Getrennt; Entwurf behalten. Beenden und Session fortsetzen."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Getrennt; Entwurf behalten. Beenden und ausführen: {command}"
        }
        "Disconnected; command outcome unknown" => "Getrennt; Befehlsergebnis unbekannt",
        "Message queued" => "Nachricht eingereiht",
        "Run a shell command · Esc to exit" => "Shell-Befehl ausführen · Esc beenden",
        "! shell mode" => "! Shell-Modus",
        "!! shell mode · not sent to the model" => "!! Shell-Modus · nicht ans Modell gesendet",
        "Leave shell mode to quote a message" => "Shell-Modus verlassen, um zu zitieren",
        "A shell command is already running" => "Ein Shell-Befehl läuft bereits",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "Läuft… · Ctrl+C unterbrechen · {seconds}s"
        }
        "exit code {code}" => "Exit-Code {code}",
        "killed by {signal}" => "beendet durch {signal}",
        "interrupted" => "unterbrochen",
        "done" => "fertig",
        "failed: {reason}" => "Fehler: {reason}",
        "Shell output was not recorded: {reason}" => "Shell-Ausgabe nicht gespeichert: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "Lese Zwischenablage…",
        "Clipboard: {reason}" => "Zwischenablage: {reason}",
        "Wait for clipboard insertion before sending" => "Vor Senden auf Zwischenablage warten",
        "A submission accepts at most 20 images" => "Pro Sendung höchstens 20 Bilder",
        "[Image #{index}]" => "[Bild #{index}]",
        "The current model does not accept images; remove {label} to send" => {
            "Modell akzeptiert keine Bilder; {label} entfernen"
        }
        "Files" => "Dateien",
        "Searching…" => "Suche…",
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ wählen · enter/tab einfügen · esc schließen"
        }
        "not sent to the model" => "nicht ans Modell gesendet",
        "… {count} more lines" => "… {count} weitere Zeilen",
        "… {count} earlier lines" => "… {count} frühere Zeilen",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Unterbrechung angefordert; erneut Ctrl+C zum Beenden"
        }
        "Press Ctrl+C again to exit" => "Erneut Ctrl+C zum Beenden",
        "User interrupted the run." => "Lauf vom Benutzer unterbrochen.",
        "Could not save the language: {error}" => "Sprache nicht gespeichert: {error}",
        "Language set to {locale}" => "Sprache auf {locale} gesetzt",
        "Language set to system default" => "Sprache auf Systemstandard gesetzt",
        "Connected {provider}" => "{provider} verbunden",
        "Opened {url}" => "{url} geöffnet",
        "Tool details not saved: {error}" => "Tool-Details nicht gespeichert: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} braucht eine Session — erst Nachricht senden"
        }
        "Nothing is running" => "Nichts läuft",
        "No queued message" => "Keine Nachricht eingereiht",
        "Only in fullscreen (/tui)" => "Nur im Vollbild (/tui)",
        "/{name} is not yet available" => "/{name} noch nicht verfügbar",
        "new session" => "neue Session",
        "resumed {id} · {count} messages" => "{id} fortgesetzt · {count} Nachrichten",
        " · earlier history: /history" => " · früherer Verlauf: /history",
        "Error: {message}" => "Fehler: {message}",
        "working…  {elapsed}" => "arbeitet…  {elapsed}",
        "thinking…  {elapsed}" => "denkt…  {elapsed}",
        "Ask anything..." => "Frage stellen…",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "Früherer Verlauf · {id} · {count} Nachrichten · /history für vorherige Seite"
        }
        "No active session" => "Keine aktive Session",
        "No earlier messages." => "Keine früheren Nachrichten.",
        "answer required" => "Antwort nötig",
        "permission required" => "Freigabe nötig",
        "esc interrupt" => "esc unterbrechen",
        "↑ edit · ctrl+s steer now · /queue" => "↑ bearbeiten · ctrl+s jetzt steuern · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "+{count} weitere · ↑ bearbeiten · ctrl+s jetzt steuern · /queue"
        }
        "Queue ({count})" => "Warteschlange ({count})",
        "Thinking…" => "Denkt…",
        "Thought" => "Gedacht",
        "Thought for {duration}" => "{duration} nachgedacht",
        "Welcome to DIM {version}!" => "Willkommen bei DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => "/ Befehle · Ctrl+O Palette",
        "Workspace" => "Workspace",
        "Git branch" => "Git-Branch",
        "Bye!" => "Tschüss!",
        "DIM Web is ready" => "DIM Web ist bereit",
        "Opened in your browser" => "Im Browser geöffnet",
        "Open the sign-in link below" => "Anmeldelink unten öffnen",
        "Web" => "Web",
        "Sign-in link:" => "Anmeldelink:",
        "Resume" => "Fortsetzen",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C beendet Web-Workspace.",
        "To resume this session:" => "Diese Session fortsetzen:",

        // composer/commands.rs: categories
        "session" => "Session",
        "run" => "Lauf",
        "view" => "Ansicht",
        "model" => "Modell",
        "app" => "App",
        "skill" => "Skill",
        // composer/commands.rs: descriptions
        "start a new session" => "neue Session starten",
        "show or set the reasoning effort" => "Denkaufwand anzeigen/setzen",
        "show or set the priority service tier (on|off)" => {
            "Prioritäts-Tier anzeigen/setzen (on|off)"
        }
        "list agents or switch this session's agent" => {
            "Agents auflisten oder Session-Agent wechseln"
        }
        "list MCP servers, sign in or out (login|logout ID)" => {
            "MCP-Server auflisten, an- oder abmelden (login|logout ID)"
        }
        "resume a previous session" => "frühere Session fortsetzen",
        "open this session in the browser" => "diese Session im Browser öffnen",
        "load the previous page of messages" => "vorherige Nachrichtenseite laden",
        "show the context window usage" => "Kontextfenster-Nutzung anzeigen",
        "interrupt the run" => "Lauf unterbrechen",
        "steer with the next queued message" => "mit nächster eingereihter Nachricht steuern",
        "scroll to the top" => "nach oben scrollen",
        "scroll to the bottom" => "nach unten scrollen",
        "pick an earlier message to edit or quote" => "frühere Nachricht bearbeiten oder zitieren",
        "collapse or expand tool details and thoughts" => {
            "Tool-Details und Gedanken ein-/ausklappen"
        }
        "switch between fullscreen and scrollback" => "Vollbild/Scrollback wechseln",
        "switch theme" => "Theme wechseln",
        "switch language" => "Sprache wechseln",
        "select a model" => "Modell wählen",
        "connect a provider" => "Provider verbinden",
        "sign in to DimAgent" => "bei DimAgent anmelden",
        "DimAgent account · usage · sign out" => "DimAgent-Konto · Nutzung · abmelden",
        "configure tool approvals" => "Tool-Freigaben konfigurieren",
        "open settings" => "Einstellungen öffnen",
        "suspend to the shell" => "in Shell pausieren",
        "exit" => "beenden",

        // composer/palette.rs
        "Enter select · Ctrl+X clear saved · Esc clear/close" => {
            "Enter wählen · Ctrl+X Gespeichertes löschen · Esc leeren/schließen"
        }
        "Enter select · Esc clear/close" => "Enter wählen · Esc leeren/schließen",
        "esc close" => "esc schließen",
        "Suggestions" => "Vorschläge",
        "Skills" => "Skills",
        "Commands" => "Befehle",
        "Command Palette" => "Befehlspalette",
        "No matches" => "Keine Treffer",
        " (unavailable)" => " (nicht verfügbar)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Text: {preview} ({count} Zeichen)]",
        "[... {count} lines]" => "[... {count} Zeilen]",
        "pasted text" => "eingefügter Text",

        // list_panel.rs

        // settings_panel.rs
        "on" => "an",
        "off" => "aus",
        "Interface" => "Oberfläche",
        "Theme" => "Theme",
        "Language" => "Sprache",
        "Startup UI form" => "Start-Oberfläche",
        "fullscreen" => "Vollbild",
        "scrollback" => "Scrollback",
        "Tool details" => "Tool-Details",
        "expanded" => "ausgeklappt",
        "collapsed" => "eingeklappt",
        "Mouse (fullscreen)" => "Maus (Vollbild)",
        "Permissions" => "Berechtigungen",
        "Tool approvals" => "Tool-Freigaben",
        "Models" => "Modelle",
        "Model" => "Modell",
        "not set" => "nicht gesetzt",
        "Settings" => "Einstellungen",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ bewegen · Enter wählen · ←→/Space ändern · Esc schließen"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ Vorschau · Enter speichern · Esc wiederherstellen"
        }
        "System default ({locale})" => "Systemstandard ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ Vorschau · Enter anwenden · Esc abbrechen"
        }
        "System" => "System",
        "Could not save the theme: {error}" => "Theme nicht gespeichert: {error}",

        // sessions_panel.rs
        "Sessions" => "Sessions",
        "Enter open · Ctrl+R refresh · Esc close" => {
            "Enter öffnen · Ctrl+R aktualisieren · Esc schließen"
        }
        "Loading sessions..." => "Lade Sessions…",
        "No saved sessions yet." => "Noch keine gespeicherten Sessions.",
        "No matching sessions." => "Keine passenden Sessions.",
        "now" => "jetzt",
        "{count}m ago" => "vor {count} Min.",
        "{count}h ago" => "vor {count} Std.",
        "{count}d ago" => "vor {count} T.",

        // models_panel.rs
        "Select a model" => "Modell wählen",
        "Loading models..." => "Lade Modelle…",
        "No models available. Connect a provider first." => {
            "Keine Modelle. Erst Provider verbinden."
        }
        "No matching models." => "Keine passenden Modelle.",
        "current" => "aktuell",
        "Edit models" => "Modelle bearbeiten",
        "Editing models is not available yet" => "Modelle bearbeiten noch nicht verfügbar",
        "Enter select · Esc back" => "Enter wählen · Esc zurück",
        "Subagent models" => "Subagent-Modelle",
        "Subagent models: {policy}" => "Subagent-Modelle: {policy}",
        "Fixed subagent model" => "Festes Subagent-Modell",
        "Agent choice (default)" => "Agent-Wahl (Standard)",
        "Agent choice" => "Agent-Wahl",
        "Inherit requesting branch" => "Vom anfragenden Zweig erben",
        "Fixed model" => "Festes Modell",
        "Applies to new members, including named Agents." => {
            "Gilt für neue Mitglieder, auch benannte Agents."
        }
        "Existing members keep their models." => "Bestehende Mitglieder behalten ihre Modelle.",
        "unknown" => "unbekannt",
        "Subagent model policy saved" => "Subagent-Modellrichtlinie gespeichert",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Subagent-Modellrichtlinie extern geändert; neu wählen"
        }
        "Subagent model policy: {error}" => "Subagent-Modellrichtlinie: {error}",
        "drawing diagram… {count} lines" => "zeichne Diagramm… {count} Zeilen",
        // notify.rs
        "Task complete" => "Aufgabe erledigt",
        "Permission needed: {tool}" => "Freigabe nötig: {tool}",
        "Question waiting for your answer" => "Frage wartet auf Antwort",
        "Notify when agent needs you" => "Benachrichtigen, wenn Agent Eingabe braucht",
        // keys.rs
        "show the key bindings" => "Tastenbelegung anzeigen",
        "Application keys" => "Anwendungstasten",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter senden · Shift+Enter/Ctrl+J neue Zeile · ↑↓ Verlauf"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O/Ctrl+P Befehle · Esc unterbrechen/leeren · Ctrl+S Eingereihte steuern · ↑ Eingereihte bearbeiten"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C leeren, zweimal beenden · Ctrl+D beendet bei leerem Entwurf"
        }
        "Fullscreen" => "Vollbild",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown scrollen · Home/End Anfang/Ende · Tab Nachricht wählen, e bearbeiten, q zitieren"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "Mausrad scrollen · ziehen markiert+kopiert · Klick öffnet Link, klappt Tool-Gruppe aus"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "Maus aus (/settings): Terminal markiert und kopiert"
        }
        "Terminal-owned history" => "Terminal-eigener Verlauf",
        "scroll: {value}" => "scrollen: {value}",
        "search: {value}" => "suchen: {value}",
        "copy: {value}" => "kopieren: {value}",
        "terminal search" => "Terminal-Suche",
        "terminal scrollback" => "Terminal-Scrollback",
        "terminal copy" => "Terminal-Kopie",
        "drag to select" => "ziehen zum Markieren",
        "drag or {shortcut}" => "ziehen oder {shortcut}",
        _ => return None,
    })
}
