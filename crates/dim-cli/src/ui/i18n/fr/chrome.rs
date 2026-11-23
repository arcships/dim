pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "File d’attente",
        "Export session" => "Exporter la session",
        "Enter steer · x delete held · Esc close" => {
            "Enter orienter · x supprimer le message retenu · Esc fermer"
        }
        "No queued messages." => "Aucun message en file d’attente.",
        "Held · awaiting application" => "Retenu · en attente d’application",
        "Held" => "Retenu",
        "Unread" => "Non lu",
        "Enter export · Esc cancel" => "Enter exporter · Esc annuler",
        "Messages with attachments stay queued; use /queue to steer or delete them" => {
            "Les messages avec pièces jointes restent en file ; utilisez /queue pour orienter ou les supprimer"
        }
        "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now" => {
            "/queue · Shift+← modifier le dernier · brouillon vide : {key} pour orienter · Ctrl+Enter envoie aussitôt"
        }
        "+{count} more · {hint}" => "+{count} de plus · {hint}",
        "steer or delete queued messages" => "orienter avec les messages en file ou les supprimer",
        "export this conversation" => "exporter cette conversation",
        "That message has already left the queue." => "Ce message a déjà quitté la file d’attente.",
        "Exported {format}: {path}" => "Exporté en {format} : {path}",
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter sortie / interrompre membre · x arrêter commande · Esc fermer"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter sortie / interrompre membre · x arrêter commande · Ctrl+X tout arrêter · Esc fermer"
        }
        "Loading branch history... Stop-all unavailable" => {
            "Chargement de l'historique… Arrêt global indisponible"
        }
        "Branch history failed; reopen to retry" => "Échec de l'historique; rouvrir pour réessayer",
        "Branch history is incomplete; stop-all unavailable" => {
            "Historique incomplet; arrêt global indisponible"
        }
        "Running" => "En cours",
        "Lines" => "Lignes",
        "Enter show the line · x end it · Esc close" => {
            "Enter afficher la ligne · x la terminer · Esc fermer"
        }
        "This session has no lines." => "Cette session n'a aucune ligne.",
        "No matching lines." => "Aucune ligne correspondante.",
        "the session itself" => "la session elle-même",
        "working" => "en travail",
        "ended itself" => "s'est terminée",
        "ended by an operation" => "terminée par une opération",
        "ended with {id}" => "terminée avec {id}",
        "creation failed" => "création échouée",
        "shown" => "affichée",
        "line" => "ligne",
        "line {id}" => "ligne {id}",
        "jump to a message of the shown line" => "aller à un message de la ligne affichée",
        "Timeline" => "Chronologie",
        "Enter actions · Esc close" => "Enter actions · Esc fermer",
        "Timeline actions" => "Actions de chronologie",
        "↑↓ select · Enter confirm · Esc cancel" => {
            "↑↓ choisir · Enter confirmer · Esc annuler"
        }
        "Jump to message" => "Aller au message",
        "Edit as a new draft" => "Modifier comme nouveau brouillon",
        "Quote message" => "Citer le message",
        "Create branch from here" => "Créer une branche ici",
        "Before this message" => "Avant ce message",
        "Fork session" => "Forker la session",
        "1. Create fork" => "1. Créer le fork",
        "2. Cancel" => "2. Annuler",
        "Creates a new session; the current session stays unchanged." => {
            "Crée une session ; la session actuelle reste inchangée."
        }
        "No messages yet." => "Aucun message pour l'instant.",
        "Already on screen above; /tui jumps to it" => "Déjà plus haut ; /tui y va directement",
        "{count} earlier messages · Enter or click to load" => {
            "{count} messages antérieurs · Enter ou clic pour charger"
        }
        "follow, talk to and end this session's lines" => {
            "suivre, dialoguer et terminer les lignes de cette session"
        }
        "End line {id}?" => "Terminer la ligne {id} ?",
        "y end it" => "y terminer",
        "n cancel" => "n annuler",
        "main is the session itself; a line ends, main does not" => {
            "main est la session elle-même ; une ligne se termine, pas main"
        }
        "Line has ended:" => "La ligne est terminée :",
        "Esc close" => "Esc fermer",
        "/{name} needs {argument}" => "/{name} nécessite {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} n’accepte que du texte ; retirez {label} pour envoyer"
        }
        "e edit · b branch · f fork" => "e modifier · b branche · f fork",
        "e edit" => "e modifier",
        "Fork the session before “{message}”?" => "Forker la session avant « {message} » ?",
        "Fork the session after the last turn?" => "Forker la session après le dernier tour ?",
        "⑂ new line from {id} · Esc cancel" => "⑂ nouvelle ligne depuis {id} · Échap annuler",
        "A fork copies main; b opens a line here" => "Un fork copie main ; b ouvre une ligne ici",
        "A line opens from text; images and files were not included" => {
            "Une ligne s'ouvre avec du texte ; images et fichiers non inclus"
        }
        "No finished turn to cut at yet" => "Aucun tour terminé où couper pour l'instant",
        "forked {id} · {count} messages" => "{id} forkée · {count} messages",
        "after message {count}" => "après le message {count}",
        "open a line from the end of the shown line" => {
            "ouvrir une ligne depuis la fin de la ligne affichée"
        }
        "fork the session at its last finished turn" => {
            "forker la session à son dernier tour terminé"
        }
        "rename this session" => "renommer cette session",
        "Renamed to {title}" => "Renommée en {title}",
        "The Session is now untitled." => "La session n'a plus de titre.",
        "This daemon does not support renaming Sessions" => {
            "Ce démon ne prend pas en charge le renommage des sessions"
        }
        "Renaming failed" => "Échec du renommage",
        "Delete \"{name}\"?" => "Supprimer « {name} » ?",
        "Deleted {name}." => "{name} supprimée.",
        "Nothing was removed: no stored Session {name}." => {
            "Rien n'a été supprimé : aucune session enregistrée {name}."
        }
        "Delete failed" => "Échec de la suppression",
        "This daemon does not support deleting Sessions" => {
            "Ce démon ne prend pas en charge la suppression des sessions"
        }
        "Starting" => "Démarrage",
        "Delegation result" => "Résultat de délégation",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter détails · x arrêter sélection · Ctrl+X tout arrêter · Esc fermer"
        }
        "x interrupt member · Esc back" => "x interrompre membre · Esc retour",
        "read-only" => "lecture seule",
        "Member Session" => "Session membre",
        "Loading conversation..." => "Chargement…",
        "No conversation yet" => "Aucune conversation",
        "Observing conversation" => "En observation",
        "Conversation unavailable" => "Conversation indisponible",
        "Session closed" => "Session fermée",
        "Member conversation" => "Conversation du membre",
        "x stop command · Esc back" => "x arrêter commande · Esc retour",
        "x stop command · Esc close" => "x arrêter commande · Esc fermer",
        "x stop workflow · Esc close" => "x arrêter workflow · Esc fermer",
        "Esc back" => "Esc retour",
        "Tail preview - last 2,000 lines / 50 KiB" => "Aperçu - 2 000 dernières lignes / 50 KiB",
        "Loading output..." => "Chargement…",
        "No output yet" => "Aucune sortie",
        "Live output unavailable" => "Sortie en direct indisponible",
        "Live output unavailable in this profile" => "Sortie en direct indisponible dans ce profil",
        "Output read failed" => "Échec de lecture",
        "Completed" => "Terminé",
        "Failed" => "Échec",
        "Cancelled" => "Annulé",
        "workflow" => "workflow",
        "Stopping" => "Arrêt…",
        "Stopped" => "Arrêté",
        "Interrupted" => "Interrompu",
        "last observed" => "dernier état",
        "Loading Workflow runs... Stop-all unavailable" => {
            "Chargement des exécutions… Arrêt global indisponible"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: dernier état; échec d'actualisation, rouvrir pour réessayer"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Exécutions indisponibles; rouvrir pour réessayer"
        }
        "The selected Workflow run is no longer available" => "Exécution sélectionnée indisponible",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Liste incomplète; arrêt global indisponible"
        }
        "Workflow stop unavailable" => "Arrêt du workflow indisponible",
        "Run no longer available; last observed record" => {
            "Exécution indisponible; dernier état observé"
        }
        "x stop workflow · Esc back" => "x arrêter workflow · Esc retour",
        "Run" => "Exécution",
        "Source" => "Source",
        "Stages" => "Étapes",
        "result" => "résultat",
        "Log" => "Journal",
        "Report" => "Rapport",
        "waiting" => "en attente",
        "launching" => "lancement",
        "completed" => "terminé",
        "failed" => "échec",
        "stopped" => "arrêté",
        "skipped" => "ignoré",
        "lost" => "perdu",
        "none" => "aucun",
        "pending" => "en attente",
        "delivered" => "remis",
        "undeliverable" => "non remis",
        "Agent is starting; no member Session to stop yet" => {
            "Agent en démarrage; aucune session à arrêter"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "Membres fournisseur gérés par le leader; arrêt impossible"
        }
        "Provider members have no conversation to open here" => {
            "Membres fournisseur: aucune conversation à ouvrir"
        }
        "That delegation is no longer in this transcript" => {
            "Délégation absente de la transcription"
        }
        "Member model" => "Modèle du membre",
        "No model observed" => "Aucun modèle observé",
        "Observing member..." => "Observation du membre…",
        "{model} (observation unavailable)" => "{model} (observation indisponible)",
        "{model} (Session closed)" => "{model} (session fermée)",
        "{model} (last observed)" => "{model} (dernier observé)",
        "Agent: {name}" => "Agent: {name}",
        "Provider member: {name}" => "Membre fournisseur: {name}",
        "Task: {task}" => "Tâche: {task}",
        "Activity: {activity}" => "Activité: {activity}",
        "Activity: not observed on this attachment" => "Activité: non observée ici",
        "Result: {result}" => "Résultat: {result}",
        "Controlled by the leader" => "Géré par le leader",
        "› Open member conversation" => "› Ouvrir la conversation",
        "Not started" => "Non démarré",
        "Reason: {reason}" => "Raison: {reason}",
        "Copied to clipboard" => "Copié",
        "Opened link" => "Lien ouvert",
        "Link: {url}" => "Lien: {url}",
        "Not available" => "Indisponible",
        "files changed  {count}" => "fichiers modifiés  {count}",
        "active session" => "session active",
        "new-session draft" => "brouillon de nouvelle session",
        "Tool approvals: {title} ({target})" => "Approbations d'outils: {title} ({target})",
        "Disconnected · draft kept" => "Déconnecté · brouillon conservé",
        "The App Server closed this interface: {reason}" => {
            "L'App Server a fermé l'interface: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Déconnecté; brouillon conservé. Quitter et reprendre la session."
        }
        "Cannot open this Session; draft kept." => {
            "Impossible d'ouvrir cette session; brouillon conservé."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Déconnecté; brouillon conservé. Quitter et lancer: {command}"
        }
        "Disconnected; command outcome unknown" => "Déconnecté; résultat de commande inconnu",
        "Message queued" => "Message en file",
        "Run a shell command · Esc to exit" => "Lancer une commande shell · Esc quitter",
        "! shell mode" => "! mode shell",
        "!! shell mode · not sent to the model" => "!! mode shell · non envoyé au modèle",
        "Leave shell mode to quote a message" => "Quitter le mode shell pour citer",
        "A shell command is already running" => "Commande shell déjà en cours",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "En cours… · Ctrl+C interrompre · {seconds}s"
        }
        "exit code {code}" => "code de sortie {code}",
        "killed by {signal}" => "tué par {signal}",
        "interrupted" => "interrompu",
        "done" => "terminé",
        "failed: {reason}" => "échec: {reason}",
        "Shell output was not recorded: {reason}" => "Sortie shell non enregistrée: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "Lecture du presse-papiers…",
        "Reading image…" => "Lecture de l'image…",
        "Wait for the image insertion before sending" => {
            "Attendez l'insertion de l'image avant d'envoyer"
        }
        "The image could not be read" => "L'image n'a pas pu être lue",
        "An image is still being inserted; paste again when it is in" => {
            "Une image est en cours d'insertion ; collez à nouveau une fois terminée"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "Le presse-papiers est en cours d'insertion ; collez à nouveau une fois terminé"
        }
        "Clipboard: {reason}" => "Presse-papiers: {reason}",
        "Wait for clipboard insertion before sending" => "Attendre le collage avant d'envoyer",
        "A submission accepts at most 20 images" => "20 images max par envoi",
        "first frame" => "première image seulement",
        "re-encoded" => "réencodée",
        "The current model does not accept images; remove {label} to send" => {
            "Le modèle n'accepte pas les images; retirer {label} pour envoyer"
        }
        "Files" => "Fichiers",
        "Searching…" => "Recherche…",
        "type filter · enter select · esc clear/close" => {
            "saisir filtre · enter choisir · esc effacer/fermer"
        }
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ choisir · enter/tab insérer · esc fermer"
        }
        "not sent to the model" => "non envoyé au modèle",
        "… {count} more lines" => "… {count} lignes de plus",
        "… {count} earlier lines" => "… {count} lignes avant",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Interruption demandée; Ctrl+C encore pour quitter"
        }
        "Press Ctrl+C again to exit" => "Ctrl+C encore pour quitter",
        "User interrupted the run." => "Interrompu par l'utilisateur.",
        "Could not save the language: {error}" => "Langue non enregistrée: {error}",
        "Language set to {locale}" => "Langue: {locale}",
        "Language set to system default" => "Langue: système",
        "Connected {provider}" => "{provider} connecté",
        "Opened {url}" => "{url} ouvert",
        "Tool details not saved: {error}" => "Détails non enregistrés: {error}",
        "Thinking details not saved: {error}" => "Détails de réflexion non enregistrés: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} requiert une session — envoyer d'abord un message"
        }
        "Nothing is running" => "Rien en cours",
        "No queued message" => "Aucun message en file",
        "Only in fullscreen (/tui)" => "Plein écran uniquement (/tui)",
        "new session" => "nouvelle session",
        "resumed {id} · {count} messages" => "{id} repris · {count} messages",
        " · earlier history: /timeline" => " · historique antérieur: /timeline",
        "Error: {message}" => "Erreur: {message}",
        "working…  {elapsed}" => "travail…  {elapsed}",
        "thinking…  {elapsed}" => "réflexion…  {elapsed}",
        "Ask anything... · Ctrl+J newline" => "Posez une question… · Ctrl+J nouvelle ligne",
        "Earlier history · {id} · {count} messages" => {
            "Historique antérieur · {id} · {count} messages"
        }
        "No active session" => "Aucune session active",
        "answer required" => "réponse requise",
        "permission required" => "autorisation requise",
        "esc interrupt" => "esc interrompre",
        "shift+← edit last · ctrl+s steer now" => {
            "shift+← modifier la dernière · ctrl+s orienter maintenant"
        }
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count} autres · shift+← modifier la dernière · ctrl+s orienter maintenant"
        }
        "Queue ({count})" => "File ({count})",
        "Thinking…" => "Réflexion…",
        "Thought" => "Réflexion",
        "Thought for {duration}" => "Réflexion de {duration}",
        "Welcome to DIM {version}!" => "Bienvenue dans DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => {
            "Tapez / pour les commandes · Ctrl+O pour la palette"
        }
        "Workspace" => "Espace de travail",
        "Git branch" => "Branche Git",
        "Bye!" => "Au revoir!",
        "DIM Web is ready" => "DIM Web est prêt",
        "Opened in your browser" => "Ouvert dans le navigateur",
        "Open the sign-in link below" => "Ouvrir le lien ci-dessous",
        "Web" => "Web",
        "Sign-in link:" => "Lien de connexion:",
        "Resume" => "Reprendre",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C pour arrêter l'espace Web.",
        "Feedback or ideas? Join us on Discord:" => {
            "Un avis ou une idée ? Rejoignez-nous sur Discord :"
        }
        "To resume this session:" => "Pour reprendre la session:",

        // commands.rs: descriptions
        "start a new session" => "démarrer une session",
        "show or set the reasoning effort" => "voir ou régler l'effort de raisonnement",
        "show or set the priority service tier (on|off)" => {
            "voir ou régler le niveau prioritaire (on|off)"
        }
        "list agents or switch this session's agent" => "lister ou changer l'agent de la session",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "lister les serveurs MCP, (dé)connexion (login|logout ID)"
        }
        "resume a previous session" => "reprendre une session",
        "turn the web interface on or off" => "activer ou désactiver l'interface web",
        "show the context window usage" => "voir l'usage du contexte",
        "interrupt the run" => "interrompre l'exécution",
        "steer with the next queued message" => "orienter avec le message en file",
        "scroll to the top" => "défiler en haut",
        "scroll to the bottom" => "défiler en bas",
        "pick an earlier message to edit or quote" => "choisir un message à modifier ou citer",
        "cycle tool details and compaction summaries" => {
            "alterner les détails des outils et les résumés de compactage"
        }
        "switch between fullscreen and scrollback" => "basculer plein écran / défilement",
        "switch theme" => "changer de thème",
        "switch language" => "changer de langue",
        "select a model" => "choisir un modèle",
        "connect a provider" => "connecter un fournisseur",
        "sign in to DimAgent" => "se connecter à DimAgent",
        "DimAgent account · usage · sign out" => "compte DimAgent · usage · déconnexion",
        "configure tool approvals" => "configurer les approbations",
        "choose ask or full access" => "choisir demander ou accès complet",
        "open settings" => "ouvrir les paramètres",
        "exit" => "quitter",

        // composer/palette.rs
        "Default for new sessions in {folder}: {model}" => {
            "Modèle par défaut des nouvelles sessions dans {folder} : {model}"
        }
        "Default for new sessions: {model}" => "Modèle par défaut des nouvelles sessions : {model}",
        "Enter select · Esc clear/close" => "Enter choisir · Esc effacer/fermer",
        "esc close" => "esc fermer",
        "Suggestions" => "Suggestions",
        "Skills" => "Skills",
        "Skill loaded: {name}" => "Skill chargé: {name}",
        "Hook context · {count} line" => "Contexte du hook · {count} ligne",
        "Hook context · {count} lines" => "Contexte du hook · {count} lignes",
        "Stop hook follow-up · {count} line" => "Suite du hook Stop · {count} ligne",
        "Stop hook follow-up · {count} lines" => "Suite du hook Stop · {count} lignes",
        "Commands" => "Commandes",
        "Command Palette" => "Palette de commandes",
        "No matches" => "Aucun résultat",
        " (unavailable)" => " (indisponible)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Texte: {preview} ({count} car.)]",
        "[... {count} lines]" => "[… {count} lignes]",
        "pasted text" => "texte collé",

        // list_panel.rs

        // settings_panel.rs
        "on" => "activé",
        "off" => "désactivé",
        "Interface" => "Interface",
        "Theme" => "Thème",
        "Language" => "Langue",
        "Startup UI form" => "Mode au démarrage",
        "Default send mode" => "Mode d’envoi par défaut",
        "fullscreen" => "plein écran",
        "scrollback" => "défilement",
        "Tool details" => "Détails d'outils",
        "Thinking details" => "Détails de réflexion",
        "expanded" => "dépliés",
        "collapsed" => "repliés",
        "Mouse (fullscreen)" => "Souris (plein écran)",
        "Permissions" => "Autorisations",
        "Tool approvals" => "Approbations d'outils",
        "Execution" => "Exécution",
        "Models" => "Modèles",
        "Settings" => "Paramètres",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ déplacer · Enter choisir · ←→/Space changer · Esc fermer"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ aperçu · Enter enregistrer · Esc restaurer"
        }
        "System default ({locale})" => "Système ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ aperçu · Enter appliquer · Esc annuler"
        }
        "System" => "Système",
        "Could not save the theme: {error}" => "Thème non enregistré: {error}",

        // sessions_panel.rs
        "Sessions" => "Sessions",
        "Sessions · This workspace" => "Sessions · Cet espace de travail",
        "Sessions · This repository" => "Sessions · Ce dépôt",
        "Sessions · All workspaces" => "Sessions · Tous les espaces de travail",
        "No saved sessions in this workspace. Ctrl+S shows more." => {
            "Aucune session enregistrée dans cet espace de travail. Ctrl+S en affiche d'autres."
        }
        "Enter open · Ctrl+X delete · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter ouvrir · Ctrl+X supprimer · Ctrl+S portée · Ctrl+R actualiser · Esc fermer"
        }
        "Loading sessions..." => "Chargement…",
        "No saved sessions yet." => "Aucune session enregistrée.",
        "No matching sessions." => "Aucun résultat.",
        "now" => "maintenant",
        "{count}m ago" => "il y a {count} min",
        "{count}h ago" => "il y a {count} h",
        "{count}d ago" => "il y a {count} j",

        // models_panel.rs
        "Select a model" => "Choisir un modèle",
        "Loading models..." => "Chargement…",
        "Previous page" => "Page précédente",
        "Next page" => "Page suivante",
        "No models available. Connect a provider first." => {
            "Aucun modèle. Connecter d'abord un fournisseur."
        }
        "No matching models." => "Aucun résultat.",
        "current" => "actuel",
        "Edit models" => "Modifier les modèles",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 1 modèle indisponible · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: {count} modèles indisponibles · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider} : 1 modèle a besoin d’une fenêtre · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider} : {count} modèles ont besoin d’une fenêtre · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: aucun modèle · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: aucun modèle · ajoutez un modèle avec provider.models.create"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable} indisponibles et {disabled} désactivés · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: catalogue illisible · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "Un modèle dont les limites ne sont pas déclarées ne peut pas être sélectionné ; déclarez-les avec dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "Déclarez-les avec dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "Aucun catalogue en cache. Connectez-vous et exécutez models PROVIDER --refresh."
        }
        "Saved model preferences cleared" => "Préférences de modèle enregistrées effacées",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 1 modèle désactivé · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: {count} modèles désactivés · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "Aucun modèle. Lancez dimcode models {id} --refresh pour les afficher."
        }
        "No models. The provider listed none." => {
            "Aucun modèle. Le fournisseur n'en a listé aucun."
        }
        "Enter select · Esc back" => "Enter choisir · Esc retour",
        "Subagent models" => "Modèles des sous-agents",
        "Subagent models: {policy}" => "Modèles des sous-agents: {policy}",
        "Fixed subagent model" => "Modèle de sous-agent fixe",
        "Agent choice (default)" => "Choix de l'agent (défaut)",
        "Agent choice" => "Choix de l'agent",
        "Inherit requesting branch" => "Hériter de la branche appelante",
        "Fixed model" => "Modèle fixe",
        "Applies to new members, including named Agents." => {
            "S'applique aux nouveaux membres, agents nommés inclus."
        }
        "Existing members keep their models." => "Les membres existants gardent leur modèle.",
        "Conversation model" => "Modèle de conversation",
        "Selected: {model}" => "Sélectionné : {model}",
        "Model management" => "Gestion des modèles",
        "Manage models" => "Gérer les modèles",
        "1 provider" => "1 fournisseur",
        "{count} providers" => "{count} fournisseurs",
        "ready" => "prêt",
        "no models" => "aucun modèle",
        "catalog unreadable" => "catalogue illisible",
        "{count} with missing limits" => "{count} sans limites",
        "{count} unavailable" => "{count} indisponibles",
        "{count} disabled" => "{count} désactivés",
        "{count} without models" => "{count} sans modèle",
        "{count} unreadable" => "{count} illisibles",
        "{unavailable} unavailable, {disabled} disabled" => {
            "{unavailable} indisponibles, {disabled} désactivés"
        }
        "Enter select + default · ←/→ effort · Tab section · Esc clear/close" => {
            "Enter choisir + défaut · ←/→ effort · Tab section · Esc effacer/fermer"
        }
        "Enter change · Tab section · Esc clear/close" => {
            "Enter modifier · Tab section · Esc effacer/fermer"
        }
        "Enter manage · Tab section · Esc clear/close" => {
            "Enter gérer · Tab section · Esc effacer/fermer"
        }
        "Enter manage · Esc back" => "Enter gérer · Esc retour",
        "unknown" => "inconnu",
        "Subagent model policy saved" => "Politique des sous-agents enregistrée",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Politique des sous-agents modifiée ailleurs; choisir à nouveau pour relire"
        }
        "Subagent model policy: {error}" => "Politique des sous-agents: {error}",
        "drawing diagram… {count} lines" => "dessin du diagramme… {count} lignes",
        // notify.rs
        "Task complete" => "Tâche terminée",
        "Background task finished: {label}" => "Tâche d'arrière-plan terminée : {label}",
        "Background task failed: {label}" => "Tâche d'arrière-plan échouée : {label}",
        "Background task stopped: {label}" => "Tâche d'arrière-plan arrêtée : {label}",
        "Background tasks" => "Tâches d'arrière-plan",
        "Nothing runs in the background." => "Rien ne s'exécute en arrière-plan.",
        "No matching tasks." => "Aucune tâche correspondante.",
        "Permission needed: {tool}" => "Autorisation requise: {tool}",
        "Question waiting for your answer" => "Question en attente de réponse",
        "Notify when agent needs you" => "Notifier si l'agent vous attend",
        "Show ACP sessions" => "Afficher les sessions ACP",
        "Prompt suggestion" => "Suggestion de prompt",
        "Prompt suggestion: {error}" => "Suggestion de prompt: {error}",
        "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume." => {
            "Les suggestions sont en pause dans cette session parce que des propositions sont restées inutilisées. Désactivez puis réactivez dans /settings pour reprendre."
        }
        // keys.rs
        "show the key bindings" => "afficher les raccourcis",
        "check for a newer version" => "rechercher une version plus récente",
        "Application keys" => "Raccourcis de l'app",
        "Enter submit when idle · Shift+Enter / Alt+Enter / Ctrl+J newline · ↑↓ history" => {
            "Enter envoie au repos · Shift+Enter / Alt+Enter / Ctrl+J nouvelle ligne · ↑↓ historique"
        }
        "Running with a draft: Enter {default} · Ctrl+S {alternate}" => {
            "Exécution en cours avec brouillon : Enter {default} · Ctrl+S {alternate}"
        }
        "Empty draft: {keys} steer first queued" => {
            "Brouillon vide : {keys} transmet le premier message en attente à l’exécution"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Shift+← edit last queued" => {
            "Ctrl+O ou Ctrl+P commandes · Esc interrompre, les messages en file reviennent · Shift+← modifier la dernière en file"
        }
        "Ctrl+Enter interrupt and send now: the draft and every held message, as one message" => {
            "Ctrl+Enter interrompt et envoie aussitôt : le brouillon et tous les messages retenus en un seul"
        }
        "Send now failed: {error}; the draft was kept" => {
            "Échec de l’envoi immédiat : {error} ; le brouillon est conservé"
        }
        "Send now cancelled; the draft was kept" => {
            "Envoi immédiat annulé ; le brouillon est conservé"
        }
        "Send now is still stopping the run; the draft was kept" => {
            "L'envoi immédiat arrête encore l'exécution ; le brouillon est conservé"
        }
        "Send now is stopping the run; paste again when the draft is sent" => {
            "L'envoi immédiat arrête l'exécution ; collez à nouveau quand le brouillon est envoyé"
        }
        "Send now is waiting for the insertion before sending" => {
            "L'envoi immédiat attend l'insertion avant d'envoyer"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C effacer, deux fois pour quitter · Ctrl+D quitter si brouillon vide"
        }
        "Shift+Tab cycle permissions (composer focused)" => {
            "Shift+Tab changer les permissions (zone de saisie active)"
        }
        "Shift+Tab newer target while picked · Esc return to composer" => {
            "Sélection active : Shift+Tab vers une cible plus récente · Esc retour à la saisie"
        }
        "(shift+tab to cycle)" => "(Shift+Tab pour changer)",
        "Fullscreen" => "Plein écran",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown défiler · Home/End haut ou bas (brouillon vide) · Tab choisir un message, e modifier, q citer"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "molette défiler · glisser sélectionner et copier · clic: ouvrir un lien, déplier un groupe d'outils"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "souris désactivée (/settings): le terminal sélectionne et copie"
        }
        "Terminal-owned history" => "Historique du terminal",
        "scroll: {value}" => "défiler: {value}",
        "search: {value}" => "rechercher: {value}",
        "copy: {value}" => "copier: {value}",
        "terminal search" => "recherche du terminal",
        "terminal scrollback" => "défilement du terminal",
        "terminal copy" => "copie du terminal",
        "drag to select" => "glisser pour sélectionner",
        "drag or {shortcut}" => "glisser ou {shortcut}",
        "Compacting context…" => "Compression du contexte…",
        "Context compacted" => "Contexte compressé",
        "Context compaction skipped" => "Compression du contexte ignorée",
        "Context compaction failed" => "La compression du contexte a échoué",
        "Context compaction cancelled" => "Compression du contexte annulée",
        "near the compaction threshold" => "proche du seuil de compression",
        "This daemon does not support context compaction" => {
            "Ce démon ne prend pas en charge la compression du contexte"
        }
        "compact the context of the shown line" => "compresser le contexte de la ligne affichée",
        "follow or change the shown line's goal" => {
            "suivre ou modifier l'objectif de la ligne affichée"
        }
        "◎ goal active" => "◎ objectif actif",
        "◎ goal paused" => "◎ objectif en pause",
        "insufficient space: {parts}" => "espace insuffisant: {parts}",
        "system prompt" => "invite système",
        "tools" => "outils",
        "appended messages" => "messages ajoutés",
        "keyed baseline" => "base à clés",
        "pinned inputs" => "entrées épinglées",
        "digest" => "condensé",
        "summary" => "résumé",
        "The first message was not sent; your new draft was kept." => {
            "Le premier message n'a pas été envoyé ; votre nouveau brouillon a été conservé."
        }
        "The App Server is still {server} because other clients are using it; this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "L'App Server est toujours {server} car d'autres clients l'utilisent ; ce client {client} l'utilise aussi. Exécutez `dimcode daemon stop` lorsqu'ils ont terminé ; le prochain démarrage exécutera {client}."
        }
        "The App Server is still {server} because other clients are using it, and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "L'App Server est toujours {server} car d'autres clients l'utilisent, et il parle le protocole v{protocol}, que ce client {client} ne peut pas utiliser. Aucune session n'a été démarrée. Exécutez `dimcode daemon stop` lorsqu'ils ont terminé ; le prochain démarrage exécutera {client}."
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}), and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "L'App Server est toujours {server} car d'autres clients l'utilisent (connexions : {connections}, sessions attachées : {attached}, tours en cours : {running}), et il parle le protocole v{protocol}, que ce client {client} ne peut pas utiliser. Aucune session n'a été démarrée. Exécutez `dimcode daemon stop` lorsqu'ils ont terminé ; le prochain démarrage exécutera {client}."
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}); this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "L'App Server est toujours {server} car d'autres clients l'utilisent (connexions : {connections}, sessions attachées : {attached}, tours en cours : {running}) ; ce client {client} l'utilise aussi. Exécutez `dimcode daemon stop` lorsqu'ils ont terminé ; le prochain démarrage exécutera {client}."
        }
        "The App Server is still using a different build of {server}. Some features may be unavailable. After active work finishes, exit this client, run `{command}`, then reopen it." => {
            "L'App Server utilise toujours une autre compilation de {server}. Certaines fonctionnalités peuvent être indisponibles. Une fois le travail en cours terminé, quittez ce client, exécutez `{command}`, puis rouvrez-le."
        }
        "The App Server's build could not be verified as this client's. If features are missing after an update, wait for active work to finish, exit this client, run `{command}`, then reopen it." => {
            "La compilation de l'App Server n'a pas pu être confirmée comme celle de ce client. Si des fonctionnalités manquent après une mise à jour, attendez la fin du travail en cours, quittez ce client, exécutez `{command}`, puis rouvrez-le."
        }
        "Upgrading the local store from format v{from} to v{to}; this can take a while on a large store." => {
            "Mise à niveau du magasin local du format v{from} vers v{to} ; cela peut prendre un certain temps sur un grand magasin."
        }
        "Upgrading the local store · {seconds} s · {stage}" => {
            "Mise à niveau du magasin local · {seconds} s · {stage}"
        }
        "moving retained images into Session files" => {
            "déplacement des images conservées vers des fichiers de session"
        }
        "updating the text index of the rows that changed" => {
            "mise à jour de l'index de texte des lignes modifiées"
        }
        "checking and committing the upgraded store" => {
            "vérification et validation du magasin mis à niveau"
        }
        "A prompt save is still in progress" => "Un enregistrement de prompt est encore en cours",
        "Base prompt" => "Prompt de base",
        "Before/after comparison is unavailable for this record." => {
            "La comparaison avant/après n'est pas disponible pour cet enregistrement."
        }
        "Cancelling generation…" => "Annulation de la génération…",
        "Changes" => "Modifications",
        "Compaction prompt" => "Prompt de compaction",
        "Could not open the share destination" => "Impossible d'ouvrir la destination de partage",
        "Could not save ZIP" => "Impossible d'enregistrer le ZIP",
        "Default: current-session diagnostics only." => {
            "Par défaut : diagnostics de la session actuelle uniquement."
        }
        "Draft a GitHub issue" => "Rédiger une issue GitHub",
        "Email the ZIP to {email}" => "Envoyer le ZIP par e-mail à {email}",
        "Enter apply · Esc back" => "Enter appliquer · Esc retour",
        "Error diagnostics only; no session text is available." => {
            "Diagnostic d'erreur uniquement ; aucun texte de session n'est disponible."
        }
        "Esc back · x stop agent · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc retour · x arrêter l'agent · ↑↓ PgUp PgDn défiler · Home/End aller"
        }
        "Esc back · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc retour · ↑↓ PgUp PgDn défiler · Home/End aller"
        }
        "Evidence · No recent session activity" => "Preuves · Aucune activité de session récente",
        "Evidence · Observed error only" => "Preuves · Erreur observée uniquement",
        "Evidence · Recent session activity" => "Preuves · Activité de session récente",
        "Evidence · Timeline omitted" => "Preuves · Chronologie omise",
        "Evidence · Timeline read failed" => "Preuves · Lecture de la chronologie impossible",
        "Evidence · Timeline unavailable" => "Preuves · Chronologie indisponible",
        "Generate report" => "Générer le rapport",
        "Generate/save never uploads. Share manually after saving." => {
            "Générer et enregistrer ne téléversent rien. Partagez manuellement après l'enregistrement."
        }
        "Generating report…" => "Génération du rapport…",
        "Give feedback on Discord" => "Donner un avis sur Discord",
        "Input" => "Entrée",
        "Invalid report ZIP" => "ZIP de rapport non valide",
        "Local generation makes zero model requests." => {
            "La génération locale n'envoie aucune requête au modèle."
        }
        "Model · Generate locally" => "Modèle · Générer localement",
        "New" => "Nouveau",
        "No content changes." => "Aucune modification de contenu.",
        "No session to report." => "Aucune session à signaler.",
        "Old" => "Ancien",
        "Opened Discord" => "Discord ouvert",
        "Opened GitHub issue draft" => "Brouillon d'issue GitHub ouvert",
        "Options" => "Options",
        "Options must fit within 1 KiB" => "Les options doivent tenir dans 1 KiB",
        "Options · for example: omit timeline, keep error codes" => {
            "Options · par exemple : omit timeline, keep error codes"
        }
        "Prepare report again" => "Préparer le rapport à nouveau",
        "Preparing report…" => "Préparation du rapport…",
        "Prompt saved; applies to new Sessions" => {
            "Prompt enregistré ; s'applique aux nouvelles sessions"
        }
        "Prompt settings are not available" => "Les paramètres de prompt ne sont pas disponibles",
        "Prompts" => "Prompts",
        "Read-only: this agent has finished." => "Lecture seule : cet agent a terminé.",
        "Read-only: you can watch this agent or stop it." => {
            "Lecture seule : vous pouvez suivre cet agent ou l'arrêter."
        }
        "Redact [ ]" => "Masquer [ ]",
        "Redact [x]" => "Masquer [x]",
        "Redact is off. The ZIP may contain sensitive text from recent messages." => {
            "Le masquage est désactivé. Le ZIP peut contenir du texte sensible des messages récents."
        }
        "Refresh report plan" => "Actualiser le plan du rapport",
        "Report ready" => "Rapport prêt",
        "Report this error" => "Signaler cette erreur",
        "Report this session" => "Signaler cette session",
        "Resetting {field}; the old text is kept in {path}" => {
            "Réinitialisation de {field} ; l'ancien texte est conservé dans {path}"
        }
        "Result" => "Résultat",
        "Run /report to create a report." => "Exécutez /report pour créer un rapport.",
        "Running…" => "En cours…",
        "Save ZIP" => "Enregistrer le ZIP",
        "Saved ZIP" => "ZIP enregistré",
        "Session model" => "Modèle de session",
        "Skills instructions" => "Instructions des skills",
        "Some options were unsupported." => "Certaines options ne sont pas prises en charge.",
        "Tab also picks tool headers · Enter/Space toggle the focused tool" => {
            "Tab choisit aussi les en-têtes d'outil · Enter/Espace ouvre ou ferme l'outil ciblé"
        }
        "This session is closed and cannot be resumed. Read its stored history with dimcode export {id} using the same --home." => {
            "Cette session est fermée et ne peut pas être reprise. Lisez son historique enregistré avec dimcode export {id} en utilisant le même --home."
        }
        "Turn Redact off to attach recent user/assistant text to ZIP; it may contain sensitive data." => {
            "Désactivez Masquer pour joindre le texte récent de l'utilisateur et de l'assistant au ZIP ; il peut contenir des données sensibles."
        }
        "Updating report plan…" => "Mise à jour du plan du rapport…",
        "Usage: /report" => "Utilisation: /report",
        "Waiting for result…" => "En attente du résultat…",
        "ZIP contents: report.md, diagnostics.json" => {
            "Contenu du ZIP : report.md, diagnostics.json"
        }
        "agent result" => "résultat d'agent",
        "built-in" => "intégré",
        "custom" => "personnalisé",
        "report the current session" => "signaler la session actuelle",
        "skills replaces the marker and call shape; the skill tool and list stay · r reset" => {
            "skills remplace le marqueur et la forme d'appel ; l'outil skill et la liste restent · r réinitialiser"
        }
        "… {count} unchanged lines" => "… {count} lignes inchangées",
        "↑↓ move · Enter edit · r reset · Esc close" => {
            "↑↓ déplacer · Enter modifier · r réinitialiser · Esc fermer"
        }
        "↑↓ move · Enter select · ←→/Space change · r reset · Esc close" => {
            "↑↓ déplacer · Enter choisir · ←→/Espace changer · r réinitialiser · Esc fermer"
        }
        "↑↓/Tab move · Enter select · Esc close" => {
            "↑↓/Tab déplacer · Enter choisir · Esc fermer"
        }
        "Permission mode" => "Mode d'autorisation",
        "Related settings" => "Paramètres associés",
        "Process isolation" => "Isolation des processus",
        "open" => "ouvrir",
        "User-wide command rules. Allow skips asks, never denials." => {
            "Règles de commande pour tout l'utilisateur. Autoriser saute les demandes, jamais les refus."
        }
        "Opens Settings; user-wide, for new processes; independent of permission mode." => {
            "Ouvre les paramètres ; pour tout l'utilisateur, pour les nouveaux processus ; indépendant du mode d'autorisation."
        }
        "keep mode" => "garder le mode",
        "use mode" => "utiliser le mode",
        "Current session; remembered for new sessions. {consequence}" => {
            "Session actuelle ; mémorisé pour les nouvelles sessions. {consequence}"
        }
        "↑↓/Tab move · Enter {action} · Esc close" => {
            "↑↓/Tab déplacer · Enter {action} · Esc fermer"
        }
        "Writes and commands denied; external reads may ask." => {
            "Écritures et commandes refusées ; les lectures externes peuvent demander."
        }
        "Workspace writes allowed; commands, network and ungranted outside access usually ask." => {
            "Écritures dans l'espace de travail autorisées ; commandes, réseau et accès externe non accordé demandent généralement."
        }
        "Skips preset approval; pending calls on this branch are reevaluated. Explicit denies and isolation remain." => {
            "Saute l'approbation du préréglage ; les appels en attente sur cette branche sont réévalués. Les refus explicites et l'isolation restent."
        }
        "↑↓ move · Enter select · Esc close" => "↑↓ déplacer · Enter choisir · Esc fermer",
        "Loading process isolation…" => "Chargement de l'isolation des processus…",
        "Sandbox is unavailable on this host: {reason}" => {
            "Le bac à sable n'est pas disponible sur cet hôte : {reason}"
        }
        "Applies to new processes and shells." => "S'applique aux nouveaux processus et shells.",
        "Off: commands use your OS permissions, not file-tool limits." => {
            "Désactivé : les commandes utilisent les permissions de votre OS, pas les limites des outils de fichiers."
        }
        "Off" => "Désactivé",
        "Sandboxed · No network" => "Bac à sable · Sans réseau",
        "Sandboxed · Network allowed" => "Bac à sable · Réseau autorisé",
        "selected" => "sélectionné",
        "Save" => "Enregistrer",
        "Reset to defaults" => "Réinitialiser aux valeurs par défaut",
        "Reload" => "Recharger",
        "Discard edits and read the current settings" => {
            "Abandonner les modifications et lire les paramètres actuels"
        }
        "Reset the execution switches to defaults?" => {
            "Réinitialiser les commutateurs d'exécution aux valeurs par défaut ?"
        }
        "Discard unsaved changes?" => "Abandonner les modifications non enregistrées ?",
        "Yes (y)" => "Oui (y)",
        "Cancel (n)" => "Annuler (n)",
        "y confirm · Enter/Esc cancel" => "y confirmer · Enter/Esc annuler",
        "Read only" => "Lecture seule",
        "Workspace write" => "Écriture espace de travail",
        "Agent" => "Agent",
        "agent" => "agent",
        "command" => "commande",
        "ended" => "terminée",
        "Signed in" => "Connecté",
        "Retry" => "Réessayer",
        "Provider changed; review the retained draft and press Apply again" => {
            "Fournisseur modifié ; vérifiez le brouillon conservé et appuyez de nouveau sur Appliquer"
        }
        "y allow · a always · n reject · ←→/Tab move · Enter confirm" => {
            "y autoriser · a toujours · n refuser · ←→/Tab déplacer · Enter confirmer"
        }
        "{count} in background · /tasks" => "{count} en arrière-plan · /tasks",
        "{field}: {error} Your text is kept in {path}" => {
            "{field} : {error} Votre texte est conservé dans {path}"
        }
        "{field}: {error}" => "{field} : {error}",
        "Prompts: {error}" => "Prompts : {error}",
        _ => return None,
    })
}
