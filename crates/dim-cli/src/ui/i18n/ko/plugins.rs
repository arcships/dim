pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "플러그인",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter 켜기/끄기 · r 다시 로드 · Ctrl+R 새로 고침 · Esc 닫기"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "잠김: Dim에 필요하므로 끌 수 없습니다."
        }
        "Loading plugins..." => "플러그인을 불러오는 중…",
        "No plugins." => "플러그인이 없습니다.",
        "Working..." => "처리 중…",
        "locked" => "잠김",
        "Uses {use}" => "사용 중 {use}",
        "Candidates: {candidates}" => "후보: {candidates}",
        "Waiting for: {names}" => "대기 중: {names}",
        "Refused {kind}: {error}" => "{kind} 거부됨: {error}",
        "Turn off {name}?" => "{name}을(를) 끌까요?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "다시 켤 때까지 새 턴에서 해당 도구와 콘텐츠를 쓸 수 없습니다."
        }
        "Turn off" => "끄기",
        "Turned on {name}." => "{name}을(를) 켰습니다.",
        "Turned off {name}." => "{name}을(를) 껐습니다.",
        "Reloaded {name}." => "{name}을(를) 다시 로드했습니다.",
        "Installed {name} ({id})." => "{name}({id})을(를) 설치했습니다.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name}({id})을(를) 설치했습니다. 켜지지 않았습니다."
        }
        "Installing {path}..." => "{path} 설치 중…",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "재시작 필요: App Server를 다시 시작하면 로드됩니다(dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} 건너뜀: 담당 모듈이 실행 중이 아닙니다."
        }
        "Usage: /plugins [install PATH]" => "사용법: /plugins [install 경로]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "플러그인 목록, 켜기/끄기, 다시 로드 (install 경로)"
        }
        _ => return None,
    })
}
