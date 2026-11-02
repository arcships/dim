pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugins",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter activar/desactivar · r recargar · Ctrl+R actualizar · Esc cerrar"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Bloqueado: Dim lo necesita, así que no se puede desactivar."
        }
        "Loading plugins..." => "Cargando plugins...",
        "No plugins." => "No hay plugins.",
        "Working..." => "Procesando...",
        "locked" => "bloqueado",
        "Uses {use}" => "Usa {use}",
        "Candidates: {candidates}" => "Candidatos: {candidates}",
        "Waiting for: {names}" => "Esperando: {names}",
        "Refused {kind}: {error}" => "{kind} rechazado: {error}",
        "Turn off {name}?" => "¿Desactivar {name}?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "Sus herramientas y contenido dejan de estar en los turnos nuevos hasta que lo vuelvas a activar."
        }
        "Turn off" => "Desactivar",
        "Turned on {name}." => "{name} activado.",
        "Turned off {name}." => "{name} desactivado.",
        "Reloaded {name}." => "{name} recargado.",
        "Installed {name} ({id})." => "{name} ({id}) instalado.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name} ({id}) instalado; no está activado."
        }
        "Installing {path}..." => "Instalando {path}...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "Se necesita reiniciar: se carga cuando el App Server se reinicia (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} omitido: su propietario no está en ejecución."
        }
        "Usage: /plugins [install PATH]" => "Uso: /plugins [install RUTA]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "listar plugins, activarlos o desactivarlos, recargar (install RUTA)"
        }
        "Enter open /runtime · o on/off · r reload · Esc close" => {
            "Enter abrir /runtime · o activar/desactivar · r recargar · Esc cerrar"
        }
        "waits for the {runtime} runtime" => "espera el runtime {runtime}",
        _ => return None,
    })
}
