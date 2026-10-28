pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter saída / interromper membro · x parar comando · Esc fechar"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter saída / interromper membro · x parar comando · Ctrl+X parar tudo · Esc fechar"
        }
        "Loading branch history... Stop-all unavailable" => {
            "Carregando histórico do branch… Parar tudo indisponível"
        }
        "Branch history failed; reopen to retry" => {
            "Falha no histórico do branch; reabra para repetir"
        }
        "Branch history is incomplete; stop-all unavailable" => {
            "Histórico do branch incompleto; parar tudo indisponível"
        }
        "Running" => "Executando",
        "Lines" => "Linhas",
        "Enter show the line · x end it · Esc close" => {
            "Enter ver a linha · x encerrá-la · Esc fechar"
        }
        "This session has no lines." => "Esta sessão não tem linhas.",
        "No matching lines." => "Nenhuma linha correspondente.",
        "the session itself" => "a própria sessão",
        "working" => "trabalhando",
        "ended itself" => "encerrou-se",
        "ended by an operation" => "encerrada por uma operação",
        "ended with {id}" => "encerrada com {id}",
        "creation failed" => "criação falhou",
        "shown" => "exibida",
        "line" => "linha",
        "line {id}" => "linha {id}",
        "follow, talk to and end this session's lines" => {
            "acompanhar, conversar e encerrar as linhas desta sessão"
        }
        "End line" => "Encerrar linha",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main é a própria sessão; uma linha encerra, main não"
        }
        "Line has ended:" => "A linha foi encerrada:",
        "Starting" => "Iniciando",
        "Delegation result" => "Resultado da delegação",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter detalhes · x parar selecionado · Ctrl+X parar tudo · Esc fechar"
        }
        "x interrupt member · Esc back" => "x interromper membro · Esc voltar",
        "read-only" => "somente leitura",
        "Member Session" => "Sessão do membro",
        "Loading conversation..." => "Carregando conversa…",
        "No conversation yet" => "Sem conversa ainda",
        "Observing conversation" => "Observando conversa",
        "Conversation unavailable" => "Conversa indisponível",
        "Session closed" => "Sessão encerrada",
        "Member conversation" => "Conversa do membro",
        "x stop command · Esc back" => "x parar comando · Esc voltar",
        "Esc back" => "Esc voltar",
        "Tail preview - last 2,000 lines / 50 KiB" => "Prévia: últimas 2.000 linhas / 50 KiB",
        "Loading output..." => "Carregando saída…",
        "No output yet" => "Sem saída ainda",
        "Live output unavailable" => "Saída ao vivo indisponível",
        "Live output unavailable in this profile" => "Saída ao vivo indisponível neste perfil",
        "Output read failed" => "Falha ao ler saída",
        "Completed" => "Concluído",
        "Failed" => "Falhou",
        "Cancelled" => "Cancelado",
        "workflow" => "workflow",
        "Stopping" => "Parando",
        "Stopped" => "Parado",
        "Interrupted" => "Interrompido",
        "last observed" => "última observação",
        "Loading Workflow runs... Stop-all unavailable" => {
            "Carregando execuções do Workflow… Parar tudo indisponível"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: última observação; falha ao atualizar, reabra para repetir"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Execuções do Workflow indisponíveis; reabra para repetir"
        }
        "The selected Workflow run is no longer available" => {
            "Execução do Workflow selecionada não existe mais"
        }
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Inventário do Workflow incompleto; parar tudo indisponível"
        }
        "Workflow stop unavailable" => "Impossível parar o Workflow",
        "Run no longer available; last observed record" => {
            "Execução indisponível; último registro observado"
        }
        "x stop workflow · Esc back" => "x parar workflow · Esc voltar",
        "Run" => "Execução",
        "Source" => "Origem",
        "Stages" => "Etapas",
        "result" => "resultado",
        "Log" => "Log",
        "Report" => "Relatório",
        "waiting" => "aguardando",
        "launching" => "iniciando",
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
            "Agente iniciando; ainda sem Sessão de membro para parar"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "O líder controla membros do provedor; não dá para parar aqui"
        }
        "Provider members have no conversation to open here" => {
            "Membros do provedor sem conversa para abrir aqui"
        }
        "That delegation is no longer in this transcript" => {
            "Delegação não está mais nesta transcrição"
        }
        "Member model" => "Modelo do membro",
        "No model observed" => "Sem modelo observado",
        "Observing member..." => "Observando membro…",
        "{model} (observation unavailable)" => "{model} (observação indisponível)",
        "{model} (Session closed)" => "{model} (Sessão encerrada)",
        "{model} (last observed)" => "{model} (última observação)",
        "Agent: {name}" => "Agente: {name}",
        "Provider member: {name}" => "Membro do provedor: {name}",
        "Task: {task}" => "Tarefa: {task}",
        "Activity: {activity}" => "Atividade: {activity}",
        "Activity: not observed on this attachment" => "Atividade: não observada nesta conexão",
        "Result: {result}" => "Resultado: {result}",
        "Controlled by the leader" => "Controlado pelo líder",
        "› Open member conversation" => "› Abrir conversa do membro",
        "Copied to clipboard" => "Copiado",
        "Opened link" => "Link aberto",
        "Link: {url}" => "Link: {url}",
        "Not available" => "Indisponível",
        "files changed  {count}" => "arquivos alterados  {count}",
        "active session" => "sessão ativa",
        "new-session draft" => "rascunho de nova sessão",
        "Tool approvals: {title} ({target})" => "Aprovações de ferramentas: {title} ({target})",
        "Disconnected · draft kept" => "Desconectado · rascunho mantido",
        "The App Server closed this interface: {reason}" => {
            "O App Server fechou esta interface: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Desconectado; rascunho mantido. Saia e retome esta Sessão."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Desconectado; rascunho mantido. Saia e execute: {command}"
        }
        "Disconnected; command outcome unknown" => {
            "Desconectado; resultado do comando desconhecido"
        }
        "Message queued" => "Mensagem na fila",
        "Run a shell command · Esc to exit" => "Executar comando shell · Esc sair",
        "! shell mode" => "! modo shell",
        "!! shell mode · not sent to the model" => "!! modo shell · não enviado ao modelo",
        "Leave shell mode to quote a message" => "Saia do modo shell para citar",
        "A shell command is already running" => "Comando shell já em execução",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "Executando… · Ctrl+C interromper · {seconds}s"
        }
        "exit code {code}" => "código de saída {code}",
        "killed by {signal}" => "encerrado por {signal}",
        "interrupted" => "interrompido",
        "done" => "concluído",
        "failed: {reason}" => "falhou: {reason}",
        "Shell output was not recorded: {reason}" => "Saída do shell não registrada: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "Lendo área de transferência…",
        "Clipboard: {reason}" => "Área de transferência: {reason}",
        "Wait for clipboard insertion before sending" => "Aguarde a colagem antes de enviar",
        "A submission accepts at most 20 images" => "Máximo de 20 imagens por envio",
        "[Image #{index}]" => "[Imagem #{index}]",
        "The current model does not accept images; remove {label} to send" => {
            "Modelo atual não aceita imagens; remova {label} para enviar"
        }
        "Files" => "Arquivos",
        "Searching…" => "Pesquisando…",
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ selecionar · enter/tab inserir · esc fechar"
        }
        "not sent to the model" => "não enviado ao modelo",
        "… {count} more lines" => "… mais {count} linhas",
        "… {count} earlier lines" => "… {count} linhas anteriores",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Interrupção solicitada; Ctrl+C de novo para sair"
        }
        "Press Ctrl+C again to exit" => "Ctrl+C de novo para sair",
        "User interrupted the run." => "Usuário interrompeu a execução.",
        "Could not save the language: {error}" => "Falha ao salvar idioma: {error}",
        "Language set to {locale}" => "Idioma definido: {locale}",
        "Language set to system default" => "Idioma: padrão do sistema",
        "Connected {provider}" => "{provider} conectado",
        "Opened {url}" => "{url} aberto",
        "Tool details not saved: {error}" => "Detalhes de ferramentas não salvos: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} requer sessão — envie uma mensagem antes"
        }
        "Nothing is running" => "Nada em execução",
        "No queued message" => "Fila vazia",
        "Only in fullscreen (/tui)" => "Só em tela cheia (/tui)",
        "/{name} is not yet available" => "/{name} ainda indisponível",
        "new session" => "nova sessão",
        "resumed {id} · {count} messages" => "{id} retomada · {count} mensagens",
        " · earlier history: /history" => " · histórico anterior: /history",
        "Error: {message}" => "Erro: {message}",
        "working…  {elapsed}" => "trabalhando…  {elapsed}",
        "thinking…  {elapsed}" => "pensando…  {elapsed}",
        "Ask anything..." => "Pergunte algo…",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "Histórico anterior · {id} · {count} mensagens · /history: página anterior"
        }
        "No active session" => "Sem sessão ativa",
        "No earlier messages." => "Sem mensagens anteriores.",
        "answer required" => "resposta necessária",
        "permission required" => "permissão necessária",
        "esc interrupt" => "esc interromper",
        "↑ edit · ctrl+s steer now · /queue" => "↑ editar · ctrl+s steer agora · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "+{count} mais · ↑ editar · ctrl+s steer agora · /queue"
        }
        "Queue ({count})" => "Fila ({count})",
        "Thinking…" => "Pensando…",
        "Thought" => "Pensou",
        "Thought for {duration}" => "Pensou por {duration}",
        "Welcome to DIM {version}!" => "Boas-vindas ao DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => "Digite / para comandos · Ctrl+O paleta",
        "Workspace" => "Workspace",
        "Git branch" => "Branch Git",
        "Bye!" => "Tchau!",
        "DIM Web is ready" => "DIM Web está pronto",
        "Opened in your browser" => "Aberto no navegador",
        "Open the sign-in link below" => "Abra o link de login abaixo",
        "Web" => "Web",
        "Sign-in link:" => "Link de login:",
        "Resume" => "Retomar",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C para parar o workspace Web.",
        "To resume this session:" => "Para retomar esta sessão:",

        // composer/commands.rs: categories
        "session" => "sessão",
        "run" => "execução",
        "view" => "exibição",
        "model" => "modelo",
        "app" => "app",
        "skill" => "skill",
        // composer/commands.rs: descriptions
        "start a new session" => "iniciar nova sessão",
        "show or set the reasoning effort" => "ver ou definir esforço de raciocínio",
        "show or set the priority service tier (on|off)" => {
            "ver ou definir nível de serviço prioritário (on|off)"
        }
        "list agents or switch this session's agent" => "listar ou trocar agente desta sessão",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "listar servidores MCP, entrar ou sair (login|logout ID)"
        }
        "resume a previous session" => "retomar uma sessão anterior",
        "open this session in the browser" => "abrir esta sessão no navegador",
        "load the previous page of messages" => "carregar página anterior de mensagens",
        "show the context window usage" => "mostrar uso da janela de contexto",
        "interrupt the run" => "interromper execução",
        "steer with the next queued message" => "steer com a próxima mensagem da fila",
        "scroll to the top" => "rolar até o topo",
        "scroll to the bottom" => "rolar até o fim",
        "pick an earlier message to edit or quote" => {
            "escolher mensagem anterior para editar ou citar"
        }
        "collapse or expand tool details and thoughts" => {
            "recolher/expandir detalhes de ferramentas e pensamentos"
        }
        "switch between fullscreen and scrollback" => "alternar entre tela cheia e scrollback",
        "switch theme" => "trocar tema",
        "switch language" => "trocar idioma",
        "select a model" => "selecionar modelo",
        "connect a provider" => "conectar provedor",
        "sign in to DimAgent" => "entrar no DimAgent",
        "DimAgent account · usage · sign out" => "Conta DimAgent · uso · sair",
        "configure tool approvals" => "configurar aprovações de ferramentas",
        "open settings" => "abrir configurações",
        "suspend to the shell" => "suspender para o shell",
        "exit" => "sair",

        // composer/palette.rs
        "Enter select · Ctrl+X clear saved · Esc clear/close" => {
            "Enter selecionar · Ctrl+X limpar salvos · Esc limpar/fechar"
        }
        "Enter select · Esc clear/close" => "Enter selecionar · Esc limpar/fechar",
        "esc close" => "esc fechar",
        "Suggestions" => "Sugestões",
        "Skills" => "Skills",
        "Commands" => "Comandos",
        "Command Palette" => "Paleta de comandos",
        "No matches" => "Sem resultados",
        " (unavailable)" => " (indisponível)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Texto: {preview} ({count} caracteres)]",
        "[... {count} lines]" => "[… {count} linhas]",
        "pasted text" => "texto colado",

        // list_panel.rs

        // settings_panel.rs
        "on" => "ligado",
        "off" => "desligado",
        "Interface" => "Interface",
        "Theme" => "Tema",
        "Language" => "Idioma",
        "Startup UI form" => "Interface inicial",
        "fullscreen" => "tela cheia",
        "scrollback" => "scrollback",
        "Tool details" => "Detalhes de ferramentas",
        "expanded" => "expandido",
        "collapsed" => "recolhido",
        "Mouse (fullscreen)" => "Mouse (tela cheia)",
        "Permissions" => "Permissões",
        "Tool approvals" => "Aprovações de ferramentas",
        "Models" => "Modelos",
        "Model" => "Modelo",
        "not set" => "não definido",
        "Settings" => "Configurações",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ mover · Enter selecionar · ←→/Space alterar · Esc fechar"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ visualizar · Enter salvar · Esc restaurar"
        }
        "System default ({locale})" => "Padrão do sistema ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ visualizar · Enter aplicar · Esc cancelar"
        }
        "System" => "Sistema",
        "Could not save the theme: {error}" => "Falha ao salvar tema: {error}",

        // sessions_panel.rs
        "Sessions" => "Sessões",
        "Enter open · Ctrl+R refresh · Esc close" => {
            "Enter abrir · Ctrl+R atualizar · Esc fechar"
        }
        "Loading sessions..." => "Carregando sessões…",
        "No saved sessions yet." => "Sem sessões salvas.",
        "No matching sessions." => "Nenhuma sessão encontrada.",
        "now" => "agora",
        "{count}m ago" => "há {count} min",
        "{count}h ago" => "há {count} h",
        "{count}d ago" => "há {count} d",

        // models_panel.rs
        "Select a model" => "Selecione um modelo",
        "Loading models..." => "Carregando modelos…",
        "No models available. Connect a provider first." => {
            "Nenhum modelo disponível. Conecte um provedor."
        }
        "No matching models." => "Nenhum modelo encontrado.",
        "current" => "atual",
        "Edit models" => "Editar modelos",
        "Editing models is not available yet" => "Editar modelos ainda indisponível",
        "Enter select · Esc back" => "Enter selecionar · Esc voltar",
        "Subagent models" => "Modelos de subagentes",
        "Subagent models: {policy}" => "Modelos de subagentes: {policy}",
        "Fixed subagent model" => "Modelo fixo de subagente",
        "Agent choice (default)" => "Escolha do agente (padrão)",
        "Agent choice" => "Escolha do agente",
        "Inherit requesting branch" => "Herdar do branch solicitante",
        "Fixed model" => "Modelo fixo",
        "Applies to new members, including named Agents." => {
            "Vale para novos membros, inclusive Agentes nomeados."
        }
        "Existing members keep their models." => "Membros existentes mantêm seus modelos.",
        "unknown" => "desconhecido",
        "Subagent model policy saved" => "Política de modelo de subagente salva",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Política de modelo de subagente alterada externamente; escolha de novo para reler"
        }
        "Subagent model policy: {error}" => "Política de modelo de subagente: {error}",
        "drawing diagram… {count} lines" => "desenhando diagrama… {count} linhas",
        // notify.rs
        "Task complete" => "Tarefa concluída",
        "Permission needed: {tool}" => "Permissão necessária: {tool}",
        "Question waiting for your answer" => "Pergunta aguardando sua resposta",
        "Notify when agent needs you" => "Avisar quando o agente precisar",
        // keys.rs
        "show the key bindings" => "mostrar atalhos de teclado",
        "Application keys" => "Atalhos do app",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter enviar · Shift+Enter ou Ctrl+J nova linha · ↑↓ histórico"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O ou Ctrl+P comandos · Esc interromper ou limpar · Ctrl+S steer da fila · ↑ editar fila"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C limpar, duas vezes para sair · Ctrl+D sair com rascunho vazio"
        }
        "Fullscreen" => "Tela cheia",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown rolar · Home/End topo ou fim · Tab escolher mensagem, e editar, q citar"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "roda rolar · arrastar seleciona e copia · clique: link abre, grupo de ferramentas expande"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "mouse desligado (/settings): o terminal seleciona e copia"
        }
        "Terminal-owned history" => "Histórico do terminal",
        "scroll: {value}" => "rolar: {value}",
        "search: {value}" => "pesquisar: {value}",
        "copy: {value}" => "copiar: {value}",
        "terminal search" => "pesquisa do terminal",
        "terminal scrollback" => "scrollback do terminal",
        "terminal copy" => "cópia do terminal",
        "drag to select" => "arraste para selecionar",
        "drag or {shortcut}" => "arraste ou {shortcut}",
        _ => return None,
    })
}
