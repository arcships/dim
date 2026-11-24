pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "대기열",
        "Export session" => "세션 내보내기",
        "Enter steer · x delete held · Esc close" => {
            "Enter steer · x 보류 중인 메시지 삭제 · Esc 닫기"
        }
        "No queued messages." => "대기열에 메시지가 없습니다.",
        "Held · awaiting application" => "보류 중 · 적용 대기",
        "Held" => "보류 중",
        "Unread" => "읽지 않음",
        "Enter export · Esc cancel" => "Enter 내보내기 · Esc 취소",
        "Messages with attachments stay queued; use /queue to steer or delete them" => {
            "첨부 파일이 있는 메시지는 대기열에 남습니다. /queue에서 steer하거나 삭제하세요"
        }
        "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now" => {
            "/queue · Shift+← 마지막 편집 · 초안이 비어 있으면 {key}로 steer · Ctrl+Enter로 즉시 전송"
        }
        "+{count} more · {hint}" => "+{count}개 더 · {hint}",
        "steer or delete queued messages" => "대기열 메시지 steer 또는 삭제",
        "export this conversation" => "이 대화 내보내기",
        "That message has already left the queue." => "해당 메시지는 이미 대기열에서 나갔습니다.",
        "Exported {format}: {path}" => "{format} 내보내기 완료: {path}",
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter 출력/멤버 중단 · x 명령 중지 · Esc 닫기"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter 출력/멤버 중단 · x 명령 중지 · Ctrl+X 모두 중지 · Esc 닫기"
        }
        "Loading branch history... Stop-all unavailable" => {
            "브랜치 기록 불러오는 중… 모두 중지 불가"
        }
        "Branch history failed; reopen to retry" => "브랜치 기록 실패, 다시 열어 재시도",
        "Branch history is incomplete; stop-all unavailable" => {
            "브랜치 기록 불완전, 모두 중지 불가"
        }
        "Running" => "실행 중",
        "Lines" => "라인",
        "Enter show · p continue from here · x end it · Esc close" => {
            "Enter 보기 · p 여기서 계속 · x 종료 · Esc 닫기"
        }
        "Main line" => "메인 라인",
        "Start" => "시작점",
        "the default branch and the lines it continues from stay open; continue on another line first" => {
            "기본 브랜치와 그 브랜치가 이어지는 라인은 닫을 수 없습니다. 먼저 다른 라인에서 계속하세요"
        }
        "The old default branch stays open and keeps the lines it opened. The root keeps its own right to open a line in either direction." => {
            "이전 기본 브랜치는 열린 채로 남고 연 라인을 유지합니다. 루트는 어느 방향에서든 라인을 열 권리를 그대로 가집니다."
        }
        "The old default branch stays open and keeps the lines it opened. The two lines swap the right to open a line." => {
            "이전 기본 브랜치는 열린 채로 남고 연 라인을 유지합니다. 두 라인이 라인을 열 권리를 교환합니다."
        }
        "This session has no lines." => "이 세션에는 아직 라인이 없습니다.",
        "No matching lines." => "일치하는 라인이 없습니다.",
        "the session itself" => "세션 자체",
        "working" => "작업 중",
        "ended itself" => "스스로 종료됨",
        "ended by an operation" => "작업으로 종료됨",
        "ended with {id}" => "{id}와 함께 종료됨",
        "creation failed" => "생성 실패",
        "shown" => "표시 중",
        "line" => "라인",
        "line {id}" => "라인 {id}",
        "jump to a message of the shown line" => "표시 중인 라인의 메시지로 이동",
        "Timeline" => "타임라인",
        "Enter actions · Esc close" => "Enter 작업 · Esc 닫기",
        "Timeline actions" => "타임라인 작업",
        "↑↓ select · Enter confirm · Esc cancel" => "↑↓ 선택 · Enter 확인 · Esc 취소",
        "Jump to message" => "메시지로 이동",
        "Edit as a new draft" => "새 초안으로 편집",
        "Quote message" => "메시지 인용",
        "Create branch from here" => "여기서 브랜치 생성",
        "Before this message" => "이 메시지 이전",
        "Fork session" => "세션 포크",
        "1. Create fork" => "1. 포크 생성",
        "2. Cancel" => "2. 취소",
        "Creates a new session; the current session stays unchanged." => {
            "새 세션을 만듭니다. 현재 세션은 변경되지 않습니다."
        }
        "No messages yet." => "아직 메시지가 없습니다.",
        "Already on screen above; /tui jumps to it" => "이미 위에 있음. /tui 에서 바로 이동",
        "{count} earlier messages · Enter or click to load" => {
            "이전 메시지 {count}개 · Enter 또는 클릭으로 불러오기"
        }
        "follow, talk to and end this session's lines" => "이 세션의 라인을 보고, 대화하고, 종료",
        "End line {id}?" => "{id} 라인을 종료할까요?",
        "y end it" => "y 종료",
        "n cancel" => "n 취소",
        "main is the session itself; a line ends, main does not" => {
            "main은 세션 자체입니다. 라인은 종료되지만 main은 그렇지 않습니다"
        }
        "Line has ended:" => "라인이 종료되었습니다:",
        "Esc close" => "Esc 닫기",
        "/{name} needs {argument}" => "/{name}에는 {argument}이(가) 필요합니다",
        "/{name} takes text only; remove {label} to send" => {
            "/{name}은(는) 텍스트만 받습니다. {label}을(를) 지우고 보내세요"
        }
        "e edit · b branch · f fork" => "e 편집 · b 브랜치 · f 포크",
        "e edit" => "e 편집",
        "Fork the session before “{message}”?" => "“{message}” 앞에서 세션을 포크할까요?",
        "Fork the session after the last turn?" => "마지막 턴 뒤에서 세션을 포크할까요?",
        "⑂ new line from {id} · Esc cancel" => "⑂ {id}에서 새 라인 · Esc 취소",
        "A fork copies main; b opens a line here" => {
            "포크는 main만 복사합니다. b로 여기서 라인을 여세요"
        }
        "A line opens from text; images and files were not included" => {
            "라인은 텍스트로 열립니다. 이미지와 파일은 포함되지 않았습니다"
        }
        "No finished turn to cut at yet" => "잘라낼 끝난 턴이 아직 없습니다",
        "forked {id} · {count} messages" => "{id} 포크됨 · 메시지 {count}개",
        "after message {count}" => "메시지 {count}개 이후",
        "open a line from the end of the shown line" => "표시 중인 라인의 끝에서 라인 열기",
        "fork the session at its last finished turn" => "마지막으로 끝난 턴에서 세션 포크",
        "rename this session" => "이 세션 이름 바꾸기",
        "Renamed to {title}" => "{title}(으)로 이름을 바꿨습니다",
        "The Session is now untitled." => "세션에 이제 제목이 없습니다.",
        "This daemon does not support renaming Sessions" => {
            "이 데몬은 세션 이름 바꾸기를 지원하지 않습니다"
        }
        "Renaming failed" => "이름 바꾸기에 실패했습니다",
        "Delete \"{name}\"?" => "“{name}”을(를) 삭제할까요?",
        "Deleted {name}." => "{name} 삭제했습니다.",
        "Nothing was removed: no stored Session {name}." => {
            "삭제된 항목이 없습니다: 저장된 세션 {name}이(가) 없습니다."
        }
        "Delete failed" => "삭제에 실패했습니다",
        "This daemon does not support deleting Sessions" => {
            "이 데몬은 세션 삭제를 지원하지 않습니다"
        }
        "Starting" => "시작 중",
        "Delegation result" => "위임 결과",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter 상세 · x 선택 중지 · Ctrl+X 모두 중지 · Esc 닫기"
        }
        "x interrupt member · Esc back" => "x 멤버 중단 · Esc 뒤로",
        "read-only" => "읽기 전용",
        "Member Session" => "멤버 세션",
        "Loading conversation..." => "대화 불러오는 중…",
        "No conversation yet" => "아직 대화 없음",
        "Observing conversation" => "대화 관찰 중",
        "Conversation unavailable" => "대화 볼 수 없음",
        "Session closed" => "세션 종료됨",
        "Member conversation" => "멤버 대화",
        "x stop command · Esc back" => "x 명령 중지 · Esc 뒤로",
        "x stop command · Esc close" => "x 명령 중지 · Esc 닫기",
        "x stop workflow · Esc close" => "x 워크플로 중지 · Esc 닫기",
        "Esc back" => "Esc 뒤로",
        "Tail preview - last 2,000 lines / 50 KiB" => "미리보기 - 마지막 2,000줄 / 50 KiB",
        "Loading output..." => "출력 불러오는 중…",
        "No output yet" => "아직 출력 없음",
        "Live output unavailable" => "실시간 출력 불가",
        "Live output unavailable in this profile" => "이 프로필은 실시간 출력 불가",
        "Output read failed" => "출력 읽기 실패",
        "Completed" => "완료",
        "Failed" => "실패",
        "Cancelled" => "취소됨",
        "workflow" => "워크플로",
        "Stopping" => "중지 중",
        "Stopped" => "중지됨",
        "Interrupted" => "중단됨",
        "last observed" => "마지막 관찰",
        "Loading Workflow runs... Stop-all unavailable" => {
            "워크플로 실행 불러오는 중… 모두 중지 불가"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "워크플로: 마지막 관찰, 새로고침 실패, 다시 열어 재시도"
        }
        "Workflow runs unavailable; reopen to retry" => "워크플로 실행 불가, 다시 열어 재시도",
        "The selected Workflow run is no longer available" => "선택한 워크플로 실행이 더는 없음",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "워크플로 목록 불완전, 모두 중지 불가"
        }
        "Workflow stop unavailable" => "워크플로 중지 불가",
        "Run no longer available; last observed record" => "실행이 더는 없음, 마지막 관찰 기록",
        "x stop workflow · Esc back" => "x 워크플로 중지 · Esc 뒤로",
        "Run" => "실행",
        "Source" => "출처",
        "Stages" => "단계",
        "result" => "결과",
        "Log" => "로그",
        "Report" => "보고서",
        "waiting" => "대기 중",
        "launching" => "시작 중",
        "completed" => "완료",
        "failed" => "실패",
        "stopped" => "중지됨",
        "skipped" => "건너뜀",
        "lost" => "유실됨",
        "none" => "없음",
        "pending" => "대기 중",
        "delivered" => "전달됨",
        "undeliverable" => "전달 불가",
        "Agent is starting; no member Session to stop yet" => {
            "에이전트 시작 중, 아직 중지할 멤버 세션 없음"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "프로바이더 멤버는 리더가 제어, 여기서 중지 불가"
        }
        "Provider members have no conversation to open here" => "프로바이더 멤버는 열 대화 없음",
        "That delegation is no longer in this transcript" => "해당 위임이 대화 기록에 더는 없음",
        "Member model" => "멤버 모델",
        "No model observed" => "관찰된 모델 없음",
        "Observing member..." => "멤버 관찰 중…",
        "{model} (observation unavailable)" => "{model} (관찰 불가)",
        "{model} (Session closed)" => "{model} (세션 종료됨)",
        "{model} (last observed)" => "{model} (마지막 관찰)",
        "Agent: {name}" => "에이전트: {name}",
        "Provider member: {name}" => "프로바이더 멤버: {name}",
        "Task: {task}" => "작업: {task}",
        "Activity: {activity}" => "활동: {activity}",
        "Activity: not observed on this attachment" => "활동: 이번 연결에서 미관찰",
        "Result: {result}" => "결과: {result}",
        "Controlled by the leader" => "리더가 제어",
        "› Open member conversation" => "› 멤버 대화 열기",
        "Not started" => "시작 안 함",
        "Reason: {reason}" => "이유: {reason}",
        "Copied to clipboard" => "클립보드에 복사됨",
        "Opened link" => "링크 열림",
        "Link: {url}" => "링크: {url}",
        "Not available" => "사용 불가",
        "files changed  {count}" => "변경된 파일  {count}",
        "active session" => "활성 세션",
        "new-session draft" => "새 세션 초안",
        "Tool approvals: {title} ({target})" => "도구 승인: {title} ({target})",
        "Disconnected · draft kept" => "연결 끊김 · 초안 보존됨",
        "The App Server closed this interface: {reason}" => "App Server가 화면을 닫음: {reason}",
        "Disconnected; draft kept. Exit and resume this Session." => {
            "연결 끊김, 초안 보존됨. 종료 후 세션 재개"
        }
        "Cannot open this Session; draft kept." => "이 세션을 열 수 없습니다, 초안 보존됨.",
        "Disconnected; draft kept. Exit and run: {command}" => {
            "연결 끊김, 초안 보존됨. 종료 후 실행: {command}"
        }
        "Disconnected; command outcome unknown" => "연결 끊김, 명령 결과 알 수 없음",
        "Message queued" => "메시지 대기 중",
        "Run a shell command · Esc to exit" => "shell 명령 실행 · Esc 나가기",
        "! shell mode" => "! shell 모드",
        "!! shell mode · not sent to the model" => "!! shell 모드 · 모델에 전송 안 됨",
        "Leave shell mode to quote a message" => "인용하려면 shell 모드 종료",
        "A shell command is already running" => "shell 명령 이미 실행 중",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "실행 중… · Ctrl+C 중단 · {seconds}s",
        "exit code {code}" => "종료 코드 {code}",
        "killed by {signal}" => "{signal}로 종료됨",
        "interrupted" => "중단됨",
        "done" => "완료",
        "failed: {reason}" => "실패: {reason}",
        "Shell output was not recorded: {reason}" => "Shell 출력 기록 안 됨: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "클립보드 읽는 중…",
        "Reading image…" => "이미지 읽는 중…",
        "Wait for the image insertion before sending" => "이미지 삽입 후 전송하세요",
        "The image could not be read" => "이미지를 읽을 수 없습니다",
        "An image is still being inserted; paste again when it is in" => {
            "이미지 삽입 중입니다. 완료 후 다시 붙여넣으세요"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "클립보드 삽입 중입니다. 완료 후 다시 붙여넣으세요"
        }
        "Clipboard: {reason}" => "클립보드: {reason}",
        "Wait for clipboard insertion before sending" => "클립보드 삽입 완료 후 전송",
        "A submission accepts at most 20 images" => "한 번에 이미지 최대 20개",
        "first frame" => "첫 프레임만",
        "re-encoded" => "재인코딩됨",
        "The current model does not accept images; remove {label} to send" => {
            "현재 모델은 이미지 미지원, {label} 삭제 후 전송"
        }
        "Files" => "파일",
        "Searching…" => "검색 중…",
        "type filter · enter select · esc clear/close" => {
            "필터 입력 · enter 선택 · esc 지우기/닫기"
        }
        "↑↓ select · enter/tab insert · esc close" => "↑↓ 선택 · enter/tab 삽입 · esc 닫기",
        "not sent to the model" => "모델에 전송 안 됨",
        "… {count} more lines" => "… {count}줄 더",
        "… {count} earlier lines" => "… 이전 {count}줄",
        "Interrupt requested; Ctrl+C again to exit" => "중단 요청됨, Ctrl+C 다시 누르면 종료",
        "Press Ctrl+C again to exit" => "Ctrl+C 다시 누르면 종료",
        "User interrupted the run." => "사용자가 실행을 중단함",
        "Could not save the language: {error}" => "언어 저장 실패: {error}",
        "Language set to {locale}" => "언어 설정됨: {locale}",
        "Language set to system default" => "언어 설정됨: 시스템 기본값",
        "Connected {provider}" => "{provider} 연결됨",
        "Opened {url}" => "{url} 열림",
        "Tool details not saved: {error}" => "도구 상세 저장 실패: {error}",
        "Thinking details not saved: {error}" => "생각 상세 저장 실패: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name}에는 세션 필요 — 먼저 메시지 전송"
        }
        "Nothing is running" => "실행 중인 작업 없음",
        "No queued message" => "대기 중인 메시지 없음",
        "Only in fullscreen (/tui)" => "전체 화면에서만 가능 (/tui)",
        "new session" => "새 세션",
        "resumed {id} · {count} messages" => "{id} 재개됨 · 메시지 {count}개",
        " · earlier history: /timeline" => " · 이전 기록: /timeline",
        "Error: {message}" => "오류: {message}",
        "working…  {elapsed}" => "작업 중…  {elapsed}",
        "thinking…  {elapsed}" => "생각 중…  {elapsed}",
        "Ask anything... · Ctrl+J newline" => "무엇이든 물어보세요… · Ctrl+J 줄바꿈",
        "Earlier history · {id} · {count} messages" => "이전 기록 · {id} · 메시지 {count}개",
        "No active session" => "활성 세션 없음",
        "answer required" => "답변 필요",
        "permission required" => "권한 필요",
        "esc interrupt" => "esc 중단",
        "shift+← edit last · ctrl+s steer now" => "shift+← 마지막 편집 · ctrl+s 지금 steer",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count}개 더 · shift+← 마지막 편집 · ctrl+s 지금 steer"
        }
        "Queue ({count})" => "대기열 ({count})",
        "Thinking…" => "생각 중…",
        "Thought" => "생각함",
        "Thought for {duration}" => "{duration} 생각함",
        "Welcome to DIM {version}!" => "DIM {version}에 오신 것을 환영합니다!",
        "Type / for commands · Ctrl+O for the palette" => "/ 명령 · Ctrl+O 팔레트",
        "Workspace" => "작업 공간",
        "Git branch" => "Git 브랜치",
        "Bye!" => "안녕히 가세요!",
        "DIM Web is ready" => "DIM Web 준비됨",
        "Opened in your browser" => "브라우저에서 열림",
        "Open the sign-in link below" => "아래 로그인 링크 열기",
        "Web" => "Web",
        "Sign-in link:" => "로그인 링크:",
        "Resume" => "재개",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C로 Web 작업 공간 중지",
        "Feedback or ideas? Join us on Discord:" => {
            "피드백이나 아이디어가 있다면 Discord로 오세요:"
        }
        "To resume this session:" => "세션 재개:",

        // commands.rs: descriptions
        "start a new session" => "새 세션 시작",
        "show or set the reasoning effort" => "추론 강도 보기/설정",
        "show or set the priority service tier (on|off)" => "우선 서비스 등급 보기/설정 (on|off)",
        "list agents or switch this session's agent" => "에이전트 목록 또는 세션 에이전트 전환",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "MCP 서버 목록, 로그인/로그아웃 (login|logout ID)"
        }
        "resume a previous session" => "이전 세션 재개",
        "turn the web interface on or off" => "웹 인터페이스 켜기 또는 끄기",
        "show the context window usage" => "컨텍스트 창 사용량 보기",
        "interrupt the run" => "실행 중단",
        "steer with the next queued message" => "다음 대기 메시지로 steer",
        "scroll to the top" => "맨 위로 스크롤",
        "scroll to the bottom" => "맨 아래로 스크롤",
        "pick an earlier message to edit or quote" => "편집/인용할 이전 메시지 선택",
        "cycle tool details and compaction summaries" => "도구 상세와 압축 요약 순환 전환",
        "switch between fullscreen and scrollback" => "전체 화면/스크롤백 전환",
        "switch theme" => "테마 전환",
        "switch language" => "언어 전환",
        "select a model" => "모델 선택",
        "connect a provider" => "프로바이더 연결",
        "sign in to DimAgent" => "DimAgent에 로그인",
        "DimAgent account · usage · sign out" => "DimAgent 계정 · 사용량 · 로그아웃",
        "configure tool approvals" => "도구 승인 설정",
        "choose ask or full access" => "확인 또는 전체 액세스 선택",
        "open settings" => "설정 열기",
        "exit" => "종료",

        // composer/palette.rs
        "Default for new sessions in {folder}: {model}" => "새 세션의 {folder} 기본 모델: {model}",
        "Default for new sessions: {model}" => "새 세션 기본 모델: {model}",
        "Enter select · Esc clear/close" => "Enter 선택 · Esc 지우기/닫기",
        "esc close" => "esc 닫기",
        "Suggestions" => "추천",
        "Skills" => "스킬",
        "Skill loaded: {name}" => "스킬 로드됨: {name}",
        "Hook context · {count} line" => "훅 컨텍스트 · {count}줄",
        "Hook context · {count} lines" => "훅 컨텍스트 · {count}줄",
        "Stop hook follow-up · {count} line" => "Stop 훅 후속 입력 · {count}줄",
        "Stop hook follow-up · {count} lines" => "Stop 훅 후속 입력 · {count}줄",
        "Commands" => "명령",
        "Command Palette" => "명령 팔레트",
        "No matches" => "결과 없음",
        " (unavailable)" => " (사용 불가)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[텍스트: {preview} ({count}자)]",
        "[... {count} lines]" => "[… {count}줄]",
        "pasted text" => "붙여넣은 텍스트",

        // list_panel.rs

        // settings_panel.rs
        "on" => "켜짐",
        "off" => "꺼짐",
        "Interface" => "인터페이스",
        "Theme" => "테마",
        "Language" => "언어",
        "Startup UI form" => "시작 UI 형태",
        "Default send mode" => "기본 전송 모드",
        "fullscreen" => "전체 화면",
        "scrollback" => "스크롤백",
        "Tool details" => "도구 상세",
        "Thinking details" => "생각 상세",
        "expanded" => "펼침",
        "collapsed" => "접힘",
        "Mouse (fullscreen)" => "마우스 (전체 화면)",
        "Permissions" => "권한",
        "Tool approvals" => "도구 승인",
        "Execution" => "실행",
        "Models" => "모델",
        "Settings" => "설정",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ 이동 · Enter 선택 · ←→/Space 변경 · Esc 닫기"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => "↑↓ 미리보기 · Enter 저장 · Esc 복원",
        "System default ({locale})" => "시스템 기본값 ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => "↑↓ 미리보기 · Enter 적용 · Esc 취소",
        "System" => "시스템",
        "Could not save the theme: {error}" => "테마를 저장할 수 없음: {error}",

        // sessions_panel.rs
        "Sessions" => "세션",
        "Sessions · This workspace" => "세션 · 이 워크스페이스",
        "Sessions · This repository" => "세션 · 이 저장소",
        "Sessions · All workspaces" => "세션 · 모든 워크스페이스",
        "No saved sessions in this workspace. Ctrl+S shows more." => {
            "이 워크스페이스에 저장된 세션 없음. Ctrl+S로 더 보기."
        }
        "Enter open · Ctrl+X delete · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter 열기 · Ctrl+X 삭제 · Ctrl+S 범위 · Ctrl+R 새로고침 · Esc 닫기"
        }
        "Loading sessions..." => "세션 불러오는 중…",
        "No saved sessions yet." => "저장된 세션 없음",
        "No matching sessions." => "일치하는 세션 없음",
        "now" => "방금",
        "{count}m ago" => "{count}분 전",
        "{count}h ago" => "{count}시간 전",
        "{count}d ago" => "{count}일 전",

        // models_panel.rs
        "Select a model" => "모델 선택",
        "Loading models..." => "모델 불러오는 중…",
        "No models available. Connect a provider first." => {
            "사용 가능한 모델 없음. 먼저 프로바이더 연결"
        }
        "No matching models." => "일치하는 모델 없음",
        "current" => "현재",
        "Edit models" => "모델 편집",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 모델 1개 사용 불가 · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: 모델 {count}개 사용 불가 · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 모델 1개에 윈도가 필요합니다 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 모델 {count}개에 윈도가 필요합니다 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: 모델 없음 · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: 모델 없음 · provider.models.create로 모델 추가"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable}개 사용 불가, {disabled}개 비활성 · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: 카탈로그를 읽을 수 없음 · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "한도가 선언되지 않은 모델은 선택할 수 없습니다. dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N로 선언하세요."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N로 선언하세요."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "캐시된 카탈로그가 없습니다. 로그인한 뒤 models PROVIDER --refresh를 실행하세요."
        }
        "Saved model preferences cleared" => "저장된 모델 환경설정을 지웠습니다",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 모델 1개 사용 안 함 · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: 모델 {count}개 사용 안 함 · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "모델이 없습니다. dimcode models {id} --refresh 로 확인하세요."
        }
        "No models. The provider listed none." => {
            "모델이 없습니다. 이 제공자는 아무것도 나열하지 않았습니다."
        }
        "Enter select · Esc back" => "Enter 선택 · Esc 뒤로",
        "Subagent models" => "서브에이전트 모델",
        "Subagent models: {policy}" => "서브에이전트 모델: {policy}",
        "Fixed subagent model" => "고정 서브에이전트 모델",
        "Agent choice (default)" => "에이전트가 선택 (기본값)",
        "Agent choice" => "에이전트가 선택",
        "Inherit requesting branch" => "요청한 브랜치 상속",
        "Fixed model" => "고정 모델",
        "Applies to new members, including named Agents." => {
            "이름 있는 에이전트 포함, 새 멤버에 적용"
        }
        "Existing members keep their models." => "기존 멤버는 모델 유지",
        "Conversation model" => "대화 모델",
        "Selected: {model}" => "선택됨: {model}",
        "Model management" => "모델 관리",
        "Manage models" => "모델 관리하기",
        "1 provider" => "프로바이더 1개",
        "{count} providers" => "프로바이더 {count}개",
        "ready" => "준비됨",
        "no models" => "모델 없음",
        "catalog unreadable" => "catalog를 읽을 수 없음",
        "{count} with missing limits" => "{count}개 한도 미선언",
        "{count} unavailable" => "{count}개 사용 불가",
        "{count} disabled" => "{count}개 비활성",
        "{count} without models" => "{count}개 모델 없음",
        "{count} unreadable" => "{count}개 읽을 수 없음",
        "{unavailable} unavailable, {disabled} disabled" => {
            "{unavailable}개 사용 불가, {disabled}개 비활성"
        }
        "Enter select + default · ←/→ effort · Tab section · Esc clear/close" => {
            "Enter 선택 및 기본 설정 · ←/→ 추론 강도 · Tab 섹션 · Esc 지우기/닫기"
        }
        "Enter change · Tab section · Esc clear/close" => {
            "Enter 변경 · Tab 섹션 · Esc 지우기/닫기"
        }
        "Enter manage · Tab section · Esc clear/close" => {
            "Enter 관리 · Tab 섹션 · Esc 지우기/닫기"
        }
        "Enter manage · Esc back" => "Enter 관리 · Esc 뒤로",
        "unknown" => "알 수 없음",
        "Subagent model policy saved" => "서브에이전트 모델 정책 저장됨",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "서브에이전트 모델 정책이 외부에서 변경됨, 다시 선택해 새로 읽기"
        }
        "Subagent model policy: {error}" => "서브에이전트 모델 정책: {error}",
        "drawing diagram… {count} lines" => "다이어그램 그리는 중… {count}줄",
        // notify.rs
        "Task complete" => "작업 완료",
        "Background task finished: {label}" => "백그라운드 작업 완료: {label}",
        "Background task failed: {label}" => "백그라운드 작업 실패: {label}",
        "Background task stopped: {label}" => "백그라운드 작업 중지됨: {label}",
        "Background tasks" => "백그라운드 작업",
        "Nothing runs in the background." => "백그라운드에서 실행 중인 작업이 없습니다.",
        "No matching tasks." => "일치하는 작업이 없습니다.",
        "Permission needed: {tool}" => "권한 필요: {tool}",
        "Question waiting for your answer" => "답변 대기 중인 질문",
        "Notify when agent needs you" => "에이전트 요청 시 알림",
        "Show ACP sessions" => "ACP 세션 표시",
        "Prompt suggestion" => "프롬프트 제안",
        "Prompt suggestion: {error}" => "프롬프트 제안: {error}",
        "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume." => {
            "이 세션에서는 제안이 사용되지 않아 프롬프트 제안이 일시 중지되었습니다. /settings에서 끄고 다시 켜면 재개됩니다."
        }
        // keys.rs
        "show the key bindings" => "단축키 보기",
        "check for a newer version" => "새 버전 확인",
        "Application keys" => "앱 단축키",
        "Enter submit when idle · Shift+Enter / Alt+Enter / Ctrl+J newline · ↑↓ history" => {
            "대기 중 Enter로 제출 · Shift+Enter / Alt+Enter / Ctrl+J 줄바꿈 · ↑↓ 기록"
        }
        "Running with a draft: Enter {default} · Ctrl+S {alternate}" => {
            "실행 중 초안 전송: Enter {default} · Ctrl+S {alternate}"
        }
        "Empty draft: {keys} steer first queued" => {
            "초안이 비어 있으면 {keys}로 대기열 첫 메시지 steer"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Shift+← edit last queued" => {
            "Ctrl+O/Ctrl+P 명령 · Esc 중단, 대기 메시지는 입력란으로 복귀 · Shift+← 마지막 대기 메시지 편집"
        }
        "Ctrl+Enter interrupt and send now: the draft and every held message, as one message" => {
            "Ctrl+Enter로 중단 후 즉시 전송: 초안과 보류 중인 모든 메시지를 하나로 보냅니다"
        }
        "Send now failed: {error}; the draft was kept" => {
            "즉시 전송 실패: {error}. 초안은 유지되었습니다"
        }
        "Send now failed: {error}" => "즉시 전송 실패: {error}",
        "Send now cancelled; the draft was kept" => {
            "즉시 전송이 취소되었습니다. 초안은 유지되었습니다"
        }
        "Send now is still stopping the run; the draft was kept" => {
            "즉시 전송이 아직 실행을 중지하는 중입니다. 초안은 유지되었습니다"
        }
        "Send now is stopping the run; paste again when the draft is sent" => {
            "즉시 전송이 실행을 중지하는 중입니다. 초안이 전송된 뒤에 붙여넣으세요"
        }
        "Send now is waiting for the insertion before sending" => {
            "즉시 전송이 삽입을 기다린 뒤 전송합니다"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 지우기, 두 번 누르면 종료 · 빈 초안에서 Ctrl+D 종료"
        }
        "Shift+Tab cycle permissions (composer focused)" => {
            "Shift+Tab 권한 순환 전환 (입력창에 포커스가 있을 때)"
        }
        "Shift+Tab newer target while picked · Esc return to composer" => {
            "선택 중 Shift+Tab으로 더 최근 항목 선택 · Esc 입력창으로 돌아가기"
        }
        "(shift+tab to cycle)" => "(Shift+Tab으로 전환)",
        "Permission unavailable" => "권한을 사용할 수 없음",
        "Fullscreen" => "전체 화면",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown 스크롤 · Home/End 맨 위/아래 (초안이 비어 있을 때) · Tab 메시지 선택, e 편집, q 인용"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "휠 스크롤 · 드래그 선택·복사 · 링크 클릭 시 열기, 도구 그룹 클릭 시 펼치기"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "마우스 꺼짐 (/settings): 터미널이 선택·복사"
        }
        "Terminal-owned history" => "터미널 관리 기록",
        "scroll: {value}" => "스크롤: {value}",
        "search: {value}" => "검색: {value}",
        "copy: {value}" => "복사: {value}",
        "terminal search" => "터미널 검색",
        "terminal scrollback" => "터미널 스크롤백",
        "terminal copy" => "터미널 복사",
        "drag to select" => "드래그 선택",
        "drag or {shortcut}" => "드래그 또는 {shortcut}",
        "Compacting context…" => "컨텍스트 압축 중…",
        "Context compacted" => "컨텍스트가 압축되었습니다",
        "Context compaction skipped" => "컨텍스트 압축을 건너뜀",
        "Context compaction failed" => "컨텍스트 압축 실패",
        "Context compaction cancelled" => "컨텍스트 압축 취소됨",
        "near the compaction threshold" => "압축 임계값에 근접",
        "This daemon does not support context compaction" => {
            "이 데몬은 컨텍스트 압축을 지원하지 않습니다"
        }
        "compact the context of the shown line" => "표시된 라인의 컨텍스트 압축",
        "follow or change the shown line's goal" => "표시된 라인의 목표 추적 또는 변경",
        "◎ goal active" => "◎ 목표 진행 중",
        "◎ goal paused" => "◎ 목표 일시정지",
        "insufficient space: {parts}" => "공간 부족: {parts}",
        "system prompt" => "시스템 프롬프트",
        "tools" => "도구",
        "appended messages" => "추가 메시지",
        "keyed baseline" => "키가 지정된 기준선",
        "pinned inputs" => "고정된 입력",
        "digest" => "다이제스트",
        "summary" => "요약",
        "The first message was not sent; your new draft was kept." => {
            "첫 메시지는 전송되지 않았습니다. 새 초안은 유지되었습니다."
        }
        "The App Server is still {server} because other clients are using it; this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server가 아직 {server}입니다. 다른 클라이언트가 사용 중이라 이 {client} 클라이언트도 그것을 사용합니다. 끝나면 `dimcode daemon stop`을 실행하세요. 다음 시작에서는 {client}가 실행됩니다."
        }
        "The App Server is still {server} because other clients are using it, and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server가 아직 {server}입니다. 다른 클라이언트가 사용 중이고, 이 서버는 프로토콜 v{protocol}을(를) 사용하므로 이 {client} 클라이언트는 사용할 수 없습니다. 세션은 시작되지 않았습니다. 끝나면 `dimcode daemon stop`을 실행하세요. 다음 시작에서는 {client}가 실행됩니다."
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}), and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server가 아직 {server}입니다. 다른 클라이언트가 사용 중이며(연결: {connections}, 연결된 세션: {attached}, 실행 중인 턴: {running}), 이 서버는 프로토콜 v{protocol}을(를) 사용하므로 이 {client} 클라이언트는 사용할 수 없습니다. 세션은 시작되지 않았습니다. 끝나면 `dimcode daemon stop`을 실행하세요. 다음 시작에서는 {client}가 실행됩니다."
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}); this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server가 아직 {server}입니다. 다른 클라이언트가 사용 중입니다(연결: {connections}, 연결된 세션: {attached}, 실행 중인 턴: {running}). 이 {client} 클라이언트도 그것을 사용합니다. 끝나면 `dimcode daemon stop`을 실행하세요. 다음 시작에서는 {client}가 실행됩니다."
        }
        "The App Server is still using a different build of {server}. Some features may be unavailable. After active work finishes, exit this client, run `{command}`, then reopen it." => {
            "App Server가 아직 {server}의 다른 빌드를 사용하고 있습니다. 일부 기능을 사용할 수 없을 수 있습니다. 작업이 끝나면 이 클라이언트를 종료하고 `{command}`를 실행한 뒤 다시 여십시오."
        }
        "The App Server's build could not be verified as this client's. If features are missing after an update, wait for active work to finish, exit this client, run `{command}`, then reopen it." => {
            "App Server의 빌드가 이 클라이언트와 같은지 확인하지 못했습니다. 업데이트 후 기능이 빠져 있다면 작업이 끝날 때까지 기다렸다가 이 클라이언트를 종료하고 `{command}`를 실행한 뒤 다시 여십시오."
        }
        "Upgrading the local store from format v{from} to v{to}; this can take a while on a large store." => {
            "로컬 저장소를 포맷 v{from}에서 v{to}(으)로 업그레이드하는 중입니다. 큰 저장소에서는 시간이 걸릴 수 있습니다."
        }
        "Upgrading the local store · {seconds} s · {stage}" => {
            "로컬 저장소 업그레이드 중 · {seconds}초 경과 · {stage}"
        }
        "moving retained images into Session files" => "보관된 이미지를 세션 파일로 옮기는 중",
        "updating the text index of the rows that changed" => {
            "변경된 행의 텍스트 인덱스를 갱신하는 중"
        }
        "checking and committing the upgraded store" => {
            "업그레이드된 저장소를 검사하고 커밋하는 중"
        }
        "A prompt save is still in progress" => "프롬프트 저장이 아직 진행 중입니다",
        "Base prompt" => "기본 프롬프트",
        "Before/after comparison is unavailable for this record." => {
            "이 기록에는 변경 전후 비교가 없습니다."
        }
        "Cancelling generation…" => "생성을 취소하는 중…",
        "Changes" => "변경 사항",
        "Compaction prompt" => "압축 프롬프트",
        "Could not open the share destination" => "공유 대상을 열지 못했습니다",
        "Could not save ZIP" => "ZIP을 저장하지 못했습니다",
        "Default: current-session diagnostics only." => "기본값: 현재 세션 진단 정보만.",
        "Draft a GitHub issue" => "GitHub 이슈 초안 작성",
        "Email the ZIP to {email}" => "ZIP을 {email}(으)로 이메일 전송",
        "Enter apply · Esc back" => "Enter 적용 · Esc 뒤로",
        "Error diagnostics only; no session text is available." => {
            "오류 진단 정보만. 수집할 세션 본문이 없습니다."
        }
        "Esc back · x stop agent · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 뒤로 · x 에이전트 중지 · ↑↓ PgUp PgDn 스크롤 · Home/End 이동"
        }
        "Esc back · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 뒤로 · ↑↓ PgUp PgDn 스크롤 · Home/End 이동"
        }
        "Evidence · No recent session activity" => "증거 · 최근 세션 활동 없음",
        "Evidence · Observed error only" => "증거 · 관찰된 오류만",
        "Evidence · Recent session activity" => "증거 · 최근 세션 활동",
        "Evidence · Timeline omitted" => "증거 · 타임라인 생략됨",
        "Evidence · Timeline read failed" => "증거 · 타임라인을 읽지 못함",
        "Evidence · Timeline unavailable" => "증거 · 타임라인 사용 불가",
        "Generate report" => "보고서 생성",
        "Generate/save never uploads. Share manually after saving." => {
            "생성과 저장은 업로드하지 않습니다. 저장 후 직접 공유하세요."
        }
        "Generating report…" => "보고서를 생성하는 중…",
        "Give feedback on Discord" => "Discord에서 피드백",
        "Input" => "입력",
        "Invalid report ZIP" => "잘못된 보고서 ZIP",
        "Local generation makes zero model requests." => "로컬 생성은 모델 요청을 보내지 않습니다.",
        "Model · Generate locally" => "모델 · 로컬에서 생성",
        "New" => "신규",
        "No content changes." => "내용 변경이 없습니다.",
        "No session to report." => "보고할 세션이 없습니다.",
        "Old" => "이전",
        "Opened Discord" => "Discord를 열었습니다",
        "Opened GitHub issue draft" => "GitHub 이슈 초안을 열었습니다",
        "Options" => "옵션",
        "Options must fit within 1 KiB" => "옵션은 1 KiB를 넘을 수 없습니다",
        "Options · for example: omit timeline, keep error codes" => {
            "옵션 · 예: omit timeline, keep error codes"
        }
        "Prepare report again" => "보고서 다시 준비",
        "Preparing report…" => "보고서를 준비하는 중…",
        "Prompt saved; applies to new Sessions" => "프롬프트를 저장했습니다. 새 세션에 적용됩니다",
        "Prompt settings are not available" => "프롬프트 설정을 사용할 수 없습니다",
        "Prompts" => "프롬프트",
        "Read-only: this agent has finished." => "읽기 전용: 이 에이전트는 종료되었습니다.",
        "Read-only: you can watch this agent or stop it." => {
            "읽기 전용: 이 에이전트를 보거나 중지할 수 있습니다."
        }
        "Redact [ ]" => "가리기 [ ]",
        "Redact [x]" => "가리기 [x]",
        "Redact is off. The ZIP may contain sensitive text from recent messages." => {
            "가리기가 꺼져 있습니다. ZIP에 최근 메시지의 민감한 텍스트가 포함될 수 있습니다."
        }
        "Refresh report plan" => "보고서 계획 새로 고침",
        "Report ready" => "보고서 준비됨",
        "Report this error" => "이 오류 보고",
        "Report this session" => "이 세션 보고",
        "Resetting {field}; the old text is kept in {path}" => {
            "{field}을(를) 초기화하는 중입니다. 이전 텍스트는 {path}에 보관됩니다"
        }
        "Result" => "결과",
        "Run /report to create a report." => "/report를 실행해 보고서를 만드세요.",
        "Running…" => "실행 중…",
        "Save ZIP" => "ZIP 저장",
        "Saved ZIP" => "ZIP 저장됨",
        "Session model" => "세션 모델",
        "Skills instructions" => "스킬 지침",
        "Some options were unsupported." => "일부 옵션은 지원되지 않습니다.",
        "Tab also picks tool headers · Enter/Space toggle the focused tool" => {
            "Tab으로도 도구 제목 선택 · Enter/스페이스로 선택한 도구 전환"
        }
        "This session is closed and cannot be resumed. Read its stored history with dimcode export {id} using the same --home." => {
            "이 세션은 닫혀 있어 이어갈 수 없습니다. 같은 --home으로 dimcode export {id}를 실행하면 저장된 기록을 읽을 수 있습니다."
        }
        "Turn Redact off to attach recent user/assistant text to ZIP; it may contain sensitive data." => {
            "가리기를 끄면 최근 사용자와 어시스턴트 본문이 ZIP에 첨부됩니다. 민감한 정보가 포함될 수 있습니다."
        }
        "Updating report plan…" => "보고서 계획을 업데이트하는 중…",
        "Usage: /report" => "사용법: /report",
        "Waiting for result…" => "결과를 기다리는 중…",
        "ZIP contents: report.md, diagnostics.json" => "ZIP 내용: report.md, diagnostics.json",
        "agent result" => "에이전트 결과",
        "built-in" => "내장",
        "custom" => "사용자 지정",
        "report the current session" => "현재 세션 보고",
        "skills replaces the marker and call shape; the skill tool and list stay · r reset" => {
            "skills는 표시와 호출 형식을 바꿉니다. 스킬 도구와 목록은 그대로 · r 초기화"
        }
        "… {count} unchanged lines" => "… 변경되지 않은 {count}줄",
        "↑↓ move · Enter edit · r reset · Esc close" => {
            "↑↓ 이동 · Enter 편집 · r 초기화 · Esc 닫기"
        }
        "↑↓ move · Enter select · ←→/Space change · r reset · Esc close" => {
            "↑↓ 이동 · Enter 선택 · ←→/스페이스 변경 · r 초기화 · Esc 닫기"
        }
        "↑↓/Tab move · Enter select · Esc close" => "↑↓/Tab 이동 · Enter 선택 · Esc 닫기",
        "Permission mode" => "권한 모드",
        "Related settings" => "관련 설정",
        "Process isolation" => "프로세스 격리",
        "open" => "열기",
        "User-wide command rules. Allow skips asks, never denials." => {
            "사용자 전체 명령 규칙. 허용은 확인을 건너뛰지만 거부를 넘지 않습니다."
        }
        "Opens Settings; user-wide, for new processes; independent of permission mode." => {
            "설정을 엽니다. 사용자 전체, 새 프로세스 대상, 권한 모드와 무관합니다."
        }
        "keep mode" => "모드 유지",
        "use mode" => "모드 사용",
        "Current session; remembered for new sessions. {consequence}" => {
            "현재 세션만. 새 세션을 위해 기억됩니다. {consequence}"
        }
        "↑↓/Tab move · Enter {action} · Esc close" => {
            "↑↓/Tab 이동 · Enter {action} · Esc 닫기"
        }
        "Writes and commands denied; external reads may ask." => {
            "쓰기와 명령은 거부됩니다. 외부 읽기는 확인을 요청할 수 있습니다."
        }
        "Workspace writes allowed; commands, network and ungranted outside access usually ask." => {
            "작업 공간 쓰기는 허용됩니다. 명령, 네트워크, 허용되지 않은 외부 접근은 보통 확인을 요청합니다."
        }
        "Skips preset approval; pending calls on this branch are reevaluated. Explicit denies and isolation remain." => {
            "프리셋 승인을 건너뜁니다. 이 브랜치의 대기 중인 호출은 다시 평가됩니다. 명시적 거부와 격리는 그대로입니다."
        }
        "↑↓ move · Enter select · Esc close" => "↑↓ 이동 · Enter 선택 · Esc 닫기",
        "Loading process isolation…" => "프로세스 격리를 불러오는 중…",
        "Sandbox is unavailable on this host: {reason}" => {
            "이 호스트에서는 샌드박스를 사용할 수 없습니다: {reason}"
        }
        "Applies to new processes and shells." => "새 프로세스와 셸에 적용됩니다.",
        "Off: commands use your OS permissions, not file-tool limits." => {
            "끔: 명령은 파일 도구 제한이 아니라 OS 권한을 사용합니다."
        }
        "Off" => "끔",
        "Sandboxed · No network" => "샌드박스 · 네트워크 없음",
        "Sandboxed · Network allowed" => "샌드박스 · 네트워크 허용",
        "selected" => "선택됨",
        "Save" => "저장",
        "Reset to defaults" => "기본값으로 초기화",
        "Reload" => "다시 로드",
        "Discard edits and read the current settings" => "편집을 버리고 현재 설정 읽기",
        "Reset the execution switches to defaults?" => "실행 스위치를 기본값으로 초기화할까요?",
        "Discard unsaved changes?" => "저장하지 않은 변경을 버릴까요?",
        "Yes (y)" => "예 (y)",
        "Cancel (n)" => "취소 (n)",
        "y confirm · Enter/Esc cancel" => "y 확인 · Enter/Esc 취소",
        "Read only" => "읽기 전용",
        "Workspace write" => "작업 공간 쓰기",
        "Agent" => "에이전트",
        "agent" => "에이전트",
        "command" => "명령",
        "ended" => "종료됨",
        "Signed in" => "로그인됨",
        "Retry" => "재시도",
        "Provider changed; review the retained draft and press Apply again" => {
            "프로바이더가 변경되었습니다. 보관된 초안을 확인하고 다시 적용을 누르세요"
        }
        "y allow · a always · n reject · ←→/Tab move · Enter confirm" => {
            "y 허용 · a 항상 · n 거부 · ←→/Tab 이동 · Enter 확인"
        }
        "{count} in background · /tasks" => "백그라운드 {count}개 · /tasks",
        "{field}: {error} Your text is kept in {path}" => {
            "{field}: {error} 입력한 텍스트는 {path}에 보관됩니다"
        }
        "{field}: {error}" => "{field}: {error}",
        "Prompts: {error}" => "프롬프트: {error}",
        _ => return None,
    })
}
