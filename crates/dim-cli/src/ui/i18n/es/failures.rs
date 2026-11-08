//! Spanish model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
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
        "Waiting for the provider · {reason} · Esc stops" => {
            "Esperando al proveedor · {reason} · Esc detiene"
        }
        _ => return None,
    })
}
