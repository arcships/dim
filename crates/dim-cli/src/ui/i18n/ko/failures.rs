//! Korean model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "The response above is incomplete." => "위 응답은 불완전합니다.",
        "Could not reach the provider, or the connection dropped" => {
            "공급자에 연결할 수 없거나 연결이 끊어졌습니다"
        }
        "The provider is rate limiting requests" => "공급자가 요청을 속도 제한하고 있습니다",
        "The provider reported a server error" => "공급자가 서버 오류를 보고했습니다",
        "The provider sent a response Dim could not use" => {
            "공급자가 Dim이 사용할 수 없는 응답을 보냈습니다"
        }
        "The provider rejected the credentials" => "공급자가 자격 증명을 거부했습니다",
        "The account has no quota left" => "계정에 남은 할당량이 없습니다",
        "The provider did not find the model or endpoint" => {
            "공급자가 모델 또는 엔드포인트를 찾지 못했습니다"
        }
        "The conversation is longer than the model accepts" => {
            "대화가 모델이 허용하는 길이를 초과했습니다"
        }
        "The provider's content filter blocked the response" => {
            "공급자의 콘텐츠 필터가 응답을 차단했습니다"
        }
        "The provider rejected the request" => "공급자가 요청을 거부했습니다",
        "The model stopped at its output limit" => "모델이 출력 한도에서 중지되었습니다",
        "The model request failed" => "모델 요청이 실패했습니다",
        "Check the network or proxy." => "네트워크 또는 프록시를 확인하세요.",
        "Wait and try again." => "잠시 후 다시 시도하세요.",
        "Try again later or choose another model." => {
            "나중에 다시 시도하거나 다른 모델을 선택하세요."
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "다시 시도하세요. 반복되면 다른 모델을 선택하거나 프록시 또는 게이트웨이를 확인하세요."
        }
        "Sign in again or check the API key." => "다시 로그인하거나 API key를 확인하세요.",
        "Check the plan or billing." => "요금제 또는 결제를 확인하세요.",
        "Check the model and the provider's base URL." => "모델과 공급자 base URL을 확인하세요.",
        "Compact the conversation or start a new session." => {
            "대화를 압축하거나 새 세션을 시작하세요."
        }
        "Rephrase the request." => "요청을 다시 작성하세요.",
        "See the details." => "세부 정보를 확인하세요.",
        "Increase the output token limit and retry." => {
            "출력 token 한도를 높인 후 다시 시도하세요."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "{seconds}초 후 {attempt}/{max}번째 재시도: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "{seconds}초 후 {attempt}/{max}번째 재시도 · {reason} · Esc 중지"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "상태: {status}",
        "provider code: {code}" => "공급자 code: {code}",
        "request id: {id}" => "요청 id: {id}",
        "tried {count} times over {seconds} s" => "{count}회 시도, {seconds}초 경과",
        "the provider asked to wait {seconds} s" => "공급자가 {seconds}초 대기를 요청했습니다",
        "cause:" => "원인:",
        "Tried {count} times over {seconds} s." => "{count}회 시도, {seconds}초 경과.",
        "Waiting for the provider · {reason} · Esc stops" => {
            "공급자를 기다리는 중 · {reason} · Esc 중지"
        }
        _ => return None,
    })
}
