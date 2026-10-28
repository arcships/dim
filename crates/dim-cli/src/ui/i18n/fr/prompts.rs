pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "Agent toujours en cours.",
        "AskUser is waiting for an answer." => "AskUser attend une réponse.",
        "A permission request is waiting." => "Demande d'autorisation en attente.",
        "Background work may still be active." => "Des tâches de fond peuvent être actives.",
        "Exit dimcode?" => "Quitter dimcode?",
        "Exit" => "Quitter",
        "Stay" => "Rester",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y quitter · n rester · ←→ déplacer · enter valider"
        }
        "Restart dimcode?" => "Redémarrer dimcode?",
        "Restart" => "Redémarrer",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y redémarrer · n rester · ←→ déplacer · enter valider"
        }
        // upgrade_dialog.rs
        "Update available" => "Mise à jour disponible",
        "Version {latest} is available (you have {current})." => {
            "Version {latest} disponible (actuelle: {current})."
        }
        "Will run: {command}" => "Exécutera: {command}",
        "To update, run: {command}" => "Pour mettre à jour: {command}",
        "Update now" => "Mettre à jour",
        "Skip this version" => "Ignorer cette version",
        "Skipped {version}." => "{version} ignorée.",
        "Later" => "Plus tard",
        "Close" => "Fermer",
        "Restart now" => "Redémarrer",
        "Updating to {version}..." => "Mise à jour vers {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "{version} installée. Redémarrer DimCode pour l'utiliser."
        }
        "Update failed: {error}" => "Échec de mise à jour: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ déplacer · Enter valider · Esc fermer",
        // prompts.rs: permission request
        "Permission required" => "Autorisation requise",
        "Allow once" => "Autoriser une fois",
        "Allow always" => "Toujours autoriser",
        "Reject" => "Refuser",
        "Approve for me" => "Approuver pour moi",
        "Full access" => "Accès complet",
        "Run {tool}" => "Exécuter {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "Mode d'approbation non modifiable ici pour l'instant"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y autoriser · n refuser · ←→/Tab déplacer · Enter valider"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter envoyer · Tab modifier · Esc décliner",
        "Space toggle · Tab next · Esc decline" => "Space cocher · Tab suivante · Esc décliner",
        "1-9 select · Tab next · Esc decline" => "1-9 choisir · Tab suivante · Esc décliner",
        "Question" => "Question",
        "Question on branch {branch}" => "Question de la branche {branch}",
        "Permission on branch {branch}" => "Permission sur la branche {branch}",
        "Sending answer…" => "Envoi…",
        "Question not ready; press Enter to send again" => {
            "Question pas prête; Enter pour renvoyer"
        }
        // prompts.rs: AskUser
        "Confirm" => "Confirmer",
        "Enter save · Esc back" => "Enter enregistrer · Esc retour",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter envoyer · 1-9 modifier · Tab/←→ retour · Esc décliner"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ déplacer · Space cocher · Enter suivante · Tab/←→ changer · Esc décliner"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ déplacer · 1-9 choisir · Enter suivante · Tab/←→ changer · Esc décliner"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion: données invalides.",
        "Missing answer for Q{index}" => "Réponse manquante pour Q{index}",
        "Type your own answer" => "Saisir votre réponse",
        "Review your answers and press Enter to submit." => {
            "Vérifier les réponses puis Enter pour envoyer."
        }
        "Not answered" => "Sans réponse",
        // approvals_panel.rs
        "Tool Approvals" => "Approbations d'outils",
        "full access" => "accès complet",
        "All tools run automatically, bypass permissions" => {
            "Tous les outils s'exécutent sans autorisation"
        }
        "approve for me" => "approuver pour moi",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "L'agent examine les commandes et actions d'outils. Les commandes risquées demandent confirmation; les actions courantes passent automatiquement: sûr, intelligent et pratique."
        }
        "Default permissions" => "Autorisations par défaut",
        "default" => "défaut",
        "Read tools run automatically, write tools require confirmation" => {
            "Outils de lecture automatiques, écriture sur confirmation"
        }
        "current" => "actuel",
        "↑↓/Tab move · Enter apply · Esc back" => {
            "↑↓/Tab déplacer · Enter appliquer · Esc retour"
        }
        "Confirm Full Access" => "Confirmer l'accès complet",
        "Cancel" => "Annuler",
        "Tab switch  Enter confirm  Esc cancel" => "Tab changer  Enter valider  Esc annuler",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "L'accès complet réduit les confirmations et peut exécuter directement actions sensibles, modifications de fichiers ou commandes externes. À utiliser seulement si vous faites confiance à la tâche."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "a exécuté",
        "running" => "exécute",
        "read" => "a lu",
        "reading" => "lit",
        "explored" => "a exploré",
        "exploring" => "explore",
        "edited" => "a modifié",
        "editing" => "modifie",
        "changed" => "a changé",
        "changing" => "change",
        "checked on" => "a vérifié",
        "checking on" => "vérifie",
        "Explored" => "Exploré",
        "Exploring" => "Exploration",
        "Edited" => "Modifié",
        "Editing" => "Modification",
        "Changed" => "Changé",
        "Changing" => "Changement",
        // transcript/tool_group.rs: counts
        "1 file" => "1 fichier",
        "{count} files" => "{count} fichiers",
        "1 web page" => "1 page web",
        "{count} web pages" => "{count} pages web",
        "1 skill" => "1 skill",
        "{count} skills" => "{count} skills",
        "1 search" => "1 recherche",
        "{count} searches" => "{count} recherches",
        "1 command" => "1 commande",
        "{count} commands" => "{count} commandes",
        "1 task" => "1 tâche",
        "{count} tasks" => "{count} tâches",
        "1 tool" => "1 outil",
        "{count} tools" => "{count} outils",
        "{files} and {pages}" => "{files} et {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} en échec",
        // transcript/tool_group.rs: call rows
        "Read" => "Lire",
        "List" => "Lister",
        "Grep" => "Grep",
        "Search the web" => "Chercher sur le web",
        "Read skill" => "Lire le skill",
        "Search git history" => "Fouiller l'historique git",
        "Edit" => "Modifier",
        "Write" => "Écrire",
        "Create" => "Créer",
        " (detached)" => " (détaché)",
        "Checked on background task" => "Tâche de fond vérifiée",
        "Waiting…" => "En attente…",
        "… +{count} lines" => "… +{count} lignes",
        _ => return None,
    })
}
