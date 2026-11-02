pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "Este daemon não oferece runtimes de plugin ({version})"
        }
        "Plugin runtimes" => "Runtimes de plugins",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter instalar ou aplicar · s origem · a automático · r tentar de novo · Esc fechar"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc fechar · a aquisição continua se você fechar"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter tentar de novo · s trocar {name} para {source} · Esc fechar"
        }
        "Loading plugin runtimes..." => "Carregando runtimes de plugins...",
        "acquiring" => "adquirindo",
        "starting" => "iniciando",
        "down" => "fora do ar",
        "1 plugin waits for {runtime}: {names}" => "1 plugin aguarda o runtime {runtime}: {names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} plugins aguardam o runtime {runtime}: {names}"
        }
        "show, install or configure the plugin runtimes" => {
            "ver, instalar ou configurar os runtimes de plugins"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "Uso: /runtime [install NOME | source NOME download|system | auto NOME on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter instalar ou aplicar · s origem · r tentar de novo · Esc fechar"
        }
        "auto on" => "automático ligado",
        "auto off" => "automático desligado",
        _ => return None,
    })
}
