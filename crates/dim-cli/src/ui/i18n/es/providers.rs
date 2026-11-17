pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Actualizar lista de modelos",
        "Model list refreshed" => "Lista de modelos actualizada",
        "Model list refresh failed: {reason}" => {
            "No se pudo actualizar la lista de modelos: {reason}"
        }
        "Refreshing model list… · esc close" => "Actualizando la lista de modelos… · Esc cierra",
        // providers_panel.rs
        "Providers" => "Proveedores",
        "Enter open · Esc clear/close" => "Enter abrir · Esc borrar/cerrar",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab mover · Enter activar · Esc volver"
        }
        "Loading providers..." => "Cargando proveedores…",
        "No matching providers." => "Sin proveedores coincidentes.",
        "Custom providers are not available yet" => "Proveedores personalizados aún no disponibles",
        "Testing connection..." => "Probando conexión…",
        "Connect {providerId}" => "Conectar {providerId}",
        "(default)" => "(predeterminado)",
        "(required)" => "(obligatorio)",
        "Test + apply" => "Probar y aplicar",
        "Testing sends the stored credential to this URL." => {
            "La prueba envía la credencial guardada a esta URL."
        }
        "Disconnect" => "Desconectar",
        "Enable provider" => "Activar proveedor",
        "Disable provider" => "Desactivar proveedor",
        "Error: {error}" => "Error: {error}",
        "disabled" => "desactivado",
        "current" => "actual",
        "Sign in" => "Iniciar sesión",
        "ready" => "listo",
        "error" => "error",
        "not connected" => "no conectado",
        "Add custom provider" => "Añadir proveedor personalizado",
        // oauth_view.rs
        "Checking account…" => "Comprobando cuenta…",
        "Not signed in" => "Sin sesión",
        "Signed in as {account}" => "Conectado como {account}",
        "Open models" => "Abrir modelos",
        "View plan" => "Ver plan",
        "Sign out" => "Cerrar sesión",
        "Checking usage…" => "Comprobando uso…",
        "Usage: {reason}" => "Uso: {reason}",
        "Credits" => "Créditos",
        "unlimited" => "ilimitado",
        "{hours}h window" => "ventana de {hours} h",
        "/month" => "/mes",
        "/year" => "/año",
        "expires {date}" => "vence el {date}",
        "resets {date}" => "se renueva el {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ mover · enter activar · ctrl+r actualizar · esc cerrar"
        }
        "Login with DimAgent" => "Acceder con DimAgent",
        "Enterprise login" => "Acceso empresarial",
        "Device login" => "Acceso con dispositivo",
        "Email" => "Correo",
        "Continue" => "Continuar",
        "Browser login" => "Acceso por navegador",
        "Restart device login" => "Reiniciar acceso con dispositivo",
        "Select to copy · Enter opens link" => "Seleccionar para copiar · Enter abre enlace",
        "Link shortened · Enter opens link" => "Enlace abreviado · Enter lo abre",
        "Callback URL" => "URL de callback",
        "Submit callback" => "Enviar callback",
        "The browser could not be opened. Open the link manually, then return here." => {
            "No se pudo abrir el navegador. Abre el enlace a mano y vuelve aquí."
        }
        "Waiting for authorization" => "Esperando autorización",
        "{name} device login" => "Acceso con dispositivo de {name}",
        "Open this URL: {url}" => "Abre esta URL: {url}",
        "Enter code:    {code}" => "Código:    {code}",
        "Waiting after server slow-down" => "Esperando: servidor ralentizado",
        "OAuth login cancelled" => "Acceso OAuth cancelado",
        "Enter your work email" => "Tu correo de trabajo",
        "Invalid OAuth state - possible CSRF attempt" => "Estado OAuth no válido - posible CSRF",
        "Preparing login... · esc cancel" => "Preparando acceso… · esc cancelar",
        "Completing login... · esc cancel" => "Completando acceso… · esc cancelar",
        "enter restart · esc back" => "enter reiniciar · esc volver",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "escribe valor · ↑↓/tab mover · enter enviar · esc cancelar"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ mover · enter activar · esc volver",
        "↑↓ move · enter activate · esc close" => "↑↓ mover · enter activar · esc cerrar",
        "Model id prefix" => "Prefijo de id de modelo",
        "(none)" => "(ninguno)",
        "identified: {model} at {provider}" => "identificado: {model} en {provider}",
        "inferred from several rows: {model} at {provider}" => {
            "inferido de varias filas: {model} en {provider}"
        }
        "ambiguous: {candidates}" => "ambiguo: {candidates}",
        "unidentified" => "sin identificar",
        "Identified {rule}" => "Identificado {rule}",
        "from the Provider's own list" => "de la lista propia del proveedor",
        "from the whole qualified id" => "del id cualificado completo",
        "by its tokens" => "por sus tokens",
        "by its canonical id or display name" => "por su id canónico o nombre visible",
        "from the custom-provider list" => "de la lista de custom-provider",
        "by merging the rows that name it" => "combinando las filas que lo nombran",
        "ambiguous" => "ambiguo",
        "{field} {value} supplied by {provider}/{model}" => {
            "{field} {value} aportado por {provider}/{model}"
        }
        "(paste here)" => "(pega aquí)",
        _ => return None,
    })
}
