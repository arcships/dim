pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Carregando…",
        "No entries." => "Nenhuma entrada.",
        "No matching entry." => "Nenhuma entrada correspondente.",
        "Issues" => "Problemas",
        "sources" => "Fontes",
        "skills" => "Competências",
        "hooks" => "hooks",
        "scheduled jobs" => "tarefas agendadas",
        "models" => "modelos",
        "workflow definitions" => "definições de fluxo de trabalho",
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
        "Saved." => "Guardado.",
        "Saved {name}." => "{name} guardado.",
        "No change." => "Nenhuma alteração.",
        "Your text is kept in {path}." => "O seu texto está guardado em {path}.",
        "The user's own document; always in effect." => {
            "O documento do próprio utilizador; sempre em vigor."
        }
        "There is no hooks file there." => "Não há ficheiro de hooks aí.",
        "Owned by plugin {plugin}." => "Pertence ao plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "O documento do utilizador desativa todos os hooks (disableAllHooks)."
        }
        "Enter toggle" => "Enter alternar",
        "Ctrl+E edit" => "Ctrl+E editar",
        "Ctrl+N add" => "Ctrl+N adicionar",
        "Ctrl+R reload" => "Ctrl+R recarregar",
        "Ctrl+T run" => "Ctrl+T executar",
        "Ctrl+X remove" => "Ctrl+X remover",
        "Tab inspect" => "Tab inspecionar",
        "Remove {name}?" => "Remover {name}?",
        "↑↓ scroll · Esc back" => "↑↓ deslocar · Esc voltar",
        "Added." => "Adicionado.",
        "Removed {name}." => "{name} removido.",
        "unavailable" => "indisponível",
        "Reset {name}?" => "Repor {name}?",
        "Enter confirm" => "Enter confirmar",
        "Esc cancel" => "Esc cancelar",
        "Reset {name} to its catalog entry." => "{name} reposto na entrada do catálogo.",
        "Ran {name}." => "{name} executado.",
        "The user document changed since it was read; nothing was written." => {
            "O documento do utilizador mudou desde que foi lido; nada foi escrito."
        }
        "The job changed since it was read; nothing was written." => {
            "A tarefa mudou desde que foi lida; nada foi escrito."
        }
        "The jobs changed since they were read; nothing was written." => {
            "As tarefas mudaram desde que foram lidas; nada foi escrito."
        }
        "Your next edit opens the current document." => {
            "A sua próxima edição abre o documento atual."
        }
        "Your next add reopens your text." => "A sua próxima adição abre novamente o seu texto.",
        "Scheduler" => "Agendador",
        "jobs" => "tarefas",
        "Added {name}." => "{name} adicionado.",
        "Run {name}: {outcome}." => "{name} executado: {outcome}.",
        "Recent runs of {name}" => "Execuções recentes de {name}",
        "No run yet." => "Ainda não há execuções.",
        "No next run." => "Não há próxima execução.",
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
        "A scheduled job's run takes no input document." => {
            "A execução de uma tarefa agendada não recebe documento de entrada."
        }
        "An MCP server's test takes no input document." => {
            "O teste de um servidor MCP não recebe um documento de entrada."
        }
        "This daemon cannot manage {area}." => "Não é possível gerir {area}.",
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
            "As definições de hooks deste espaço de trabalho mudaram desde que foram lidas; nada foi escrito."
        }
        "Models" => "Modelos",
        "catalog" => "catálogo",
        "overrides" => "substituições",
        "added" => "adicionados",
        "Models · {name}" => "Modelos · {name}",
        "undeclared" => "não declarado",
        "Context window {window} · max output {output}" => {
            "Janela de contexto {window} · saída máx. {output}"
        }
        "Endpoint {endpoint}" => "Endpoint {endpoint}",
        "Endpoint {endpoint} · override {value}" => "Endpoint {endpoint} · substituição {value}",
        "The provider reports it unavailable." => "O fornecedor indica-o como indisponível.",
        "Limits undeclared; edit the model to declare them." => {
            "Limites não declarados; edite o modelo para os declarar."
        }
        "Sign in to the account to use its models." => {
            "Inicie sessão na conta para usar os modelos dela."
        }
        "The signed-in account changed; reload to list its models." => {
            "A conta iniciada mudou; recarregue para listar os modelos dela."
        }
        "The provider has no catalog yet." => "Este fornecedor ainda não tem catálogo.",
        "The provider listed no models." => "O fornecedor não listou nenhum modelo.",
        "Listed {count} models." => "{count} modelos listados.",
        "Refreshed the models." => "Modelos atualizados.",
        "Could not list models: {error}. Showing the stored list." => {
            "Não foi possível listar os modelos: {error}. A mostrar a lista guardada."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "A conta iniciada mudou; a recarga limpou preferências, substituições e desativações."
        }
        "The models changed since they were read; nothing was removed." => {
            "Os modelos mudaram desde a leitura; nada foi removido."
        }
        "The models changed since they were read; nothing was written." => {
            "Os modelos mudaram desde a leitura; nada foi escrito."
        }
        "No provider has that id." => "Nenhum fornecedor tem esse id.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} não está disponível ({reason}); a sua ativação mantém-se como está."
        }
        "No model has that id." => "Nenhum modelo tem esse id.",
        "This model cannot be removed." => "Este modelo não pode ser removido.",
        "A Provider's models have no area document." => {
            "Os modelos de um fornecedor não têm documento de área."
        }
        "Models have no run operation." => "Os modelos não têm operação de execução.",
        "The document is not an object; nothing was written." => {
            "O documento não é um objeto; nada foi escrito."
        }
        "The document changes no editable field; nothing was written." => {
            "O documento não altera nenhum campo editável; nada foi escrito."
        }
        "The model document names no model." => "O documento do modelo não indica nenhum modelo.",
        "The provider has no model catalog to write." => {
            "Este fornecedor não tem catálogo de modelos para escrever."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Origem {source}",
        "Status {status}" => "Estado {status}",
        "Tools {value}" => "Ferramentas {value}",
        "Vision {value}" => "Visão {value}",
        "Reasoning {value}" => "Raciocínio {value}",
        "Modalities {list}" => "Modalidades {list}",
        "Description {text}" => "Descrição {text}",
        "Release date {date}" => "Data de lançamento {date}",
        "Pricing {value}" => "Preço {value}",
        "Rate limit {value}" => "Limite de taxa {value}",
        "Matched provider {id}" => "Fornecedor correspondente {id}",
        "Matched model {id}" => "Modelo correspondente {id}",
        "Match strategy {strategy}" => "Estratégia de correspondência {strategy}",
        "Capability overrides {fields}" => "Substituições de capacidades {fields}",
        "yes" => "sim",
        "no" => "não",
        "not supported" => "não suportado",
        "supported" => "suportado",

        "Workflow" => "Fluxo de trabalho",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} guardado. Permanece desativado até o ativar."
        }
        "The definitions changed since they were read; nothing was written." => {
            "As definições mudaram desde que foram lidas; nada foi escrito."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "O nome de uma definição é a sua chave; adicione-a para a guardar com outro nome."
        }
        "No project definition is named {name}." => "Nenhuma definição de projeto se chama {name}.",
        "No workflow definition is named {name}." => {
            "Nenhuma definição de fluxo de trabalho se chama {name}."
        }
        "Saved in the user workflow source; always enabled." => {
            "Guardado na fonte de fluxos de trabalho do utilizador; sempre ativado."
        }
        "Starts only after you enable it." => "Só inicia depois de o ativar.",
        "Defined by package {package}." => "Definido pelo pacote {package}.",
        "{name} — {description} (required)" => "{name} — {description} (obrigatório)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — obrigatório",
        "{name} — default: {default}" => "{name} — predefinição: {default}",
        "and {count} more" => "e mais {count}",
        "Recent runs" => "Execuções recentes",
        "A workflow definition is saved as an object with a source and a document." => {
            "Uma definição de fluxo de trabalho é guardada como um objeto com uma fonte e um documento."
        }
        "A definition is saved to the user source or the project source." => {
            "Uma definição é guardada na fonte do utilizador ou na fonte do projeto."
        }
        "Started {name}: {run_id}." => "{name} iniciado: {run_id}.",
        "Parameters are a JSON object of strings." => {
            "Os parâmetros são um objeto JSON de cadeias de caracteres."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "A lista atingiu o seu limite de tamanho; as definições seguintes não são mostradas."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Nenhuma definição de workflow chamada {name} está na lista, que atingiu o seu limite de tamanho."
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
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} está em mais de uma origem MCP: {sources}. Indique uma com --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "A ação não nomeia nenhuma origem MCP; uma linha transporta a origem que o seu dono reporta."
        }
        "No MCP server {id} is in source {source}." => {
            "Nenhum servidor MCP {id} na origem {source}."
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
        "user" => "utilizador",
        "Matches only this command." => "Corresponde apenas a este comando.",
        "Matches commands whose first words are these." => {
            "Corresponde a comandos cujas primeiras palavras são estas."
        }
        "Allows a simple command that would otherwise ask." => {
            "Permite um comando simples que de outra forma perguntaria."
        }
        "Denies the command under every preset." => "Recusa o comando em qualquer predefinição.",
        "Applies in every Session, in every workspace." => {
            "Aplica-se em todas as sessões e em todos os espaços de trabalho."
        }
        "Rule added: {rule}." => "Regra adicionada: {rule}.",
        "Rule already present: {rule}." => "A regra já existe: {rule}.",
        "Rule removed: {rule}." => "Regra removida: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Essa regra de comando já não está listada; nada foi removido."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "As regras de comando mudaram desde que foram lidas; nada foi removido."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Uma regra de comando escreve-se DECISION MATCH COMMAND, por exemplo deny prefix git push."
        }
        "Command: {command}" => "Comando: {command}",
        "Use the line prompt or dimcode permissions." => {
            "Use a linha de comandos ou dimcode permissions."
        }
        "Tool call timeout: {millis} ms" => "Tempo limite da chamada de ferramenta: {millis} ms",
        "Variables: {names}" => "Variáveis: {names}",
        "Unset variables: {names}" => "Variáveis não definidas: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "A fonte MCP mudou desde a pré-visualização; nenhuma entrada foi importada, e uma entrada que ela teria substituído pode estar agora desativada. Pré-visualize novamente."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "As entradas de projeto foram escritas e os valores retidos não; substitua-as novamente para tentar de novo."
        }
        "Replaced {id}." => "{id} substituído.",
        "Added {id}." => "{id} adicionado.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Entrega à sessão {session}, ramo {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Inicia uma nova sessão em {cwd}.",
        "Disabled: {code} at {instant}." => "Desativado: {code} em {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "A definição mudou desde que foi lida; nada foi escrito."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Colunas: ID, cliente, âmbito, caminho, estado, entradas ou erro."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Nenhum valor de um armazenamento é lido ou impresso; --from ID pré-visualiza um dentro do daemon."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Colunas: ID, ação, ID de destino.\nUma entrada renomeada é escrita com o id de destino; a entrada da fonte com o id antigo fica como estava."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Entradas importadas e substituídas começam desativadas: uma entrada de utilizador é escrita com enabled: false, e uma entrada de projeto não tem registo de ativação até a ativares localmente."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Este daemon não consegue listar os armazenamentos MCP de outros clientes."
        }
        "This daemon cannot import MCP servers." => {
            "Este daemon não consegue importar servidores MCP."
        }
        "The document holds no mcpServers entries." => {
            "O documento não contém entradas mcpServers."
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Colunas: ID, estado, transporte, campos alterados, campos não suportados, erro.\nUm campo alterado indica o que uma escrita mudaria e o tipo de valor que contém; nenhum valor é impresso.\nUm campo não suportado fica sem uso ou é descartado, como mostrado depois do campo."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Uma entrada de projeto é escrita como o seu esqueleto público: o ficheiro do espaço de trabalho recebe o comando, o cwd declarado, a origem do URL e os campos OAuth públicos. Os seus argumentos, valores de ambiente e cabeçalho, token bearer, um URL com userinfo, caminho, consulta ou fragmento, e membros desconhecidos vão para o seu registo mcp-secrets de âmbito de utilizador, nunca para o ficheiro do espaço de trabalho."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} tem mais {count} nomes de campo do que as listas acima mostram."
        }
        "Top-level members the import does not carry: none." => {
            "Membros de nível superior que a importação não transporta: nenhum."
        }
        "Top-level members the import does not carry: {names}." => {
            "Membros de nível superior que a importação não transporta: {names}."
        }
        "Applying the selection at this revision." => "A aplicar a seleção nesta revisão.",
        "An unchanged entry was already in the source and was not written." => {
            "Uma entrada inalterada já estava na fonte e não foi escrita."
        }
        _ => return None,
    })
}
