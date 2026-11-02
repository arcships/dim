pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Chargement…",
        "No entries." => "Aucune entrée.",
        "No matching entry." => "Aucune entrée correspondante.",
        "Issues" => "Problèmes",
        "sources" => "Sources",
        "skills" => "Skills",
        "hooks" => "hooks",
        "scheduled jobs" => "tâches planifiées",
        "workflow definitions" => "définitions de flux de travail",
        "command rules" => "Règles de commande",
        "enabled" => "activé",
        "shadowed by {name}" => "masqué par {name}",
        "Reloaded." => "Rechargé.",
        "Enabled {name}." => "{name} activé.",
        "Disabled {name}." => "{name} désactivé.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} rechargé : {entries} entrées, {disabled} désactivées."
        }
        "Reloaded {area}: {entries} entries." => "{area} rechargé : {entries} entrées.",
        "Hooks" => "Hooks",
        "invalid" => "non valide",
        "Saved." => "Enregistré.",
        "Saved {name}." => "{name} enregistré.",
        "No change." => "Aucun changement.",
        "Your text is kept in {path}." => "Votre texte est conservé dans {path}.",
        "The user's own document; always in effect." => {
            "Le document de l'utilisateur ; toujours actif."
        }
        "There is no hooks file there." => "Il n'y a pas de fichier de hooks à cet endroit.",
        "Owned by plugin {plugin}." => "Appartient au plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "Le document utilisateur désactive tous les hooks (disableAllHooks)."
        }
        "Enter toggle" => "Enter basculer",
        "Ctrl+E edit" => "Ctrl+E modifier",
        "Ctrl+N add" => "Ctrl+N ajouter",
        "Ctrl+R reload" => "Ctrl+R recharger",
        "Ctrl+T run" => "Ctrl+T exécuter",
        "Ctrl+X remove" => "Ctrl+X supprimer",
        "Tab inspect" => "Tab inspecter",
        "Remove {name}? Enter confirm · Esc cancel" => {
            "Supprimer {name} ? Enter confirmer · Esc annuler"
        }
        "↑↓ scroll · Esc back" => "↑↓ défiler · Esc revenir",
        "Added." => "Ajouté.",
        "Removed {name}." => "{name} supprimé.",
        "Ran {name}." => "{name} exécuté.",
        "The user document changed since it was read; nothing was written." => {
            "Le document utilisateur a changé depuis sa lecture ; rien n'a été écrit."
        }
        "The job changed since it was read; nothing was written." => {
            "La tâche a changé depuis sa lecture ; rien n'a été écrit."
        }
        "The jobs changed since they were read; nothing was written." => {
            "Les tâches ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "Your next edit opens the current document." => {
            "Votre prochaine modification ouvrira le document actuel."
        }
        "Your next add reopens your text." => "Votre prochain ajout rouvrira votre texte.",
        "Scheduler" => "Planificateur",
        "jobs" => "tâches",
        "Added {name}." => "{name} ajouté.",
        "Run {name}: {outcome}." => "{name} exécuté : {outcome}.",
        "Recent runs of {name}" => "Exécutions récentes de {name}",
        "No run yet." => "Aucune exécution pour l'instant.",
        "No next run." => "Aucune exécution à venir.",
        "Next run {instant}." => "Prochaine exécution {instant}.",
        "Last run {outcome} at {instant}." => "Dernière exécution {outcome} à {instant}.",
        "every {minutes} minutes" => "toutes les {minutes} minutes",
        "at {instant}" => "à {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} en {zone}",
        "The job changed since it was read; nothing was removed." => {
            "La tâche a changé depuis sa lecture ; rien n'a été supprimé."
        }
        "The Scheduler has no area document." => "Le planificateur n'a pas de document de domaine.",
        "No scheduled job has that id." => "Aucune tâche planifiée n'a cet id.",
        "A scheduled job's run takes no input document." => {
            "L'exécution d'une tâche planifiée ne prend aucun document d'entrée."
        }
        "An MCP server's test takes no input document." => {
            "Le test d'un serveur MCP ne prend pas de document d'entrée."
        }
        "This daemon cannot manage {area}." => "Impossible de gérer {area}.",
        "This daemon lists {area} but cannot toggle them." => {
            "Le daemon liste {area}, mais ne peut pas les basculer."
        }
        "This daemon lists {area} but cannot edit them." => {
            "Le daemon liste {area}, mais ne peut pas les modifier."
        }
        "This daemon lists {area} but cannot reload them." => {
            "Le daemon liste {area}, mais ne peut pas les recharger."
        }
        "This daemon lists {area} but cannot add them." => {
            "Le daemon liste {area}, mais ne peut pas les ajouter."
        }
        "This daemon lists {area} but cannot remove them." => {
            "Le daemon liste {area}, mais ne peut pas les supprimer."
        }
        "This daemon lists {area} but cannot run them." => {
            "Le daemon liste {area}, mais ne peut pas les exécuter."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "Le daemon liste {area}, mais ne peut pas afficher leur détail ou leur historique."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "Le document supprime {field}, que le propriétaire ne peut pas effacer ; rien n'a été écrit."
        }
        "The document changes no field; nothing was written." => {
            "Le document ne modifie aucun champ ; rien n'a été écrit."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "Les réglages des hooks de cet espace de travail ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "Workflow" => "Flux de travail",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} enregistré. Il reste désactivé jusqu'à ce que vous l'activiez."
        }
        "The definitions changed since they were read; nothing was written." => {
            "Les définitions ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "Le nom d'une définition est sa clé ; ajoutez-la pour l'enregistrer sous un autre nom."
        }
        "No project definition is named {name}." => {
            "Aucune définition de projet ne s'appelle {name}."
        }
        "No workflow definition is named {name}." => {
            "Aucune définition de flux de travail ne s'appelle {name}."
        }
        "Saved in the user workflow source; always enabled." => {
            "Enregistré dans la source de flux de travail de l'utilisateur ; toujours activé."
        }
        "Starts only after you enable it." => "Ne démarre qu'après que vous l'avez activé.",
        "Defined by package {package}." => "Défini par le paquet {package}.",
        "{name} — {description} (required)" => "{name} — {description} (requis)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — requis",
        "{name} — default: {default}" => "{name} — par défaut : {default}",
        "and {count} more" => "et {count} de plus",
        "Recent runs" => "Exécutions récentes",
        "A workflow definition is saved as an object with a source and a document." => {
            "Une définition de flux de travail est enregistrée comme un objet avec une source et un document."
        }
        "A definition is saved to the user source or the project source." => {
            "Une définition est enregistrée dans la source utilisateur ou la source du projet."
        }
        "Started {name}: {run_id}." => "{name} démarré : {run_id}.",
        "Parameters are a JSON object of strings." => {
            "Les paramètres sont un objet JSON de chaînes."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "La liste a atteint sa limite de taille ; les définitions suivantes ne sont pas affichées."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Aucune définition de workflow nommée {name} n'est dans la liste, qui a atteint sa limite de taille."
        }
        "MCP" => "MCP",
        "MCP servers" => "serveurs MCP",
        "Ctrl+T test" => "Ctrl+T tester",
        "sign-in required" => "connexion requise",
        "signed in" => "connecté",
        "sign-in expired" => "connexion expirée",
        "missing scopes: {scopes}" => "portées manquantes : {scopes}",
        "Executable: {command}" => "Exécutable : {command}",
        "Stored arguments (hidden): {count}" => "Arguments enregistrés (masqués) : {count}",
        "Working directory: {cwd}" => "Répertoire de travail : {cwd}",
        "Environment names: {names}" => "Noms d'environnement : {names}",
        "Endpoint: {endpoint}" => "Point de terminaison : {endpoint}",
        "Header names: {names}" => "Noms d'en-tête : {names}",
        "Bearer token: {state}" => "Jeton Bearer : {state}",
        "OAuth: configured (client ID {id})" => "OAuth : configuré (ID client {id})",
        "OAuth: configured" => "OAuth : configuré",
        "OAuth: not configured" => "OAuth : non configuré",
        "Metadata URL: {state}" => "URL des métadonnées : {state}",
        "configured" => "configuré",
        "Owned by a plugin; change it through Plugins." => {
            "Fourni par un plugin ; modifiez-le via Plugins."
        }
        "MCP entry {id}" => "Entrée MCP {id}",
        "Connection {number} · {status}" => "Connexion {number} · {status}",
        "Tools have not been discovered." => "Les outils n'ont pas encore été découverts.",
        "No live session connections." => "Aucune connexion de session active.",
        "Tools of {id}" => "Outils de {id}",
        "The server exposes no tools." => "Le serveur n'expose aucun outil.",
        "The add document needs an id." => "Le document d'ajout a besoin d'un id.",
        "The add document needs a source of user or project." => {
            "La source du document d'ajout doit être user ou project."
        }
        "The add document needs a configuration object." => {
            "Le document d'ajout a besoin d'un objet configuration."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} figure déjà dans mcp list ; supprimez-le d'abord ou choisissez un autre id."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} figure déjà dans mcp list ; choisissez un autre id ou modifiez-le via son plugin."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "Les paramètres de cette entrée ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "L'entrée MCP a changé depuis sa lecture ; rien n'a été écrit."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "Les entrées MCP ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "L'entrée MCP a changé depuis sa lecture ; rien n'a été supprimé."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "Les documents MCP appartiennent à une entrée ; cet enregistrement n'en nomme aucune."
        }
        "No MCP server has that id." => "Aucun serveur MCP n'a cet id.",
        "This workspace has no project MCP source." => {
            "Cet espace de travail n'a pas de source MCP de projet."
        }
        "Enabled {id} ({source})." => "{id} ({source}) activé.",
        "Disabled {id} ({source})." => "{id} ({source}) désactivé.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "Il exécute l'entrée .mcp.json de cet espace de travail, y compris ses modifications ultérieures, dans les sessions actives et nouvelles."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} n'est pas dans mcp list ; toute activation enregistrée dans cet espace de travail a été retirée."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "Le catalogue MCP n'a pas pu être lu ({error}) ; l'activation de {id} dans cet espace de travail a été retirée. Une entrée utilisateur avec cet ID reste inchangée."
        }
        "MCP configuration: {error}" => "Configuration MCP : {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "Le serveur a rejeté les identifiants enregistrés ; reconnectez-vous avec mcp login."
        }
        "The server requires additional scopes: {scopes}." => {
            "Le serveur exige des portées supplémentaires : {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "Le serveur exige une autorisation ; exécutez mcp login ID."
        }
        "The probe ended: {status}." => "La sonde s'est terminée : {status}.",

        "Command rules" => "Règles de commande",
        "user" => "utilisateur",
        "Matches only this command." => "Correspond uniquement à cette commande.",
        "Matches commands whose first words are these." => {
            "Correspond aux commandes qui commencent par ces mots."
        }
        "Allows a simple command that would otherwise ask." => {
            "Autorise une commande simple qui serait autrement demandée."
        }
        "Denies the command under every preset." => {
            "Refuse la commande quel que soit le préréglage."
        }
        "Applies in every Session, in every workspace." => {
            "S'applique à chaque session et à chaque espace de travail."
        }
        "Rule added: {rule}." => "Règle ajoutée : {rule}.",
        "Rule already present: {rule}." => "Règle déjà présente : {rule}.",
        "Rule removed: {rule}." => "Règle supprimée : {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Cette règle de commande n'est plus répertoriée ; rien n'a été supprimé."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "Les règles de commande ont changé depuis leur lecture ; rien n'a été supprimé."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Une règle de commande s'écrit DECISION MATCH COMMAND, par exemple deny prefix git push."
        }
        "Command: {command}" => "Commande : {command}",
        "Use the line prompt or dimcode permissions." => {
            "Utilisez l'invite de ligne ou dimcode permissions."
        }
        _ => return None,
    })
}
