pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Atualizar lista de modelos",
        "Model list refreshed" => "Lista de modelos atualizada",
        "Model list refresh failed: {reason}" => "Falha ao atualizar a lista de modelos: {reason}",
        "Refreshing model list… · esc close" => "A atualizar a lista de modelos… · Esc fecha",
        // providers_panel.rs
        "Providers" => "Fornecedores",
        "Enter open · Esc clear/close" => "Enter abrir · Esc limpar/fechar",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab mover · Enter ativar · Esc voltar"
        }
        "Loading providers..." => "A carregar fornecedores…",
        "No matching providers." => "Nenhum fornecedor corresponde.",
        "Custom providers are not available yet" => {
            "Fornecedores personalizados ainda indisponíveis"
        }
        "Testing connection..." => "A testar ligação…",
        "Connect {providerId}" => "Ligar {providerId}",
        "(default)" => "(predefinido)",
        "(required)" => "(obrigatório)",
        "Test + apply" => "Testar + aplicar",
        "Testing sends the stored credential to this URL." => {
            "O teste envia a credencial guardada para este URL."
        }
        "Disconnect" => "Desligar",
        "Enable provider" => "Ativar fornecedor",
        "Disable provider" => "Desativar fornecedor",
        "Error: {error}" => "Erro: {error}",
        "disabled" => "desativado",
        "current" => "atual",
        "Sign in" => "Iniciar sessão",
        "ready" => "pronto",
        "error" => "erro",
        "not connected" => "não ligado",
        "Add custom provider" => "Adicionar fornecedor personalizado",
        // oauth_view.rs
        "Checking account…" => "A verificar conta…",
        "Not signed in" => "Sessão não iniciada",
        "Signed in as {account}" => "Sessão iniciada como {account}",
        "Open models" => "Abrir modelos",
        "View plan" => "Ver plano",
        "Sign out" => "Terminar sessão",
        "Checking usage…" => "A verificar utilização…",
        "Usage: {reason}" => "Utilização: {reason}",
        "Credits" => "Créditos",
        "unlimited" => "ilimitado",
        "{hours}h window" => "janela de {hours} h",
        "/month" => "/mês",
        "/year" => "/ano",
        "expires {date}" => "expira a {date}",
        "resets {date}" => "repõe a {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ mover · enter ativar · ctrl+r atualizar · esc fechar"
        }
        "Login with DimAgent" => "Iniciar sessão com DimAgent",
        "Enterprise login" => "Início de sessão empresarial",
        "Device login" => "Início de sessão no dispositivo",
        "Email" => "Email",
        "Continue" => "Continuar",
        "Browser login" => "Início de sessão no navegador",
        "Restart device login" => "Reiniciar início de sessão no dispositivo",
        "Select to copy · Enter opens link" => "Selecionar para copiar · Enter abre a ligação",
        "Link shortened · Enter opens link" => "Ligação abreviada · Enter abre",
        "Callback URL" => "URL de callback",
        "Submit callback" => "Enviar callback",
        "The browser could not be opened. Open the link manually, then return here." => {
            "Impossível abrir o navegador. Abra a ligação manualmente e volte aqui."
        }
        "Waiting for authorization" => "À espera de autorização",
        "{name} device login" => "Início de sessão no dispositivo {name}",
        "Open this URL: {url}" => "Abra este URL: {url}",
        "Enter code:    {code}" => "Código:    {code}",
        "Waiting after server slow-down" => "À espera após abrandamento do servidor",
        "OAuth login cancelled" => "Início de sessão OAuth cancelado",
        "Enter your work email" => "Introduza o email profissional",
        "Invalid OAuth state - possible CSRF attempt" => {
            "Estado OAuth inválido - possível tentativa de CSRF"
        }
        "Preparing login... · esc cancel" => "A preparar início de sessão… · esc cancelar",
        "Completing login... · esc cancel" => "A concluir início de sessão… · esc cancelar",
        "enter restart · esc back" => "enter reiniciar · esc voltar",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "escrever valor · ↑↓/tab mover · enter enviar · esc cancelar"
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
        "from the Provider's own list" => "da própria lista do fornecedor",
        "from the whole qualified id" => "do id qualificado completo",
        "by its tokens" => "pelos seus tokens",
        "by its canonical id or display name" => "pelo id canónico ou nome de apresentação",
        "from the custom-provider list" => "da lista custom-provider",
        "by merging the rows that name it" => "ao juntar as linhas que o nomeiam",
        "ambiguous" => "ambíguo",
        "{field} {value} supplied by {provider}/{model}" => {
            "{field} {value} fornecido por {provider}/{model}"
        }
        "(paste here)" => "(colar aqui)",
        _ => return None,
    })
}
