//! Spanish model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Conversation state could not be read; retry the read before submitting" => {
            "No se pudo leer el estado de la conversación. Vuelve a cargarla antes de enviar"
        }
        "The centre moved before the command ran; the draft was kept" => {
            "Cambió la sesión o la rama antes del envío. Se conservó el borrador"
        }
        "The selected device restarted during file search" => {
            "El dispositivo seleccionado se reinició durante la búsqueda"
        }
        "Target does not support file completion; type the remote path" => {
            "El destino no admite autocompletar archivos. Escribe la ruta remota"
        }
        "Select a regular file to upload" => "Selecciona un archivo normal para subirlo",
        "Target did not return an image" => "El destino no devolvió ninguna imagen",
        "File search interrupted" => "Búsqueda de archivos interrumpida",
        "Target changed during attachment preparation" => {
            "El destino cambió al preparar los adjuntos"
        }
        "Attachment preparation cancelled" => "Preparación de adjuntos cancelada",
        "The session or line changed during upload; the draft was kept" => {
            "La sesión o la línea cambió durante la subida. Se conservó el borrador"
        }
        "Preparing attachments · Ctrl+C to cancel" => "Preparando adjuntos · Ctrl+C para cancelar",
        "{reason}; draft retained" => "{reason}. Se conservó el borrador",
        "The previous attachment submission is pending; draft retained" => {
            "El envío anterior con adjuntos está pendiente. Se conservó el borrador"
        }
        "Submitting attachments" => "Enviando mensaje con adjuntos",
        "Delivery unconfirmed · Enter on an empty draft to check and retry" => {
            "Envío sin confirmar · Enter en un borrador vacío para comprobar y reintentar"
        }
        "Delivery unconfirmed: {reason}" => "Envío sin confirmar: {reason}",
        "Delivery unconfirmed: {reason}. Enter to check and retry, Ctrl+C to exit" => {
            "Envío sin confirmar: {reason}. Enter para comprobar y reintentar, Ctrl+C para salir"
        }
        "The selected device restarted" => "El dispositivo seleccionado se reinició",
        "The original submission was accepted" => "Se aceptó el envío original",
        "The current model does not accept clipboard images; draft retained" => {
            "El modelo actual no admite imágenes del portapapeles. Se conservó el borrador"
        }
        "Details unavailable; collapse and expand to retry" => {
            "Detalles no disponibles; contrae y expande para reintentar"
        }
        "The response above is incomplete." => "La respuesta anterior está incompleta.",
        "Could not reach the provider, or the connection dropped" => {
            "No se pudo alcanzar al proveedor, o se perdió la conexión"
        }
        "The provider is rate limiting requests" => "El proveedor está limitando las solicitudes",
        "The provider reported a server error" => "El proveedor informó un error del servidor",
        "The provider sent a response Dim could not use" => {
            "El proveedor envió una respuesta que Dim no pudo usar"
        }
        "The provider rejected the credentials" => "El proveedor rechazó las credenciales",
        "The account has no quota left" => "La cuenta no tiene cuota disponible",
        "The provider did not find the model or endpoint" => {
            "El proveedor no encontró el modelo ni el endpoint"
        }
        "The conversation is longer than the model accepts" => {
            "La conversación es más larga de lo que acepta el modelo"
        }
        "The provider's content filter blocked the response" => {
            "El filtro de contenido del proveedor bloqueó la respuesta"
        }
        "The provider rejected the request" => "El proveedor rechazó la solicitud",
        "The model stopped at its output limit" => "El modelo se detuvo en su límite de salida",
        "The model request failed" => "La solicitud al modelo falló",
        "Check the network or proxy." => "Comprueba la red o el proxy.",
        "Wait and try again." => "Espera y vuelve a intentarlo.",
        "Try again later or choose another model." => {
            "Vuelve a intentarlo más tarde o elige otro modelo."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Vuelve a intentarlo; si se repite, elige otro modelo o comprueba el proxy o la pasarela."
        }
        "Sign in again or check the API key." => "Vuelve a iniciar sesión o comprueba la API key.",
        "Check the plan or billing." => "Comprueba el plan o la facturación.",
        "Check the model and the provider's base URL." => {
            "Comprueba el modelo y la base URL del proveedor."
        }
        "Compact the conversation or start a new session." => {
            "Compacta la conversación o inicia una sesión nueva."
        }
        "Rephrase the request." => "Reformula la solicitud.",
        "See the details." => "Consulta los detalles.",
        "Increase the output token limit and retry." => {
            "Aumenta el límite de tokens de salida y vuelve a intentarlo."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "Reintentando {attempt}/{max} en {seconds} s: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s" => "Reintentando {attempt}/{max} en {seconds} s",
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "Reintentando {attempt}/{max} en {seconds} s · {reason} · Esc detiene"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "estado: {status}",
        "provider code: {code}" => "código del proveedor: {code}",
        "request id: {id}" => "id de solicitud: {id}",
        "tried {count} times over {seconds} s" => "{count} intentos en {seconds} s",
        "the provider asked to wait {seconds} s" => "el proveedor pidió esperar {seconds} s",
        "cause:" => "causa:",
        "Tried {count} times over {seconds} s." => "{count} intentos en {seconds} s.",
        "Waiting for the provider" => "Esperando al proveedor",
        "Waiting for the provider · {reason} · Esc stops" => {
            "Esperando al proveedor · {reason} · Esc detiene"
        }
        _ => return None,
    })
}
