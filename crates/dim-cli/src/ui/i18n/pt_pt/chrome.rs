pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter saída/interromper membro · x parar comando · Esc fechar"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter saída/interromper membro · x parar comando · Ctrl+X parar tudo · Esc fechar"
        }
        "Loading branch history... Stop-all unavailable" => {
            "A carregar histórico do ramo… Parar tudo indisponível"
        }
        "Branch history failed; reopen to retry" => {
            "Falha no histórico do ramo; reabra para repetir"
        }
        "Branch history is incomplete; stop-all unavailable" => {
            "Histórico do ramo incompleto; parar tudo indisponível"
        }
        "Running" => "Em execução",
        "Starting" => "A iniciar",
        "Delegation result" => "Resultado da delegação",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter detalhes · x parar seleção · Ctrl+X parar tudo · Esc fechar"
        }
        "x interrupt member · Esc back" => "x interromper membro · Esc voltar",
        "read-only" => "só leitura",
        "Member Session" => "Sessão do membro",
        "Loading conversation..." => "A carregar conversa…",
        "No conversation yet" => "Ainda sem conversa",
        "Observing conversation" => "A observar conversa",
        "Conversation unavailable" => "Conversa indisponível",
        "Session closed" => "Sessão fechada",
        "Member conversation" => "Conversa do membro",
        "x stop command · Esc back" => "x parar comando · Esc voltar",
        "Esc back" => "Esc voltar",
        "Tail preview - last 2,000 lines / 50 KiB" => {
            "Excerto final - últimas 2.000 linhas / 50 KiB"
        }
        "Loading output..." => "A carregar saída…",
        "No output yet" => "Ainda sem saída",
        "Live output unavailable" => "Saída em direto indisponível",
        "Live output unavailable in this profile" => "Saída em direto indisponível neste perfil",
        "Output read failed" => "Falha ao ler saída",
        "Completed" => "Concluído",
        "Failed" => "Falhou",
        "Cancelled" => "Cancelado",
        "workflow" => "workflow",
        "Stopping" => "A parar",
        "Stopped" => "Parado",
        "Interrupted" => "Interrompido",
        "last observed" => "última observação",
        "Loading Workflow runs... Stop-all unavailable" => {
            "A carregar execuções do Workflow… Parar tudo indisponível"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: última observação; falha ao atualizar, reabra para repetir"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Execuções do Workflow indisponíveis; reabra para repetir"
        }
        "The selected Workflow run is no longer available" => {
            "Execução do Workflow selecionada já não disponível"
        }
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Inventário do Workflow incompleto; parar tudo indisponível"
        }
        "Workflow stop unavailable" => "Impossível parar o Workflow",
        "Run no longer available; last observed record" => {
            "Execução já não disponível; último registo observado"
        }
        "x stop workflow · Esc back" => "x parar workflow · Esc voltar",
        "Run" => "Execução",
        "Source" => "Origem",
        "Stages" => "Etapas",
        "result" => "resultado",
        "Log" => "Registo",
        "Report" => "Relatório",
        "waiting" => "em espera",
        "launching" => "a iniciar",
        "completed" => "concluído",
        "failed" => "falhou",
        "stopped" => "parado",
        "skipped" => "ignorado",
        "lost" => "perdido",
        "none" => "nenhum",
        "pending" => "pendente",
        "delivered" => "entregue",
        "undeliverable" => "não entregue",
        "Agent is starting; no member Session to stop yet" => {
            "Agente a iniciar; ainda sem sessão de membro para parar"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "Membros do fornecedor são controlados pelo líder; impossível parar aqui"
        }
        "Provider members have no conversation to open here" => {
            "Membros do fornecedor sem conversa para abrir aqui"
        }
        "That delegation is no longer in this transcript" => {
            "Essa delegação já não está nesta transcrição"
        }
        "Member model" => "Modelo do membro",
        "No model observed" => "Sem modelo observado",
        "Observing member..." => "A observar membro…",
        "{model} (observation unavailable)" => "{model} (observação indisponível)",
        "{model} (Session closed)" => "{model} (sessão fechada)",
        "{model} (last observed)" => "{model} (última observação)",
        "Agent: {name}" => "Agente: {name}",
        "Provider member: {name}" => "Membro do fornecedor: {name}",
        "Task: {task}" => "Tarefa: {task}",
        "Activity: {activity}" => "Atividade: {activity}",
        "Activity: not observed on this attachment" => "Atividade: não observada nesta ligação",
        "Result: {result}" => "Resultado: {result}",
        "Controlled by the leader" => "Controlado pelo líder",
        "› Open member conversation" => "› Abrir conversa do membro",
        "Copied to clipboard" => "Copiado",
        "Opened link" => "Ligação aberta",
        "Link: {url}" => "Ligação: {url}",
        "Not available" => "Indisponível",
        "files changed  {count}" => "ficheiros alterados  {count}",
        "active session" => "sessão ativa",
        "new-session draft" => "rascunho de nova sessão",
        "Tool approvals: {title} ({target})" => "Aprovações de ferramentas: {title} ({target})",
        "Disconnected · draft kept" => "Desligado · rascunho mantido",
        "The App Server closed this interface: {reason}" => {
            "O App Server fechou esta interface: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Desligado; rascunho mantido. Saia e retome esta sessão."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Desligado; rascunho mantido. Saia e execute: {command}"
        }
        "Disconnected; command outcome unknown" => "Desligado; resultado do comando desconhecido",
        "Message queued" => "Mensagem em fila",
        "Run a shell command · Esc to exit" => "Executar comando shell · Esc sair",
        "! shell mode" => "! modo shell",
        "!! shell mode · not sent to the model" => "!! modo shell · não enviado ao modelo",
        "Leave shell mode to quote a message" => "Saia do modo shell para citar mensagem",
        "A shell command is already running" => "Já há um comando shell em execução",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "Em execução… · Ctrl+C interromper · {seconds}s"
        }
        "exit code {code}" => "código de saída {code}",
        "killed by {signal}" => "terminado por {signal}",
        "interrupted" => "interrompido",
        "done" => "concluído",
        "failed: {reason}" => "falhou: {reason}",
        "Shell output was not recorded: {reason}" => "Saída da shell não registada: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "A ler área de transferência…",
        "Clipboard: {reason}" => "Área de transferência: {reason}",
        "Wait for clipboard insertion before sending" => "Aguarde a colagem antes de enviar",
        "A submission accepts at most 20 images" => "Máximo de 20 imagens por envio",
        "[Image #{index}]" => "[Imagem #{index}]",
        "The current model does not accept images; remove {label} to send" => {
            "Modelo atual não aceita imagens; remova {label} para enviar"
        }
        "Files" => "Ficheiros",
        "Searching…" => "A pesquisar…",
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ selecionar · enter/tab inserir · esc fechar"
        }
        "not sent to the model" => "não enviado ao modelo",
        "… {count} more lines" => "… mais {count} linhas",
        "… {count} earlier lines" => "… {count} linhas anteriores",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Interrupção pedida; Ctrl+C de novo para sair"
        }
        "Press Ctrl+C again to exit" => "Prima Ctrl+C de novo para sair",
        "User interrupted the run." => "O utilizador interrompeu a execução.",
        "Could not save the language: {error}" => "Erro ao guardar idioma: {error}",
        "Language set to {locale}" => "Idioma definido: {locale}",
        "Language set to system default" => "Idioma definido: predefinição do sistema",
        "Connected {provider}" => "{provider} ligado",
        "Opened {url}" => "{url} aberto",
        "Tool details not saved: {error}" => "Detalhes das ferramentas não guardados: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} requer sessão — envie primeiro uma mensagem"
        }
        "Nothing is running" => "Nada em execução",
        "No queued message" => "Sem mensagens em fila",
        "Only in fullscreen (/tui)" => "Só em ecrã inteiro (/tui)",
        "/{name} is not yet available" => "/{name} ainda indisponível",
        "new session" => "nova sessão",
        "resumed {id} · {count} messages" => "{id} retomada · {count} mensagens",
        " · earlier history: /history" => " · histórico anterior: /history",
        "Error: {message}" => "Erro: {message}",
        "working…  {elapsed}" => "a trabalhar…  {elapsed}",
        "thinking…  {elapsed}" => "a pensar…  {elapsed}",
        "Ask anything..." => "Pergunte o que quiser…",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "Histórico anterior · {id} · {count} mensagens · /history página anterior"
        }
        "No active session" => "Sem sessão ativa",
        "No earlier messages." => "Sem mensagens anteriores.",
        "answer required" => "resposta necessária",
        "permission required" => "permissão necessária",
        "esc interrupt" => "esc interromper",
        "↑ edit · ctrl+s steer now · /queue" => "↑ editar · ctrl+s orientar já · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "+{count} mais · ↑ editar · ctrl+s orientar já · /queue"
        }
        "Queue ({count})" => "Fila ({count})",
        "Thinking…" => "A pensar…",
        "Thought" => "Pensou",
        "Thought for {duration}" => "Pensou durante {duration}",
        "Welcome to DIM {version}!" => "Bem-vindo ao DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => {
            "Escreva / para comandos · Ctrl+O paleta"
        }
        "Workspace" => "Espaço de trabalho",
        "Git branch" => "Ramo Git",
        "Bye!" => "Adeus!",
        "DIM Web is ready" => "DIM Web está pronto",
        "Opened in your browser" => "Aberto no navegador",
        "Open the sign-in link below" => "Abra a ligação de início de sessão abaixo",
        "Web" => "Web",
        "Sign-in link:" => "Ligação de início de sessão:",
        "Resume" => "Retomar",
        "Press Ctrl+C to stop the Web workspace." => {
            "Prima Ctrl+C para parar o espaço de trabalho Web."
        }
        "To resume this session:" => "Para retomar esta sessão:",

        // composer/commands.rs: categories
        "session" => "sessão",
        "run" => "execução",
        "view" => "vista",
        "model" => "modelo",
        "app" => "aplicação",
        "skill" => "competência",
        // composer/commands.rs: descriptions
        "start a new session" => "iniciar nova sessão",
        "show or set the reasoning effort" => "ver ou definir o esforço de raciocínio",
        "show or set the priority service tier (on|off)" => {
            "ver ou definir o nível de serviço prioritário (on|off)"
        }
        "list agents or switch this session's agent" => {
            "listar agentes ou mudar o agente desta sessão"
        }
        "list MCP servers, sign in or out (login|logout ID)" => {
            "listar servidores MCP, iniciar ou terminar sessão (login|logout ID)"
        }
        "resume a previous session" => "retomar uma sessão anterior",
        "open this session in the browser" => "abrir esta sessão no navegador",
        "load the previous page of messages" => "carregar página anterior de mensagens",
        "show the context window usage" => "ver uso da janela de contexto",
        "interrupt the run" => "interromper a execução",
        "steer with the next queued message" => "orientar com a próxima mensagem em fila",
        "scroll to the top" => "ir para o topo",
        "scroll to the bottom" => "ir para o fundo",
        "pick an earlier message to edit or quote" => {
            "escolher mensagem anterior para editar ou citar"
        }
        "collapse or expand tool details and thoughts" => {
            "recolher ou expandir detalhes de ferramentas e pensamentos"
        }
        "switch between fullscreen and scrollback" => {
            "alternar entre ecrã inteiro e histórico do terminal"
        }
        "switch theme" => "mudar tema",
        "switch language" => "mudar idioma",
        "select a model" => "selecionar modelo",
        "connect a provider" => "ligar fornecedor",
        "sign in to DimAgent" => "iniciar sessão no DimAgent",
        "DimAgent account · usage · sign out" => "Conta DimAgent · uso · terminar sessão",
        "configure tool approvals" => "configurar aprovações de ferramentas",
        "open settings" => "abrir definições",
        "suspend to the shell" => "suspender para a shell",
        "exit" => "sair",

        // composer/palette.rs
        "Enter select · Ctrl+X clear saved · Esc clear/close" => {
            "Enter selecionar · Ctrl+X limpar guardado · Esc limpar/fechar"
        }
        "Enter select · Esc clear/close" => "Enter selecionar · Esc limpar/fechar",
        "esc close" => "esc fechar",
        "Suggestions" => "Sugestões",
        "Skills" => "Competências",
        "Commands" => "Comandos",
        "Command Palette" => "Paleta de comandos",
        "No matches" => "Sem resultados",
        " (unavailable)" => " (indisponível)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Texto: {preview} ({count} carateres)]",
        "[... {count} lines]" => "[… {count} linhas]",
        "pasted text" => "texto colado",

        // list_panel.rs

        // settings_panel.rs
        "on" => "ligado",
        "off" => "desligado",
        "Interface" => "Interface",
        "Theme" => "Tema",
        "Language" => "Idioma",
        "Startup UI form" => "Interface ao iniciar",
        "fullscreen" => "ecrã inteiro",
        "scrollback" => "histórico do terminal",
        "Tool details" => "Detalhes das ferramentas",
        "expanded" => "expandido",
        "collapsed" => "recolhido",
        "Mouse (fullscreen)" => "Rato (ecrã inteiro)",
        "Permissions" => "Permissões",
        "Tool approvals" => "Aprovações de ferramentas",
        "Models" => "Modelos",
        "Model" => "Modelo",
        "not set" => "não definido",
        "Settings" => "Definições",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ mover · Enter selecionar · ←→/Space alterar · Esc fechar"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ pré-visualizar · Enter guardar · Esc repor"
        }
        "System default ({locale})" => "Predefinição do sistema ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ pré-visualizar · Enter aplicar · Esc cancelar"
        }
        "System" => "Sistema",
        "Could not save the theme: {error}" => "Erro ao guardar tema: {error}",

        // sessions_panel.rs
        "Sessions" => "Sessões",
        "Enter open · Ctrl+R refresh · Esc close" => {
            "Enter abrir · Ctrl+R atualizar · Esc fechar"
        }
        "Loading sessions..." => "A carregar sessões…",
        "No saved sessions yet." => "Sem sessões guardadas.",
        "No matching sessions." => "Nenhuma sessão corresponde.",
        "now" => "agora",
        "{count}m ago" => "há {count} min",
        "{count}h ago" => "há {count} h",
        "{count}d ago" => "há {count} d",

        // models_panel.rs
        "Select a model" => "Selecionar modelo",
        "Loading models..." => "A carregar modelos…",
        "No models available. Connect a provider first." => {
            "Sem modelos disponíveis. Ligue primeiro um fornecedor."
        }
        "No matching models." => "Nenhum modelo corresponde.",
        "current" => "atual",
        "Edit models" => "Editar modelos",
        "Editing models is not available yet" => "Edição de modelos ainda indisponível",
        "Enter select · Esc back" => "Enter selecionar · Esc voltar",
        "Subagent models" => "Modelos de subagentes",
        "Subagent models: {policy}" => "Modelos de subagentes: {policy}",
        "Fixed subagent model" => "Modelo fixo de subagente",
        "Agent choice (default)" => "Escolha do agente (predefinição)",
        "Agent choice" => "Escolha do agente",
        "Inherit requesting branch" => "Herdar do ramo que pediu",
        "Fixed model" => "Modelo fixo",
        "Applies to new members, including named Agents." => {
            "Aplica-se a novos membros, incluindo agentes com nome."
        }
        "Existing members keep their models." => "Membros existentes mantêm os seus modelos.",
        "unknown" => "desconhecido",
        "Subagent model policy saved" => "Política de modelos de subagentes guardada",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Política de modelos de subagentes mudou noutro local; escolha de novo para reler"
        }
        "Subagent model policy: {error}" => "Política de modelos de subagentes: {error}",
        "drawing diagram… {count} lines" => "a desenhar diagrama… {count} linhas",
        // notify.rs
        "Task complete" => "Tarefa concluída",
        "Permission needed: {tool}" => "Permissão necessária: {tool}",
        "Question waiting for your answer" => "Pergunta à espera de resposta",
        "Notify when agent needs you" => "Notificar quando o agente precisar de si",
        // keys.rs
        "show the key bindings" => "ver atalhos de teclado",
        "Application keys" => "Teclas da aplicação",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter enviar · Shift+Enter ou Ctrl+J nova linha · ↑↓ histórico"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O ou Ctrl+P comandos · Esc interromper ou limpar · Ctrl+S orientar com fila · ↑ editar fila"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C limpar, duas vezes para sair · Ctrl+D sair com rascunho vazio"
        }
        "Fullscreen" => "Ecrã inteiro",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown deslocar · Home/End topo ou fundo · Tab escolher mensagem, e editar, q citar"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "roda deslocar · arrastar selecionar e copiar · clicar ligação para abrir, grupo de ferramentas para expandir"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "rato desligado (/settings): o terminal seleciona e copia"
        }
        "Terminal-owned history" => "Histórico gerido pelo terminal",
        "scroll: {value}" => "deslocar: {value}",
        "search: {value}" => "pesquisar: {value}",
        "copy: {value}" => "copiar: {value}",
        "terminal search" => "pesquisa do terminal",
        "terminal scrollback" => "histórico do terminal",
        "terminal copy" => "cópia do terminal",
        "drag to select" => "arrastar para selecionar",
        "drag or {shortcut}" => "arrastar ou {shortcut}",
        _ => return None,
    })
}
