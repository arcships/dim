//! Brazilian Portuguese model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Conversation state could not be read; retry the read before submitting" => {
            "Não foi possível ler o estado da conversa. Recarregue antes de enviar"
        }
        "The centre moved before the command ran; the draft was kept" => {
            "A sessão ou a ramificação mudou antes do envio. O rascunho foi mantido"
        }
        "The selected device restarted during file search" => {
            "O dispositivo selecionado reiniciou durante a busca"
        }
        "Target does not support file completion; type the remote path" => {
            "O destino não oferece conclusão de arquivos. Digite o caminho remoto"
        }
        "Select a regular file to upload" => "Selecione um arquivo comum para enviar",
        "Target did not return an image" => "O destino não retornou uma imagem",
        "File search interrupted" => "Busca de arquivos interrompida",
        "Target changed during attachment preparation" => {
            "O destino mudou durante a preparação dos anexos"
        }
        "Attachment preparation cancelled" => "Preparação dos anexos cancelada",
        "The session or line changed during upload; the draft was kept" => {
            "A sessão ou a linha mudou durante o envio. Rascunho mantido"
        }
        "Preparing attachments · Ctrl+C to cancel" => "Preparando anexos · Ctrl+C para cancelar",
        "{reason}; draft retained" => "{reason}. Rascunho mantido",
        "The previous attachment submission is pending; draft retained" => {
            "O envio anterior com anexos está pendente. Rascunho mantido"
        }
        "Submitting attachments" => "Enviando mensagem com anexos",
        "Delivery unconfirmed · Enter on an empty draft to check and retry" => {
            "Envio não confirmado · Enter em um rascunho vazio para verificar e tentar novamente"
        }
        "Delivery unconfirmed: {reason}" => "Envio não confirmado: {reason}",
        "Delivery unconfirmed: {reason}. Enter to check and retry, Ctrl+C to exit" => {
            "Envio não confirmado: {reason}. Enter para verificar e tentar novamente, Ctrl+C para sair"
        }
        "The selected device restarted" => "O dispositivo selecionado reiniciou",
        "The original submission was accepted" => "O envio original foi aceito",
        "The current model does not accept clipboard images; draft retained" => {
            "O modelo atual não aceita imagens da área de transferência. Rascunho mantido"
        }
        "Details unavailable; collapse and expand to retry" => {
            "Detalhes indisponíveis; recolha e expanda para tentar novamente"
        }
        "The response above is incomplete." => "A resposta acima está incompleta.",
        "Could not reach the provider, or the connection dropped" => {
            "Não foi possível alcançar o provedor, ou a conexão caiu"
        }
        "The provider is rate limiting requests" => "O provedor está limitando as solicitações",
        "The provider reported a server error" => "O provedor relatou um erro de servidor",
        "The provider sent a response Dim could not use" => {
            "O provedor enviou uma resposta que o Dim não conseguiu usar"
        }
        "The provider rejected the credentials" => "O provedor rejeitou as credenciais",
        "The account has no quota left" => "A conta não tem mais cota",
        "The provider did not find the model or endpoint" => {
            "O provedor não encontrou o modelo ou o endpoint"
        }
        "The conversation is longer than the model accepts" => {
            "A conversa é mais longa do que o modelo aceita"
        }
        "The provider's content filter blocked the response" => {
            "O filtro de conteúdo do provedor bloqueou a resposta"
        }
        "The provider rejected the request" => "O provedor rejeitou a solicitação",
        "The model stopped at its output limit" => "O modelo parou em seu limite de saída",
        "The model request failed" => "A solicitação ao modelo falhou",
        "Check the network or proxy." => "Verifique a rede ou o proxy.",
        "Wait and try again." => "Aguarde e tente novamente.",
        "Try again later or choose another model." => {
            "Tente novamente mais tarde ou escolha outro modelo."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Tente novamente; se repetir, escolha outro modelo ou verifique o proxy ou o gateway."
        }
        "Sign in again or check the API key." => "Entre novamente ou verifique a API key.",
        "Check the plan or billing." => "Verifique o plano ou o faturamento.",
        "Check the model and the provider's base URL." => {
            "Verifique o modelo e a base URL do provedor."
        }
        "Compact the conversation or start a new session." => {
            "Compacte a conversa ou inicie uma nova sessão."
        }
        "Rephrase the request." => "Reformule a solicitação.",
        "See the details." => "Veja os detalhes.",
        "Increase the output token limit and retry." => {
            "Aumente o limite de tokens de saída e tente novamente."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "Tentando novamente {attempt}/{max} em {seconds} s: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s" => {
            "Tentando novamente {attempt}/{max} em {seconds} s"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "Tentando novamente {attempt}/{max} em {seconds} s · {reason} · Esc interrompe"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "status: {status}",
        "provider code: {code}" => "código do provedor: {code}",
        "request id: {id}" => "id da solicitação: {id}",
        "tried {count} times over {seconds} s" => "{count} tentativas em {seconds} s",
        "the provider asked to wait {seconds} s" => "o provedor pediu para aguardar {seconds} s",
        "cause:" => "causa:",
        "Tried {count} times over {seconds} s." => "{count} tentativas em {seconds} s.",
        "Waiting for the provider" => "Aguardando o provedor",
        "Waiting for the provider · {reason} · Esc stops" => {
            "Aguardando o provedor · {reason} · Esc interrompe"
        }
        _ => return None,
    })
}
