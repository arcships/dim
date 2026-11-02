pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "Dieser Daemon bietet keine Plugin-Runtimes an ({version})"
        }
        "Plugin runtimes" => "Plugin-Laufzeiten",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter installieren oder anwenden · s Quelle · a automatisch · r erneut · Esc schließen"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc schließen · der Bezug läuft weiter, wenn Sie schließen"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter erneut versuchen · s {name} auf {source} umstellen · Esc schließen"
        }
        "Loading plugin runtimes..." => "Plugin-Laufzeiten werden geladen...",
        "acquiring" => "wird bezogen",
        "starting" => "startet",
        "down" => "beendet",
        "1 plugin waits for {runtime}: {names}" => {
            "1 Plugin wartet auf die {runtime}-Laufzeit: {names}"
        }
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} Plugins warten auf die {runtime}-Laufzeit: {names}"
        }
        "show, install or configure the plugin runtimes" => {
            "Plugin-Laufzeiten anzeigen, installieren oder einstellen"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "Verwendung: /runtime [install NAME | source NAME download|system | auto NAME on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter installieren oder anwenden · s Quelle · r erneut · Esc schließen"
        }
        "auto on" => "auto an",
        "auto off" => "auto aus",
        _ => return None,
    })
}
