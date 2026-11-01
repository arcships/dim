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
        "Lines" => "Líneas",
        "Enter show the line · x end it · Esc close" => {
            "Enter ver la línea · x terminarla · Esc cerrar"
        }
        "This session has no lines." => "Esta sesión no tiene líneas.",
        "No matching lines." => "Ninguna línea coincide.",
        "the session itself" => "la propia sesión",
        "working" => "trabajando",
        "ended itself" => "se terminó",
        "ended by an operation" => "terminada por una operación",
        "ended with {id}" => "terminada con {id}",
        "creation failed" => "creación fallida",
        "shown" => "mostrada",
        "line" => "línea",
        "line {id}" => "línea {id}",
        "jump to a message of the shown line" => "saltar a un mensaje de la línea mostrada",
        "Timeline" => "Cronología",
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter saltar · e editar · q citar · b rama · f bifurcar · Esc cerrar"
        }
        "No messages yet." => "Aún no hay mensajes.",
        "Already on screen above; /tui jumps to it" => "Ya está arriba; /tui salta hasta él",
        "{count} earlier messages · Enter or click to load" => {
            "{count} mensajes anteriores · Enter o clic para cargar"
        }
        "follow, talk to and end this session's lines" => {
            "seguir, hablar y terminar las líneas de esta sesión"
        }
        "End line" => "Terminar línea",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main es la propia sesión; una línea termina, main no"
        }
        "Line has ended:" => "La línea ha terminado:",
        "Esc close" => "Esc cerrar",
        "/{name} needs {argument}" => "/{name} necesita {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} solo admite texto; quita {label} para enviar"
        }
        "e edit · b branch · f fork" => "e editar · b rama · f bifurcar",
        "e edit" => "e editar",
        "Fork the session before “{message}”? (y/N)" => {
            "¿Bifurcar la sesión antes de «{message}»? (y/N)"
        }
        "Fork the session after the last turn? (y/N)" => {
            "¿Bifurcar la sesión tras el último turno? (y/N)"
        }
        "⑂ new line from {id} · Esc cancel" => "⑂ nueva línea desde {id} · Esc cancelar",
        "A fork copies main; b opens a line here" => {
            "Una bifurcación copia main; b abre una línea aquí"
        }
        "A line opens from text; images and files were not included" => {
            "Una línea se abre con texto; no se incluyeron imágenes ni archivos"
        }
        "No finished turn to cut at yet" => "Aún no hay un turno terminado donde cortar",
        "forked {id} · {count} messages" => "bifurcada {id} · {count} mensajes",
        "after message {count}" => "después del mensaje {count}",
        "open a line from the end of the shown line" => {
            "abrir una línea desde el final de la línea mostrada"
        }
        "fork the session at its last finished turn" => {
            "bifurcar la sesión en su último turno terminado"
        }
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
        "Not started" => "Sin iniciar",
        "Reason: {reason}" => "Motivo: {reason}",
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
        "Reading image…" => "Leyendo imagen…",
        "Wait for the image insertion before sending" => {
            "Espera a que se inserte la imagen antes de enviar"
        }
        "The image could not be read" => "No se pudo leer la imagen",
        "An image is still being inserted; paste again when it is in" => {
            "Se está insertando una imagen; pega de nuevo cuando termine"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "Se está insertando el portapapeles; pega de nuevo cuando termine"
        }
        "Clipboard: {reason}" => "Portapapeles: {reason}",
        "Wait for clipboard insertion before sending" => {
            "Espera a pegar el portapapeles antes de enviar"
        }
        "A submission accepts at most 20 images" => "Máximo 20 imágenes por envío",
        "first frame" => "solo el primer fotograma",
        "re-encoded" => "recodificada",
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
        "new session" => "nueva sesión",
        "resumed {id} · {count} messages" => "reanudada {id} · {count} mensajes",
        " · earlier history: /timeline" => " · historial anterior: /timeline",
        "Error: {message}" => "Error: {message}",
        "working…  {elapsed}" => "trabajando…  {elapsed}",
        "thinking…  {elapsed}" => "pensando…  {elapsed}",
        "Ask anything..." => "Pregunta algo…",
        "Earlier history · {id} · {count} messages" => {
            "Historial anterior · {id} · {count} mensajes"
        }
        "No active session" => "Sin sesión activa",
        "answer required" => "respuesta requerida",
        "permission required" => "permiso requerido",
        "esc interrupt" => "esc interrumpir",
        "shift+← edit last · ctrl+s steer now" => {
            "shift+← editar la última · ctrl+s steer ahora"
        }
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count} más · shift+← editar la última · ctrl+s steer ahora"
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
        "show the context window usage" => "ver uso de la ventana de contexto",
        "interrupt the run" => "interrumpir ejecución",
        "steer with the next queued message" => "steer con el próximo mensaje en cola",
        "scroll to the top" => "ir al principio",
        "scroll to the bottom" => "ir al final",
        "pick an earlier message to edit or quote" => "elegir mensaje anterior para editar o citar",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "plegar/desplegar detalles de herramientas, reflexiones y resúmenes de compresión"
        }
        "switch between fullscreen and scrollback" => "alternar pantalla completa y desplazamiento",
        "switch theme" => "cambiar tema",
        "switch language" => "cambiar idioma",
        "select a model" => "elegir modelo",
        "connect a provider" => "conectar proveedor",
        "sign in to DimAgent" => "iniciar sesión en DimAgent",
        "DimAgent account · usage · sign out" => "cuenta DimAgent · uso · cerrar sesión",
        "configure tool approvals" => "configurar aprobaciones",
        "choose ask or full access" => "elegir preguntar o acceso total",
        "open settings" => "abrir ajustes",
        "suspend to the shell" => "suspender al shell",
        "exit" => "salir",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter elegir · ←/→ esfuerzo · Ctrl+X borrar guardado · Esc borrar/cerrar"
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
        "Execution" => "Ejecución",
        "Models" => "Modelos",
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
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 1 modelo no disponible · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: {count} modelos no disponibles · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 1 modelo necesita una ventana · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: {count} modelos necesitan una ventana · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: sin modelos · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: sin modelos · añade un modelo con provider.models.create"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable} no disponibles y {disabled} desactivados · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: catálogo ilegible · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "Un modelo cuyos límites no están declarados no se puede seleccionar; decláralos con dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "Decláralos con dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "Sin catálogo en caché. Inicia sesión y ejecuta models PROVIDER --refresh."
        }
        "Saved model preferences cleared" => "Preferencias de modelo guardadas borradas",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 1 modelo desactivado · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: {count} modelos desactivados · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "Sin modelos. Ejecuta dimcode models {id} --refresh para verlos."
        }
        "No models. The provider listed none." => "Sin modelos. El proveedor no enumeró ninguno.",
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
        "Show ACP sessions" => "Mostrar sesiones ACP",
        // keys.rs
        "show the key bindings" => "ver atajos de teclado",
        "Application keys" => "Atajos de la app",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter enviar o encolar · borrador vacío: steer la primera en cola · Shift+Enter o Ctrl+J nueva línea · ↑↓ historial"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O o Ctrl+P comandos · Esc interrumpir, los mensajes en cola vuelven · Ctrl+S steer el borrador o la primera en cola · Shift+← editar la última en cola"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C borrar, dos veces salir · Ctrl+D salir con borrador vacío"
        }
        "Fullscreen" => "Pantalla completa",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown desplazar · Home/End inicio o final (con el borrador vacío) · Tab elegir mensaje, e editar, q citar"
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
        "Compacting context…" => "Comprimiendo el contexto…",
        "Context compacted" => "Contexto comprimido",
        "Context compaction skipped" => "Compresión del contexto omitida",
        "Context compaction failed" => "La compresión del contexto falló",
        "Context compaction cancelled" => "Compresión del contexto cancelada",
        "near the compaction threshold" => "cerca del umbral de compresión",
        "This daemon does not support context compaction" => {
            "Este daemon no admite la compresión del contexto"
        }
        "compact the context of the shown line" => "comprimir el contexto de la línea mostrada",
        "Kept verbatim" => "Conservado tal cual",
        "Tool effects" => "Acciones de las herramientas",
        "insufficient space: {parts}" => "espacio insuficiente: {parts}",
        "system prompt" => "indicación del sistema",
        "tools" => "herramientas",
        "appended messages" => "mensajes añadidos",
        "keyed baseline" => "línea base con claves",
        "pinned inputs" => "entradas fijadas",
        "digest" => "compendio",
        "summary" => "resumen",
        _ => return None,
    })
}
