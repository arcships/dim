pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugins",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter ligar/desligar · r recarregar · Ctrl+R atualizar · Esc fechar"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Bloqueado: o Dim precisa dele, por isso não pode ser desligado."
        }
        "Loading plugins..." => "A carregar plugins...",
        "No plugins." => "Nenhum plugin.",
        "Working..." => "A processar...",
        "locked" => "bloqueado",
        "Uses {use}" => "Usa {use}",
        "Candidates: {candidates}" => "Candidatos: {candidates}",
        "Waiting for: {names}" => "A aguardar: {names}",
        "Refused {kind}: {error}" => "{kind} recusado: {error}",
        "Turn off {name}?" => "Desligar {name}?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "As ferramentas e o conteúdo dele deixam de estar nos novos turnos até o voltar a ligar."
        }
        "Turn off" => "Desligar",
        "Turned on {name}." => "{name} ligado.",
        "Turned off {name}." => "{name} desligado.",
        "Reloaded {name}." => "{name} recarregado.",
        "Installed {name} ({id})." => "{name} ({id}) instalado.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name} ({id}) instalado; não está ligado."
        }
        "Installing {path}..." => "A instalar {path}...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "É preciso reiniciar: carrega depois de o App Server reiniciar (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} ignorado: o responsável não está em execução."
        }
        "Usage: /plugins [install PATH]" => "Utilização: /plugins [install CAMINHO]",
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
