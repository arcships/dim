//! French model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
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
        "Waiting for the provider · {reason} · Esc stops" => {
            "En attente du fournisseur · {reason} · Esc arrête"
        }
        _ => return None,
    })
}
