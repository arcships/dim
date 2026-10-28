pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "El agente sigue activo.",
        "AskUser is waiting for an answer." => "AskUser espera una respuesta.",
        "A permission request is waiting." => "Solicitud de permiso pendiente.",
        "Background work may still be active." => "Puede haber trabajo en segundo plano.",
        "Exit dimcode?" => "¿Salir de dimcode?",
        "Exit" => "Salir",
        "Stay" => "Quedarse",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y salir · n quedarse · ←→ mover · enter confirmar"
        }
        "Restart dimcode?" => "¿Reiniciar dimcode?",
        "Restart" => "Reiniciar",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y reiniciar · n quedarse · ←→ mover · enter confirmar"
        }
        // upgrade_dialog.rs
        "Update available" => "Actualización disponible",
        "Version {latest} is available (you have {current})." => {
            "Versión {latest} disponible (tienes {current})."
        }
        "Will run: {command}" => "Se ejecutará: {command}",
        "To update, run: {command}" => "Para actualizar: {command}",
        "Update now" => "Actualizar ahora",
        "Skip this version" => "Omitir esta versión",
        "Skipped {version}." => "{version} omitida.",
        "Later" => "Más tarde",
        "Close" => "Cerrar",
        "Restart now" => "Reiniciar ahora",
        "Updating to {version}..." => "Actualizando a {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "Actualizado a {version}. Reinicia DimCode para usarla."
        }
        "Update failed: {error}" => "Error al actualizar: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ mover · Enter confirmar · Esc cerrar",
        // prompts.rs: permission request
        "Permission required" => "Permiso requerido",
        "Allow once" => "Permitir una vez",
        "Allow always" => "Permitir siempre",
        "Reject" => "Rechazar",
        "Approve for me" => "Aprobar por mí",
        "Full access" => "Acceso total",
        "Run {tool}" => "Ejecutar {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "Aún no se puede cambiar el modo de aprobación aquí"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y permitir · n rechazar · ←→/Tab mover · Enter confirmar"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter enviar · Tab editar · Esc declinar",
        "Space toggle · Tab next · Esc decline" => "Space marcar · Tab siguiente · Esc declinar",
        "1-9 select · Tab next · Esc decline" => "1-9 elegir · Tab siguiente · Esc declinar",
        "Question" => "Pregunta",
        "Question on branch {branch}" => "Pregunta en la rama {branch}",
        "Sending answer…" => "Enviando respuesta…",
        "Question not ready; press Enter to send again" => "Pregunta no lista; Enter para reenviar",
        // prompts.rs: AskUser
        "Confirm" => "Confirmar",
        "Enter save · Esc back" => "Enter guardar · Esc volver",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter enviar · 1-9 editar · Tab/←→ volver · Esc declinar"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ mover · Space marcar · Enter siguiente · Tab/←→ cambiar · Esc declinar"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ mover · 1-9 elegir · Enter siguiente · Tab/←→ cambiar · Esc declinar"
        }
        "AskUserQuestion received an invalid payload." => {
            "AskUserQuestion recibió datos no válidos."
        }
        "Missing answer for Q{index}" => "Falta la respuesta de P{index}",
        "Type your own answer" => "Escribe tu respuesta",
        "Review your answers and press Enter to submit." => {
            "Revisa tus respuestas y pulsa Enter para enviar."
        }
        "Not answered" => "Sin responder",
        // approvals_panel.rs
        "Tool Approvals" => "Aprobaciones de herramientas",
        "full access" => "acceso total",
        "All tools run automatically, bypass permissions" => {
            "Todas las herramientas se ejecutan solas, sin permisos"
        }
        "approve for me" => "aprobar por mí",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "El agente revisa comandos y acciones de herramientas. Los comandos inseguros piden confirmación; las acciones rutinarias pasan solas: seguro, inteligente y cómodo."
        }
        "Default permissions" => "Permisos predeterminados",
        "default" => "predeterminado",
        "Read tools run automatically, write tools require confirmation" => {
            "Lectura automática; la escritura pide confirmación"
        }
        "current" => "actual",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab mover · Enter aplicar · Esc volver",
        "Confirm Full Access" => "Confirmar acceso total",
        "Cancel" => "Cancelar",
        "Tab switch  Enter confirm  Esc cancel" => "Tab cambiar  Enter confirmar  Esc cancelar",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "El acceso total reduce confirmaciones y puede ejecutar directamente acciones sensibles, cambios de archivos o comandos externos. Úsalo solo si confías en la tarea."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "ejecutó",
        "running" => "ejecutando",
        "read" => "leyó",
        "reading" => "leyendo",
        "explored" => "exploró",
        "exploring" => "explorando",
        "edited" => "editó",
        "editing" => "editando",
        "changed" => "cambió",
        "changing" => "cambiando",
        "checked on" => "revisó",
        "checking on" => "revisando",
        "Explored" => "Exploró",
        "Exploring" => "Explorando",
        "Edited" => "Editó",
        "Editing" => "Editando",
        "Changed" => "Cambió",
        "Changing" => "Cambiando",
        // transcript/tool_group.rs: counts
        "1 file" => "1 archivo",
        "{count} files" => "{count} archivos",
        "1 web page" => "1 página web",
        "{count} web pages" => "{count} páginas web",
        "1 skill" => "1 skill",
        "{count} skills" => "{count} skills",
        "1 search" => "1 búsqueda",
        "{count} searches" => "{count} búsquedas",
        "1 command" => "1 comando",
        "{count} commands" => "{count} comandos",
        "1 task" => "1 tarea",
        "{count} tasks" => "{count} tareas",
        "1 tool" => "1 herramienta",
        "{count} tools" => "{count} herramientas",
        "{files} and {pages}" => "{files} y {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} fallidos",
        // transcript/tool_group.rs: call rows
        "Read" => "Leer",
        "List" => "Listar",
        "Grep" => "Grep",
        "Search the web" => "Buscar en la web",
        "Read skill" => "Leer skill",
        "Search git history" => "Buscar en historial git",
        "Edit" => "Editar",
        "Write" => "Escribir",
        "Create" => "Crear",
        " (detached)" => " (en segundo plano)",
        "Checked on background task" => "Revisó tarea en segundo plano",
        "Waiting…" => "Esperando…",
        "… +{count} lines" => "… +{count} líneas",
        _ => return None,
    })
}
