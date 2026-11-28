//! French model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Conversation state could not be read; retry the read before submitting" => {
            "Impossible de lire l’état de la conversation. Rechargez-la avant l’envoi"
        }
        "The centre moved before the command ran; the draft was kept" => {
            "La session ou la branche a changé avant l’envoi. Le brouillon a été conservé"
        }
        "The selected device restarted during file search" => {
            "L’appareil sélectionné a redémarré pendant la recherche"
        }
        "Target does not support file completion; type the remote path" => {
            "La cible ne prend pas en charge la complétion de fichiers. Saisissez le chemin distant"
        }
        "Select a regular file to upload" => "Sélectionnez un fichier ordinaire à envoyer",
        "Target did not return an image" => "La cible n’a renvoyé aucune image",
        "File search interrupted" => "Recherche de fichiers interrompue",
        "Target changed during attachment preparation" => {
            "La cible a changé pendant la préparation des pièces jointes"
        }
        "Attachment preparation cancelled" => "Préparation des pièces jointes annulée",
        "The session or line changed during upload; the draft was kept" => {
            "La session ou la ligne a changé pendant l’envoi. Le brouillon est conservé"
        }
        "Preparing attachments · Ctrl+C to cancel" => {
            "Préparation des pièces jointes · Ctrl+C pour annuler"
        }
        "{reason}; draft retained" => "{reason}. Brouillon conservé",
        "The previous attachment submission is pending; draft retained" => {
            "L’envoi précédent avec pièces jointes est en attente. Brouillon conservé"
        }
        "Submitting attachments" => "Envoi du message avec pièces jointes",
        "Delivery unconfirmed · Enter on an empty draft to check and retry" => {
            "Envoi non confirmé · Entrée dans un brouillon vide pour vérifier et réessayer"
        }
        "Delivery unconfirmed: {reason}" => "Envoi non confirmé : {reason}",
        "Delivery unconfirmed: {reason}. Enter to check and retry, Ctrl+C to exit" => {
            "Envoi non confirmé : {reason}. Entrée pour vérifier et réessayer, Ctrl+C pour quitter"
        }
        "The selected device restarted" => "L’appareil sélectionné a redémarré",
        "The original submission was accepted" => "L’envoi initial a été accepté",
        "The current model does not accept clipboard images; draft retained" => {
            "Le modèle actuel n’accepte pas les images du presse-papiers. Brouillon conservé"
        }
        "Details unavailable; collapse and expand to retry" => {
            "Détails indisponibles ; repliez puis dépliez pour réessayer"
        }
        "The response above is incomplete." => "La réponse ci-dessus est incomplète.",
        "Could not reach the provider, or the connection dropped" => {
            "Le fournisseur est injoignable, ou la connexion a été coupée"
        }
        "The provider is rate limiting requests" => "Le fournisseur limite les requêtes",
        "The provider reported a server error" => "Le fournisseur a signalé une erreur serveur",
        "The provider sent a response Dim could not use" => {
            "Le fournisseur a envoyé une réponse que Dim n'a pas pu utiliser"
        }
        "The provider rejected the credentials" => "Le fournisseur a rejeté les identifiants",
        "The account has no quota left" => "Le compte n'a plus de quota",
        "The provider did not find the model or endpoint" => {
            "Le fournisseur n'a pas trouvé le modèle ni le point de terminaison"
        }
        "The conversation is longer than the model accepts" => {
            "La conversation dépasse la longueur acceptée par le modèle"
        }
        "The provider's content filter blocked the response" => {
            "Le filtre de contenu du fournisseur a bloqué la réponse"
        }
        "The provider rejected the request" => "Le fournisseur a rejeté la requête",
        "The model stopped at its output limit" => "Le modèle s'est arrêté à sa limite de sortie",
        "The model request failed" => "La requête au modèle a échoué",
        "Check the network or proxy." => "Vérifiez le réseau ou le proxy.",
        "Wait and try again." => "Attendez puis réessayez.",
        "Try again later or choose another model." => {
            "Réessayez plus tard ou choisissez un autre modèle."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Réessayez ; si cela se répète, choisissez un autre modèle ou vérifiez le proxy ou la passerelle."
        }
        "Sign in again or check the API key." => "Reconnectez-vous ou vérifiez la clé API.",
        "Check the plan or billing." => "Vérifiez l'offre ou la facturation.",
        "Check the model and the provider's base URL." => {
            "Vérifiez le modèle et l'URL de base du fournisseur."
        }
        "Compact the conversation or start a new session." => {
            "Compactez la conversation ou démarrez une nouvelle session."
        }
        "Rephrase the request." => "Reformulez la requête.",
        "See the details." => "Consultez les détails.",
        "Increase the output token limit and retry." => {
            "Augmentez la limite de tokens de sortie puis réessayez."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "Nouvelle tentative {attempt}/{max} dans {seconds} s : {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s" => {
            "Nouvelle tentative {attempt}/{max} dans {seconds} s"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "Nouvelle tentative {attempt}/{max} dans {seconds} s · {reason} · Esc arrête"
        }
        "kind: {kind}" => "kind : {kind}",
        "status: {status}" => "statut : {status}",
        "provider code: {code}" => "code fournisseur : {code}",
        "request id: {id}" => "id de requête : {id}",
        "tried {count} times over {seconds} s" => "{count} tentatives en {seconds} s",
        "the provider asked to wait {seconds} s" => {
            "le fournisseur a demandé une attente de {seconds} s"
        }
        "cause:" => "cause :",
        "Tried {count} times over {seconds} s." => "{count} tentatives en {seconds} s.",
        "Waiting for the provider" => "En attente du fournisseur",
        "Waiting for the provider · {reason} · Esc stops" => {
            "En attente du fournisseur · {reason} · Esc arrête"
        }
        _ => return None,
    })
}
