pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugins",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter ligar/desligar · r recarregar · Ctrl+R atualizar · Esc fechar"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Bloqueado: o Dim precisa dele, então não pode ser desligado."
        }
        "Loading plugins..." => "Carregando plugins...",
        "No plugins." => "Nenhum plugin.",
        "Working..." => "Processando...",
        "locked" => "bloqueado",
        "Uses {use}" => "Usa {use}",
        "Candidates: {candidates}" => "Candidatos: {candidates}",
        "Waiting for: {names}" => "Aguardando: {names}",
        "Refused {kind}: {error}" => "{kind} recusado: {error}",
        "Turn off {name}?" => "Desligar {name}?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "As ferramentas e o conteúdo dele saem dos novos turnos até você ligá-lo de novo."
        }
        "Turn off" => "Desligar",
        "Turned on {name}." => "{name} ligado.",
        "Turned off {name}." => "{name} desligado.",
        "Reloaded {name}." => "{name} recarregado.",
        "Installed {name} ({id})." => "{name} ({id}) instalado.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name} ({id}) instalado; não está ligado."
        }
        "Installing {path}..." => "Instalando {path}...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "Reinício necessário: carrega depois que o App Server reiniciar (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} ignorado: o responsável não está em execução."
        }
        "Usage: /plugins [install PATH]" => "Uso: /plugins [install CAMINHO]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "listar plugins, ligar ou desligar, recarregar (install CAMINHO)"
        }
        "Enter open /runtime · o on/off · r reload · Esc close" => {
            "Enter abrir /runtime · o ligar/desligar · r recarregar · Esc fechar"
        }
        "waits for the {runtime} runtime" => "aguarda o runtime {runtime}",
        _ => return None,
    })
}
