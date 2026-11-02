pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Cargando…",
        "No entries." => "No hay entradas.",
        "No matching entry." => "Ninguna entrada coincide.",
        "Issues" => "Problemas",
        "sources" => "Fuentes",
        "skills" => "Skills",
        "hooks" => "hooks",
        "scheduled jobs" => "tareas programadas",
        "command rules" => "Reglas de comandos",
        "enabled" => "activado",
        "shadowed by {name}" => "ocultado por {name}",
        "Reloaded." => "Recargado.",
        "Enabled {name}." => "{name} activado.",
        "Disabled {name}." => "{name} desactivado.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} recargado: {entries} entradas, {disabled} desactivadas."
        }
        "Reloaded {area}: {entries} entries." => "{area} recargado: {entries} entradas.",
        "Hooks" => "Hooks",
        "invalid" => "no válido",
        "Saved." => "Guardado.",
        "Saved {name}." => "{name} guardado.",
        "No change." => "Sin cambios.",
        "Your text is kept in {path}." => "Tu texto se conserva en {path}.",
        "The user's own document; always in effect." => {
            "El documento propio del usuario; siempre en efecto."
        }
        "There is no hooks file there." => "No hay ningún archivo de hooks allí.",
        "Owned by plugin {plugin}." => "Propiedad del plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "El documento del usuario desactiva todos los hooks (disableAllHooks)."
        }
        "Enter toggle" => "Enter alternar",
        "Ctrl+E edit" => "Ctrl+E editar",
        "Ctrl+N add" => "Ctrl+N añadir",
        "Ctrl+R reload" => "Ctrl+R recargar",
        "Ctrl+T run" => "Ctrl+T ejecutar",
        "Ctrl+X remove" => "Ctrl+X eliminar",
        "Tab inspect" => "Tab inspeccionar",
        "Remove {name}? Enter confirm · Esc cancel" => {
            "¿Eliminar {name}? Enter confirmar · Esc cancelar"
        }
        "↑↓ scroll · Esc back" => "↑↓ desplazar · Esc volver",
        "Added." => "Añadido.",
        "Removed {name}." => "{name} eliminado.",
        "Ran {name}." => "{name} ejecutado.",
        "The user document changed since it was read; nothing was written." => {
            "El documento del usuario cambió desde que se leyó; no se escribió nada."
        }
        "The job changed since it was read; nothing was written." => {
            "La tarea cambió desde que se leyó; no se escribió nada."
        }
        "The jobs changed since they were read; nothing was written." => {
            "Las tareas cambiaron desde que se leyeron; no se escribió nada."
        }
        "Your next edit opens the current document." => {
            "Tu próxima edición abre el documento actual."
        }
        "Your next add reopens your text." => {
            "Tu próxima acción de añadir vuelve a abrir tu texto."
        }
        "Scheduler" => "Programador",
        "jobs" => "tareas",
        "Added {name}." => "{name} añadido.",
        "Run {name}: {outcome}." => "{name} ejecutado: {outcome}.",
        "Recent runs of {name}" => "Ejecuciones recientes de {name}",
        "No run yet." => "Todavía no hay ejecuciones.",
        "No next run." => "No hay próxima ejecución.",
        "Next run {instant}." => "Próxima ejecución {instant}.",
        "Last run {outcome} at {instant}." => "Última ejecución {outcome} a las {instant}.",
        "every {minutes} minutes" => "cada {minutes} minutos",
        "at {instant}" => "a las {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} en {zone}",
        "The job changed since it was read; nothing was removed." => {
            "La tarea cambió desde que se leyó; no se eliminó nada."
        }
        "The Scheduler has no area document." => "El Programador no tiene documento de área.",
        "No scheduled job has that id." => "Ninguna tarea programada tiene ese id.",
        "This daemon cannot manage {area}." => "No se puede gestionar {area}.",
        "This daemon lists {area} but cannot toggle them." => {
            "El daemon lista {area}, pero no permite alternar."
        }
        "This daemon lists {area} but cannot edit them." => {
            "El daemon lista {area}, pero no permite editar."
        }
        "This daemon lists {area} but cannot reload them." => {
            "El daemon lista {area}, pero no permite recargar."
        }
        "This daemon lists {area} but cannot add them." => {
            "El daemon lista {area}, pero no permite añadir."
        }
        "This daemon lists {area} but cannot remove them." => {
            "El daemon lista {area}, pero no permite eliminar."
        }
        "This daemon lists {area} but cannot run them." => {
            "El daemon lista {area}, pero no permite ejecutar."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "El daemon lista {area}, pero no permite ver sus detalles o su historial."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "El documento elimina {field}, que el propietario no puede borrar; no se escribió nada."
        }
        "The document changes no field; nothing was written." => {
            "El documento no cambia ningún campo; no se escribió nada."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "La configuración de hooks de este espacio de trabajo cambió desde que se leyó; no se escribió nada."
        }
        "MCP" => "MCP",
        "MCP servers" => "servidores MCP",
        "Ctrl+T test" => "Ctrl+T probar",
        "sign-in required" => "inicio de sesión requerido",
        "signed in" => "sesión iniciada",
        "sign-in expired" => "sesión caducada",
        "missing scopes: {scopes}" => "ámbitos faltantes: {scopes}",
        "Executable: {command}" => "Ejecutable: {command}",
        "Stored arguments (hidden): {count}" => "Argumentos guardados (ocultos): {count}",
        "Working directory: {cwd}" => "Directorio de trabajo: {cwd}",
        "Environment names: {names}" => "Nombres de entorno: {names}",
        "Endpoint: {endpoint}" => "Extremo: {endpoint}",
        "Header names: {names}" => "Nombres de encabezado: {names}",
        "Bearer token: {state}" => "Token Bearer: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: configurado (ID de cliente {id})",
        "OAuth: configured" => "OAuth: configurado",
        "OAuth: not configured" => "OAuth: no configurado",
        "Metadata URL: {state}" => "URL de metadatos: {state}",
        "configured" => "configurado",
        "Owned by a plugin; change it through Plugins." => {
            "Propiedad de un plugin; cámbielo desde Plugins."
        }
        "MCP entry {id}" => "Entrada MCP {id}",
        "Connection {number} · {status}" => "Conexión {number} · {status}",
        "Tools have not been discovered." => "Los recursos aún no se han descubierto.",
        "No live session connections." => "No hay conexiones de sesión activas.",
        "Tools of {id}" => "Recursos de {id}",
        "The server exposes no tools." => "El servidor no expone recursos.",
        "The add document needs an id." => "El documento de alta necesita un id.",
        "The add document needs a source of user or project." => {
            "La fuente del documento de alta debe ser user o project."
        }
        "The add document needs a configuration object." => {
            "El documento de alta necesita un objeto configuration."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} ya está en mcp list; elimínelo primero o elija otro id."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} ya está en mcp list; elija otro id o cámbielo a través de su plugin."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "La configuración de esta entrada cambió desde que se leyó; no se escribió nada."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "La entrada MCP cambió desde que se leyó; no se escribió nada."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "Las entradas MCP cambiaron desde que se leyeron; no se escribió nada."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "La entrada MCP cambió desde que se leyó; no se eliminó nada."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "Los documentos MCP pertenecen a una entrada; este guardado no nombra ninguna."
        }
        "No MCP server has that id." => "Ningún servidor MCP tiene ese id.",
        "This workspace has no project MCP source." => {
            "Este espacio de trabajo no tiene una fuente MCP de proyecto."
        }
        "Enabled {id} ({source})." => "{id} ({source}) activado.",
        "Disabled {id} ({source})." => "{id} ({source}) desactivado.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "Ejecuta la entrada .mcp.json de este espacio de trabajo, incluidas sus ediciones posteriores, en sesiones activas y nuevas."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} no está en mcp list; se retiró cualquier activación guardada en este espacio de trabajo."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "No se pudo leer el catálogo MCP ({error}); se retiró la activación de {id} en este espacio de trabajo. Una entrada de usuario con este ID no cambia."
        }
        "MCP configuration: {error}" => "Configuración MCP: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "El servidor rechazó las credenciales guardadas; vuelve a iniciar sesión con mcp login."
        }
        "The server requires additional scopes: {scopes}." => {
            "El servidor requiere ámbitos adicionales: {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "El servidor requiere autorización; ejecuta mcp login ID."
        }
        "The probe ended: {status}." => "La sonda terminó: {status}.",

        "Command rules" => "Reglas de comandos",
        "user" => "usuario",
        "Matches only this command." => "Solo coincide con este comando.",
        "Matches commands whose first words are these." => {
            "Coincide con los comandos cuyas primeras palabras son estas."
        }
        "Allows a simple command that would otherwise ask." => {
            "Permite un comando simple que de otro modo preguntaría."
        }
        "Denies the command under every preset." => "Deniega el comando con cualquier preajuste.",
        "Applies in every Session, in every workspace." => {
            "Se aplica en cada sesión y en cada espacio de trabajo."
        }
        "Rule added: {rule}." => "Regla añadida: {rule}.",
        "Rule already present: {rule}." => "La regla ya existe: {rule}.",
        "Rule removed: {rule}." => "Regla eliminada: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Esa regla de comando ya no aparece en la lista; no se eliminó nada."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "Las reglas de comandos cambiaron desde que se leyeron; no se eliminó nada."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Una regla de comando se escribe DECISION MATCH COMMAND, por ejemplo deny prefix git push."
        }
        "Command: {command}" => "Comando: {command}",
        "Use the line prompt or dimcode permissions." => {
            "Usa el indicador de línea o dimcode permissions."
        }
        _ => return None,
    })
}
