//! European Portuguese model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Details unavailable; collapse and expand to retry" => {
            "Detalhes indisponíveis; recolha e expanda para tentar novamente"
        }
        "The response above is incomplete." => "A resposta acima está incompleta.",
        "Could not reach the provider, or the connection dropped" => {
            "Não foi possível alcançar o fornecedor, ou a ligação foi interrompida"
        }
        "The provider is rate limiting requests" => "O fornecedor está a limitar os pedidos",
        "The provider reported a server error" => "O fornecedor reportou um erro de servidor",
        "The provider sent a response Dim could not use" => {
            "O fornecedor enviou uma resposta que o Dim não conseguiu utilizar"
        }
        "The provider rejected the credentials" => "O fornecedor rejeitou as credenciais",
        "The account has no quota left" => "A conta não tem quota disponível",
        "The provider did not find the model or endpoint" => {
            "O fornecedor não encontrou o modelo nem o endpoint"
        }
        "The conversation is longer than the model accepts" => {
            "A conversa é mais longa do que o modelo aceita"
        }
        "The provider's content filter blocked the response" => {
            "O filtro de conteúdo do fornecedor bloqueou a resposta"
        }
        "The provider rejected the request" => "O fornecedor rejeitou o pedido",
        "The model stopped at its output limit" => "O modelo parou no seu limite de saída",
        "The model request failed" => "O pedido ao modelo falhou",
        "Check the network or proxy." => "Verifique a rede ou o proxy.",
        "Wait and try again." => "Aguarde e tente novamente.",
        "Try again later or choose another model." => {
            "Tente novamente mais tarde ou escolha outro modelo."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Tente novamente; se se repetir, escolha outro modelo ou verifique o proxy ou a gateway."
        }
        "Sign in again or check the API key." => "Inicie sessão novamente ou verifique a API key.",
        "Check the plan or billing." => "Verifique o plano ou a faturação.",
        "Check the model and the provider's base URL." => {
            "Verifique o modelo e a base URL do fornecedor."
        }
        "Compact the conversation or start a new session." => {
            "Compacte a conversa ou inicie uma nova sessão."
        }
        "Rephrase the request." => "Reformule o pedido.",
        "See the details." => "Consulte os detalhes.",
        "Increase the output token limit and retry." => {
            "Aumente o limite de tokens de saída e tente novamente."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "A tentar novamente {attempt}/{max} em {seconds} s: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "A tentar novamente {attempt}/{max} em {seconds} s · {reason} · Esc para"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "estado: {status}",
        "provider code: {code}" => "código do fornecedor: {code}",
        "request id: {id}" => "id do pedido: {id}",
        "tried {count} times over {seconds} s" => "{count} tentativas em {seconds} s",
        "the provider asked to wait {seconds} s" => "o fornecedor pediu para aguardar {seconds} s",
        "cause:" => "causa:",
        "Tried {count} times over {seconds} s." => "{count} tentativas em {seconds} s.",
        "Waiting for the provider · {reason} · Esc stops" => {
            "A aguardar o fornecedor · {reason} · Esc para"
        }
        _ => return None,
    })
}
