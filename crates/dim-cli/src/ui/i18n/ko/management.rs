pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "불러오는 중…",
        "No entries." => "항목이 없습니다.",
        "No matching entry." => "일치하는 항목이 없습니다.",
        "Issues" => "문제",
        "sources" => "소스",
        "skills" => "스킬",
        "hooks" => "훅",
        "scheduled jobs" => "예약된 작업",
        "models" => "모델",
        "workflow definitions" => "워크플로 정의",
        "command rules" => "명령 규칙",
        "enabled" => "활성",
        "shadowed by {name}" => "{name}에 의해 가려짐",
        "Reloaded." => "다시 로드했습니다.",
        "Enabled {name}." => "{name}을(를) 활성화했습니다.",
        "Disabled {name}." => "{name}을(를) 비활성화했습니다.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} 다시 로드됨: 항목 {entries}개, 비활성 {disabled}개."
        }
        "Reloaded {area}: {entries} entries." => "{area} 다시 로드됨: 항목 {entries}개.",
        "Hooks" => "훅",
        "invalid" => "유효하지 않음",
        "Saved." => "저장했습니다.",
        "Saved {name}." => "{name}을(를) 저장했습니다.",
        "No change." => "변경 사항이 없습니다.",
        "Your text is kept in {path}." => "작성한 내용은 {path}에 보관됩니다.",
        "The user's own document; always in effect." => "사용자 자신의 문서이며 항상 적용됩니다.",
        "There is no hooks file there." => "거기에 훅 파일이 없습니다.",
        "Owned by plugin {plugin}." => "플러그인 {plugin} 소유입니다.",
        "The user document turns every hook off (disableAllHooks)." => {
            "사용자 문서가 모든 훅을 끕니다(disableAllHooks)."
        }
        "Enter toggle" => "Enter 전환",
        "Ctrl+E edit" => "Ctrl+E 편집",
        "Ctrl+N add" => "Ctrl+N 추가",
        "Ctrl+R reload" => "Ctrl+R 다시 로드",
        "Ctrl+T run" => "Ctrl+T 실행",
        "Ctrl+X remove" => "Ctrl+X 삭제",
        "Tab inspect" => "Tab 상세",
        "Remove {name}?" => "{name}을(를) 삭제할까요?",
        "↑↓ scroll · Esc back" => "↑↓ 스크롤 · Esc 뒤로",
        "Added." => "추가했습니다.",
        "Removed {name}." => "{name}을(를) 삭제했습니다.",
        "unavailable" => "사용 불가",
        "Reset {name}?" => "{name}을(를) 초기화할까요?",
        "Enter confirm" => "Enter 확인",
        "Esc cancel" => "Esc 취소",
        "Reset {name} to its catalog entry." => "{name}을(를) 카탈로그 항목으로 초기화했습니다.",
        "Ran {name}." => "{name}을(를) 실행했습니다.",
        "The user document changed since it was read; nothing was written." => {
            "사용자 문서가 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "The job changed since it was read; nothing was written." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "The jobs changed since they were read; nothing was written." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "Your next edit opens the current document." => "다음 편집은 현재 문서를 엽니다.",
        "Your next add reopens your text." => "다음 추가는 작성한 내용을 다시 엽니다.",
        "Scheduler" => "스케줄러",
        "jobs" => "작업",
        "Added {name}." => "{name}을(를) 추가했습니다.",
        "Run {name}: {outcome}." => "{name}을(를) 실행했습니다: {outcome}.",
        "Recent runs of {name}" => "{name}의 최근 실행",
        "No run yet." => "아직 실행 기록이 없습니다.",
        "No next run." => "다음 실행이 없습니다.",
        "Next run {instant}." => "다음 실행 {instant}.",
        "Last run {outcome} at {instant}." => "마지막 실행 {outcome}, {instant}.",
        "every {minutes} minutes" => "{minutes}분마다",
        "at {instant}" => "{instant}에",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} ({zone})",
        "The job changed since it was read; nothing was removed." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 삭제하지 않았습니다."
        }
        "The Scheduler has no area document." => "스케줄러에는 영역 문서가 없습니다.",
        "No scheduled job has that id." => "해당 id의 예약 작업이 없습니다.",
        "A scheduled job's run takes no input document." => {
            "예약된 작업의 실행은 입력 문서를 받지 않습니다."
        }
        "An MCP server's test takes no input document." => {
            "MCP 서버 테스트는 입력 문서를 받지 않습니다."
        }
        "This daemon cannot manage {area}." => "{area}을(를) 관리할 수 없습니다.",
        "This daemon lists {area} but cannot toggle them." => {
            "{area} 목록은 표시할 수 있지만 전환할 수 없습니다."
        }
        "This daemon lists {area} but cannot edit them." => {
            "{area} 목록은 표시할 수 있지만 편집할 수 없습니다."
        }
        "This daemon lists {area} but cannot reload them." => {
            "{area} 목록은 표시할 수 있지만 다시 로드할 수 없습니다."
        }
        "This daemon lists {area} but cannot add them." => {
            "{area} 목록은 표시할 수 있지만 추가할 수 없습니다."
        }
        "This daemon lists {area} but cannot remove them." => {
            "{area} 목록은 표시할 수 있지만 삭제할 수 없습니다."
        }
        "This daemon lists {area} but cannot run them." => {
            "{area} 목록은 표시할 수 있지만 실행할 수 없습니다."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "{area} 목록은 표시할 수 있지만 세부 정보나 기록을 볼 수 없습니다."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "{field}이(가) 문서에서 삭제되었지만 소유자가 지울 수 없습니다. 아무것도 쓰지 않았습니다."
        }
        "The document changes no field; nothing was written." => {
            "문서가 어떤 필드도 바꾸지 않았습니다. 아무것도 쓰지 않았습니다."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "이 작업 공간의 훅 설정이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "Models" => "모델",
        "catalog" => "카탈로그",
        "overrides" => "재정의",
        "added" => "추가",
        "Models · {name}" => "모델 · {name}",
        "undeclared" => "선언되지 않음",
        "Context window {window} · max output {output}" => {
            "컨텍스트 창 {window} · 최대 출력 {output}"
        }
        "Endpoint {endpoint}" => "엔드포인트 {endpoint}",
        "Endpoint {endpoint} · override {value}" => "엔드포인트 {endpoint} · 재정의 {value}",
        "The provider reports it unavailable." => "공급자가 사용할 수 없다고 보고합니다.",
        "Limits undeclared; edit the model to declare them." => {
            "한도가 선언되지 않았습니다. 모델을 편집해 선언하세요."
        }
        "Sign in to the account to use its models." => {
            "계정에 로그인하면 모델을 사용할 수 있습니다."
        }
        "The signed-in account changed; reload to list its models." => {
            "로그인한 계정이 바뀌었습니다. 다시 로드해 모델을 나열하세요."
        }
        "The provider has no catalog yet." => "이 공급자에는 아직 카탈로그가 없습니다.",
        "The provider listed no models." => "공급자가 모델을 나열하지 않았습니다.",
        "Listed {count} models." => "모델 {count}개를 나열했습니다.",
        "Refreshed the models." => "모델을 새로 고쳤습니다.",
        "Could not list models: {error}. Showing the stored list." => {
            "모델을 나열할 수 없습니다: {error}. 저장된 목록을 표시합니다."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "로그인한 계정이 바뀌었습니다. 새로 고침이 기본 설정, 재정의, 비활성화를 지웠습니다."
        }
        "The models changed since they were read; nothing was removed." => {
            "모델이 읽은 뒤 변경되었습니다. 이번에는 아무것도 제거하지 않았습니다."
        }
        "The models changed since they were read; nothing was written." => {
            "모델이 읽은 뒤 변경되었습니다. 이번에는 아무것도 쓰지 않았습니다."
        }
        "No provider has that id." => "해당 id의 공급자가 없습니다.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name}은(는) 사용할 수 없습니다({reason}). 활성화 상태는 그대로 유지됩니다."
        }
        "No model has that id." => "해당 id의 모델이 없습니다.",
        "This model cannot be removed." => "이 모델은 제거할 수 없습니다.",
        "A Provider's models have no area document." => "공급자의 모델에는 영역 문서가 없습니다.",
        "Models have no run operation." => "모델에는 실행 작업이 없습니다.",
        "The document is not an object; nothing was written." => {
            "문서가 객체가 아닙니다. 이번에는 아무것도 쓰지 않았습니다."
        }
        "The document changes no editable field; nothing was written." => {
            "문서가 편집 가능한 필드를 변경하지 않았습니다. 이번에는 아무것도 쓰지 않았습니다."
        }
        "The model document names no model." => "모델 문서가 모델을 지정하지 않습니다.",
        "The provider has no model catalog to write." => {
            "이 공급자에는 쓸 수 있는 모델 카탈로그가 없습니다."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "소스 {source}",
        "Status {status}" => "상태 {status}",
        "Tools {value}" => "도구 {value}",
        "Vision {value}" => "비전 {value}",
        "Reasoning {value}" => "추론 {value}",
        "Modalities {list}" => "모달리티 {list}",
        "Description {text}" => "설명 {text}",
        "Release date {date}" => "출시일 {date}",
        "Pricing {value}" => "가격 {value}",
        "Rate limit {value}" => "속도 제한 {value}",
        "Matched provider {id}" => "일치 공급자 {id}",
        "Matched model {id}" => "일치 모델 {id}",
        "Match strategy {strategy}" => "일치 전략 {strategy}",
        "Capability overrides {fields}" => "기능 재정의 {fields}",
        "yes" => "예",
        "no" => "아니요",
        "not supported" => "지원되지 않음",
        "supported" => "지원됨",

        "Workflow" => "워크플로",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name}을(를) 저장했습니다. 활성화할 때까지 비활성 상태로 유지됩니다."
        }
        "The definitions changed since they were read; nothing was written." => {
            "정의가 읽은 후 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "정의의 이름은 그 키입니다. 다른 이름으로 저장하려면 추가하세요."
        }
        "No project definition is named {name}." => "{name}이라는 프로젝트 정의가 없습니다.",
        "No workflow definition is named {name}." => "{name}이라는 워크플로 정의가 없습니다.",
        "Saved in the user workflow source; always enabled." => {
            "사용자 워크플로 소스에 저장됩니다. 항상 활성화됩니다."
        }
        "Starts only after you enable it." => "활성화한 후에만 시작됩니다.",
        "Defined by package {package}." => "패키지 {package}에서 정의합니다.",
        "{name} — {description} (required)" => "{name} — {description} (필수)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — 필수",
        "{name} — default: {default}" => "{name} — 기본값: {default}",
        "and {count} more" => "외 {count}개",
        "Recent runs" => "최근 실행",
        "A workflow definition is saved as an object with a source and a document." => {
            "워크플로 정의는 소스와 문서를 가진 객체로 저장합니다."
        }
        "A definition is saved to the user source or the project source." => {
            "정의는 사용자 소스 또는 프로젝트 소스에 저장합니다."
        }
        "Started {name}: {run_id}." => "{name} 시작됨: {run_id}.",
        "Parameters are a JSON object of strings." => {
            "매개변수는 문자열로 이루어진 JSON 객체입니다."
        }
        "The list reached its size limit; later definitions are not shown." => {
            "목록이 크기 제한에 도달하여 이후 정의는 표시되지 않습니다."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "크기 제한에 도달한 목록에 {name} 이름의 워크플로 정의가 없습니다."
        }
        "MCP" => "MCP",
        "MCP servers" => "MCP 서버",
        "Ctrl+T test" => "Ctrl+T 테스트",
        "sign-in required" => "로그인 필요",
        "signed in" => "로그인됨",
        "sign-in expired" => "로그인 만료",
        "missing scopes: {scopes}" => "누락된 범위: {scopes}",
        "Executable: {command}" => "실행 파일: {command}",
        "Stored arguments (hidden): {count}" => "저장된 인수(숨김): {count}",
        "Working directory: {cwd}" => "작업 디렉터리: {cwd}",
        "Environment names: {names}" => "환경 변수 이름: {names}",
        "Endpoint: {endpoint}" => "엔드포인트: {endpoint}",
        "Header names: {names}" => "헤더 이름: {names}",
        "Bearer token: {state}" => "Bearer 토큰: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: 구성됨 (클라이언트 ID {id})",
        "OAuth: configured" => "OAuth: 구성됨",
        "OAuth: not configured" => "OAuth: 구성되지 않음",
        "Metadata URL: {state}" => "메타데이터 URL: {state}",
        "configured" => "구성됨",
        "Owned by a plugin; change it through Plugins." => {
            "플러그인이 소유합니다. 플러그인에서 변경하세요."
        }
        "MCP entry {id}" => "MCP 항목 {id}",
        "Connection {number} · {status}" => "연결 {number} · {status}",
        "Tools have not been discovered." => "도구가 아직 발견되지 않았습니다.",
        "No live session connections." => "활성 세션 연결이 없습니다.",
        "Tools of {id}" => "{id}의 도구",
        "The server exposes no tools." => "서버가 도구를 제공하지 않습니다.",
        "The add document needs an id." => "추가 문서에는 id가 필요합니다.",
        "The add document needs a source of user or project." => {
            "추가 문서의 source는 user 또는 project여야 합니다."
        }
        "The add document needs a configuration object." => {
            "추가 문서에는 configuration 객체가 필요합니다."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id}은(는) 이미 mcp list에 있습니다. 먼저 제거하거나 다른 id를 선택하세요."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id}은(는) 이미 mcp list에 있습니다. 다른 id를 선택하거나 플러그인에서 변경하세요."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "이 항목의 설정이 읽은 후 변경되었습니다. 아무것도 기록되지 않았습니다."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "MCP 항목이 읽은 후 변경되었습니다. 아무것도 기록되지 않았습니다."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "MCP 항목이 읽은 후 변경되었습니다. 아무것도 기록되지 않았습니다."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "MCP 항목이 읽은 후 변경되었습니다. 아무것도 제거되지 않았습니다."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "MCP 문서는 항목별입니다. 이 저장은 항목을 지정하지 않았습니다."
        }
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id}이(가) 여러 MCP 소스에 있습니다: {sources}. --source로 하나를 지정하세요."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "이 작업은 MCP 소스를 지정하지 않았습니다. 각 행은 소유자가 보고한 소스를 가집니다."
        }
        "No MCP server {id} is in source {source}." => {
            "{source} 소스에 MCP 서버 {id}이(가) 없습니다."
        }
        "No MCP server has that id." => "해당 id의 MCP 서버가 없습니다.",
        "This workspace has no project MCP source." => {
            "이 작업 공간에는 프로젝트 MCP 소스가 없습니다."
        }
        "Enabled {id} ({source})." => "{id}({source})을(를) 활성화했습니다.",
        "Disabled {id} ({source})." => "{id}({source})을(를) 비활성화했습니다.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "활성 및 새 세션에서 이후 수정을 포함해 이 작업 공간의 .mcp.json 항목을 실행합니다."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id}은(는) mcp list에 없습니다. 이 작업 공간에 저장된 활성화를 철회했습니다."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "MCP 카탈로그를 읽을 수 없습니다({error}). 이 작업 공간에서 {id}의 활성화를 철회했습니다. 같은 ID의 사용자 항목은 변경되지 않았습니다."
        }
        "MCP configuration: {error}" => "MCP 구성: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "서버가 저장된 자격 증명을 거부했습니다. mcp login으로 다시 로그인하세요."
        }
        "The server requires additional scopes: {scopes}." => {
            "서버가 추가 범위를 요구합니다: {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "서버가 인증을 요구합니다. mcp login ID를 실행하세요."
        }
        "The probe ended: {status}." => "프로브가 종료되었습니다: {status}.",

        "Command rules" => "명령 규칙",
        "user" => "사용자",
        "Matches only this command." => "이 명령만 일치합니다.",
        "Matches commands whose first words are these." => {
            "앞부분이 이 단어들인 명령과 일치합니다."
        }
        "Allows a simple command that would otherwise ask." => {
            "원래 확인을 요청하는 단순 명령을 허용합니다."
        }
        "Denies the command under every preset." => "모든 프리셋에서 이 명령을 거부합니다.",
        "Applies in every Session, in every workspace." => {
            "모든 세션, 모든 워크스페이스에서 적용됩니다."
        }
        "Rule added: {rule}." => "규칙을 추가했습니다: {rule}.",
        "Rule already present: {rule}." => "규칙이 이미 있습니다: {rule}.",
        "Rule removed: {rule}." => "규칙을 삭제했습니다: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "해당 명령 규칙이 더 이상 목록에 없습니다. 아무것도 삭제되지 않았습니다."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "명령 규칙이 읽은 뒤 변경되었습니다. 아무것도 삭제되지 않았습니다."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "명령 규칙은 DECISION MATCH COMMAND 형식입니다. 예: deny prefix git push."
        }
        "Command: {command}" => "명령: {command}",
        "Use the line prompt or dimcode permissions." => {
            "줄 프롬프트 또는 dimcode permissions에서 사용하세요."
        }
        "Tool call timeout: {millis} ms" => "도구 호출 시간 제한: {millis} ms",
        "Variables: {names}" => "변수: {names}",
        "Unset variables: {names}" => "설정되지 않은 변수: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "MCP 소스가 미리보기 후 변경되었습니다. 항목은 가져오지 않았고, 대체하려던 항목은 이제 비활성일 수 있습니다. 다시 미리보기하세요."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "프로젝트 항목은 기록되었지만 보류된 값은 기록되지 않았습니다. 다시 대체해 재시도하세요."
        }
        "Replaced {id}." => "{id}을(를) 대체했습니다.",
        "Added {id}." => "{id}을(를) 추가했습니다.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "세션 {session} 브랜치 {branch}({mode})(으)로 전달합니다."
        }
        "Launches a new Session in {cwd}." => "{cwd}에서 새 세션을 시작합니다.",
        "Disabled: {code} at {instant}." => "비활성: {instant}의 {code}.",
        "The definition changed since it was read; nothing was written." => {
            "정의가 읽은 뒤 변경되었습니다. 아무것도 기록되지 않았습니다."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "열: ID, 클라이언트, 범위, 경로, 상태, 항목 수 또는 오류."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "저장소에서 값을 읽거나 출력하지 않습니다. --from ID는 데몬 안에서 하나를 미리보기합니다."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "열: ID, 작업, 대상 ID.\n이름이 바뀐 항목은 대상 id로 기록되고, 소스의 이전 id 항목은 그대로 남습니다."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "가져오거나 대체된 항목은 비활성 상태로 시작합니다. 사용자 항목은 enabled: false로 기록되고, 프로젝트 항목은 로컬에서 활성화하기 전까지 활성화 기록이 없습니다."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "이 데몬은 다른 클라이언트의 MCP 저장소를 나열할 수 없습니다."
        }
        "This daemon cannot import MCP servers." => "이 데몬은 MCP 서버를 가져올 수 없습니다.",
        "The document holds no mcpServers entries." => "문서에 mcpServers 항목이 없습니다.",
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "열: ID, 상태, 전송 방식, 변경된 필드, 지원되지 않는 필드, 오류.\n변경된 필드는 쓰기가 무엇을 바꾸는지와 값의 종류를 나타냅니다. 값은 출력되지 않습니다.\n지원되지 않는 필드는 필드 뒤 설명대로 사용되지 않거나 버려집니다."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "프로젝트 항목은 공개 뼈대만 기록됩니다. 작업 공간 파일에는 명령, 선언된 cwd, URL 출처, 공개 OAuth 필드가 들어갑니다. 인수, 환경 변수와 헤더 값, bearer 토큰, userinfo·경로·쿼리·프래그먼트가 있는 URL, 알 수 없는 멤버는 사용자 범위 mcp-secrets 기록에 기록되며 작업 공간 파일에는 절대 기록되지 않습니다."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id}에는 위 목록에 표시되지 않은 필드 이름이 {count}개 더 있습니다."
        }
        "Top-level members the import does not carry: none." => {
            "가져오기가 옮기지 않는 최상위 멤버: 없음."
        }
        "Top-level members the import does not carry: {names}." => {
            "가져오기가 옮기지 않는 최상위 멤버: {names}."
        }
        "Applying the selection at this revision." => "이 리비전에서 선택을 적용하는 중입니다.",
        "An unchanged entry was already in the source and was not written." => {
            "변경되지 않은 항목은 이미 소스에 있어 기록하지 않았습니다."
        }
        _ => return None,
    })
}
