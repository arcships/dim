pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Modellliste aktualisieren",
        "Model list refreshed" => "Modellliste aktualisiert",
        "Model list refresh failed: {reason}" => {
            "Modellliste konnte nicht aktualisiert werden: {reason}"
        }
        "Refreshing model list… · esc close" => "Modellliste wird aktualisiert… · Esc schließt",
        // providers_panel.rs
        "Providers" => "Provider",
        "Enter open · Esc clear/close" => "Enter öffnen · Esc leeren/schließen",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab bewegen · Enter ausführen · Esc zurück"
        }
        "Loading providers..." => "Lade Provider…",
        "No matching providers." => "Keine passenden Provider.",
        "Custom providers are not available yet" => "Eigene Provider noch nicht verfügbar",
        "Testing connection..." => "Teste Verbindung…",
        "Connect {providerId}" => "{providerId} verbinden",
        "(default)" => "(Standard)",
        "(required)" => "(erforderlich)",
        "Test + apply" => "Testen + anwenden",
        "Testing sends the stored credential to this URL." => {
            "Test sendet gespeicherte Zugangsdaten an diese URL."
        }
        "Disconnect" => "Trennen",
        "Enable provider" => "Provider aktivieren",
        "Disable provider" => "Provider deaktivieren",
        "Error: {error}" => "Fehler: {error}",
        "disabled" => "deaktiviert",
        "current" => "aktuell",
        "Sign in" => "Anmelden",
        "ready" => "bereit",
        "error" => "Fehler",
        "not connected" => "nicht verbunden",
        "Add custom provider" => "Eigenen Provider hinzufügen",
        // oauth_view.rs
        "Checking account…" => "Prüfe Konto…",
        "Not signed in" => "Nicht angemeldet",
        "Signed in as {account}" => "Angemeldet als {account}",
        "Open models" => "Modelle öffnen",
        "View plan" => "Tarif anzeigen",
        "Sign out" => "Abmelden",
        "Checking usage…" => "Prüfe Nutzung…",
        "Usage: {reason}" => "Nutzung: {reason}",
        "Credits" => "Guthaben",
        "unlimited" => "unbegrenzt",
        "{hours}h window" => "{hours}-Std.-Fenster",
        "/month" => "/Monat",
        "/year" => "/Jahr",
        "expires {date}" => "endet {date}",
        "resets {date}" => "Reset am {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ bewegen · enter ausführen · ctrl+r aktualisieren · esc schließen"
        }
        "Login with DimAgent" => "Mit DimAgent anmelden",
        "Enterprise login" => "Firmen-Login",
        "Device login" => "Geräte-Login",
        "Email" => "E-Mail",
        "Continue" => "Weiter",
        "Browser login" => "Browser-Login",
        "Restart device login" => "Geräte-Login neu starten",
        "Select to copy · Enter opens link" => "Zum Kopieren markieren · Enter öffnet Link",
        "Link shortened · Enter opens link" => "Link gekürzt · Enter öffnet Link",
        "Callback URL" => "Callback-URL",
        "Submit callback" => "Callback senden",
        "The browser could not be opened. Open the link manually, then return here." => {
            "Browser-Start fehlgeschlagen. Link manuell öffnen, dann zurückkehren."
        }
        "Waiting for authorization" => "Warte auf Autorisierung",
        "{name} device login" => "{name}-Geräte-Login",
        "Open this URL: {url}" => "Diese URL öffnen: {url}",
        "Enter code:    {code}" => "Code eingeben:    {code}",
        "Waiting after server slow-down" => "Warte nach Server-Drosselung",
        "OAuth login cancelled" => "OAuth-Login abgebrochen",
        "Enter your work email" => "Firmen-E-Mail eingeben",
        "Invalid OAuth state - possible CSRF attempt" => {
            "Ungültiger OAuth-State - möglicher CSRF-Versuch"
        }
        "Preparing login... · esc cancel" => "Login wird vorbereitet… · esc abbrechen",
        "Completing login... · esc cancel" => "Login wird abgeschlossen… · esc abbrechen",
        "enter restart · esc back" => "enter neu starten · esc zurück",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "Wert eingeben · ↑↓/tab bewegen · enter senden · esc abbrechen"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ bewegen · enter ausführen · esc zurück",
        "↑↓ move · enter activate · esc close" => {
            "↑↓ bewegen · enter ausführen · esc schließen"
        }
        "Model id prefix" => "Modell-ID-Präfix",
        "(none)" => "(keins)",
        "identified: {model} at {provider}" => "erkannt: {model} bei {provider}",
        "inferred from several rows: {model} at {provider}" => {
            "aus mehreren Zeilen abgeleitet: {model} bei {provider}"
        }
        "ambiguous: {candidates}" => "mehrdeutig: {candidates}",
        "unidentified" => "nicht erkannt",
        "Identified {rule}" => "Erkannt {rule}",
        "from the Provider's own list" => "aus der eigenen Liste des Anbieters",
        "from the whole qualified id" => "aus der vollständigen qualifizierten ID",
        "by its tokens" => "über seine Tokens",
        "by its canonical id or display name" => "über kanonische ID oder Anzeigename",
        "from the custom-provider list" => "aus der custom-provider-Liste",
        "by merging the rows that name it" => "durch Zusammenführen der benennenden Zeilen",
        "ambiguous" => "mehrdeutig",
        "{field} {value} supplied by {provider}/{model}" => {
            "{field} {value} geliefert von {provider}/{model}"
        }
        "(paste here)" => "(hier einfügen)",
        _ => return None,
    })
}
