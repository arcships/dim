pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Actualiser la liste des modèles",
        "Model list refreshed" => "Liste des modèles actualisée",
        "Model list refresh failed: {reason}" => "Échec de l’actualisation des modèles : {reason}",
        "Refreshing model list… · esc close" => "Actualisation des modèles… · Échap ferme",
        // providers_panel.rs
        "Providers" => "Fournisseurs",
        "Enter open · Esc clear/close" => "Enter ouvrir · Esc effacer/fermer",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab déplacer · Enter activer · Esc retour"
        }
        "Loading providers..." => "Chargement…",
        "No matching providers." => "Aucun résultat.",
        "Custom providers are not available yet" => {
            "Fournisseurs personnalisés pas encore disponibles"
        }
        "Testing connection..." => "Test de connexion…",
        "Connect {providerId}" => "Connecter {providerId}",
        "(default)" => "(défaut)",
        "(required)" => "(requis)",
        "Test + apply" => "Tester + appliquer",
        "Testing sends the stored credential to this URL." => {
            "Le test envoie l'identifiant à cette URL."
        }
        "Disconnect" => "Déconnecter",
        "Enable provider" => "Activer",
        "Disable provider" => "Désactiver",
        "Error: {error}" => "Erreur: {error}",
        "disabled" => "désactivé",
        "current" => "actuel",
        "Sign in" => "Se connecter",
        "ready" => "prêt",
        "error" => "erreur",
        "not connected" => "non connecté",
        "Add custom provider" => "Ajouter un fournisseur personnalisé",
        // oauth_view.rs
        "Checking account…" => "Vérification du compte…",
        "Not signed in" => "Non connecté",
        "Signed in as {account}" => "Connecté: {account}",
        "Open models" => "Ouvrir les modèles",
        "View plan" => "Voir l'offre",
        "Sign out" => "Se déconnecter",
        "Checking usage…" => "Vérification de l'usage…",
        "Usage: {reason}" => "Usage: {reason}",
        "Credits" => "Crédits",
        "unlimited" => "illimité",
        "{hours}h window" => "fenêtre {hours} h",
        "/month" => "/mois",
        "/year" => "/an",
        "expires {date}" => "expire le {date}",
        "resets {date}" => "réinitialisé le {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ déplacer · enter activer · ctrl+r actualiser · esc fermer"
        }
        "Login with DimAgent" => "Connexion avec DimAgent",
        "Enterprise login" => "Connexion entreprise",
        "Device login" => "Connexion par appareil",
        "Email" => "E-mail",
        "Continue" => "Continuer",
        "Browser login" => "Connexion par navigateur",
        "Restart device login" => "Relancer la connexion",
        "Select to copy · Enter opens link" => "Sélectionner pour copier · Entrée ouvre le lien",
        "Link shortened · Enter opens link" => "Lien abrégé · Entrée ouvre le lien",
        "Callback URL" => "URL de rappel",
        "Submit callback" => "Envoyer le rappel",
        "The browser could not be opened. Open the link manually, then return here." => {
            "Navigateur impossible à ouvrir. Ouvrir le lien manuellement, puis revenir ici."
        }
        "Waiting for authorization" => "En attente d'autorisation",
        "{name} device login" => "Connexion par appareil {name}",
        "Open this URL: {url}" => "Ouvrir cette URL: {url}",
        "Enter code:    {code}" => "Saisir le code:    {code}",
        "Waiting after server slow-down" => "Attente (serveur ralenti)",
        "OAuth login cancelled" => "Connexion OAuth annulée",
        "Enter your work email" => "Saisir votre e-mail pro",
        "Invalid OAuth state - possible CSRF attempt" => "État OAuth invalide - CSRF possible",
        "Preparing login... · esc cancel" => "Préparation… · esc annuler",
        "Completing login... · esc cancel" => "Finalisation… · esc annuler",
        "enter restart · esc back" => "enter relancer · esc retour",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "saisir · ↑↓/tab déplacer · enter envoyer · esc annuler"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ déplacer · enter activer · esc retour",
        "↑↓ move · enter activate · esc close" => "↑↓ déplacer · enter activer · esc fermer",
        "(paste here)" => "(collez ici)",
        _ => return None,
    })
}
