pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "이 데몬은 플러그인 런타임을 제공하지 않습니다({version})"
        }
        "Plugin runtimes" => "플러그인 런타임",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter 설치 또는 적용 · s 소스 · a 자동 · r 재시도 · Esc 닫기"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc 닫기 · 닫아도 진행 중인 설치는 계속됩니다"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter 재시도 · s {name}을(를) {source}(으)로 전환 · Esc 닫기"
        }
        "Loading plugin runtimes..." => "플러그인 런타임을 불러오는 중…",
        "acquiring" => "가져오는 중",
        "starting" => "시작 중",
        "down" => "중단됨",
        "1 plugin waits for {runtime}: {names}" => "{runtime}을(를) 기다리는 플러그인 1개: {names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{runtime}을(를) 기다리는 플러그인 {count}개: {names}"
        }
        "show, install or configure the plugin runtimes" => "플러그인 런타임 보기, 설치 또는 설정",
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "사용법: /runtime [install 이름 | source 이름 download|system | auto 이름 on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter 설치 또는 적용 · s 소스 · r 재시도 · Esc 닫기"
        }
        "auto on" => "자동 켜기",
        "auto off" => "자동 끄기",
        _ => return None,
    })
}
