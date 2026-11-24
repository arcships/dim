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
        "Remove {name}?" => "Remover {name}?",
        "↑↓ scroll · Esc back" => "↑↓ rolar · Esc voltar",
        "Added." => "Adicionado.",
        "Removed {name}." => "{name} removido.",
        "unavailable" => "indisponível",
        "Reset {name}?" => "Restaurar {name}?",
        "Enter confirm" => "Enter confirmar",
        "Esc cancel" => "Esc cancelar",
        "Reset {name} to its catalog entry." => "{name} restaurado para a entrada do catálogo.",
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
        "A scheduled job's run takes no input document." => {
            "A execução de um job agendado não recebe documento de entrada."
        }
        "An MCP server's test takes no input document." => {
            "O teste de um servidor MCP não recebe um documento de entrada."
        }
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
        "The provider reports it unavailable." => "O provedor o informa como indisponível.",
        "Limits undeclared; edit the model to declare them." => {
            "Limites não declarados; edite o modelo para declará-los."
        }
        "Sign in to the account to use its models." => "Entre na conta para usar os modelos dela.",
        "The signed-in account changed; reload to list its models." => {
            "A conta conectada mudou; recarregue para listar os modelos dela."
        }
        "The provider has no catalog yet." => "Este provedor ainda não tem catálogo.",
        "The provider listed no models." => "O provedor não listou nenhum modelo.",
        "Listed {count} models." => "{count} modelos listados.",
        "Refreshed the models." => "Modelos atualizados.",
        "Could not list models: {error}. Showing the stored list." => {
            "Não foi possível listar os modelos: {error}. Mostrando a lista salva."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "A conta conectada mudou; a recarga limpou preferências, substituições e desativações."
        }
        "The models changed since they were read; nothing was removed." => {
            "Os modelos mudaram desde a leitura; nada foi removido."
        }
        "The models changed since they were read; nothing was written." => {
            "Os modelos mudaram desde a leitura; nada foi escrito."
        }
        "No provider has that id." => "Nenhum provedor tem esse id.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} não está disponível ({reason}); sua ativação permanece como está."
        }
        "No model has that id." => "Nenhum modelo tem esse id.",
        "This model cannot be removed." => "Este modelo não pode ser removido.",
        "A Provider's models have no area document." => {
            "Os modelos de um provedor não têm documento de área."
        }
        "Models have no run operation." => "Modelos não têm operação de execução.",
        "The document is not an object; nothing was written." => {
            "O documento não é um objeto; nada foi escrito."
        }
        "The document changes no editable field; nothing was written." => {
            "O documento não altera nenhum campo editável; nada foi escrito."
        }
        "The model document names no model." => "O documento do modelo não nomeia nenhum modelo.",
        "The provider has no model catalog to write." => {
            "Este provedor não tem catálogo de modelos para escrever."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Origem {source}",
        "Status {status}" => "Status {status}",
        "Tools {value}" => "Ferramentas {value}",
        "Vision {value}" => "Visão {value}",
        "Reasoning {value}" => "Raciocínio {value}",
        "Modalities {list}" => "Modalidades {list}",
        "Description {text}" => "Descrição {text}",
        "Release date {date}" => "Data de lançamento {date}",
        "Pricing {value}" => "Preço {value}",
        "Rate limit {value}" => "Limite de taxa {value}",
        "Matched provider {id}" => "Provedor correspondente {id}",
        "Matched model {id}" => "Modelo correspondente {id}",
        "Match strategy {strategy}" => "Estratégia de correspondência {strategy}",
        "Capability overrides {fields}" => "Substituições de capacidades {fields}",
        "yes" => "sim",
        "no" => "não",
        "not supported" => "sem suporte",
        "supported" => "com suporte",

        "Workflow" => "Fluxo de trabalho",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} salvo. Permanece desativado até você ativá-lo."
        }
        "The definitions changed since they were read; nothing was written." => {
            "As definições mudaram desde que foram lidas; nada foi escrito."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "O nome de uma definição é a sua chave; adicione-a para salvá-la com outro nome."
        }
        "No project definition is named {name}." => "Nenhuma definição de projeto se chama {name}.",
        "No workflow definition is named {name}." => {
            "Nenhuma definição de fluxo de trabalho se chama {name}."
        }
        "Saved in the user workflow source; always enabled." => {
            "Salvo na fonte de fluxos de trabalho do usuário; sempre ativado."
        }
        "Starts only after you enable it." => "Só inicia depois que você o ativa.",
        "Defined by package {package}." => "Definido pelo pacote {package}.",
        "{name} — {description} (required)" => "{name} — {description} (obrigatório)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — obrigatório",
        "{name} — default: {default}" => "{name} — padrão: {default}",
        "and {count} more" => "e mais {count}",
        "Recent runs" => "Execuções recentes",
        "A workflow definition is saved as an object with a source and a document." => {
            "Uma definição de fluxo de trabalho é salva como um objeto com uma fonte e um documento."
        }
        "A definition is saved to the user source or the project source." => {
            "Uma definição é salva na fonte do usuário ou na fonte do projeto."
        }
        "Started {name}: {run_id}." => "{name} iniciado: {run_id}.",
        "Parameters are a JSON object of strings." => {
            "Os parâmetros são um objeto JSON de strings."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "A lista atingiu seu limite de tamanho; as definições seguintes não são mostradas."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Nenhuma definição de workflow chamada {name} está na lista, que atingiu seu limite de tamanho."
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
            "{id} está em mais de uma origem MCP: {sources}. Nomeie uma com --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "A ação não nomeia nenhuma origem MCP; uma linha carrega a origem que seu dono relata."
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
        "Tool call timeout: {millis} ms" => "Tempo limite da chamada de ferramenta: {millis} ms",
        "Variables: {names}" => "Variáveis: {names}",
        "Unset variables: {names}" => "Variáveis não definidas: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "A fonte MCP mudou desde a prévia; nenhuma entrada foi importada, e uma entrada que ela teria substituído pode estar desativada agora. Visualize novamente."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "As entradas de projeto foram gravadas e os valores retidos não; substitua-as novamente para tentar de novo."
        }
        "Replaced {id}." => "{id} substituído.",
        "Added {id}." => "{id} adicionado.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Entrega à sessão {session}, ramo {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Inicia uma nova sessão em {cwd}.",
        "Disabled: {code} at {instant}." => "Desativado: {code} em {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "A definição mudou desde que foi lida; nada foi gravado."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Colunas: ID, cliente, escopo, caminho, estado, entradas ou erro."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Nenhum valor de um armazenamento é lido ou impresso; --from ID visualiza um dentro do daemon."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Colunas: ID, ação, ID de destino.\nUma entrada renomeada é gravada com o id de destino; a entrada da fonte com o id antigo fica como estava."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Entradas importadas e substituídas começam desativadas: uma entrada de usuário é gravada com enabled: false, e uma entrada de projeto não tem registro de ativação até você ativá-la localmente."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Este daemon não pode listar os armazenamentos MCP de outros clientes."
        }
        "This daemon cannot import MCP servers." => "Este daemon não pode importar servidores MCP.",
        "The document holds no mcpServers entries." => {
            "O documento não contém entradas mcpServers."
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Colunas: ID, estado, transporte, campos alterados, campos não suportados, erro.\nUm campo alterado indica o que uma gravação mudaria e o tipo de valor que ele contém; nenhum valor é impresso.\nUm campo não suportado fica sem uso ou é descartado, como mostrado após o campo."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Uma entrada de projeto é gravada como seu esqueleto público: o arquivo do espaço de trabalho recebe o comando, o cwd declarado, a origem da URL e os campos OAuth públicos. Seus argumentos, valores de ambiente e cabeçalho, token bearer, uma URL com userinfo, caminho, consulta ou fragmento, e membros desconhecidos vão para o seu registro mcp-secrets de escopo de usuário, nunca para o arquivo do espaço de trabalho."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} tem {count} nomes de campo a mais do que as listas acima mostram."
        }
        "Top-level members the import does not carry: none." => {
            "Membros de nível superior que a importação não leva: nenhum."
        }
        "Top-level members the import does not carry: {names}." => {
            "Membros de nível superior que a importação não leva: {names}."
        }
        "Applying the selection at this revision." => "Aplicando a seleção nesta revisão.",
        "An unchanged entry was already in the source and was not written." => {
            "Uma entrada inalterada já estava na fonte e não foi gravada."
        }
        _ => return None,
    })
}
