//! Traditional Chinese model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Details unavailable; collapse and expand to retry" => "詳細資料無法載入；收合後展開重試",
        "The response above is incomplete." => "以上回應不完整。",
        "Could not reach the provider, or the connection dropped" => "無法連線至提供方，或連線中斷",
        "The provider is rate limiting requests" => "提供方正在限流",
        "The provider reported a server error" => "提供方回報了伺服器錯誤",
        "The provider sent a response Dim could not use" => "提供方傳來的回應 Dim 無法使用",
        "The provider rejected the credentials" => "提供方拒絕了憑證",
        "The account has no quota left" => "帳戶已無可用配額",
        "The provider did not find the model or endpoint" => "提供方找不到模型或端點",
        "The conversation is longer than the model accepts" => "對話超過模型可接受的長度",
        "The provider's content filter blocked the response" => "提供方的內容過濾攔截了回應",
        "The provider rejected the request" => "提供方拒絕了請求",
        "The model stopped at its output limit" => "模型達到輸出上限",
        "The model request failed" => "模型請求失敗",
        "Check the network or proxy." => "檢查網路或代理。",
        "Wait and try again." => "稍後重試。",
        "Try again later or choose another model." => "稍後重試或換一個模型。",
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "重試；若反覆出現，換一個模型或檢查代理或閘道。"
        }
        "Sign in again or check the API key." => "重新登入或檢查 API key。",
        "Check the plan or billing." => "檢查方案或計費。",
        "Check the model and the provider's base URL." => "檢查模型和提供方 base URL。",
        "Compact the conversation or start a new session." => "壓縮對話或新開工作階段。",
        "Rephrase the request." => "改寫請求。",
        "See the details." => "查看詳情。",
        "Increase the output token limit and retry." => "提高輸出 token 上限後重試。",
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "{seconds} 秒後第 {attempt}/{max} 次重試：{reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "{seconds} 秒後第 {attempt}/{max} 次重試 · {reason} · Esc 停止"
        }
        "kind: {kind}" => "kind：{kind}",
        "status: {status}" => "狀態：{status}",
        "provider code: {code}" => "提供方 code：{code}",
        "request id: {id}" => "請求 id：{id}",
        "tried {count} times over {seconds} s" => "已嘗試 {count} 次，歷時 {seconds} 秒",
        "the provider asked to wait {seconds} s" => "提供方要求等待 {seconds} 秒",
        "cause:" => "原因：",
        "Tried {count} times over {seconds} s." => "已嘗試 {count} 次，歷時 {seconds} 秒。",
        "Waiting for the provider · {reason} · Esc stops" => {
            "正在等待提供方 · {reason} · Esc 停止"
        }
        _ => return None,
    })
}
