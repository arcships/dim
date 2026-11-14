pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Atualizar lista de modelos",
        "Model list refreshed" => "Lista de modelos atualizada",
        "Model list refresh failed: {reason}" => "Falha ao atualizar a lista de modelos: {reason}",
        "Refreshing model list… · esc close" => "Atualizando a lista de modelos… · Esc fecha",
        // providers_panel.rs
        "Providers" => "Provedores",
        "Enter open · Esc clear/close" => "Enter abrir · Esc limpar/fechar",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab mover · Enter ativar · Esc voltar"
        }
        "Loading providers..." => "Carregando provedores…",
        "No matching providers." => "Nenhum provedor encontrado.",
        "Custom providers are not available yet" => "Provedores personalizados ainda indisponíveis",
        "Testing connection..." => "Testando conexão…",
        "Connect {providerId}" => "Conectar {providerId}",
        "(default)" => "(padrão)",
        "(required)" => "(obrigatório)",
        "Test + apply" => "Testar + aplicar",
        "Testing sends the stored credential to this URL." => {
            "O teste envia a credencial salva para esta URL."
        }
        "Disconnect" => "Desconectar",
        "Enable provider" => "Ativar provedor",
        "Disable provider" => "Desativar provedor",
        "Error: {error}" => "Erro: {error}",
        "disabled" => "desativado",
        "current" => "atual",
        "Sign in" => "Entrar",
        "ready" => "pronto",
        "error" => "erro",
        "not connected" => "não conectado",
        "Add custom provider" => "Adicionar provedor personalizado",
        // oauth_view.rs
        "Checking account…" => "Verificando conta…",
        "Not signed in" => "Não conectado",
        "Signed in as {account}" => "Conectado como {account}",
        "Open models" => "Abrir modelos",
        "View plan" => "Ver plano",
        "Sign out" => "Sair",
        "Checking usage…" => "Verificando uso…",
        "Usage: {reason}" => "Uso: {reason}",
        "Credits" => "Créditos",
        "unlimited" => "ilimitado",
        "{hours}h window" => "janela de {hours}h",
        "/month" => "/mês",
        "/year" => "/ano",
        "expires {date}" => "expira em {date}",
        "resets {date}" => "renova em {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ mover · enter ativar · ctrl+r atualizar · esc fechar"
        }
        "Login with DimAgent" => "Entrar com DimAgent",
        "Enterprise login" => "Login corporativo",
        "Device login" => "Login por dispositivo",
        "Email" => "E-mail",
        "Continue" => "Continuar",
        "Browser login" => "Login pelo navegador",
        "Restart device login" => "Reiniciar login por dispositivo",
        "Select to copy · Enter opens link" => "Selecione para copiar · Enter abre o link",
        "Link shortened · Enter opens link" => "Link abreviado · Enter abre o link",
        "Callback URL" => "URL de callback",
        "Submit callback" => "Enviar callback",
        "The browser could not be opened. Open the link manually, then return here." => {
            "Falha ao abrir o navegador. Abra o link manualmente e volte aqui."
        }
        "Waiting for authorization" => "Aguardando autorização",
        "{name} device login" => "Login por dispositivo {name}",
        "Open this URL: {url}" => "Abra esta URL: {url}",
        "Enter code:    {code}" => "Digite o código:    {code}",
        "Waiting after server slow-down" => "Aguardando: servidor pediu pausa",
        "OAuth login cancelled" => "Login OAuth cancelado",
        "Enter your work email" => "Digite seu e-mail corporativo",
        "Invalid OAuth state - possible CSRF attempt" => {
            "OAuth state inválido - possível tentativa de CSRF"
        }
        "Preparing login... · esc cancel" => "Preparando login… · esc cancelar",
        "Completing login... · esc cancel" => "Concluindo login… · esc cancelar",
        "enter restart · esc back" => "enter reiniciar · esc voltar",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "digite valor · ↑↓/tab mover · enter enviar · esc cancelar"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ mover · enter ativar · esc voltar",
        "↑↓ move · enter activate · esc close" => "↑↓ mover · enter ativar · esc fechar",
        "Model id prefix" => "Prefixo de id do modelo",
        "(none)" => "(nenhum)",
        "identified: {model} at {provider}" => "identificado: {model} em {provider}",
        "inferred from several rows: {model} at {provider}" => {
            "inferido de várias linhas: {model} em {provider}"
        }
        "ambiguous: {candidates}" => "ambíguo: {candidates}",
        "unidentified" => "não identificado",
        "Identified {rule}" => "Identificado {rule}",
        "from the Provider's own list" => "da própria lista do provedor",
        "from the whole qualified id" => "do id qualificado completo",
        "by its tokens" => "pelos seus tokens",
        "by its canonical id or display name" => "pelo id canônico ou nome de exibição",
        "from the custom-provider list" => "da lista custom-provider",
        "by merging the rows that name it" => "mesclando as linhas que o nomeiam",
        "ambiguous" => "ambíguo",
        "{field} {value} supplied by {provider}/{model}" => {
            "{field} {value} fornecido por {provider}/{model}"
        }
        "(paste here)" => "(cole aqui)",
        _ => return None,
    })
}
