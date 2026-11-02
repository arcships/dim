pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Carregando…",
        "No entries." => "Nenhuma entrada.",
        "No matching entry." => "Nenhuma entrada correspondente.",
        "Issues" => "Problemas",
        "sources" => "Fontes",
        "skills" => "Skills",
        "hooks" => "hooks",
        "scheduled jobs" => "tarefas agendadas",
        "command rules" => "Regras de comando",
        "enabled" => "ativado",
        "shadowed by {name}" => "ocultado por {name}",
        "Reloaded." => "Recarregado.",
        "Enabled {name}." => "{name} ativado.",
        "Disabled {name}." => "{name} desativado.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} recarregado: {entries} entradas, {disabled} desativadas."
        }
        "Reloaded {area}: {entries} entries." => "{area} recarregado: {entries} entradas.",
        "Hooks" => "Hooks",
        "invalid" => "inválido",
        "Saved." => "Salvo.",
        "Saved {name}." => "{name} salvo.",
        "No change." => "Nenhuma alteração.",
        "Your text is kept in {path}." => "Seu texto está guardado em {path}.",
        "The user's own document; always in effect." => {
            "O documento do próprio usuário; sempre em vigor."
        }
        "There is no hooks file there." => "Não há arquivo de hooks ali.",
        "Owned by plugin {plugin}." => "Pertence ao plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "O documento do usuário desativa todos os hooks (disableAllHooks)."
        }
        "Enter toggle" => "Enter alternar",
        "Ctrl+E edit" => "Ctrl+E editar",
        "Ctrl+N add" => "Ctrl+N adicionar",
        "Ctrl+R reload" => "Ctrl+R recarregar",
        "Ctrl+T run" => "Ctrl+T executar",
        "Ctrl+X remove" => "Ctrl+X remover",
        "Tab inspect" => "Tab inspecionar",
        "Remove {name}? Enter confirm · Esc cancel" => {
            "Remover {name}? Enter confirmar · Esc cancelar"
        }
        "↑↓ scroll · Esc back" => "↑↓ rolar · Esc voltar",
        "Added." => "Adicionado.",
        "Removed {name}." => "{name} removido.",
        "Ran {name}." => "{name} executado.",
        "The user document changed since it was read; nothing was written." => {
            "O documento do usuário mudou desde que foi lido; nada foi escrito."
        }
        "The job changed since it was read; nothing was written." => {
            "A tarefa mudou desde que foi lida; nada foi escrito."
        }
        "The jobs changed since they were read; nothing was written." => {
            "As tarefas mudaram desde que foram lidas; nada foi escrito."
        }
        "Your next edit opens the current document." => {
            "Sua próxima edição abre o documento atual."
        }
        "Your next add reopens your text." => "Sua próxima adição abre seu texto novamente.",
        "Scheduler" => "Agendador",
        "jobs" => "tarefas",
        "Added {name}." => "{name} adicionado.",
        "Run {name}: {outcome}." => "{name} executado: {outcome}.",
        "Recent runs of {name}" => "Execuções recentes de {name}",
        "No run yet." => "Nenhuma execução ainda.",
        "No next run." => "Nenhuma próxima execução.",
        "Next run {instant}." => "Próxima execução {instant}.",
        "Last run {outcome} at {instant}." => "Última execução {outcome} em {instant}.",
        "every {minutes} minutes" => "a cada {minutes} minutos",
        "at {instant}" => "em {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} em {zone}",
        "The job changed since it was read; nothing was removed." => {
            "A tarefa mudou desde que foi lida; nada foi removido."
        }
        "The Scheduler has no area document." => "O Agendador não tem documento de área.",
        "No scheduled job has that id." => "Nenhuma tarefa agendada tem esse id.",
        "This daemon cannot manage {area}." => "Não é possível gerenciar {area}.",
        "This daemon lists {area} but cannot toggle them." => {
            "O daemon lista {area}, mas não permite alternar."
        }
        "This daemon lists {area} but cannot edit them." => {
            "O daemon lista {area}, mas não permite editar."
        }
        "This daemon lists {area} but cannot reload them." => {
            "O daemon lista {area}, mas não permite recarregar."
        }
        "This daemon lists {area} but cannot add them." => {
            "O daemon lista {area}, mas não permite adicionar."
        }
        "This daemon lists {area} but cannot remove them." => {
            "O daemon lista {area}, mas não permite remover."
        }
        "This daemon lists {area} but cannot run them." => {
            "O daemon lista {area}, mas não permite executar."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "O daemon lista {area}, mas não permite ver os detalhes ou o histórico."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "O documento removeu {field}, que o proprietário não pode limpar; nada foi escrito."
        }
        "The document changes no field; nothing was written." => {
            "O documento não altera nenhum campo; nada foi escrito."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "As configurações de hooks deste espaço de trabalho mudaram desde que foram lidas; nada foi escrito."
        }
        "MCP" => "MCP",
        "MCP servers" => "servidores MCP",
        "Ctrl+T test" => "Ctrl+T testar",
        "sign-in required" => "login necessário",
        "signed in" => "conectado",
        "sign-in expired" => "login expirado",
        "missing scopes: {scopes}" => "escopos ausentes: {scopes}",
        "Executable: {command}" => "Executável: {command}",
        "Stored arguments (hidden): {count}" => "Argumentos armazenados (ocultos): {count}",
        "Working directory: {cwd}" => "Diretório de trabalho: {cwd}",
        "Environment names: {names}" => "Nomes de ambiente: {names}",
        "Endpoint: {endpoint}" => "Endpoint: {endpoint}",
        "Header names: {names}" => "Nomes de cabeçalho: {names}",
        "Bearer token: {state}" => "Token Bearer: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: configurado (ID do cliente {id})",
        "OAuth: configured" => "OAuth: configurado",
        "OAuth: not configured" => "OAuth: não configurado",
        "Metadata URL: {state}" => "URL de metadados: {state}",
        "configured" => "configurado",
        "Owned by a plugin; change it through Plugins." => {
            "Pertence a um plugin; altere-o em Plugins."
        }
        "MCP entry {id}" => "Entrada MCP {id}",
        "Connection {number} · {status}" => "Conexão {number} · {status}",
        "Tools have not been discovered." => "As ferramentas ainda não foram descobertas.",
        "No live session connections." => "Nenhuma conexão de sessão ativa.",
        "Tools of {id}" => "Ferramentas de {id}",
        "The server exposes no tools." => "O servidor não expõe ferramentas.",
        "The add document needs an id." => "O documento de adição precisa de um id.",
        "The add document needs a source of user or project." => {
            "A origem do documento de adição deve ser user ou project."
        }
        "The add document needs a configuration object." => {
            "O documento de adição precisa de um objeto configuration."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} já está em mcp list; remova-o primeiro ou escolha outro id."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} já está em mcp list; escolha outro id ou altere-o pelo plugin."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "As configurações desta entrada mudaram desde a leitura; nada foi escrito."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "A entrada MCP mudou desde a leitura; nada foi escrito."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "As entradas MCP mudaram desde a leitura; nada foi escrito."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "A entrada MCP mudou desde a leitura; nada foi removido."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "Os documentos MCP pertencem a uma entrada; esta gravação não nomeia nenhuma."
        }
        "No MCP server has that id." => "Nenhum servidor MCP tem esse id.",
        "This workspace has no project MCP source." => {
            "Este espaço de trabalho não tem uma origem MCP de projeto."
        }
        "Enabled {id} ({source})." => "{id} ({source}) ativado.",
        "Disabled {id} ({source})." => "{id} ({source}) desativado.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "Executa a entrada .mcp.json deste espaço de trabalho, incluindo edições posteriores, em sessões ativas e novas."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} não está em mcp list; qualquer ativação armazenada neste espaço de trabalho foi retirada."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "Não foi possível ler o catálogo MCP ({error}); a ativação de {id} neste espaço de trabalho foi retirada. Uma entrada de usuário com este ID permanece inalterada."
        }
        "MCP configuration: {error}" => "Configuração MCP: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "O servidor rejeitou as credenciais armazenadas; entre novamente com mcp login."
        }
        "The server requires additional scopes: {scopes}." => {
            "O servidor exige escopos adicionais: {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "O servidor exige autorização; execute mcp login ID."
        }
        "The probe ended: {status}." => "A sondagem terminou: {status}.",

        "Command rules" => "Regras de comando",
        "user" => "usuário",
        "Matches only this command." => "Corresponde apenas a este comando.",
        "Matches commands whose first words are these." => {
            "Corresponde a comandos cujas primeiras palavras são estas."
        }
        "Allows a simple command that would otherwise ask." => {
            "Permite um comando simples que de outra forma perguntaria."
        }
        "Denies the command under every preset." => "Nega o comando em qualquer predefinição.",
        "Applies in every Session, in every workspace." => {
            "Aplica-se em toda Sessão e em todo workspace."
        }
        "Rule added: {rule}." => "Regra adicionada: {rule}.",
        "Rule already present: {rule}." => "Regra já presente: {rule}.",
        "Rule removed: {rule}." => "Regra removida: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Essa regra de comando não está mais listada; nada foi removido."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "As regras de comando mudaram desde que foram lidas; nada foi removido."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Uma regra de comando é escrita como DECISION MATCH COMMAND, por exemplo deny prefix git push."
        }
        "Command: {command}" => "Comando: {command}",
        "Use the line prompt or dimcode permissions." => {
            "Use o prompt de linha ou dimcode permissions."
        }
        _ => return None,
    })
}
