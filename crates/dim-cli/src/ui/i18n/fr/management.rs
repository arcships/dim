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
        "models" => "modèles",
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
        "Remove {name}?" => "Supprimer {name} ?",
        "↑↓ scroll · Esc back" => "↑↓ défiler · Esc revenir",
        "Added." => "Ajouté.",
        "Removed {name}." => "{name} supprimé.",
        "unavailable" => "indisponible",
        "Reset {name}?" => "Réinitialiser {name} ?",
        "Enter confirm" => "Enter confirmer",
        "Esc cancel" => "Esc annuler",
        "Reset {name} to its catalog entry." => "{name} réinitialisé sur son entrée de catalogue.",
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
        "Models" => "Modèles",
        "catalog" => "catalogue",
        "overrides" => "surcharges",
        "added" => "ajoutés",
        "Models · {name}" => "Modèles · {name}",
        "undeclared" => "non déclaré",
        "Context window {window} · max output {output}" => {
            "Fenêtre de contexte {window} · sortie max {output}"
        }
        "Endpoint {endpoint}" => "Point de terminaison {endpoint}",
        "Endpoint {endpoint} · override {value}" => {
            "Point de terminaison {endpoint} · surcharge {value}"
        }
        "The provider reports it unavailable." => "Le fournisseur le signale indisponible.",
        "Limits undeclared; edit the model to declare them." => {
            "Limites non déclarées ; modifiez le modèle pour les déclarer."
        }
        "Sign in to the account to use its models." => {
            "Connectez-vous au compte pour utiliser ses modèles."
        }
        "The signed-in account changed; reload to list its models." => {
            "Le compte connecté a changé ; rechargez pour lister ses modèles."
        }
        "The provider has no catalog yet." => "Ce fournisseur n'a pas encore de catalogue.",
        "The provider listed no models." => "Le fournisseur n'a listé aucun modèle.",
        "Listed {count} models." => "{count} modèles listés.",
        "Refreshed the models." => "Modèles actualisés.",
        "Could not list models: {error}. Showing the stored list." => {
            "Impossible de lister les modèles : {error}. La liste enregistrée est affichée."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "Le compte connecté a changé ; le rechargement a effacé les préférences, surcharges et désactivations."
        }
        "The models changed since they were read; nothing was removed." => {
            "Les modèles ont changé depuis leur lecture ; rien n'a été supprimé."
        }
        "The models changed since they were read; nothing was written." => {
            "Les modèles ont changé depuis leur lecture ; rien n'a été écrit."
        }
        "No provider has that id." => "Aucun fournisseur n'a cet id.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} n'est pas disponible ({reason}) ; son activation reste inchangée."
        }
        "No model has that id." => "Aucun modèle n'a cet id.",
        "This model cannot be removed." => "Ce modèle ne peut pas être supprimé.",
        "A Provider's models have no area document." => {
            "Les modèles d'un fournisseur n'ont pas de document de zone."
        }
        "Models have no run operation." => "Les modèles n'ont pas d'opération d'exécution.",
        "The document is not an object; nothing was written." => {
            "Le document n'est pas un objet ; rien n'a été écrit."
        }
        "The document changes no editable field; nothing was written." => {
            "Le document ne modifie aucun champ modifiable ; rien n'a été écrit."
        }
        "The model document names no model." => "Le document de modèle ne nomme aucun modèle.",
        "The provider has no model catalog to write." => {
            "Ce fournisseur n'a pas de catalogue de modèles à écrire."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Source {source}",
        "Status {status}" => "Statut {status}",
        "Tools {value}" => "Outils {value}",
        "Vision {value}" => "Vision {value}",
        "Reasoning {value}" => "Raisonnement {value}",
        "Modalities {list}" => "Modalités {list}",
        "Description {text}" => "Description {text}",
        "Release date {date}" => "Date de sortie {date}",
        "Pricing {value}" => "Tarif {value}",
        "Rate limit {value}" => "Limite de débit {value}",
        "Matched provider {id}" => "Fournisseur correspondant {id}",
        "Matched model {id}" => "Modèle correspondant {id}",
        "Match strategy {strategy}" => "Stratégie de correspondance {strategy}",
        "Capability overrides {fields}" => "Surcharges de capacités {fields}",
        "yes" => "oui",
        "no" => "non",
        "not supported" => "non pris en charge",
        "supported" => "pris en charge",

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
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} figure dans plusieurs sources MCP : {sources}. Indiquez-en une avec --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "L'action ne nomme aucune source MCP ; une ligne porte la source rapportée par son propriétaire."
        }
        "No MCP server {id} is in source {source}." => {
            "Aucun serveur MCP {id} dans la source {source}."
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
        "Tool call timeout: {millis} ms" => "Délai d'appel d'outil : {millis} ms",
        "Variables: {names}" => "Variables : {names}",
        "Unset variables: {names}" => "Variables non définies : {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "La source MCP a changé depuis l'aperçu ; aucune entrée n'a été importée, et une entrée qu'elle aurait remplacée peut désormais être désactivée. Relancez l'aperçu."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "Les entrées de projet ont été écrites, pas leurs valeurs retenues ; remplacez-les à nouveau pour réessayer."
        }
        "Replaced {id}." => "{id} remplacé.",
        "Added {id}." => "{id} ajouté.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Livre à la session {session}, branche {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Lance une nouvelle session dans {cwd}.",
        "Disabled: {code} at {instant}." => "Désactivé : {code} à {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "La définition a changé depuis sa lecture ; rien n'a été écrit."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Colonnes : ID, client, portée, chemin, état, entrées ou erreur."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Aucune valeur d'un stock n'est lue ni affichée ; --from ID en prévisualise un dans le démon."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Colonnes : ID, action, ID de destination.\nUne entrée renommée est écrite sous son id de destination ; l'entrée de la source portant l'ancien id reste telle quelle."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Les entrées importées et remplacées démarrent désactivées : une entrée utilisateur est écrite avec enabled: false, et une entrée de projet n'a aucun enregistrement d'activation avant que vous ne l'activiez localement."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Ce démon ne peut pas lister les stocks MCP des autres clients."
        }
        "This daemon cannot import MCP servers." => {
            "Ce démon ne peut pas importer de serveurs MCP."
        }
        "The document holds no mcpServers entries." => {
            "Le document ne contient aucune entrée mcpServers."
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Colonnes : ID, état, transport, champs modifiés, champs non pris en charge, erreur.\nUn champ modifié nomme ce qu'une écriture changerait et le type de valeur qu'il porte ; aucune valeur n'est affichée.\nUn champ non pris en charge reste inutilisé ou est supprimé, comme indiqué après le champ."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Une entrée de projet est écrite sous sa forme publique : le fichier d'espace de travail reçoit la commande, le cwd déclaré, l'origine de l'URL et les champs OAuth publics. Ses arguments, valeurs d'environnement et d'en-tête, jeton bearer, une URL avec userinfo, chemin, requête ou fragment, et les membres inconnus vont dans votre enregistrement mcp-secrets de portée utilisateur, jamais dans le fichier d'espace de travail."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} a {count} noms de champs de plus que les listes ci-dessus n'en montrent."
        }
        "Top-level members the import does not carry: none." => {
            "Membres de premier niveau que l'importation ne reprend pas : aucun."
        }
        "Top-level members the import does not carry: {names}." => {
            "Membres de premier niveau que l'importation ne reprend pas : {names}."
        }
        "Applying the selection at this revision." => {
            "Application de la sélection à cette révision."
        }
        "An unchanged entry was already in the source and was not written." => {
            "Une entrée inchangée était déjà dans la source et n'a pas été écrite."
        }
        _ => return None,
    })
}
