pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugins",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter an/aus · r neu laden · Ctrl+R aktualisieren · Esc schließen"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Gesperrt: Dim braucht es, daher kann es nicht ausgeschaltet werden."
        }
        "Loading plugins..." => "Plugins werden geladen...",
        "No plugins." => "Keine Plugins.",
        "Working..." => "Wird ausgeführt...",
        "locked" => "gesperrt",
        "Uses {use}" => "Verwendet {use}",
        "Candidates: {candidates}" => "Kandidaten: {candidates}",
        "Waiting for: {names}" => "Wartet auf: {names}",
        "Refused {kind}: {error}" => "{kind} abgelehnt: {error}",
        "Turn off {name}?" => "{name} ausschalten?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "Seine Tools und Inhalte fehlen in neuen Runden, bis Sie es wieder einschalten."
        }
        "Turn off" => "Ausschalten",
        "Turned on {name}." => "{name} eingeschaltet.",
        "Turned off {name}." => "{name} ausgeschaltet.",
        "Reloaded {name}." => "{name} neu geladen.",
        "Installed {name} ({id})." => "{name} ({id}) installiert.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name} ({id}) installiert; nicht eingeschaltet."
        }
        "Installing {path}..." => "{path} wird installiert...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "Neustart nötig: wird nach einem Neustart des App Servers geladen (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} übersprungen: ihr Besitzer läuft nicht."
        }
        "Usage: /plugins [install PATH]" => "Verwendung: /plugins [install PFAD]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "Plugins auflisten, ein- oder ausschalten, neu laden (install PFAD)"
        }
        _ => return None,
    })
}
