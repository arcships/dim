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
        "Lines" => "Linhas",
        "Enter show the line · x end it · Esc close" => {
            "Enter ver a linha · x terminá-la · Esc fechar"
        }
        "This session has no lines." => "Esta sessão não tem linhas.",
        "No matching lines." => "Nenhuma linha correspondente.",
        "the session itself" => "a própria sessão",
        "working" => "a trabalhar",
        "ended itself" => "terminou-se",
        "ended by an operation" => "terminada por uma operação",
        "ended with {id}" => "terminada com {id}",
        "creation failed" => "criação falhada",
        "shown" => "apresentada",
        "line" => "linha",
        "line {id}" => "linha {id}",
        "jump to a message of the shown line" => "ir para uma mensagem da linha mostrada",
        "Timeline" => "Cronologia",
        "Enter actions · Esc close" => "Enter ações · Esc fechar",
        "Timeline actions" => "Ações da cronologia",
        "↑↓ select · Enter confirm · Esc cancel" => {
            "↑↓ selecionar · Enter confirmar · Esc cancelar"
        }
        "Jump to message" => "Ir para a mensagem",
        "Edit as a new draft" => "Editar como novo rascunho",
        "Quote message" => "Citar mensagem",
        "Create branch from here" => "Criar ramo a partir daqui",
        "Before this message" => "Antes desta mensagem",
        "Fork session" => "Fazer fork da sessão",
        "1. Create fork" => "1. Criar fork",
        "2. Cancel" => "2. Cancelar",
        "Creates a new session; the current session stays unchanged." => {
            "Cria uma sessão; a sessão atual permanece inalterada."
        }
        "No messages yet." => "Ainda não há mensagens.",
        "Already on screen above; /tui jumps to it" => "Já está acima; /tui vai diretamente",
        "{count} earlier messages · Enter or click to load" => {
            "{count} mensagens anteriores · Enter ou clique para carregar"
        }
        "follow, talk to and end this session's lines" => {
            "seguir, falar e terminar as linhas desta sessão"
        }
        "End line {id}?" => "Terminar a linha {id}?",
        "y end it" => "y terminar",
        "n cancel" => "n cancelar",
        "main is the session itself; a line ends, main does not" => {
            "main é a própria sessão; uma linha termina, main não"
        }
        "Line has ended:" => "A linha terminou:",
        "Esc close" => "Esc fechar",
        "/{name} needs {argument}" => "/{name} precisa de {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} aceita só texto; remova {label} para enviar"
        }
        "e edit · b branch · f fork" => "e editar · b ramo · f fork",
        "e edit" => "e editar",
        "Fork the session before “{message}”?" => "Fazer fork da sessão antes de “{message}”?",
        "Fork the session after the last turn?" => "Fazer fork da sessão após o último turno?",
        "⑂ new line from {id} · Esc cancel" => "⑂ nova linha a partir de {id} · Esc cancelar",
        "A fork copies main; b opens a line here" => "Um fork copia main; b abre uma linha aqui",
        "A line opens from text; images and files were not included" => {
            "Uma linha abre com texto; imagens e ficheiros não foram incluídos"
        }
        "No finished turn to cut at yet" => "Ainda não há turno terminado para cortar",
        "forked {id} · {count} messages" => "fork {id} · {count} mensagens",
        "after message {count}" => "após a mensagem {count}",
        "open a line from the end of the shown line" => {
            "abrir uma linha a partir do fim da linha mostrada"
        }
        "fork the session at its last finished turn" => {
            "fazer fork da sessão no último turno terminado"
        }
        "rename this session" => "renomear esta sessão",
        "Renamed to {title}" => "Renomeada para {title}",
        "The Session is now untitled." => "A sessão está agora sem título.",
        "This daemon does not support renaming Sessions" => {
            "Este daemon não suporta renomear sessões"
        }
        "Renaming failed" => "Falha ao renomear",
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
        "Not started" => "Não iniciado",
        "Reason: {reason}" => "Motivo: {reason}",
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
        "Reading image…" => "A ler imagem…",
        "Wait for the image insertion before sending" => {
            "Aguarde a inserção da imagem antes de enviar"
        }
        "The image could not be read" => "Não foi possível ler a imagem",
        "An image is still being inserted; paste again when it is in" => {
            "Uma imagem está a ser inserida; cole novamente quando terminar"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "A área de transferência está a ser inserida; cole novamente quando terminar"
        }
        "Clipboard: {reason}" => "Área de transferência: {reason}",
        "Wait for clipboard insertion before sending" => "Aguarde a colagem antes de enviar",
        "A submission accepts at most 20 images" => "Máximo de 20 imagens por envio",
        "first frame" => "apenas o primeiro fotograma",
        "re-encoded" => "recodificada",
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
        "Thinking details not saved: {error}" => "Detalhes de raciocínio não guardados: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} requer sessão — envie primeiro uma mensagem"
        }
        "Nothing is running" => "Nada em execução",
        "No queued message" => "Sem mensagens em fila",
        "Only in fullscreen (/tui)" => "Só em ecrã inteiro (/tui)",
        "new session" => "nova sessão",
        "resumed {id} · {count} messages" => "{id} retomada · {count} mensagens",
        " · earlier history: /timeline" => " · histórico anterior: /timeline",
        "Error: {message}" => "Erro: {message}",
        "working…  {elapsed}" => "a trabalhar…  {elapsed}",
        "thinking…  {elapsed}" => "a pensar…  {elapsed}",
        "Ask anything..." => "Pergunte o que quiser…",
        "Earlier history · {id} · {count} messages" => {
            "Histórico anterior · {id} · {count} mensagens"
        }
        "No active session" => "Sem sessão ativa",
        "answer required" => "resposta necessária",
        "permission required" => "permissão necessária",
        "esc interrupt" => "esc interromper",
        "shift+← edit last · ctrl+s steer now" => "shift+← editar a última · ctrl+s orientar já",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count} mais · shift+← editar a última · ctrl+s orientar já"
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
        "turn the web interface on or off" => "ativar ou desativar a interface web",
        "show the context window usage" => "ver uso da janela de contexto",
        "interrupt the run" => "interromper a execução",
        "steer with the next queued message" => "orientar com a próxima mensagem em fila",
        "scroll to the top" => "ir para o topo",
        "scroll to the bottom" => "ir para o fundo",
        "pick an earlier message to edit or quote" => {
            "escolher mensagem anterior para editar ou citar"
        }
        "cycle tool details and compaction summaries" => {
            "alternar detalhes de ferramentas e resumos de compactação"
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
        "choose ask or full access" => "escolher perguntar ou acesso total",
        "open settings" => "abrir definições",
        "exit" => "sair",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter selecionar · ←/→ esforço · Ctrl+X limpar guardado · Esc limpar/fechar"
        }
        "Enter select · Esc clear/close" => "Enter selecionar · Esc limpar/fechar",
        "esc close" => "esc fechar",
        "Suggestions" => "Sugestões",
        "Skills" => "Competências",
        "Skill loaded: {name}" => "Competência carregada: {name}",
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
        "Thinking details" => "Detalhes de raciocínio",
        "expanded" => "expandido",
        "collapsed" => "recolhido",
        "Mouse (fullscreen)" => "Rato (ecrã inteiro)",
        "Permissions" => "Permissões",
        "Tool approvals" => "Aprovações de ferramentas",
        "Execution" => "Execução",
        "Models" => "Modelos",
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
        "Sessions · This workspace" => "Sessões · Este espaço de trabalho",
        "Sessions · This repository" => "Sessões · Este repositório",
        "Sessions · All workspaces" => "Sessões · Todos os espaços de trabalho",
        "No saved sessions in this workspace. Ctrl+S shows more." => {
            "Sem sessões guardadas neste espaço de trabalho. Ctrl+S mostra mais."
        }
        "Enter open · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter abrir · Ctrl+S âmbito · Ctrl+R atualizar · Esc fechar"
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
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 1 modelo indisponível · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: {count} modelos indisponíveis · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 1 modelo precisa de uma janela · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: {count} modelos precisam de uma janela · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: sem modelos · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: sem modelos · adicione um modelo com provider.models.create"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable} indisponíveis e {disabled} desativados · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: catálogo ilegível · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "Um modelo cujos limites não estão declarados não pode ser selecionado; declare-os com dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "Declare-os com dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "Nenhum catálogo em cache. Inicie sessão e execute models PROVIDER --refresh."
        }
        "Saved model preferences cleared" => "Preferências de modelo guardadas apagadas",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 1 modelo desativado · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: {count} modelos desativados · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "Sem modelos. Execute dimcode models {id} --refresh para os listar."
        }
        "No models. The provider listed none." => "Sem modelos. O fornecedor não listou nenhum.",
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
        "Show ACP sessions" => "Mostrar sessões ACP",
        "Prompt suggestion" => "Sugestão de prompt",
        "Prompt suggestion: {error}" => "Sugestão de prompt: {error}",
        "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume." => {
            "As sugestões estão em pausa nesta sessão porque algumas propostas não foram utilizadas. Desative e volte a ativar em /settings para retomar."
        }
        // keys.rs
        "show the key bindings" => "ver atalhos de teclado",
        "Application keys" => "Teclas da aplicação",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter enviar ou enfileirar · rascunho vazio: orientar a primeira em fila · Shift+Enter ou Ctrl+J nova linha · ↑↓ histórico"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O ou Ctrl+P comandos · Esc interromper, as mensagens em fila voltam · Ctrl+S orientar o rascunho ou a primeira em fila · Shift+← editar a última em fila"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C limpar, duas vezes para sair · Ctrl+D sair com rascunho vazio"
        }
        "Fullscreen" => "Ecrã inteiro",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown deslocar · Home/End topo ou fundo (com rascunho vazio) · Tab escolher mensagem, e editar, q citar"
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
        "Compacting context…" => "A compactar o contexto…",
        "Context compacted" => "Contexto compactado",
        "Context compaction skipped" => "Compactação do contexto ignorada",
        "Context compaction failed" => "Falha na compactação do contexto",
        "Context compaction cancelled" => "Compactação do contexto cancelada",
        "near the compaction threshold" => "perto do limite de compactação",
        "This daemon does not support context compaction" => {
            "Este daemon não suporta a compactação do contexto"
        }
        "compact the context of the shown line" => "compactar o contexto da linha apresentada",
        "follow or change the shown line's goal" => {
            "seguir ou alterar o objetivo da linha apresentada"
        }
        "◎ goal active" => "◎ objetivo ativo",
        "◎ goal paused" => "◎ objetivo em pausa",
        "insufficient space: {parts}" => "espaço insuficiente: {parts}",
        "system prompt" => "prompt do sistema",
        "tools" => "ferramentas",
        "appended messages" => "mensagens anexadas",
        "keyed baseline" => "linha de base com chaves",
        "pinned inputs" => "entradas fixadas",
        "digest" => "compêndio",
        "summary" => "resumo",
        _ => return None,
    })
}
