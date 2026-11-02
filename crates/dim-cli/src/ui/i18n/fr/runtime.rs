pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "Ce démon ne fournit pas de runtimes de plugin ({version})"
        }
        "Plugin runtimes" => "Runtimes de plugins",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter installer ou appliquer · s source · a auto · r réessayer · Esc fermer"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc fermer · l'acquisition continue si vous fermez"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter réessayer · s basculer {name} vers {source} · Esc fermer"
        }
        "Loading plugin runtimes..." => "Chargement des runtimes de plugins...",
        "acquiring" => "acquisition",
        "starting" => "démarrage",
        "down" => "arrêté",
        "1 plugin waits for {runtime}: {names}" => "1 plugin attend le runtime {runtime} : {names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} plugins attendent le runtime {runtime} : {names}"
        }
        "show, install or configure the plugin runtimes" => {
            "afficher, installer ou configurer les runtimes de plugins"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "Utilisation : /runtime [install NOM | source NOM download|system | auto NOM on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter installer ou appliquer · s source · r réessayer · Esc fermer"
        }
        "auto on" => "auto activé",
        "auto off" => "auto désactivé",
        _ => return None,
    })
}
