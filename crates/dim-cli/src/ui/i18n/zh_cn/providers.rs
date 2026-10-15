pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // providers_panel.rs
        "Providers" => "模型服务",
        "type filter · enter open · esc clear/close" => "输入筛选 · Enter 打开 · Esc 清除/关闭",
        "type value · ↑↓/tab move · enter activate · esc back" => {
            "输入值 · ↑↓/Tab 移动 · Enter 执行 · Esc 返回"
        }
        "Loading providers..." => "正在加载模型服务…",
        "No matching providers." => "无匹配的模型服务。",
        "Custom providers are not available yet" => "暂不支持自定义模型服务",
        "Testing connection..." => "正在测试连接…",
        "Connect {providerId}" => "连接 {providerId}",
        "(default)" => "（默认）",
        "(required)" => "（必填）",
        "Apply + test" => "应用并测试",
        "Disconnect" => "断开",
        "Enable provider" => "启用模型服务",
        "Disable provider" => "禁用模型服务",
        "Error: {error}" => "错误：{error}",
        "Suggested" => "建议",
        "Connected" => "已连接",
        "disabled" => "已禁用",
        "Browser sign-in · no API key" => "浏览器登录 · 无需 API key",
        "Connect with DimAgent (OAuth)" => "使用 DimAgent 连接（OAuth）",
        "current" => "当前",
        "Sign in" => "需登录",
        "ready" => "就绪",
        "error" => "错误",
        "not connected" => "未连接",
        "Add custom provider" => "添加自定义模型服务",
        // oauth_view.rs
        "Login with DimAgent" => "使用 DimAgent 登录",
        "Enterprise login" => "企业登录",
        "Device login" => "设备登录",
        "Email" => "邮箱",
        "Continue" => "继续",
        "Browser mode" => "浏览器模式",
        "Device mode" => "设备模式",
        "Browser login" => "浏览器登录",
        "Print verification URL + pairing code again" => "再次输出验证 URL 和配对码",
        "Restart device login" => "重新开始设备登录",
        "Print OAuth URL again" => "再次输出 OAuth URL",
        "Callback URL" => "回调 URL",
        "Submit callback" => "提交回调",
        "The browser could not be opened. Open the link manually, then return here." => {
            "无法打开浏览器，请手动打开链接，完成后返回。"
        }
        "Enter code {code} at {url}" => "在 {url} 输入配对码 {code}",
        "Waiting for authorization" => "等待授权",
        "{name} device login" => "{name} 设备登录",
        "Open this URL: {url}" => "打开此 URL：{url}",
        "Enter code:    {code}" => "输入配对码：{code}",
        "Waiting after server slow-down" => "服务端要求减慢检查频率，继续等待",
        "OAuth login cancelled" => "OAuth 登录已取消",
        "Enter your work email" => "请输入工作邮箱",
        "Invalid OAuth state - possible CSRF attempt" => "无效的 OAuth state — 可能有 CSRF 尝试",
        "Preparing login... · esc cancel" => "准备登录… · Esc 取消",
        "Completing login... · esc cancel" => "完成登录… · Esc 取消",
        "enter restart · esc back" => "Enter 重新开始 · Esc 返回",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "输入值 · ↑↓/Tab 移动 · Enter 提交 · Esc 取消"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ 移动 · Enter 执行 · Esc 返回",
        "(paste here)" => "（粘贴到这里）",
        _ => return None,
    })
}
