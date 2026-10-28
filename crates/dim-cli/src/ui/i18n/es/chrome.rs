pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter salida / interrumpir miembro · x parar comando · Esc cerrar"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter salida / interrumpir miembro · x parar comando · Ctrl+X parar todo · Esc cerrar"
        }
        "Loading branch history... Stop-all unavailable" => {
            "Cargando historial de ramas… Parar todo no disponible"
        }
        "Branch history failed; reopen to retry" => {
            "Falló historial de ramas; reabre para reintentar"
        }
        "Branch history is incomplete; stop-all unavailable" => {
            "Historial de ramas incompleto; parar todo no disponible"
        }
        "Running" => "En curso",
        "Starting" => "Iniciando",
        "Delegation result" => "Resultado de delegación",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter detalles · x parar selección · Ctrl+X parar todo · Esc cerrar"
        }
        "x interrupt member · Esc back" => "x interrumpir miembro · Esc volver",
        "read-only" => "solo lectura",
        "Member Session" => "Sesión de miembro",
        "Loading conversation..." => "Cargando conversación…",
        "No conversation yet" => "Sin conversación aún",
        "Observing conversation" => "Observando conversación",
        "Conversation unavailable" => "Conversación no disponible",
        "Session closed" => "Sesión cerrada",
        "Member conversation" => "Conversación de miembro",
        "x stop command · Esc back" => "x parar comando · Esc volver",
        "Esc back" => "Esc volver",
        "Tail preview - last 2,000 lines / 50 KiB" => "Final - últimas 2000 líneas / 50 KiB",
        "Loading output..." => "Cargando salida…",
        "No output yet" => "Sin salida aún",
        "Live output unavailable" => "Salida en vivo no disponible",
        "Live output unavailable in this profile" => "Salida en vivo no disponible en este perfil",
        "Output read failed" => "Error al leer salida",
        "Completed" => "Completado",
        "Failed" => "Fallido",
        "Cancelled" => "Cancelado",
        "workflow" => "workflow",
        "Stopping" => "Parando",
        "Stopped" => "Detenido",
        "Interrupted" => "Interrumpido",
        "last observed" => "última observación",
        "Loading Workflow runs... Stop-all unavailable" => {
            "Cargando ejecuciones de Workflow… Parar todo no disponible"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: última observación; error al actualizar, reabre para reintentar"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Ejecuciones de Workflow no disponibles; reabre y reintenta"
        }
        "The selected Workflow run is no longer available" => {
            "Ejecución de Workflow elegida ya no disponible"
        }
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Inventario de Workflow incompleto; parar todo no disponible"
        }
        "Workflow stop unavailable" => "No se puede parar Workflow",
        "Run no longer available; last observed record" => {
            "Ejecución ya no disponible; último registro observado"
        }
        "x stop workflow · Esc back" => "x parar workflow · Esc volver",
        "Run" => "Ejecución",
        "Source" => "Origen",
        "Stages" => "Etapas",
        "result" => "resultado",
        "Log" => "Registro",
        "Report" => "Informe",
        "waiting" => "esperando",
        "launching" => "lanzando",
        "completed" => "completado",
        "failed" => "fallido",
        "stopped" => "detenido",
        "skipped" => "omitido",
        "lost" => "perdido",
        "none" => "ninguno",
        "pending" => "pendiente",
        "delivered" => "entregado",
        "undeliverable" => "no entregable",
        "Agent is starting; no member Session to stop yet" => {
            "Agente iniciando; aún sin Sesión de miembro que parar"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "El líder controla los miembros del proveedor; no se paran aquí"
        }
        "Provider members have no conversation to open here" => {
            "Miembros del proveedor sin conversación que abrir aquí"
        }
        "That delegation is no longer in this transcript" => {
            "Esa delegación ya no está en la transcripción"
        }
        "Member model" => "Modelo de miembro",
        "No model observed" => "Sin modelo observado",
        "Observing member..." => "Observando miembro…",
        "{model} (observation unavailable)" => "{model} (observación no disponible)",
        "{model} (Session closed)" => "{model} (Sesión cerrada)",
        "{model} (last observed)" => "{model} (última observación)",
        "Agent: {name}" => "Agente: {name}",
        "Provider member: {name}" => "Miembro del proveedor: {name}",
        "Task: {task}" => "Tarea: {task}",
        "Activity: {activity}" => "Actividad: {activity}",
        "Activity: not observed on this attachment" => "Actividad: no observada en esta conexión",
        "Result: {result}" => "Resultado: {result}",
        "Controlled by the leader" => "Controlado por el líder",
        "› Open member conversation" => "› Abrir conversación del miembro",
        "Copied to clipboard" => "Copiado al portapapeles",
        "Opened link" => "Enlace abierto",
        "Link: {url}" => "Enlace: {url}",
        "Not available" => "No disponible",
        "files changed  {count}" => "archivos cambiados  {count}",
        "active session" => "sesión activa",
        "new-session draft" => "borrador de nueva sesión",
        "Tool approvals: {title} ({target})" => "Aprobaciones: {title} ({target})",
        "Disconnected · draft kept" => "Desconectado · borrador guardado",
        "The App Server closed this interface: {reason}" => {
            "El App Server cerró esta interfaz: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Desconectado; borrador guardado. Sal y reanuda esta Sesión."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Desconectado; borrador guardado. Sal y ejecuta: {command}"
        }
        "Disconnected; command outcome unknown" => {
            "Desconectado; resultado del comando desconocido"
        }
        "Message queued" => "Mensaje en cola",
        "Run a shell command · Esc to exit" => "Ejecutar comando shell · Esc salir",
        "! shell mode" => "! modo shell",
        "!! shell mode · not sent to the model" => "!! modo shell · no se envía al modelo",
        "Leave shell mode to quote a message" => "Sal del modo shell para citar",
        "A shell command is already running" => "Ya hay un comando shell en curso",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "Ejecutando… · Ctrl+C interrumpir · {seconds}s"
        }
        "exit code {code}" => "código de salida {code}",
        "killed by {signal}" => "terminado por {signal}",
        "interrupted" => "interrumpido",
        "done" => "hecho",
        "failed: {reason}" => "error: {reason}",
        "Shell output was not recorded: {reason}" => "Salida del shell no registrada: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "Leyendo portapapeles…",
        "Clipboard: {reason}" => "Portapapeles: {reason}",
        "Wait for clipboard insertion before sending" => {
            "Espera a pegar el portapapeles antes de enviar"
        }
        "A submission accepts at most 20 images" => "Máximo 20 imágenes por envío",
        "[Image #{index}]" => "[Imagen #{index}]",
        "The current model does not accept images; remove {label} to send" => {
            "El modelo actual no admite imágenes; quita {label} para enviar"
        }
        "Files" => "Archivos",
        "Searching…" => "Buscando…",
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ elegir · enter/tab insertar · esc cerrar"
        }
        "not sent to the model" => "no se envía al modelo",
        "… {count} more lines" => "… {count} líneas más",
        "… {count} earlier lines" => "… {count} líneas anteriores",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Interrupción solicitada; Ctrl+C de nuevo para salir"
        }
        "Press Ctrl+C again to exit" => "Ctrl+C otra vez para salir",
        "User interrupted the run." => "Interrumpido por el usuario.",
        "Could not save the language: {error}" => "No se pudo guardar el idioma: {error}",
        "Language set to {locale}" => "Idioma: {locale}",
        "Language set to system default" => "Idioma: predeterminado del sistema",
        "Connected {provider}" => "{provider} conectado",
        "Opened {url}" => "Abierto {url}",
        "Tool details not saved: {error}" => "Detalles no guardados: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} requiere sesión: envía un mensaje primero"
        }
        "Nothing is running" => "Nada en ejecución",
        "No queued message" => "Sin mensajes en cola",
        "Only in fullscreen (/tui)" => "Solo en pantalla completa (/tui)",
        "/{name} is not yet available" => "/{name} aún no disponible",
        "new session" => "nueva sesión",
        "resumed {id} · {count} messages" => "reanudada {id} · {count} mensajes",
        " · earlier history: /history" => " · historial anterior: /history",
        "Error: {message}" => "Error: {message}",
        "working…  {elapsed}" => "trabajando…  {elapsed}",
        "thinking…  {elapsed}" => "pensando…  {elapsed}",
        "Ask anything..." => "Pregunta algo…",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "Historial anterior · {id} · {count} mensajes · /history: página anterior"
        }
        "No active session" => "Sin sesión activa",
        "No earlier messages." => "Sin mensajes anteriores.",
        "answer required" => "respuesta requerida",
        "permission required" => "permiso requerido",
        "esc interrupt" => "esc interrumpir",
        "↑ edit · ctrl+s steer now · /queue" => "↑ editar · ctrl+s steer ahora · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "+{count} más · ↑ editar · ctrl+s steer ahora · /queue"
        }
        "Queue ({count})" => "Cola ({count})",
        "Thinking…" => "Pensando…",
        "Thought" => "Pensado",
        "Thought for {duration}" => "Pensó {duration}",
        "Welcome to DIM {version}!" => "¡Bienvenido a DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => {
            "Escribe / para comandos · Ctrl+O paleta"
        }
        "Workspace" => "Área de trabajo",
        "Git branch" => "Rama de Git",
        "Bye!" => "¡Adiós!",
        "DIM Web is ready" => "DIM Web listo",
        "Opened in your browser" => "Abierto en el navegador",
        "Open the sign-in link below" => "Abre el enlace de acceso de abajo",
        "Web" => "Web",
        "Sign-in link:" => "Enlace de acceso:",
        "Resume" => "Reanudar",
        "Press Ctrl+C to stop the Web workspace." => {
            "Pulsa Ctrl+C para parar el área de trabajo Web."
        }
        "To resume this session:" => "Para reanudar esta sesión:",

        // composer/commands.rs: categories
        "session" => "sesión",
        "run" => "ejecución",
        "view" => "vista",
        "model" => "modelo",
        "app" => "app",
        "skill" => "skill",
        // composer/commands.rs: descriptions
        "start a new session" => "iniciar nueva sesión",
        "show or set the reasoning effort" => "ver o ajustar esfuerzo de razonamiento",
        "show or set the priority service tier (on|off)" => {
            "ver o ajustar nivel de servicio prioritario (on|off)"
        }
        "list agents or switch this session's agent" => {
            "listar agentes o cambiar el de esta sesión"
        }
        "list MCP servers, sign in or out (login|logout ID)" => {
            "listar servidores MCP, iniciar/cerrar sesión (login|logout ID)"
        }
        "resume a previous session" => "reanudar sesión anterior",
        "open this session in the browser" => "abrir esta sesión en el navegador",
        "load the previous page of messages" => "cargar página anterior de mensajes",
        "show the context window usage" => "ver uso de la ventana de contexto",
        "interrupt the run" => "interrumpir ejecución",
        "steer with the next queued message" => "steer con el próximo mensaje en cola",
        "scroll to the top" => "ir al principio",
        "scroll to the bottom" => "ir al final",
        "pick an earlier message to edit or quote" => "elegir mensaje anterior para editar o citar",
        "collapse or expand tool details and thoughts" => {
            "contraer/expandir detalles de herramientas y pensamientos"
        }
        "switch between fullscreen and scrollback" => "alternar pantalla completa y desplazamiento",
        "switch theme" => "cambiar tema",
        "switch language" => "cambiar idioma",
        "select a model" => "elegir modelo",
        "connect a provider" => "conectar proveedor",
        "sign in to DimAgent" => "iniciar sesión en DimAgent",
        "DimAgent account · usage · sign out" => "cuenta DimAgent · uso · cerrar sesión",
        "configure tool approvals" => "configurar aprobaciones",
        "open settings" => "abrir ajustes",
        "suspend to the shell" => "suspender al shell",
        "exit" => "salir",

        // composer/palette.rs
        "Enter select · Ctrl+X clear saved · Esc clear/close" => {
            "Enter elegir · Ctrl+X borrar guardado · Esc borrar/cerrar"
        }
        "Enter select · Esc clear/close" => "Enter elegir · Esc borrar/cerrar",
        "esc close" => "esc cerrar",
        "Suggestions" => "Sugerencias",
        "Skills" => "Skills",
        "Commands" => "Comandos",
        "Command Palette" => "Paleta de comandos",
        "No matches" => "Sin coincidencias",
        " (unavailable)" => " (no disponible)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Texto: {preview} ({count} caracteres)]",
        "[... {count} lines]" => "[… {count} líneas]",
        "pasted text" => "texto pegado",

        // list_panel.rs

        // settings_panel.rs
        "on" => "sí",
        "off" => "no",
        "Interface" => "Interfaz",
        "Theme" => "Tema",
        "Language" => "Idioma",
        "Startup UI form" => "Interfaz al iniciar",
        "fullscreen" => "pantalla completa",
        "scrollback" => "desplazamiento",
        "Tool details" => "Detalles de herramientas",
        "expanded" => "expandido",
        "collapsed" => "contraído",
        "Mouse (fullscreen)" => "Ratón (pantalla completa)",
        "Permissions" => "Permisos",
        "Tool approvals" => "Aprobaciones",
        "Models" => "Modelos",
        "Model" => "Modelo",
        "not set" => "sin definir",
        "Settings" => "Ajustes",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ mover · Enter elegir · ←→/Space cambiar · Esc cerrar"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ vista previa · Enter guardar · Esc restaurar"
        }
        "System default ({locale})" => "Del sistema ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ vista previa · Enter aplicar · Esc cancelar"
        }
        "System" => "Sistema",
        "Could not save the theme: {error}" => "No se pudo guardar el tema: {error}",

        // sessions_panel.rs
        "Sessions" => "Sesiones",
        "Enter open · Ctrl+R refresh · Esc close" => {
            "Enter abrir · Ctrl+R actualizar · Esc cerrar"
        }
        "Loading sessions..." => "Cargando sesiones…",
        "No saved sessions yet." => "Sin sesiones guardadas aún.",
        "No matching sessions." => "Sin sesiones coincidentes.",
        "now" => "ahora",
        "{count}m ago" => "hace {count} min",
        "{count}h ago" => "hace {count} h",
        "{count}d ago" => "hace {count} d",

        // models_panel.rs
        "Select a model" => "Elige un modelo",
        "Loading models..." => "Cargando modelos…",
        "No models available. Connect a provider first." => {
            "Sin modelos. Conecta un proveedor primero."
        }
        "No matching models." => "Sin modelos coincidentes.",
        "current" => "actual",
        "Edit models" => "Editar modelos",
        "Editing models is not available yet" => "Editar modelos aún no disponible",
        "Enter select · Esc back" => "Enter elegir · Esc volver",
        "Subagent models" => "Modelos de subagentes",
        "Subagent models: {policy}" => "Modelos de subagentes: {policy}",
        "Fixed subagent model" => "Modelo fijo de subagente",
        "Agent choice (default)" => "Elección del agente (predeterminado)",
        "Agent choice" => "Elección del agente",
        "Inherit requesting branch" => "Heredar de la rama solicitante",
        "Fixed model" => "Modelo fijo",
        "Applies to new members, including named Agents." => {
            "Para miembros nuevos, incluidos Agentes con nombre."
        }
        "Existing members keep their models." => "Los miembros actuales mantienen su modelo.",
        "unknown" => "desconocido",
        "Subagent model policy saved" => "Política de modelos de subagentes guardada",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Política de modelos de subagentes cambiada en otro sitio; vuelve a elegir para releerla"
        }
        "Subagent model policy: {error}" => "Política de modelos de subagentes: {error}",
        "drawing diagram… {count} lines" => "dibujando diagrama… {count} líneas",
        // notify.rs
        "Task complete" => "Tarea completada",
        "Permission needed: {tool}" => "Permiso necesario: {tool}",
        "Question waiting for your answer" => "Pregunta pendiente de respuesta",
        "Notify when agent needs you" => "Avisar si el agente te necesita",
        // keys.rs
        "show the key bindings" => "ver atajos de teclado",
        "Application keys" => "Atajos de la app",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter enviar · Shift+Enter o Ctrl+J nueva línea · ↑↓ historial"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O o Ctrl+P comandos · Esc interrumpir o borrar · Ctrl+S steer en cola · ↑ editar en cola"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C borrar, dos veces salir · Ctrl+D salir con borrador vacío"
        }
        "Fullscreen" => "Pantalla completa",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown desplazar · Home/End inicio o final · Tab elegir mensaje, e editar, q citar"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "rueda desplazar · arrastrar selecciona y copia · clic abre enlaces y expande grupos de herramientas"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "sin ratón (/settings): el terminal selecciona y copia"
        }
        "Terminal-owned history" => "Historial del terminal",
        "scroll: {value}" => "desplazar: {value}",
        "search: {value}" => "buscar: {value}",
        "copy: {value}" => "copiar: {value}",
        "terminal search" => "búsqueda del terminal",
        "terminal scrollback" => "desplazamiento del terminal",
        "terminal copy" => "copia del terminal",
        "drag to select" => "seleccionar arrastrando",
        "drag or {shortcut}" => "arrastrar o {shortcut}",
        _ => return None,
    })
}
