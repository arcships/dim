pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "에이전트 실행 중",
        "AskUser is waiting for an answer." => "AskUser 답변 대기 중",
        "A permission request is waiting." => "권한 요청 대기 중",
        "Background work may still be active." => "백그라운드 작업 실행 중일 수 있음",
        "Exit dimcode?" => "dimcode를 종료할까요?",
        "Exit" => "종료",
        "Stay" => "취소",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y 종료 · n 취소 · ←→ 이동 · enter 확인"
        }
        "Restart dimcode?" => "dimcode를 재시작할까요?",
        "Restart" => "재시작",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y 재시작 · n 취소 · ←→ 이동 · enter 확인"
        }
        // upgrade_dialog.rs
        "Update available" => "업데이트 있음",
        "Version {latest} is available (you have {current})." => {
            "{latest} 버전 출시 (현재 {current})"
        }
        "Will run: {command}" => "실행 예정: {command}",
        "To update, run: {command}" => "업데이트하려면 실행: {command}",
        "Update now" => "지금 업데이트",
        "Skip this version" => "이 버전 건너뛰기",
        "Skipped {version}." => "{version} 건너뜀.",
        "Later" => "나중에",
        "Close" => "닫기",
        "Restart now" => "지금 재시작",
        "Updating to {version}..." => "{version}(으)로 업데이트 중…",
        "Updated to {version}. Restart DimCode to use it." => {
            "{version}(으)로 업데이트됨. DimCode 재시작 시 적용"
        }
        "Update failed: {error}" => "업데이트 실패: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ 이동 · Enter 확인 · Esc 닫기",
        // prompts.rs: permission request
        "Permission required" => "권한 필요",
        "Allow once" => "한 번 허용",
        "Allow always" => "항상 허용",
        "Allow for this session" => "이 세션에서 허용",
        "Reject" => "거부",
        "Approve for me" => "스마트 권한",
        "Full access" => "전체 액세스",
        "Run {tool}" => "{tool} 실행",
        "Changing the approvals mode from here is not yet available" => {
            "여기서 승인 모드 변경 아직 미지원"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y 허용 · n 거부 · ←→/Tab 이동 · Enter 확인"
        }
        "Enter submit · Tab edit · / commands · Esc decline" => {
            "Enter 제출 · Tab 편집 · / 명령 · Esc 거절"
        }
        "Space toggle · Tab next · / commands · Esc decline" => {
            "Space 토글 · Tab 다음 · / 명령 · Esc 거절"
        }
        "1-9 select · Tab next · / commands · Esc decline" => {
            "1-9 선택 · Tab 다음 · / 명령 · Esc 거절"
        }
        "Question" => "질문",
        "Question on branch {branch}" => "브랜치 {branch}의 질문",
        "Permission on branch {branch}" => "브랜치 {branch} 권한",
        "Sending answer…" => "답변 전송 중…",
        "Question not ready; press Enter to send again" => "질문 준비 안 됨, Enter로 재전송",
        // prompts.rs: AskUser
        "Confirm" => "확인",
        "Enter save · Esc back" => "Enter 저장 · Esc 뒤로",
        "Enter submit · 1-9 edit · Tab/←→ back · / commands · Esc decline" => {
            "Enter 제출 · 1-9 수정 · Tab/←→ 뒤로 · / 명령 · Esc 거절"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · / commands · Esc decline" => {
            "↑↓ 이동 · Space 토글 · Enter 다음 · Tab/←→ 전환 · / 명령 · Esc 거절"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · / commands · Esc decline" => {
            "↑↓ 이동 · 1-9 선택 · Enter 다음 · Tab/←→ 전환 · / 명령 · Esc 거절"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion: 잘못된 데이터 수신",
        "Missing answer for Q{index}" => "Q{index}에 대한 답변 없음",
        "Type your own answer" => "직접 답변 입력",
        "Review your answers and press Enter to submit." => "답변 확인 후 Enter로 제출",
        "Not answered" => "답변 안 함",
        // approvals_panel.rs
        "Tool Approvals" => "도구 승인",
        "Approval" => "승인",
        "Ask" => "확인",
        "Ask before commands, network, outside writes and unclassified tools" => {
            "명령, 네트워크, 작업 공간 밖 쓰기와 미분류 도구는 실행 전에 확인"
        }
        "full access" => "전체 액세스",
        "All tools run automatically, bypass permissions" => "모든 도구 자동 실행, 권한 확인 생략",
        "approve for me" => "스마트 권한",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "에이전트가 명령과 도구 동작을 자동 검토. 위험한 명령은 확인 요청, 일반 동작은 자동 통과. 안전·스마트·편리."
        }
        "Default permissions" => "기본 권한",
        "default" => "기본",
        "Read tools run automatically, write tools require confirmation" => {
            "읽기 도구는 자동 실행, 쓰기 도구는 확인 필요"
        }
        "current" => "현재",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab 이동 · Enter 적용 · Esc 뒤로",
        "Confirm Full Access" => "전체 액세스 확인",
        "Cancel" => "취소",
        "Tab switch  Enter confirm  Esc cancel" => "Tab 전환  Enter 확인  Esc 취소",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "확인 단계를 줄이고 민감한 작업, 파일 변경, 외부 명령을 바로 실행 가능. 현재 작업을 신뢰할 때만 사용."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "실행함",
        "running" => "실행 중",
        "read" => "읽음",
        "reading" => "읽는 중",
        "explored" => "탐색함",
        "exploring" => "탐색 중",
        "edited" => "편집함",
        "editing" => "편집 중",
        "changed" => "변경함",
        "changing" => "변경 중",
        "checked on" => "확인함",
        "checking on" => "확인 중",
        "Explored" => "탐색함",
        "Exploring" => "탐색 중",
        "Edited" => "편집함",
        "Editing" => "편집 중",
        "Changed" => "변경함",
        "Changing" => "변경 중",
        // transcript/tool_group.rs: counts
        "1 file" => "파일 1개",
        "{count} files" => "파일 {count}개",
        "… {count} more" => "… 외 {count}개",
        "1 web page" => "웹 페이지 1개",
        "{count} web pages" => "웹 페이지 {count}개",
        "1 skill" => "스킬 1개",
        "{count} skills" => "스킬 {count}개",
        "1 search" => "검색 1회",
        "{count} searches" => "검색 {count}회",
        "1 command" => "명령 1개",
        "{count} commands" => "명령 {count}개",
        "1 task" => "작업 1개",
        "{count} tasks" => "작업 {count}개",
        "1 tool" => "도구 1개",
        "{count} tools" => "도구 {count}개",
        "1 delegation rejected" => "위임 1개 거부됨",
        "{count} delegations rejected" => "위임 {count}개 거부됨",
        "{files} and {pages}" => "{files} 및 {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count}개 실패",
        " · {count} rejected" => " · {count}개 거부됨",
        // transcript/tool_group.rs: call rows
        "Read" => "읽기",
        "List" => "목록",
        "Grep" => "검색",
        "Search the web" => "웹 검색",
        "Read skill" => "스킬 읽기",
        "Search git history" => "git 기록 검색",
        "Edit" => "편집",
        "Write" => "쓰기",
        "Create" => "생성",
        " (detached)" => " (백그라운드)",
        "Checked on background task" => "백그라운드 작업 확인함",
        "Waiting…" => "대기 중…",
        "… +{count} lines" => "… +{count}줄",
        _ => return None,
    })
}
