pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Wird geladen…",
        "No entries." => "Keine Einträge.",
        "No matching entry." => "Kein passender Eintrag.",
        "Issues" => "Probleme",
        "sources" => "Quellen",
        "skills" => "Skills",
        "hooks" => "Hooks",
        "scheduled jobs" => "geplante Jobs",
        "models" => "Modelle",
        "workflow definitions" => "Workflow-Definitionen",
        "command rules" => "Befehlsregeln",
        "enabled" => "aktiviert",
        "shadowed by {name}" => "von {name} verdeckt",
        "Reloaded." => "Neu geladen.",
        "Enabled {name}." => "{name} aktiviert.",
        "Disabled {name}." => "{name} deaktiviert.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} neu geladen: {entries} Einträge, {disabled} deaktiviert."
        }
        "Reloaded {area}: {entries} entries." => "{area} neu geladen: {entries} Einträge.",
        "Hooks" => "Hooks",
        "invalid" => "ungültig",
        "Saved." => "Gespeichert.",
        "Saved {name}." => "{name} gespeichert.",
        "No change." => "Keine Änderung.",
        "Your text is kept in {path}." => "Dein Text bleibt in {path} erhalten.",
        "The user's own document; always in effect." => {
            "Das eigene Dokument des Benutzers; immer wirksam."
        }
        "There is no hooks file there." => "Dort gibt es keine Hook-Datei.",
        "Owned by plugin {plugin}." => "Gehört zum Plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "Das Benutzerdokument schaltet alle Hooks ab (disableAllHooks)."
        }
        "Enter toggle" => "Enter umschalten",
        "Ctrl+E edit" => "Ctrl+E bearbeiten",
        "Ctrl+N add" => "Ctrl+N hinzufügen",
        "Ctrl+R reload" => "Ctrl+R neu laden",
        "Ctrl+T run" => "Ctrl+T ausführen",
        "Ctrl+X remove" => "Ctrl+X entfernen",
        "Tab inspect" => "Tab ansehen",
        "Remove {name}?" => "{name} entfernen?",
        "↑↓ scroll · Esc back" => "↑↓ scrollen · Esc zurück",
        "Added." => "Hinzugefügt.",
        "Removed {name}." => "{name} entfernt.",
        "unavailable" => "nicht verfügbar",
        "Reset {name}?" => "{name} zurücksetzen?",
        "Enter confirm" => "Enter bestätigen",
        "Esc cancel" => "Esc abbrechen",
        "Reset {name} to its catalog entry." => "{name} auf den Katalogeintrag zurückgesetzt.",
        "Ran {name}." => "{name} ausgeführt.",
        "The user document changed since it was read; nothing was written." => {
            "Das Benutzerdokument wurde seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "The job changed since it was read; nothing was written." => {
            "Der Job wurde seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "The jobs changed since they were read; nothing was written." => {
            "Die Jobs wurden seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "Your next edit opens the current document." => {
            "Deine nächste Bearbeitung öffnet das aktuelle Dokument."
        }
        "Your next add reopens your text." => "Dein nächstes Hinzufügen öffnet deinen Text erneut.",
        "Scheduler" => "Scheduler",
        "jobs" => "Jobs",
        "Added {name}." => "{name} hinzugefügt.",
        "Run {name}: {outcome}." => "{name} ausgeführt: {outcome}.",
        "Recent runs of {name}" => "Letzte Läufe von {name}",
        "No run yet." => "Noch kein Lauf.",
        "No next run." => "Kein nächster Lauf.",
        "Next run {instant}." => "Nächster Lauf {instant}.",
        "Last run {outcome} at {instant}." => "Letzter Lauf {outcome} um {instant}.",
        "every {minutes} minutes" => "alle {minutes} Minuten",
        "at {instant}" => "um {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} in {zone}",
        "The job changed since it was read; nothing was removed." => {
            "Der Job wurde seit dem Lesen geändert; es wurde nichts entfernt."
        }
        "The Scheduler has no area document." => "Der Scheduler hat kein Bereichsdokument.",
        "No scheduled job has that id." => "Kein geplanter Job hat diese ID.",
        "A scheduled job's run takes no input document." => {
            "Der Lauf eines geplanten Jobs nimmt kein Eingabedokument an."
        }
        "An MCP server's test takes no input document." => {
            "Der Test eines MCP-Servers nimmt kein Eingabedokument an."
        }
        "This daemon cannot manage {area}." => "Kann {area} nicht verwalten.",
        "This daemon lists {area} but cannot toggle them." => {
            "Der Daemon listet {area}, kann sie aber nicht umschalten."
        }
        "This daemon lists {area} but cannot edit them." => {
            "Der Daemon listet {area}, kann sie aber nicht bearbeiten."
        }
        "This daemon lists {area} but cannot reload them." => {
            "Der Daemon listet {area}, kann sie aber nicht neu laden."
        }
        "This daemon lists {area} but cannot add them." => {
            "Der Daemon listet {area}, kann sie aber nicht hinzufügen."
        }
        "This daemon lists {area} but cannot remove them." => {
            "Der Daemon listet {area}, kann sie aber nicht entfernen."
        }
        "This daemon lists {area} but cannot run them." => {
            "Der Daemon listet {area}, kann sie aber nicht ausführen."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "Der Daemon listet {area}, kann ihre Details und ihren Verlauf aber nicht anzeigen."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "Das Dokument hat {field} entfernt, was der Eigentümer nicht leeren kann; es wurde nichts geschrieben."
        }
        "The document changes no field; nothing was written." => {
            "Das Dokument ändert kein Feld; es wurde nichts geschrieben."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "Die Hook-Einstellungen dieses Arbeitsbereichs wurden seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "Models" => "Modelle",
        "catalog" => "Katalog",
        "overrides" => "Überschreibungen",
        "added" => "hinzugefügt",
        "Models · {name}" => "Modelle · {name}",
        "undeclared" => "nicht deklariert",
        "Context window {window} · max output {output}" => {
            "Kontextfenster {window} · maximale Ausgabe {output}"
        }
        "Endpoint {endpoint}" => "Endpunkt {endpoint}",
        "Endpoint {endpoint} · override {value}" => "Endpunkt {endpoint} · Überschreibung {value}",
        "The provider reports it unavailable." => "Der Anbieter meldet es als nicht verfügbar.",
        "Limits undeclared; edit the model to declare them." => {
            "Limits sind nicht deklariert; bearbeite das Modell, um sie zu deklarieren."
        }
        "Sign in to the account to use its models." => {
            "Melde dich beim Konto an, um seine Modelle zu nutzen."
        }
        "The signed-in account changed; reload to list its models." => {
            "Das angemeldete Konto hat sich geändert; lade neu, um seine Modelle aufzulisten."
        }
        "The provider has no catalog yet." => "Der Anbieter hat noch keinen Katalog.",
        "The provider listed no models." => "Der Anbieter hat keine Modelle aufgelistet.",
        "Listed {count} models." => "{count} Modelle aufgelistet.",
        "Refreshed the models." => "Modelle aktualisiert.",
        "Could not list models: {error}. Showing the stored list." => {
            "Modelle konnten nicht aufgelistet werden: {error}. Die gespeicherte Liste wird gezeigt."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "Das angemeldete Konto hat sich geändert; das Neuladen hat Einstellungen, Überschreibungen und Deaktivierungen gelöscht."
        }
        "The models changed since they were read; nothing was removed." => {
            "Die Modelle haben sich seit dem Lesen geändert; es wurde nichts entfernt."
        }
        "The models changed since they were read; nothing was written." => {
            "Die Modelle haben sich seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "No provider has that id." => "Kein Anbieter hat diese id.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} ist nicht verfügbar ({reason}); seine Aktivierung bleibt unverändert."
        }
        "No model has that id." => "Kein Modell hat diese id.",
        "This model cannot be removed." => "Dieses Modell kann nicht entfernt werden.",
        "A Provider's models have no area document." => {
            "Die Modelle eines Anbieters haben kein Bereichsdokument."
        }
        "Models have no run operation." => "Modelle haben keine Ausführungsoperation.",
        "The document is not an object; nothing was written." => {
            "Das Dokument ist kein Objekt; es wurde nichts geschrieben."
        }
        "The document changes no editable field; nothing was written." => {
            "Das Dokument ändert kein bearbeitbares Feld; es wurde nichts geschrieben."
        }
        "The model document names no model." => "Das Modelldokument nennt kein Modell.",
        "The provider has no model catalog to write." => {
            "Dieser Anbieter hat keinen Modellkatalog zum Schreiben."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Quelle {source}",
        "Status {status}" => "Status {status}",
        "Tools {value}" => "Werkzeuge {value}",
        "Vision {value}" => "Vision {value}",
        "Reasoning {value}" => "Reasoning {value}",
        "Modalities {list}" => "Modalitäten {list}",
        "Description {text}" => "Beschreibung {text}",
        "Release date {date}" => "Veröffentlichung {date}",
        "Pricing {value}" => "Preis {value}",
        "Rate limit {value}" => "Ratenlimit {value}",
        "Matched provider {id}" => "Passender Anbieter {id}",
        "Matched model {id}" => "Passendes Modell {id}",
        "Match strategy {strategy}" => "Passstrategie {strategy}",
        "Capability overrides {fields}" => "Fähigkeitsüberschreibungen {fields}",
        "yes" => "ja",
        "no" => "nein",
        "not supported" => "nicht unterstützt",
        "supported" => "unterstützt",

        "Workflow" => "Workflow",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} gespeichert. Es bleibt deaktiviert, bis du es aktivierst."
        }
        "The definitions changed since they were read; nothing was written." => {
            "Die Definitionen wurden seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "Der Name einer Definition ist ihr Schlüssel; füge sie hinzu, um sie unter einem anderen Namen zu speichern."
        }
        "No project definition is named {name}." => "Keine Projektdefinition heißt {name}.",
        "No workflow definition is named {name}." => "Keine Workflow-Definition heißt {name}.",
        "Saved in the user workflow source; always enabled." => {
            "In der Workflow-Quelle des Benutzers gespeichert; immer aktiviert."
        }
        "Starts only after you enable it." => "Startet erst, nachdem du es aktivierst.",
        "Defined by package {package}." => "Definiert durch Paket {package}.",
        "{name} — {description} (required)" => "{name} — {description} (erforderlich)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — erforderlich",
        "{name} — default: {default}" => "{name} — Standard: {default}",
        "and {count} more" => "und {count} weitere",
        "Recent runs" => "Letzte Läufe",
        "A workflow definition is saved as an object with a source and a document." => {
            "Eine Workflow-Definition wird als Objekt mit einer Quelle und einem Dokument gespeichert."
        }
        "A definition is saved to the user source or the project source." => {
            "Eine Definition wird in der Benutzerquelle oder der Projektquelle gespeichert."
        }
        "Started {name}: {run_id}." => "{name} gestartet: {run_id}.",
        "Parameters are a JSON object of strings." => {
            "Parameter sind ein JSON-Objekt aus Zeichenketten."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "Die Liste hat ihr Größenlimit erreicht; spätere Definitionen werden nicht angezeigt."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Keine Workflow-Definition namens {name} ist in der Liste, die ihr Größenlimit erreicht hat."
        }
        "MCP" => "MCP",
        "MCP servers" => "MCP-Server",
        "Ctrl+T test" => "Ctrl+T testen",
        "sign-in required" => "Anmeldung erforderlich",
        "signed in" => "angemeldet",
        "sign-in expired" => "Anmeldung abgelaufen",
        "missing scopes: {scopes}" => "fehlende Bereiche: {scopes}",
        "Executable: {command}" => "Ausführbar: {command}",
        "Stored arguments (hidden): {count}" => "Gespeicherte Argumente (verborgen): {count}",
        "Working directory: {cwd}" => "Arbeitsverzeichnis: {cwd}",
        "Environment names: {names}" => "Umgebungsnamen: {names}",
        "Endpoint: {endpoint}" => "Endpunkt: {endpoint}",
        "Header names: {names}" => "Header-Namen: {names}",
        "Bearer token: {state}" => "Bearer-Token: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: konfiguriert (Client-ID {id})",
        "OAuth: configured" => "OAuth: konfiguriert",
        "OAuth: not configured" => "OAuth: nicht konfiguriert",
        "Metadata URL: {state}" => "Metadaten-URL: {state}",
        "configured" => "konfiguriert",
        "Owned by a plugin; change it through Plugins." => {
            "Von einem Plugin bereitgestellt; ändern Sie es über Plugins."
        }
        "MCP entry {id}" => "MCP-Eintrag {id}",
        "Connection {number} · {status}" => "Verbindung {number} · {status}",
        "Tools have not been discovered." => "Tools wurden noch nicht ermittelt.",
        "No live session connections." => "Keine aktiven Sitzungsverbindungen.",
        "Tools of {id}" => "Tools von {id}",
        "The server exposes no tools." => "Der Server stellt keine Tools bereit.",
        "The add document needs an id." => "Das Hinzufügen-Dokument braucht eine id.",
        "The add document needs a source of user or project." => {
            "Die source des Hinzufügen-Dokuments ist user oder project."
        }
        "The add document needs a configuration object." => {
            "Das Hinzufügen-Dokument braucht ein configuration-Objekt."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} steht bereits in mcp list; entfernen Sie es zuerst oder wählen Sie eine andere id."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} steht bereits in mcp list; wählen Sie eine andere id oder ändern Sie es über sein Plugin."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "Die Einstellungen dieses Eintrags haben sich seit dem Lesen geändert; nichts wurde geschrieben."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "Der MCP-Eintrag hat sich seit dem Lesen geändert; nichts wurde geschrieben."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "Die MCP-Einträge haben sich seit dem Lesen geändert; nichts wurde geschrieben."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "Der MCP-Eintrag hat sich seit dem Lesen geändert; nichts wurde entfernt."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "MCP-Dokumente gehören zu je einem Eintrag; dieses Speichern nennt keinen."
        }
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} steht in mehreren MCP-Quellen: {sources}. Nenne eine mit --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "Die Aktion nennt keine MCP-Quelle; eine Zeile trägt die Quelle, die ihr Eigentümer meldet."
        }
        "No MCP server {id} is in source {source}." => "Kein MCP-Server {id} in Quelle {source}.",
        "No MCP server has that id." => "Kein MCP-Server hat diese id.",
        "This workspace has no project MCP source." => {
            "Dieser Arbeitsbereich hat keine Projekt-MCP-Quelle."
        }
        "Enabled {id} ({source})." => "{id} ({source}) aktiviert.",
        "Disabled {id} ({source})." => "{id} ({source}) deaktiviert.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "Es führt den .mcp.json-Eintrag dieses Arbeitsbereichs aus, einschließlich späterer Änderungen, in aktiven und neuen Sitzungen."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} steht nicht in mcp list; eine gespeicherte Aktivierung in diesem Arbeitsbereich wurde zurückgenommen."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "Der MCP-Katalog konnte nicht gelesen werden ({error}); die Aktivierung von {id} in diesem Arbeitsbereich wurde zurückgenommen. Ein Benutzereintrag mit dieser ID bleibt unverändert."
        }
        "MCP configuration: {error}" => "MCP-Konfiguration: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "Der Server hat die gespeicherten Anmeldedaten abgelehnt; melden Sie sich mit mcp login erneut an."
        }
        "The server requires additional scopes: {scopes}." => {
            "Der Server verlangt zusätzliche Bereiche: {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "Der Server verlangt eine Autorisierung; führen Sie mcp login ID aus."
        }
        "The probe ended: {status}." => "Die Probe endete: {status}.",

        "Command rules" => "Befehlsregeln",
        "user" => "Benutzer",
        "Matches only this command." => "Passt nur auf diesen Befehl.",
        "Matches commands whose first words are these." => {
            "Passt auf Befehle, die mit diesen Wörtern beginnen."
        }
        "Allows a simple command that would otherwise ask." => {
            "Erlaubt einen einfachen Befehl, der sonst nachfragen würde."
        }
        "Denies the command under every preset." => {
            "Lehnt den Befehl unter jeder Voreinstellung ab."
        }
        "Applies in every Session, in every workspace." => {
            "Gilt in jeder Sitzung und in jedem Arbeitsbereich."
        }
        "Rule added: {rule}." => "Regel hinzugefügt: {rule}.",
        "Rule already present: {rule}." => "Regel ist bereits vorhanden: {rule}.",
        "Rule removed: {rule}." => "Regel entfernt: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Diese Befehlsregel ist nicht mehr aufgeführt; es wurde nichts entfernt."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "Die Befehlsregeln wurden seit dem Lesen geändert; es wurde nichts entfernt."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Eine Befehlsregel lautet DECISION MATCH COMMAND, zum Beispiel deny prefix git push."
        }
        "Command: {command}" => "Befehl: {command}",
        "Use the line prompt or dimcode permissions." => {
            "Nutze die Zeilenaufforderung oder dimcode permissions."
        }
        "Tool call timeout: {millis} ms" => "Tool-Aufruf-Timeout: {millis} ms",
        "Variables: {names}" => "Variablen: {names}",
        "Unset variables: {names}" => "Nicht gesetzte Variablen: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "Die MCP-Quelle wurde seit der Vorschau geändert; es wurde kein Eintrag importiert, und ein Eintrag, den sie ersetzt hätte, kann jetzt deaktiviert sein. Erneut in der Vorschau ansehen."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "Die Projekteinträge wurden geschrieben, ihre zurückgehaltenen Werte nicht; erneut ersetzen, um es noch einmal zu versuchen."
        }
        "Replaced {id}." => "{id} ersetzt.",
        "Added {id}." => "{id} hinzugefügt.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Liefert an Session {session}, Zweig {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Startet eine neue Session in {cwd}.",
        "Disabled: {code} at {instant}." => "Deaktiviert: {code} um {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "Die Definition wurde seit dem Lesen geändert; es wurde nichts geschrieben."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Spalten: ID, Client, Bereich, Pfad, Status, Einträge oder Fehler."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Kein Wert wird aus einem Speicher gelesen oder ausgegeben; --from ID zeigt einen im Daemon in der Vorschau."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Spalten: ID, Aktion, Ziel-ID.\nEin umbenannter Eintrag wird unter seiner Ziel-id geschrieben; der Eintrag der Quelle mit der alten id bleibt, wie er war."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Importierte und ersetzte Einträge beginnen deaktiviert: Ein Benutzereintrag wird mit enabled: false geschrieben, und ein Projekteintrag hat keinen Aktivierungsdatensatz, bis du ihn lokal aktivierst."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Dieser Daemon kann die MCP-Speicher anderer Clients nicht auflisten."
        }
        "This daemon cannot import MCP servers." => {
            "Dieser Daemon kann keine MCP-Server importieren."
        }
        "The document holds no mcpServers entries." => {
            "Das Dokument enthält keine mcpServers-Einträge."
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Spalten: ID, Status, Transport, geänderte Felder, nicht unterstützte Felder, Fehler.\nEin geändertes Feld nennt, was ein Schreiben ändern würde und welche Art von Wert es hält; kein Wert wird ausgegeben.\nEin nicht unterstütztes Feld bleibt ungenutzt oder wird verworfen, wie hinter dem Feld angegeben."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Ein Projekteintrag wird als sein öffentliches Gerüst geschrieben: Die Arbeitsbereichsdatei erhält den Befehl, das deklarierte cwd, den URL-Ursprung und die öffentlichen OAuth-Felder. Seine Argumente, Umgebungs- und Header-Werte, das Bearer-Token, eine URL mit userinfo, Pfad, Abfrage oder Fragment sowie unbekannte Mitglieder gehen in deinen user-scope-Datensatz mcp-secrets, nie in die Arbeitsbereichsdatei."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} hat {count} weitere Feldnamen, als die Listen oben zeigen."
        }
        "Top-level members the import does not carry: none." => {
            "Top-Level-Mitglieder, die der Import nicht übernimmt: keine."
        }
        "Top-level members the import does not carry: {names}." => {
            "Top-Level-Mitglieder, die der Import nicht übernimmt: {names}."
        }
        "Applying the selection at this revision." => {
            "Die Auswahl wird in dieser Revision angewendet."
        }
        "An unchanged entry was already in the source and was not written." => {
            "Ein unveränderter Eintrag war bereits in der Quelle und wurde nicht geschrieben."
        }
        _ => return None,
    })
}
