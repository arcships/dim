//! German model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Conversation state could not be read; retry the read before submitting" => {
            "Der Gesprächsstatus konnte nicht gelesen werden. Bitte vor dem Senden erneut laden"
        }
        "The centre moved before the command ran; the draft was kept" => {
            "Die Sitzung oder der Gesprächszweig wurde gewechselt. Der Entwurf wurde behalten"
        }
        "The selected device restarted during file search" => {
            "Das ausgewählte Gerät wurde während der Dateisuche neu gestartet"
        }
        "Target does not support file completion; type the remote path" => {
            "Das Ziel unterstützt keine Dateivervollständigung. Gib den entfernten Pfad ein"
        }
        "Select a regular file to upload" => "Wähle eine reguläre Datei zum Hochladen",
        "Target did not return an image" => "Das Ziel hat kein Bild zurückgegeben",
        "File search interrupted" => "Dateisuche unterbrochen",
        "Target changed during attachment preparation" => {
            "Das Ziel hat sich bei der Vorbereitung der Anhänge geändert"
        }
        "Attachment preparation cancelled" => "Vorbereitung der Anhänge abgebrochen",
        "The session or line changed during upload; the draft was kept" => {
            "Sitzung oder Gesprächslinie beim Hochladen gewechselt. Entwurf beibehalten"
        }
        "Preparing attachments · Ctrl+C to cancel" => {
            "Anhänge werden vorbereitet · Ctrl+C zum Abbrechen"
        }
        "{reason}; draft retained" => "{reason}. Entwurf beibehalten",
        "The previous attachment submission is pending; draft retained" => {
            "Die vorherige Nachricht mit Anhängen ist noch offen. Entwurf beibehalten"
        }
        "Submitting attachments" => "Nachricht mit Anhängen wird gesendet",
        "Delivery unconfirmed · Enter on an empty draft to check and retry" => {
            "Zustellung unbestätigt · Enter im leeren Entwurf zum Prüfen und Wiederholen"
        }
        "Delivery unconfirmed: {reason}" => "Zustellung unbestätigt: {reason}",
        "Delivery unconfirmed: {reason}. Enter to check and retry, Ctrl+C to exit" => {
            "Zustellung unbestätigt: {reason}. Enter zum Prüfen und Wiederholen, Ctrl+C zum Beenden"
        }
        "The selected device restarted" => "Das ausgewählte Gerät wurde neu gestartet",
        "The original submission was accepted" => "Die ursprüngliche Nachricht wurde angenommen",
        "The current model does not accept clipboard images; draft retained" => {
            "Das aktuelle Modell akzeptiert keine Zwischenablagebilder. Entwurf beibehalten"
        }
        "Details unavailable; collapse and expand to retry" => {
            "Details nicht verfügbar; zum Wiederholen schließen und öffnen"
        }
        "The response above is incomplete." => "Die obige Antwort ist unvollständig.",
        "Could not reach the provider, or the connection dropped" => {
            "Der Anbieter ist nicht erreichbar, oder die Verbindung wurde getrennt"
        }
        "The provider is rate limiting requests" => "Der Anbieter begrenzt Anfragen",
        "The provider reported a server error" => "Der Anbieter meldet einen Serverfehler",
        "The provider sent a response Dim could not use" => {
            "Der Anbieter hat eine Antwort gesendet, die Dim nicht verwenden konnte"
        }
        "The provider rejected the credentials" => "Der Anbieter hat die Anmeldedaten abgelehnt",
        "The account has no quota left" => "Das Konto hat kein Kontingent mehr",
        "The provider did not find the model or endpoint" => {
            "Der Anbieter hat das Modell oder den Endpunkt nicht gefunden"
        }
        "The conversation is longer than the model accepts" => {
            "Die Unterhaltung ist länger, als das Modell akzeptiert"
        }
        "The provider's content filter blocked the response" => {
            "Der Inhaltsfilter des Anbieters hat die Antwort blockiert"
        }
        "The provider rejected the request" => "Der Anbieter hat die Anfrage abgelehnt",
        "The model stopped at its output limit" => "Das Modell hat sein Ausgabelimit erreicht",
        "The model request failed" => "Die Modellanfrage ist fehlgeschlagen",
        "Check the network or proxy." => "Netzwerk oder Proxy prüfen.",
        "Wait and try again." => "Warten und erneut versuchen.",
        "Try again later or choose another model." => {
            "Später erneut versuchen oder ein anderes Modell wählen."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Erneut versuchen; wiederholt es sich, ein anderes Modell wählen oder Proxy bzw. Gateway prüfen."
        }
        "Sign in again or check the API key." => "Erneut anmelden oder den API key prüfen.",
        "Check the plan or billing." => "Tarif oder Abrechnung prüfen.",
        "Check the model and the provider's base URL." => {
            "Modell und base URL des Anbieters prüfen."
        }
        "Compact the conversation or start a new session." => {
            "Unterhaltung komprimieren oder eine neue Sitzung starten."
        }
        "Rephrase the request." => "Die Anfrage umformulieren.",
        "See the details." => "Details ansehen.",
        "Increase the output token limit and retry." => {
            "Das Ausgabe-token-Limit erhöhen und erneut versuchen."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "Neuer Versuch {attempt}/{max} in {seconds} s: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s" => "Neuer Versuch {attempt}/{max} in {seconds} s",
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "Neuer Versuch {attempt}/{max} in {seconds} s · {reason} · Esc beendet"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "Status: {status}",
        "provider code: {code}" => "Anbieter-code: {code}",
        "request id: {id}" => "Anfrage-id: {id}",
        "tried {count} times over {seconds} s" => "{count} Versuche über {seconds} s",
        "the provider asked to wait {seconds} s" => "Der Anbieter bittet um {seconds} s Wartezeit",
        "cause:" => "Ursache:",
        "Tried {count} times over {seconds} s." => "{count} Versuche über {seconds} s.",
        "Waiting for the provider" => "Warte auf den Anbieter",
        "Waiting for the provider · {reason} · Esc stops" => {
            "Warte auf den Anbieter · {reason} · Esc beendet"
        }
        _ => return None,
    })
}
