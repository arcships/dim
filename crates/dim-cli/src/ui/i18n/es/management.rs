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
        "models" => "modelos",
        "workflow definitions" => "definiciones de flujo de trabajo",
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
        "Remove {name}?" => "¿Eliminar {name}?",
        "↑↓ scroll · Esc back" => "↑↓ desplazar · Esc volver",
        "Added." => "Añadido.",
        "Removed {name}." => "{name} eliminado.",
        "unavailable" => "no disponible",
        "Reset {name}?" => "¿Restablecer {name}?",
        "Enter confirm" => "Enter confirmar",
        "Esc cancel" => "Esc cancelar",
        "Reset {name} to its catalog entry." => "{name} restablecido a su entrada del catálogo.",
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
        "A scheduled job's run takes no input document." => {
            "La ejecución de una tarea programada no toma ningún documento de entrada."
        }
        "An MCP server's test takes no input document." => {
            "La prueba de un servidor MCP no toma un documento de entrada."
        }
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
        "Models" => "Modelos",
        "catalog" => "catálogo",
        "overrides" => "anulaciones",
        "added" => "añadidos",
        "Models · {name}" => "Modelos · {name}",
        "undeclared" => "sin declarar",
        "Context window {window} · max output {output}" => {
            "Ventana de contexto {window} · salida máx. {output}"
        }
        "Endpoint {endpoint}" => "Punto de conexión {endpoint}",
        "Endpoint {endpoint} · override {value}" => {
            "Punto de conexión {endpoint} · anulación {value}"
        }
        "The provider reports it unavailable." => "El proveedor lo informa como no disponible.",
        "Limits undeclared; edit the model to declare them." => {
            "Límites sin declarar; edita el modelo para declararlos."
        }
        "Sign in to the account to use its models." => {
            "Inicia sesión en la cuenta para usar sus modelos."
        }
        "The signed-in account changed; reload to list its models." => {
            "La cuenta iniciada cambió; recarga para listar sus modelos."
        }
        "The provider has no catalog yet." => "Este proveedor aún no tiene catálogo.",
        "The provider listed no models." => "El proveedor no listó ningún modelo.",
        "Listed {count} models." => "Se listaron {count} modelos.",
        "Refreshed the models." => "Modelos actualizados.",
        "Could not list models: {error}. Showing the stored list." => {
            "No se pudieron listar los modelos: {error}. Se muestra la lista guardada."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "La cuenta iniciada cambió; la recarga borró preferencias, anulaciones y desactivaciones."
        }
        "The models changed since they were read; nothing was removed." => {
            "Los modelos cambiaron desde su lectura; no se eliminó nada."
        }
        "The models changed since they were read; nothing was written." => {
            "Los modelos cambiaron desde su lectura; no se escribió nada."
        }
        "No provider has that id." => "Ningún proveedor tiene ese id.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} no está disponible ({reason}); su activación se mantiene como está."
        }
        "No model has that id." => "Ningún modelo tiene ese id.",
        "This model cannot be removed." => "Este modelo no se puede eliminar.",
        "A Provider's models have no area document." => {
            "Los modelos de un proveedor no tienen documento de área."
        }
        "Models have no run operation." => "Los modelos no tienen operación de ejecución.",
        "The document is not an object; nothing was written." => {
            "El documento no es un objeto; no se escribió nada."
        }
        "The document changes no editable field; nothing was written." => {
            "El documento no cambia ningún campo editable; no se escribió nada."
        }
        "The model document names no model." => "El documento del modelo no nombra ningún modelo.",
        "The provider has no model catalog to write." => {
            "Este proveedor no tiene catálogo de modelos que escribir."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Origen {source}",
        "Status {status}" => "Estado {status}",
        "Tools {value}" => "Herramientas {value}",
        "Vision {value}" => "Visión {value}",
        "Reasoning {value}" => "Razonamiento {value}",
        "Modalities {list}" => "Modalidades {list}",
        "Description {text}" => "Descripción {text}",
        "Release date {date}" => "Fecha de lanzamiento {date}",
        "Pricing {value}" => "Precio {value}",
        "Rate limit {value}" => "Límite de tasa {value}",
        "Matched provider {id}" => "Proveedor coincidente {id}",
        "Matched model {id}" => "Modelo coincidente {id}",
        "Match strategy {strategy}" => "Estrategia de coincidencia {strategy}",
        "Capability overrides {fields}" => "Anulaciones de capacidades {fields}",
        "yes" => "sí",
        "no" => "no",
        "not supported" => "no compatible",
        "supported" => "compatible",

        "Workflow" => "Flujo de trabajo",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} guardado. Permanece desactivado hasta que lo actives."
        }
        "The definitions changed since they were read; nothing was written." => {
            "Las definiciones cambiaron desde que se leyeron; no se escribió nada."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "El nombre de una definición es su clave; añádela para guardarla con otro nombre."
        }
        "No project definition is named {name}." => {
            "Ninguna definición de proyecto se llama {name}."
        }
        "No workflow definition is named {name}." => {
            "Ninguna definición de flujo de trabajo se llama {name}."
        }
        "Saved in the user workflow source; always enabled." => {
            "Guardado en la fuente de flujos de trabajo del usuario; siempre activado."
        }
        "Starts only after you enable it." => "Solo se inicia después de que lo actives.",
        "Defined by package {package}." => "Definido por el paquete {package}.",
        "{name} — {description} (required)" => "{name} — {description} (obligatorio)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — obligatorio",
        "{name} — default: {default}" => "{name} — predeterminado: {default}",
        "and {count} more" => "y {count} más",
        "Recent runs" => "Ejecuciones recientes",
        "A workflow definition is saved as an object with a source and a document." => {
            "Una definición de flujo de trabajo se guarda como un objeto con una fuente y un documento."
        }
        "A definition is saved to the user source or the project source." => {
            "Una definición se guarda en la fuente del usuario o en la del proyecto."
        }
        "Started {name}: {run_id}." => "{name} iniciado: {run_id}.",
        "Parameters are a JSON object of strings." => {
            "Los parámetros son un objeto JSON de cadenas."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "La lista alcanzó su límite de tamaño; las definiciones posteriores no se muestran."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Ninguna definición de workflow llamada {name} está en la lista, que alcanzó su límite de tamaño."
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
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} está en más de una fuente MCP: {sources}. Indica una con --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "La acción no nombra ninguna fuente MCP; una fila lleva la fuente que informa su propietario."
        }
        "No MCP server {id} is in source {source}." => {
            "Ningún servidor MCP {id} en la fuente {source}."
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
        "Tool call timeout: {millis} ms" => {
            "Tiempo de espera de la llamada a la herramienta: {millis} ms"
        }
        "Variables: {names}" => "Variables: {names}",
        "Unset variables: {names}" => "Variables sin definir: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "La fuente MCP cambió desde la vista previa; no se importó ninguna entrada, y una entrada que habría reemplazado puede estar ahora desactivada. Vuelve a previsualizar."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "Las entradas de proyecto se escribieron y sus valores retenidos no; reemplázalas otra vez para reintentar."
        }
        "Replaced {id}." => "{id} reemplazado.",
        "Added {id}." => "{id} añadido.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Entrega a la sesión {session}, rama {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Inicia una sesión nueva en {cwd}.",
        "Disabled: {code} at {instant}." => "Desactivado: {code} el {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "La definición cambió desde que se leyó; no se escribió nada."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Columnas: ID, cliente, ámbito, ruta, estado, entradas o error."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Ningún valor de un almacén se lee ni se imprime; --from ID previsualiza uno dentro del daemon."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Columnas: ID, acción, ID de destino.\nUna entrada renombrada se escribe con su id de destino; la entrada de la fuente con el id antiguo queda como estaba."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Las entradas importadas y reemplazadas empiezan desactivadas: una entrada de usuario se escribe con enabled: false, y una entrada de proyecto no tiene registro de activación hasta que la actives localmente."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Este daemon no puede listar los almacenes MCP de otros clientes."
        }
        "This daemon cannot import MCP servers." => "Este daemon no puede importar servidores MCP.",
        "The document holds no mcpServers entries." => {
            "El documento no contiene entradas mcpServers."
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Columnas: ID, estado, transporte, campos cambiados, campos no compatibles, error.\nUn campo cambiado indica qué cambiaría una escritura y el tipo de valor que contiene; no se imprime ningún valor.\nUn campo no compatible queda sin usar o se descarta, como se muestra tras el campo."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Una entrada de proyecto se escribe como su esqueleto público: el archivo del espacio de trabajo recibe el comando, el cwd declarado, el origen de la URL y los campos OAuth públicos. Sus argumentos, valores de entorno y cabeceras, token bearer, una URL con userinfo, ruta, consulta o fragmento, y los miembros desconocidos van a tu registro mcp-secrets de ámbito de usuario, nunca al archivo del espacio de trabajo."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} tiene {count} nombres de campo más de los que muestran las listas de arriba."
        }
        "Top-level members the import does not carry: none." => {
            "Miembros de nivel superior que la importación no traslada: ninguno."
        }
        "Top-level members the import does not carry: {names}." => {
            "Miembros de nivel superior que la importación no traslada: {names}."
        }
        "Applying the selection at this revision." => "Aplicando la selección en esta revisión.",
        "An unchanged entry was already in the source and was not written." => {
            "Una entrada sin cambios ya estaba en la fuente y no se escribió."
        }
        _ => return None,
    })
}
