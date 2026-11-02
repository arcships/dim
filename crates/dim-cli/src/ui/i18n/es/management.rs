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
