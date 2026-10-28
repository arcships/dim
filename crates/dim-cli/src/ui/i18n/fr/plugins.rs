pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugins",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter activer/désactiver · r recharger · Ctrl+R actualiser · Esc fermer"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Verrouillé : Dim en a besoin, il ne peut donc pas être désactivé."
        }
        "Loading plugins..." => "Chargement des plugins...",
        "No plugins." => "Aucun plugin.",
        "Working..." => "En cours...",
        "locked" => "verrouillé",
        "Uses {use}" => "Utilise {use}",
        "Candidates: {candidates}" => "Candidats : {candidates}",
        "Waiting for: {names}" => "En attente de : {names}",
        "Refused {kind}: {error}" => "{kind} refusé : {error}",
        "Turn off {name}?" => "Désactiver {name} ?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "Ses outils et son contenu disparaissent des nouveaux tours jusqu'à ce que vous le réactiviez."
        }
        "Turn off" => "Désactiver",
        "Turned on {name}." => "{name} activé.",
        "Turned off {name}." => "{name} désactivé.",
        "Reloaded {name}." => "{name} rechargé.",
        "Installed {name} ({id})." => "{name} ({id}) installé.",
        "Installed {name} ({id}); it is not turned on." => "{name} ({id}) installé ; non activé.",
        "Installing {path}..." => "Installation de {path}...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "Redémarrage requis : chargé après le redémarrage de l'App Server (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} ignoré : son propriétaire ne tourne pas."
        }
        "Usage: /plugins [install PATH]" => "Usage : /plugins [install CHEMIN]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "lister les plugins, les activer ou désactiver, recharger (install CHEMIN)"
        }
        _ => return None,
    })
}
