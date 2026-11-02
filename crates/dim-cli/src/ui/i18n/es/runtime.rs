pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "Este daemon no ofrece runtimes de plugin ({version})"
        }
        "Plugin runtimes" => "Runtimes de plugins",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter instalar o aplicar · s origen · a auto · r reintentar · Esc cerrar"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc cerrar · la adquisición continúa si cierras"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter reintentar · s cambiar {name} a {source} · Esc cerrar"
        }
        "Loading plugin runtimes..." => "Cargando runtimes de plugins...",
        "acquiring" => "adquiriendo",
        "starting" => "iniciando",
        "down" => "caído",
        "1 plugin waits for {runtime}: {names}" => "1 plugin espera el runtime {runtime}: {names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} plugins esperan el runtime {runtime}: {names}"
        }
        "show, install or configure the plugin runtimes" => {
            "ver, instalar o configurar los runtimes de plugins"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "Uso: /runtime [install NOMBRE | source NOMBRE download|system | auto NOMBRE on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter instalar o aplicar · s origen · r reintentar · Esc cerrar"
        }
        "auto on" => "auto activado",
        "auto off" => "auto desactivado",
        _ => return None,
    })
}
