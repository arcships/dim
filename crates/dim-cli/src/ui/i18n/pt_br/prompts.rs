pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "Agente ainda em execução.",
        "AskUser is waiting for an answer." => "AskUser aguarda resposta.",
        "A permission request is waiting." => "Pedido de permissão pendente.",
        "Background work may still be active." => "Tarefas em segundo plano podem estar ativas.",
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
            "Versão {latest} disponível (você tem {current})."
        }
        "Will run: {command}" => "Executará: {command}",
        "To update, run: {command}" => "Para atualizar, execute: {command}",
        "Update now" => "Atualizar agora",
        "Skip this version" => "Pular esta versão",
        "Skipped {version}." => "{version} pulada.",
        "Later" => "Depois",
        "Close" => "Fechar",
        "Restart now" => "Reiniciar agora",
        "Updating to {version}..." => "Atualizando para {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "Atualizado para {version}. Reinicie o DimCode."
        }
        "Update failed: {error}" => "Falha ao atualizar: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ mover · Enter confirmar · Esc fechar",
        // prompts.rs: permission request
        "Permission required" => "Permissão necessária",
        "Allow once" => "Permitir uma vez",
        "Allow always" => "Sempre permitir",
        "Reject" => "Rejeitar",
        "Approve for me" => "Aprovar por mim",
        "Full access" => "Acesso total",
        "Run {tool}" => "Executar {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "Mudar modo de aprovação aqui ainda indisponível"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y permitir · n rejeitar · ←→/Tab mover · Enter confirmar"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter enviar · Tab editar · Esc recusar",
        "Space toggle · Tab next · Esc decline" => "Space alternar · Tab próxima · Esc recusar",
        "1-9 select · Tab next · Esc decline" => "1-9 selecionar · Tab próxima · Esc recusar",
        "Question" => "Pergunta",
        "Question on branch {branch}" => "Pergunta no branch {branch}",
        "Permission on branch {branch}" => "Permissão no branch {branch}",
        "Sending answer…" => "Enviando resposta…",
        "Question not ready; press Enter to send again" => {
            "Pergunta não pronta; Enter para reenviar"
        }
        // prompts.rs: AskUser
        "Confirm" => "Confirmar",
        "Enter save · Esc back" => "Enter salvar · Esc voltar",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter enviar · 1-9 editar · Tab/←→ voltar · Esc recusar"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ mover · Space marcar · Enter próxima · Tab/←→ trocar · Esc recusar"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ mover · 1-9 selecionar · Enter próxima · Tab/←→ trocar · Esc recusar"
        }
        "AskUserQuestion received an invalid payload." => {
            "AskUserQuestion recebeu um payload inválido."
        }
        "Missing answer for Q{index}" => "Falta resposta para P{index}",
        "Type your own answer" => "Digite sua resposta",
        "Review your answers and press Enter to submit." => {
            "Revise as respostas; Enter para enviar."
        }
        "Not answered" => "Sem resposta",
        // approvals_panel.rs
        "Tool Approvals" => "Aprovações de ferramentas",
        "full access" => "acesso total",
        "All tools run automatically, bypass permissions" => {
            "Todas as ferramentas rodam sem pedir permissão"
        }
        "approve for me" => "aprovar por mim",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "O agente revisa comandos e ações de ferramentas. Comandos inseguros pedem confirmação; ações rotineiras passam direto — seguro, inteligente e prático."
        }
        "Default permissions" => "Permissões padrão",
        "default" => "padrão",
        "Read tools run automatically, write tools require confirmation" => {
            "Leitura roda automaticamente; escrita pede confirmação"
        }
        "current" => "atual",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab mover · Enter aplicar · Esc voltar",
        "Confirm Full Access" => "Confirmar acesso total",
        "Cancel" => "Cancelar",
        "Tab switch  Enter confirm  Esc cancel" => "Tab alternar  Enter confirmar  Esc cancelar",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "O acesso total reduz confirmações e pode executar ações sensíveis, alterar arquivos ou rodar comandos externos diretamente. Use só se confiar na tarefa atual."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "executou",
        "running" => "executando",
        "read" => "leu",
        "reading" => "lendo",
        "explored" => "explorou",
        "exploring" => "explorando",
        "edited" => "editou",
        "editing" => "editando",
        "changed" => "alterou",
        "changing" => "alterando",
        "checked on" => "verificou",
        "checking on" => "verificando",
        "Explored" => "Explorou",
        "Exploring" => "Explorando",
        "Edited" => "Editou",
        "Editing" => "Editando",
        "Changed" => "Alterou",
        "Changing" => "Alterando",
        // transcript/tool_group.rs: counts
        "1 file" => "1 arquivo",
        "{count} files" => "{count} arquivos",
        "1 web page" => "1 página web",
        "{count} web pages" => "{count} páginas web",
        "1 skill" => "1 skill",
        "{count} skills" => "{count} skills",
        "1 search" => "1 pesquisa",
        "{count} searches" => "{count} pesquisas",
        "1 command" => "1 comando",
        "{count} commands" => "{count} comandos",
        "1 task" => "1 tarefa",
        "{count} tasks" => "{count} tarefas",
        "1 tool" => "1 ferramenta",
        "{count} tools" => "{count} ferramentas",
        "{files} and {pages}" => "{files} e {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} com falha",
        // transcript/tool_group.rs: call rows
        "Read" => "Ler",
        "List" => "Listar",
        "Grep" => "Grep",
        "Search the web" => "Buscar na web",
        "Read skill" => "Ler skill",
        "Search git history" => "Pesquisar histórico git",
        "Edit" => "Editar",
        "Write" => "Escrever",
        "Create" => "Criar",
        " (detached)" => " (em segundo plano)",
        "Checked on background task" => "Verificou tarefa em segundo plano",
        "Waiting…" => "Aguardando…",
        "… +{count} lines" => "… +{count} linhas",
        _ => return None,
    })
}
