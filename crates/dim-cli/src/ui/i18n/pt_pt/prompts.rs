pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "Agente ainda em execução.",
        "AskUser is waiting for an answer." => "AskUser à espera de resposta.",
        "A permission request is waiting." => "Pedido de permissão pendente.",
        "Background work may still be active." => "Pode haver trabalho em segundo plano ativo.",
        "Exit dimcode?" => "Sair do dimcode?",
        "Exit" => "Sair",
        "Stay" => "Ficar",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y sair · n ficar · ←→ mover · enter confirmar"
        }
        "Restart dimcode?" => "Reiniciar dimcode?",
        "Restart" => "Reiniciar",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y reiniciar · n ficar · ←→ mover · enter confirmar"
        }
        // upgrade_dialog.rs
        "Update available" => "Atualização disponível",
        "Version {latest} is available (you have {current})." => {
            "Versão {latest} disponível (tem a {current})."
        }
        "Will run: {command}" => "Vai executar: {command}",
        "To update, run: {command}" => "Para atualizar, execute: {command}",
        "Update now" => "Atualizar agora",
        "Skip this version" => "Ignorar esta versão",
        "Skipped {version}." => "{version} ignorada.",
        "Later" => "Mais tarde",
        "Close" => "Fechar",
        "Restart now" => "Reiniciar agora",
        "Updating to {version}..." => "A atualizar para {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "Atualizado para {version}. Reinicie o DimCode para a usar."
        }
        "Update failed: {error}" => "Falha na atualização: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ mover · Enter confirmar · Esc fechar",
        // prompts.rs: permission request
        "Permission required" => "Permissão necessária",
        "Allow once" => "Permitir uma vez",
        "Allow always" => "Permitir sempre",
        "Allow for this session" => "Permitir para esta sessão",
        "Reject" => "Rejeitar",
        "Approve for me" => "Aprovar por mim",
        "Full access" => "Acesso total",
        "Run {tool}" => "Executar {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "Ainda não é possível mudar o modo de aprovações aqui"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y permitir · n rejeitar · ←→/Tab mover · Enter confirmar"
        }
        "Enter submit · Tab edit · / commands · Esc decline" => {
            "Enter enviar · Tab editar · / comandos · Esc recusar"
        }
        "Space toggle · Tab next · / commands · Esc decline" => {
            "Space alternar · Tab seguinte · / comandos · Esc recusar"
        }
        "1-9 select · Tab next · / commands · Esc decline" => {
            "1-9 selecionar · Tab seguinte · / comandos · Esc recusar"
        }
        "Question" => "Pergunta",
        "Question on branch {branch}" => "Pergunta no ramo {branch}",
        "Permission on branch {branch}" => "Permissão no ramo {branch}",
        "Sending answer…" => "A enviar resposta…",
        "Question not ready; press Enter to send again" => {
            "Pergunta não pronta; prima Enter para reenviar"
        }
        // prompts.rs: AskUser
        "Confirm" => "Confirmar",
        "Enter save · Esc back" => "Enter guardar · Esc voltar",
        "Enter submit · 1-9 edit · Tab/←→ back · / commands · Esc decline" => {
            "Enter enviar · 1-9 editar · Tab/←→ voltar · / comandos · Esc recusar"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · / commands · Esc decline" => {
            "↑↓ mover · Space alternar · Enter seguinte · Tab/←→ mudar · / comandos · Esc recusar"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · / commands · Esc decline" => {
            "↑↓ mover · 1-9 selecionar · Enter seguinte · Tab/←→ mudar · / comandos · Esc recusar"
        }
        "AskUserQuestion received an invalid payload." => {
            "AskUserQuestion recebeu dados inválidos."
        }
        "Missing answer for Q{index}" => "Falta a resposta à P{index}",
        "Type your own answer" => "Escreva a sua resposta",
        "Review your answers and press Enter to submit." => {
            "Reveja as respostas e prima Enter para enviar."
        }
        "Not answered" => "Sem resposta",
        // approvals_panel.rs
        "Tool Approvals" => "Aprovações de ferramentas",
        "Approval" => "Aprovação",
        "Ask" => "Perguntar",
        "Ask before commands, network, outside writes and unclassified tools" => {
            "Perguntar antes de comandos, rede, escritas fora da área de trabalho e ferramentas não classificadas"
        }
        "full access" => "acesso total",
        "All tools run automatically, bypass permissions" => {
            "Todas as ferramentas correm sem pedir permissão"
        }
        "approve for me" => "aprovar por mim",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "O agente revê comandos e ações de ferramentas. Comandos inseguros pedem confirmação; ações de rotina passam automaticamente — seguro, inteligente e prático."
        }
        "Default permissions" => "Permissões predefinidas",
        "default" => "predefinido",
        "Read tools run automatically, write tools require confirmation" => {
            "Leitura automática; ferramentas de escrita pedem confirmação"
        }
        "current" => "atual",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab mover · Enter aplicar · Esc voltar",
        "Confirm Full Access" => "Confirmar acesso total",
        "Cancel" => "Cancelar",
        "Tab switch  Enter confirm  Esc cancel" => "Tab mudar  Enter confirmar  Esc cancelar",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "O acesso total reduz pedidos de confirmação e pode executar diretamente ações sensíveis, alterações a ficheiros ou comandos externos. Use-o só se confiar na tarefa atual."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "executou",
        "running" => "a executar",
        "read" => "leu",
        "reading" => "a ler",
        "explored" => "explorou",
        "exploring" => "a explorar",
        "edited" => "editou",
        "editing" => "a editar",
        "changed" => "alterou",
        "changing" => "a alterar",
        "checked on" => "verificou",
        "checking on" => "a verificar",
        "Explored" => "Explorou",
        "Exploring" => "A explorar",
        "Edited" => "Editou",
        "Editing" => "A editar",
        "Changed" => "Alterou",
        "Changing" => "A alterar",
        // transcript/tool_group.rs: counts
        "1 file" => "1 ficheiro",
        "{count} files" => "{count} ficheiros",
        "… {count} more" => "… mais {count}",
        "1 web page" => "1 página web",
        "{count} web pages" => "{count} páginas web",
        "1 skill" => "1 competência",
        "{count} skills" => "{count} competências",
        "1 search" => "1 pesquisa",
        "{count} searches" => "{count} pesquisas",
        "1 command" => "1 comando",
        "{count} commands" => "{count} comandos",
        "1 task" => "1 tarefa",
        "{count} tasks" => "{count} tarefas",
        "1 tool" => "1 ferramenta",
        "{count} tools" => "{count} ferramentas",
        "1 delegation rejected" => "1 delegação rejeitada",
        "{count} delegations rejected" => "{count} delegações rejeitadas",
        "{files} and {pages}" => "{files} e {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} falharam",
        " · {count} rejected" => " · {count} rejeitado",
        // transcript/tool_group.rs: call rows
        "Read" => "Ler",
        "List" => "Listar",
        "Grep" => "Grep",
        "Search the web" => "Pesquisar na web",
        "Read skill" => "Ler competência",
        "Search git history" => "Pesquisar histórico git",
        "Edit" => "Editar",
        "Write" => "Escrever",
        "Create" => "Criar",
        " (detached)" => " (em segundo plano)",
        "Checked on background task" => "Verificou tarefa em segundo plano",
        "Waiting…" => "Em espera…",
        "… +{count} lines" => "… +{count} linhas",
        _ => return None,
    })
}
