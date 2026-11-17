//! Japanese model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Details unavailable; collapse and expand to retry" => {
            "詳細を取得できません。閉じてから開き直してください"
        }
        "The response above is incomplete." => "上記の応答は不完全です。",
        "Could not reach the provider, or the connection dropped" => {
            "プロバイダーに接続できないか、接続が切断されました"
        }
        "The provider is rate limiting requests" => {
            "プロバイダーがリクエストをレート制限しています"
        }
        "The provider reported a server error" => "プロバイダーがサーバーエラーを報告しました",
        "The provider sent a response Dim could not use" => {
            "プロバイダーから Dim が使用できない応答が返されました"
        }
        "The provider rejected the credentials" => "プロバイダーが認証情報を拒否しました",
        "The account has no quota left" => "アカウントに残りのクォータがありません",
        "The provider did not find the model or endpoint" => {
            "プロバイダーがモデルまたはエンドポイントを見つけられませんでした"
        }
        "The conversation is longer than the model accepts" => {
            "会話がモデルの受け付ける長さを超えています"
        }
        "The provider's content filter blocked the response" => {
            "プロバイダーのコンテンツフィルターが応答をブロックしました"
        }
        "The provider rejected the request" => "プロバイダーがリクエストを拒否しました",
        "The model stopped at its output limit" => "モデルが出力上限で停止しました",
        "The model request failed" => "モデルリクエストが失敗しました",
        "Check the network or proxy." => "ネットワークまたはプロキシを確認してください。",
        "Wait and try again." => "しばらく待ってから再試行してください。",
        "Try again later or choose another model." => {
            "後で再試行するか、別のモデルを選んでください。"
        }
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "再試行してください。繰り返す場合は、別のモデルを選ぶか、プロキシまたはゲートウェイを確認してください。"
        }
        "Sign in again or check the API key." => {
            "再度サインインするか、API key を確認してください。"
        }
        "Check the plan or billing." => "プランまたは請求を確認してください。",
        "Check the model and the provider's base URL." => {
            "モデルとプロバイダーの base URL を確認してください。"
        }
        "Compact the conversation or start a new session." => {
            "会話を圧縮するか、新しいセッションを開始してください。"
        }
        "Rephrase the request." => "リクエストを書き換えてください。",
        "See the details." => "詳細を確認してください。",
        "Increase the output token limit and retry." => {
            "出力 token 上限を上げて再試行してください。"
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "{seconds} 秒後に {attempt}/{max} 回目の再試行：{reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "{seconds} 秒後に {attempt}/{max} 回目の再試行 · {reason} · Esc 停止"
        }
        "kind: {kind}" => "kind：{kind}",
        "status: {status}" => "ステータス：{status}",
        "provider code: {code}" => "プロバイダー code：{code}",
        "request id: {id}" => "リクエスト id：{id}",
        "tried {count} times over {seconds} s" => "{count} 回試行、{seconds} 秒経過",
        "the provider asked to wait {seconds} s" => {
            "プロバイダーは {seconds} 秒待つよう要求しました"
        }
        "cause:" => "原因：",
        "Tried {count} times over {seconds} s." => "{count} 回試行、{seconds} 秒経過。",
        "Waiting for the provider · {reason} · Esc stops" => {
            "プロバイダーを待機中 · {reason} · Esc 停止"
        }
        _ => return None,
    })
}
