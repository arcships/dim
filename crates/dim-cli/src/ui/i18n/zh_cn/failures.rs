//! Model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "The response above is incomplete." => "以上响应不完整。",
        "Could not reach the provider, or the connection dropped" => "无法连接提供方，或连接中断",
        "The provider is rate limiting requests" => "提供方正在限流",
        "The provider reported a server error" => "提供方报告了服务器错误",
        "The provider sent a response Dim could not use" => "提供方发来的响应 Dim 无法使用",
        "The provider rejected the credentials" => "提供方拒绝了凭据",
        "The account has no quota left" => "账户已无可用配额",
        "The provider did not find the model or endpoint" => "提供方找不到模型或端点",
        "The conversation is longer than the model accepts" => "对话超过模型可接受的长度",
        "The provider's content filter blocked the response" => "提供方的内容过滤拦截了响应",
        "The provider rejected the request" => "提供方拒绝了请求",
        "The model stopped at its output limit" => "模型达到输出上限",
        "The model request failed" => "模型请求失败",
        "Check the network or proxy." => "检查网络或代理。",
        "Wait and try again." => "稍后重试。",
        "Try again later or choose another model." => "稍后重试或换一个模型。",
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "重试；若反复出现，换一个模型或检查代理或网关。"
        }
        "Sign in again or check the API key." => "重新登录或检查 API key。",
        "Check the plan or billing." => "检查套餐或计费。",
        "Check the model and the provider's base URL." => "检查模型和提供方 base URL。",
        "Compact the conversation or start a new session." => "压缩对话或新开会话。",
        "Rephrase the request." => "改写请求。",
        "See the details." => "查看详情。",
        "Increase the output token limit and retry." => "提高输出 token 上限后重试。",
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "{seconds} 秒后第 {attempt}/{max} 次重试：{reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "{seconds} 秒后第 {attempt}/{max} 次重试 · {reason} · Esc 停止"
        }
        "kind: {kind}" => "kind：{kind}",
        "status: {status}" => "状态：{status}",
        "provider code: {code}" => "提供方 code：{code}",
        "request id: {id}" => "请求 id：{id}",
        "tried {count} times over {seconds} s" => "已尝试 {count} 次，历时 {seconds} 秒",
        "the provider asked to wait {seconds} s" => "提供方要求等待 {seconds} 秒",
        "cause:" => "原因：",
        "Tried {count} times over {seconds} s." => "已尝试 {count} 次，历时 {seconds} 秒。",
        "Waiting for the provider · {reason} · Esc stops" => {
            "正在等待提供方 · {reason} · Esc 停止"
        }
        _ => return None,
    })
}
