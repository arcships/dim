pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "Der Agent läuft noch.",
        "AskUser is waiting for an answer." => "AskUser wartet auf Antwort.",
        "A permission request is waiting." => "Freigabeanfrage wartet.",
        "Background work may still be active." => "Hintergrundarbeit läuft evtl. noch.",
        "Exit dimcode?" => "dimcode beenden?",
        "Exit" => "Beenden",
        "Stay" => "Bleiben",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y beenden · n bleiben · ←→ bewegen · enter bestätigen"
        }
        "Restart dimcode?" => "dimcode neu starten?",
        "Restart" => "Neu starten",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y neu starten · n bleiben · ←→ bewegen · enter bestätigen"
        }
        // upgrade_dialog.rs
        "Update available" => "Update verfügbar",
        "Version {latest} is available (you have {current})." => {
            "Version {latest} verfügbar (installiert: {current})."
        }
        "Will run: {command}" => "Ausführen: {command}",
        "To update, run: {command}" => "Update mit: {command}",
        "Update now" => "Jetzt aktualisieren",
        "Skip this version" => "Version überspringen",
        "Skipped {version}." => "{version} übersprungen.",
        "Later" => "Später",
        "Close" => "Schließen",
        "Restart now" => "Jetzt neu starten",
        "Updating to {version}..." => "Aktualisiere auf {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "Auf {version} aktualisiert. DimCode neu starten."
        }
        "Update failed: {error}" => "Update fehlgeschlagen: {error}",
        "←→ move · Enter confirm · Esc close" => {
            "←→ bewegen · Enter bestätigen · Esc schließen"
        }
        // prompts.rs: permission request
        "Permission required" => "Freigabe nötig",
        "Allow once" => "Einmal erlauben",
        "Allow always" => "Immer erlauben",
        "Reject" => "Ablehnen",
        "Approve for me" => "Für mich freigeben",
        "Full access" => "Vollzugriff",
        "Run {tool}" => "{tool} ausführen",
        "Changing the approvals mode from here is not yet available" => {
            "Freigabemodus hier noch nicht änderbar"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y erlauben · n ablehnen · ←→/Tab bewegen · Enter bestätigen"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter senden · Tab bearbeiten · Esc ablehnen",
        "Space toggle · Tab next · Esc decline" => "Space umschalten · Tab weiter · Esc ablehnen",
        "1-9 select · Tab next · Esc decline" => "1-9 wählen · Tab weiter · Esc ablehnen",
        "Question" => "Frage",
        "Question on branch {branch}" => "Frage aus Zweig {branch}",
        "Permission on branch {branch}" => "Berechtigung für Zweig {branch}",
        "Sending answer…" => "Sende Antwort…",
        "Question not ready; press Enter to send again" => {
            "Frage nicht bereit; Enter sendet erneut"
        }
        // prompts.rs: AskUser
        "Confirm" => "Bestätigen",
        "Enter save · Esc back" => "Enter speichern · Esc zurück",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter senden · 1-9 bearbeiten · Tab/←→ zurück · Esc ablehnen"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ bewegen · Space umschalten · Enter weiter · Tab/←→ wechseln · Esc ablehnen"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ bewegen · 1-9 wählen · Enter weiter · Tab/←→ wechseln · Esc ablehnen"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion: ungültige Daten.",
        "Missing answer for Q{index}" => "Antwort für F{index} fehlt",
        "Type your own answer" => "Eigene Antwort eingeben",
        "Review your answers and press Enter to submit." => {
            "Antworten prüfen und mit Enter senden."
        }
        "Not answered" => "Nicht beantwortet",
        // approvals_panel.rs
        "Tool Approvals" => "Tool-Freigaben",
        "full access" => "Vollzugriff",
        "All tools run automatically, bypass permissions" => {
            "Alle Tools laufen automatisch, ohne Freigabe"
        }
        "approve for me" => "für mich freigeben",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "Agent prüft Befehle und Tool-Aktionen automatisch. Unsichere Befehle fragen nach; Routineaktionen laufen automatisch – sicher, smart, bequem."
        }
        "Default permissions" => "Standardberechtigungen",
        "default" => "Standard",
        "Read tools run automatically, write tools require confirmation" => {
            "Lese-Tools laufen automatisch, Schreib-Tools brauchen Bestätigung"
        }
        "current" => "aktuell",
        "↑↓/Tab move · Enter apply · Esc back" => {
            "↑↓/Tab bewegen · Enter anwenden · Esc zurück"
        }
        "Confirm Full Access" => "Vollzugriff bestätigen",
        "Cancel" => "Abbrechen",
        "Tab switch  Enter confirm  Esc cancel" => "Tab wechseln  Enter bestätigen  Esc abbrechen",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "Vollzugriff reduziert Rückfragen und kann sensible Aktionen, Dateiänderungen oder externe Befehle direkt ausführen. Nur bei vertrauenswürdiger Aufgabe nutzen."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "ausgeführt",
        "running" => "führt aus",
        "read" => "gelesen",
        "reading" => "liest",
        "explored" => "erkundet",
        "exploring" => "erkundet",
        "edited" => "bearbeitet",
        "editing" => "bearbeitet",
        "changed" => "geändert",
        "changing" => "ändert",
        "checked on" => "geprüft",
        "checking on" => "prüft",
        "Explored" => "Erkundet",
        "Exploring" => "Erkundet",
        "Edited" => "Bearbeitet",
        "Editing" => "Bearbeitet",
        "Changed" => "Geändert",
        "Changing" => "Ändert",
        // transcript/tool_group.rs: counts
        "1 file" => "1 Datei",
        "{count} files" => "{count} Dateien",
        "1 web page" => "1 Webseite",
        "{count} web pages" => "{count} Webseiten",
        "1 skill" => "1 Skill",
        "{count} skills" => "{count} Skills",
        "1 search" => "1 Suche",
        "{count} searches" => "{count} Suchen",
        "1 command" => "1 Befehl",
        "{count} commands" => "{count} Befehle",
        "1 task" => "1 Aufgabe",
        "{count} tasks" => "{count} Aufgaben",
        "1 tool" => "1 Tool",
        "{count} tools" => "{count} Tools",
        "{files} and {pages}" => "{files} und {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} fehlgeschlagen",
        // transcript/tool_group.rs: call rows
        "Read" => "Lesen",
        "List" => "Auflisten",
        "Grep" => "Grep",
        "Search the web" => "Websuche",
        "Read skill" => "Skill lesen",
        "Search git history" => "Git-Verlauf durchsuchen",
        "Edit" => "Bearbeiten",
        "Write" => "Schreiben",
        "Create" => "Erstellen",
        " (detached)" => " (im Hintergrund)",
        "Checked on background task" => "Hintergrundaufgabe geprüft",
        "Waiting…" => "Warte…",
        "… +{count} lines" => "… +{count} Zeilen",
        _ => return None,
    })
}
