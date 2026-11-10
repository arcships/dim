pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "모델 목록 새로 고침",
        "Model list refreshed" => "모델 목록을 새로 고쳤습니다",
        "Model list refresh failed: {reason}" => "모델 목록을 새로 고치지 못했습니다: {reason}",
        "Refreshing model list… · esc close" => "모델 목록을 새로 고치는 중… · Esc 닫기",
        // providers_panel.rs
        "Providers" => "프로바이더",
        "Enter open · Esc clear/close" => "Enter 열기 · Esc 지우기/닫기",
        "↑↓/Tab move · Enter activate · Esc back" => "↑↓/Tab 이동 · Enter 실행 · Esc 뒤로",
        "Loading providers..." => "프로바이더 불러오는 중…",
        "No matching providers." => "일치하는 프로바이더 없음",
        "Custom providers are not available yet" => "사용자 지정 프로바이더 아직 미지원",
        "Testing connection..." => "연결 테스트 중…",
        "Connect {providerId}" => "{providerId} 연결",
        "(default)" => "(기본값)",
        "(required)" => "(필수)",
        "Test + apply" => "테스트 후 적용",
        "Testing sends the stored credential to this URL." => {
            "테스트 시 저장된 자격 증명을 이 URL로 전송"
        }
        "Disconnect" => "연결 해제",
        "Enable provider" => "프로바이더 활성화",
        "Disable provider" => "프로바이더 비활성화",
        "Error: {error}" => "오류: {error}",
        "disabled" => "비활성",
        "current" => "현재",
        "Sign in" => "로그인",
        "ready" => "준비됨",
        "error" => "오류",
        "not connected" => "연결 안 됨",
        "Add custom provider" => "사용자 지정 프로바이더 추가",
        // oauth_view.rs
        "Checking account…" => "계정 확인 중…",
        "Not signed in" => "로그인 안 됨",
        "Signed in as {account}" => "{account}(으)로 로그인됨",
        "Open models" => "모델 열기",
        "View plan" => "요금제 보기",
        "Sign out" => "로그아웃",
        "Checking usage…" => "사용량 확인 중…",
        "Usage: {reason}" => "사용량: {reason}",
        "Credits" => "크레딧",
        "unlimited" => "무제한",
        "{hours}h window" => "{hours}시간 창",
        "/month" => "/월",
        "/year" => "/년",
        "expires {date}" => "{date} 만료",
        "resets {date}" => "{date} 초기화",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ 이동 · enter 실행 · ctrl+r 새로고침 · esc 닫기"
        }
        "Login with DimAgent" => "DimAgent로 로그인",
        "Enterprise login" => "기업 로그인",
        "Device login" => "기기 로그인",
        "Email" => "이메일",
        "Continue" => "계속",
        "Browser login" => "브라우저 로그인",
        "Restart device login" => "기기 로그인 재시작",
        "Select to copy · Enter opens link" => "선택하여 복사 · Enter로 링크 열기",
        "Link shortened · Enter opens link" => "링크 축약됨 · Enter로 전체 열기",
        "Callback URL" => "콜백 URL",
        "Submit callback" => "콜백 제출",
        "The browser could not be opened. Open the link manually, then return here." => {
            "브라우저 열기 실패. 링크를 직접 열고 여기로 복귀"
        }
        "Waiting for authorization" => "승인 대기 중",
        "{name} device login" => "{name} 기기 로그인",
        "Open this URL: {url}" => "이 URL 열기: {url}",
        "Enter code:    {code}" => "코드 입력:    {code}",
        "Waiting after server slow-down" => "서버 감속 요청으로 대기 중",
        "OAuth login cancelled" => "OAuth 로그인 취소됨",
        "Enter your work email" => "회사 이메일 입력",
        "Invalid OAuth state - possible CSRF attempt" => "잘못된 OAuth state - CSRF 시도 가능성",
        "Preparing login... · esc cancel" => "로그인 준비 중… · esc 취소",
        "Completing login... · esc cancel" => "로그인 완료 중… · esc 취소",
        "enter restart · esc back" => "enter 재시작 · esc 뒤로",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "값 입력 · ↑↓/tab 이동 · enter 제출 · esc 취소"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ 이동 · enter 실행 · esc 뒤로",
        "↑↓ move · enter activate · esc close" => "↑↓ 이동 · enter 실행 · esc 닫기",
        "(paste here)" => "(붙여넣기)",
        _ => return None,
    })
}
