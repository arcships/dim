pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
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
        "Clipboard: {reason}" => "Presse-papiers: {reason}",
        "Wait for clipboard insertion before sending" => "Attendre le collage avant d'envoyer",
        "A submission accepts at most 20 images" => "20 images max par envoi",
        "[Image #{index}]" => "[Image #{index}]",
        "The current model does not accept images; remove {label} to send" => {
            "Le modèle n'accepte pas les images; retirer {label} pour envoyer"
        }
        "Files" => "Fichiers",
        "Searching…" => "Recherche…",
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
        "/{name} needs a session — send a message first" => {
            "/{name} requiert une session — envoyer d'abord un message"
        }
        "Nothing is running" => "Rien en cours",
        "No queued message" => "Aucun message en file",
        "Only in fullscreen (/tui)" => "Plein écran uniquement (/tui)",
        "/{name} is not yet available" => "/{name} pas encore disponible",
        "new session" => "nouvelle session",
        "resumed {id} · {count} messages" => "{id} repris · {count} messages",
        " · earlier history: /history" => " · historique antérieur: /history",
        "Error: {message}" => "Erreur: {message}",
        "working…  {elapsed}" => "travail…  {elapsed}",
        "thinking…  {elapsed}" => "réflexion…  {elapsed}",
        "Ask anything..." => "Posez une question…",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "Historique antérieur · {id} · {count} messages · /history page précédente"
        }
        "No active session" => "Aucune session active",
        "No earlier messages." => "Aucun message antérieur.",
        "answer required" => "réponse requise",
        "permission required" => "autorisation requise",
        "esc interrupt" => "esc interrompre",
        "↑ edit · ctrl+s steer now · /queue" => "↑ modifier · ctrl+s orienter · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "+{count} autres · ↑ modifier · ctrl+s orienter · /queue"
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
        "To resume this session:" => "Pour reprendre la session:",

        // composer/commands.rs: categories
        "session" => "session",
        "run" => "exécution",
        "view" => "affichage",
        "model" => "modèle",
        "app" => "app",
        "skill" => "skill",
        // composer/commands.rs: descriptions
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
        "open this session in the browser" => "ouvrir la session dans le navigateur",
        "load the previous page of messages" => "charger la page précédente",
        "show the context window usage" => "voir l'usage du contexte",
        "interrupt the run" => "interrompre l'exécution",
        "steer with the next queued message" => "orienter avec le message en file",
        "scroll to the top" => "défiler en haut",
        "scroll to the bottom" => "défiler en bas",
        "pick an earlier message to edit or quote" => "choisir un message à modifier ou citer",
        "collapse or expand tool details and thoughts" => "replier/déplier outils et réflexions",
        "switch between fullscreen and scrollback" => "basculer plein écran / défilement",
        "switch theme" => "changer de thème",
        "switch language" => "changer de langue",
        "select a model" => "choisir un modèle",
        "connect a provider" => "connecter un fournisseur",
        "sign in to DimAgent" => "se connecter à DimAgent",
        "DimAgent account · usage · sign out" => "compte DimAgent · usage · déconnexion",
        "configure tool approvals" => "configurer les approbations",
        "open settings" => "ouvrir les paramètres",
        "suspend to the shell" => "suspendre vers le shell",
        "exit" => "quitter",

        // composer/palette.rs
        "Enter select · Ctrl+X clear saved · Esc clear/close" => {
            "Enter choisir · Ctrl+X oublier · Esc effacer/fermer"
        }
        "Enter select · Esc clear/close" => "Enter choisir · Esc effacer/fermer",
        "esc close" => "esc fermer",
        "Suggestions" => "Suggestions",
        "Skills" => "Skills",
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
        "fullscreen" => "plein écran",
        "scrollback" => "défilement",
        "Tool details" => "Détails d'outils",
        "expanded" => "dépliés",
        "collapsed" => "repliés",
        "Mouse (fullscreen)" => "Souris (plein écran)",
        "Permissions" => "Autorisations",
        "Tool approvals" => "Approbations d'outils",
        "Models" => "Modèles",
        "Model" => "Modèle",
        "not set" => "non défini",
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
        "Enter open · Ctrl+R refresh · Esc close" => {
            "Enter ouvrir · Ctrl+R actualiser · Esc fermer"
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
        "No models available. Connect a provider first." => {
            "Aucun modèle. Connecter d'abord un fournisseur."
        }
        "No matching models." => "Aucun résultat.",
        "current" => "actuel",
        "Edit models" => "Modifier les modèles",
        "Editing models is not available yet" => "Édition des modèles pas encore disponible",
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
        "unknown" => "inconnu",
        "Subagent model policy saved" => "Politique des sous-agents enregistrée",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Politique des sous-agents modifiée ailleurs; choisir à nouveau pour relire"
        }
        "Subagent model policy: {error}" => "Politique des sous-agents: {error}",
        "drawing diagram… {count} lines" => "dessin du diagramme… {count} lignes",
        // notify.rs
        "Task complete" => "Tâche terminée",
        "Permission needed: {tool}" => "Autorisation requise: {tool}",
        "Question waiting for your answer" => "Question en attente de réponse",
        "Notify when agent needs you" => "Notifier si l'agent vous attend",
        // keys.rs
        "show the key bindings" => "afficher les raccourcis",
        "Application keys" => "Raccourcis de l'app",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter envoyer · Shift+Enter ou Ctrl+J nouvelle ligne · ↑↓ historique"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O ou Ctrl+P commandes · Esc interrompre ou effacer · Ctrl+S orienter (file) · ↑ modifier (file)"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C effacer, deux fois pour quitter · Ctrl+D quitter si brouillon vide"
        }
        "Fullscreen" => "Plein écran",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown défiler · Home/End haut ou bas · Tab choisir un message, e modifier, q citer"
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
        _ => return None,
    })
}
