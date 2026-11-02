pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "该守护进程不提供插件运行时（{version}）"
        }
        "Plugin runtimes" => "插件运行时",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter 安装或应用 · s 来源 · a 自动 · r 重试 · Esc 关闭"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc 关闭 · 关闭面板不会中断正在进行的获取"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter 重试 · s 把 {name} 切换到 {source} · Esc 关闭"
        }
        "Loading plugin runtimes..." => "正在加载插件运行时…",
        "acquiring" => "获取中",
        "starting" => "启动中",
        "down" => "已下线",
        "1 plugin waits for {runtime}: {names}" => "1 个插件等待 {runtime}：{names}",
        "{count} plugins wait for {runtime}: {names}" => "{count} 个插件等待 {runtime}：{names}",
        "show, install or configure the plugin runtimes" => "查看、安装或配置插件运行时",
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "用法：/runtime [install 名称 | source 名称 download|system | auto 名称 on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter 安装或应用 · s 来源 · r 重试 · Esc 关闭"
        }
        "auto on" => "自动开",
        "auto off" => "自动关",
        _ => return None,
    })
}
