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
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter aller · e modifier · q citer · b branche · f fork · Esc fermer"
        }
        "No messages yet." => "Aucun message pour l'instant.",
        "Already on screen above; /tui jumps to it" => "Déjà plus haut ; /tui y va directement",
        "{count} earlier messages · Enter or click to load" => {
            "{count} messages antérieurs · Enter ou clic pour charger"
        }
        "follow, talk to and end this session's lines" => {
            "suivre, dialoguer et terminer les lignes de cette session"
        }
        "End line" => "Terminer la ligne",
        "y/N" => "y/N",
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
        "Fork the session before “{message}”? (y/N)" => {
            "Forker la session avant « {message} » ? (y/N)"
        }
        "Fork the session after the last turn? (y/N)" => {
            "Forker la session après le dernier tour ? (y/N)"
        }
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
        " · earlier history: /timeline" => " · historique antérieur: /timeline",
        "Error: {message}" => "Erreur: {message}",
        "working…  {elapsed}" => "travail…  {elapsed}",
        "thinking…  {elapsed}" => "réflexion…  {elapsed}",
        "Ask anything..." => "Posez une question…",
        "Earlier history · {id} · {count} messages" => {
            "Historique antérieur · {id} · {count} messages"
        }
        "No active session" => "Aucune session active",
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
        "show the context window usage" => "voir l'usage du contexte",
        "interrupt the run" => "interrompre l'exécution",
        "steer with the next queued message" => "orienter avec le message en file",
        "scroll to the top" => "défiler en haut",
        "scroll to the bottom" => "défiler en bas",
        "pick an earlier message to edit or quote" => "choisir un message à modifier ou citer",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "replier/déplier outils, réflexions et résumés de compression"
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
        "suspend to the shell" => "suspendre vers le shell",
        "exit" => "quitter",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter choisir · ←/→ effort · Ctrl+X oublier · Esc effacer/fermer"
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
        "Command isolation" => "Isolation des commandes",
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
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 1 modèle indisponible · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: {count} modèles indisponibles · dimcode models {id}"
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
        "Kept verbatim" => "Conservé tel quel",
        "Tool effects" => "Actions des outils",
        "insufficient space: {parts}" => "espace insuffisant: {parts}",
        "system prompt" => "invite système",
        "tools" => "outils",
        "appended messages" => "messages ajoutés",
        "keyed baseline" => "base à clés",
        "pinned inputs" => "entrées épinglées",
        "digest" => "condensé",
        "summary" => "résumé",
        _ => return None,
    })
}
