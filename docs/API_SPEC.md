# CLI / IPC / MCP API 명세

## CLI POC에서 실제 가능한 명령
```
python3 -m dsg init --root PATH
python3 -m dsg doctor --root PATH
python3 -m dsg scan --root PATH
python3 -m dsg docker-scan
python3 -m dsg check -- docker volume prune
python3 -m dsg gc-plan --root PATH
python3 -m dsg gc-apply PLAN_ID --i-understand-delete
python3 -m dsg exec --root PATH -- python3 -V
python3 -m dsg hook --agent codex
```

## Rust CLI Foundation

소스 워크스페이스는 `rust-scaffold/`이며 바이너리 이름은 `dsg`다. 명령은 `init`, `doctor`, `scan`, `check -- <argv...>`, `gc-plan`이다. 전역 옵션 `--root`, `--state-dir`, `--global-policy`, `--session-policy`, `--json`으로 대상과 출력 형식을 선택한다.

`init`은 `.dsg.json`을 덮어쓰지 않고 생성하고 프로젝트를 등록한다. `doctor`와 `scan`은 관측 시각, 사용량, 예산 상태 및 검사 완전성을 보여 준다. `normal`, `warning`, `critical`, `unknown`을 구분하며 알 수 없는 수치는 JSON `null`로 표시한다. `gc-plan`은 보호·미확인 후보의 제외 사유와 계획 번호를 제공한다. SQLite에 계획과 감사 이벤트를 함께 저장한다.

`--json` 성공 응답은 `schema_version`, `command`, `result`를 반환한다. 실행 오류는 stderr에 `schema_version`과 `error.code`·`error.message`를 반환한다. `check`의 결과는 아래 공통 판단 응답 형태이며 종료 코드는 허용·경고 0, 실행 오류 2, 거부 3, 승인 필요 4다. 입력 명령의 인자는 판단 응답이나 감사 기록에 포함하지 않는다.

### Docker 읽기 전용 인벤토리

`dsg --json docker-contexts [--max-contexts <1~64>]`는 로컬에 등록된 Docker 컨텍스트 이름을 정렬해 반환한다. 컨텍스트를 선택하거나 엔진에 연결하는 작업은 사용자 요청으로 수행한다. 조회가 제한을 넘거나 이름을 검증할 수 없으면 `contexts=null`, `complete=false`와 종료 코드 5를 반환한다.

`dsg --json docker-scan --context <context> [--max-resources <1~64>]`는 컨텍스트를 명시해 엔진 ID, 컨테이너·이미지·볼륨, Compose 중복 후보를 조회한다. 허용된 필드만 수집하며 컨텍스트 환경변수로 대상이 바뀌지 않게 한다. 조회가 불완전하면 `complete=false`와 `issues`가 반환된다. 예산 안에서 조회 결과가 비어 있는 경우에는 완전한 0건으로 기록한다. 엔진 ID를 확인할 수 없으면 동일 구조의 JSON과 종료 코드 5를 반환한다.

중복 후보는 관측된 동일성에 관한 정보다. 실제 재사용에 필요한 엔진·구성 해시·이미지·마운트·health가 확인되지 않으면 판정을 미확인으로 남긴다. 마운트 대상 위치와 명명 볼륨을 비교한다. bind 소스는 수집 범위 밖이며, healthcheck가 없거나 이미지 digest가 없는 경우도 재사용 미확인이다.

`dsg --json docker-build-cache --context <context>`는 지정한 Docker 엔진의 내장 빌더 캐시 총량과 활성 항목 수를 조회한다. 조회 전후 엔진 ID가 일치해야 한다. 개별 캐시 소유권은 감지할 수 없어 모든 경우에 `disposition=preserve`를 반환한다. 집계 실패 시 수치는 `null`이며 종료 코드는 5다. 이 경로의 지원 범위는 엔진이 보고하는 내장 빌더 집계다.

`dsg --json docker-buildx-cache --context <context>`는 지정 엔진에 기본 제공되는 Buildx 빌더의 `total_count`, `reclaimable_count`, `shared_count`, `mutable_count`를 읽는다. 이 명령은 빈 임시 Buildx 설정을 사용하고 조회 전후에 엔진 ID를 대조한다. Buildx의 회수 가능 표시는 캐시 상태이며 프로젝트 소유권이나 삭제 승인과 별개다. 이 명령은 항상 `disposition=preserve`를 반환하며 조회 실패·초과 시 수치는 `null`, 종료 코드는 5다. [Docker Buildx 사용량 명령](https://docs.docker.com/reference/cli/docker/buildx/du/)

Linux ARM64의 `dsg --json buildkit-local-cache --socket <절대 Unix 소켓> --expected-pid <데몬 PID> --buildctl <절대 실행 경로>`는 이미 실행 중인 별도 BuildKit의 사용량만 읽는다. 소켓이 속한 디렉터리는 현재 UID 소유의 권한 0700이어야 한다. 같은 PID 네임스페이스의 상대 PID·UID와 소켓 식별자를 조회 전후에 확인한다. 실행 가능 상태 및 SHA-256을 검사한 공식 v0.35.1 `buildctl`만 `du`에 사용한다. 성공 응답의 `counts`에 `total`, `reclaimable`, `shared`, `mutable`을 제공한다. 소유권 판정은 `unknown_ownership`이며 `disposition=preserve`를 유지한다. 검증이나 조회 실패는 `status=unknown`, `counts=null`, 종료 5로 반환한다. [공식 BuildKit 릴리스](https://github.com/moby/buildkit/releases/tag/v0.35.1) · [실측 근거](HANDOFF.md#별도-buildkit-읽기-전용-실측)

### 관리형 실행과 Compose 재사용

`dsg exec --timeout 1~120 -- <argv...>`는 등록된 프로젝트의 정책과 호스트·프로젝트 예산을 확인한 뒤 직접 실행한다. 관리형 실행은 프로젝트별 lease 하나와 동일 상태 저장소의 전역 두 슬롯으로 제한한다. 조회가 불완전하거나 정책상 추가 확인이 필요하면 기존 판단 형식으로 반환한다. 성공 0, 정책 거부 3, 승인 필요 4, 제어 미확인 5, 시간 초과 124, 정상 중단 130을 사용한다. 자식이 실패하면 자식의 종료 코드를 반환한다. `--json`에서는 명령과 자식 원문 출력 없이 `run.outcome`, `run.exit_code`, `run.tree_confirmed_gone`과 `lease_released`를 제공한다.

`dsg exec --provider <cargo|node|pnpm|gradle|uv|go|playwright> -- <작업>`는 등록된 정형 작업을 실행한다. Cargo는 `build`, `check`, `test`, pnpm·Gradle·Go는 `build`, `test`, Node·uv·Playwright는 `test`를 받는다. 하나의 작업만 받으며 각 도구의 병렬 실행 옵션을 제한한다. pnpm이 호출한 프로젝트 스크립트 내부의 작업자는 프로젝트 설정을 따른다. Playwright는 프로젝트에 준비된 실행기를 사용하고 실행마다 독립된 상태 폴더에 성공·실패 추적 자료를 저장한다. 응답 `evidence_path`는 상태 디렉터리 기준 상대 경로다. 트리 종료가 확인되면 Provider·작업·결과 코드가 해당 lease 감사 이벤트에 기록된다.

`dsg docker-reuse --context <context> --expectation-json <JSON>`은 `engine_id`, `project`, `service`, `number`, `config_hash`, `image_id`, `image_digest`, `mounts`를 받는다. 같은 실행 중인 정상 컨테이너가 두 번 모두 유일한 후보면 `verified=true`와 `assessment.container_id`를 반환한다. 확인할 수 없으면 종료 코드 5와 판정 사유를 반환한다. 이미 실행 중인 컨테이너 식별자를 전달하는 경로다.

### 승인 기반 정리

등록된 프로젝트에서 `gc-own --candidate <이름> --confirm-generated`로 직접 하위의 생성 폴더에 소유권을 기록한다. `gc-plan` 결과에서 후보를 확인한 뒤 `gc-approve --plan <번호> --candidate <이름> --confirm`으로 한 후보에 대한 일회용 승인을 발급한다. 승인 유효 기간은 10분 이내이며 계획 만료 시각을 넘지 않는다.

`gc-apply --plan <번호> --approval-stdin`에 승인값을 표준 입력으로 전달한다. 적용은 활성 lease, 소유권, 파일 식별자와 트리 메타데이터를 재확인하며, 입력 승인 원문은 감사 기록과 명령 인자로 보관하지 않는다. 결과는 `applied`, `preserved`, `recovery_required`로 구분한다. 복구가 필요하면 `gc-recover --plan <번호>`가 일치하는 격리 자료를 확인해 빈 원래 경로로 복원을 시도한다.

`gc-plan --docker-context <context> --docker-engine <ID> --docker-image <sha256:전체ID>`는 파일 정리 계획과 별도로 `plan.docker_images`에 이미지 후보 또는 보존 사유를 기록한다. 후보는 선택한 엔진, 프로젝트의 소유 표식 `io.dsg.gc.owner`(정규 경로의 SHA-256), 전체 이미지 ID가 일치하며 태그·digest·컨테이너 참조·활성 lease가 없어야 한다. 관측 한도 초과와 이미지 마운트의 참조 불명은 보존한다. `docker-image-recheck --plan <번호>`는 유효기간과 정책을 확인하고 같은 엔진에서 후보 지문을 비교한다. 이미지 경로의 기능 범위는 계획과 재확인이다.

`exec --auto-clean-cargo -- cargo check …`는 자동 회수 대상인 전용 Cargo 산출물 디렉터리를 만든다. 실행 결과의 `cleanup`에서 회수·보존 건수와 확인된 바이트를 읽는다. Git 완료는 `scripts/git-completion.sh`로 전달한다. 이 경로는 여섯 참조의 파일 일치 후 사용자 원본과 브랜치를 보존하면서 등록된 보조 워크트리만 검사한다.

### 에이전트 훅

`dsg --root <프로젝트> agents install --agent <claude|codex|gemini>`는 macOS와 Linux에서 프로젝트 설정에 DSG 훅을 등록한다. `agents uninstall --agent …`는 실행 파일과 등록 내용이 일치하는 DSG 훅을 제거한다. `--json` 응답의 `result.changed`는 설정 변경 여부, `result.installed`는 요청한 설치 상태를 표시한다. 잘못된 경로나 안전하게 수정할 수 없는 설정은 `AGENT_CONFIG_UNAVAILABLE`로 보고한다.

`dsg hook --agent <claude|codex|gemini>`는 해당 CLI의 셸 훅 JSON을 표준 입력으로 읽고 그 CLI에 맞는 JSON을 반환한다. 최대 입력은 64 KiB다. 보호 명령, 별도 승인이 필요한 명령, 셸 해석 결과를 확정하기 어려운 입력은 안정적인 사유 코드와 함께 거부한다. 허용 시 빈 객체를 반환해 CLI의 자체 권한 확인을 유지한다. [설치 및 운영 범위](../integrations/README.md)

### 로컬 Docker 조회 브로커

`dsgd`는 macOS·Linux용 Unix 소켓 서버다. `--listen`, `--upstream`, `--engine-id`, `--docker-executable`, `--empty-config-dir`, `--allow-uid`를 받아 대상을 고정한다. 연결 상대의 OS UID를 판별해 허용된 연결에서만 요청을 읽는다.

| HTTP GET 경로 | 응답 |
|---|---|
| `/_ping` | 예상 Docker 엔진을 조회할 수 있을 때 `OK`. |
| `/info`, `/v1.<버전>/info` | 고정한 엔진의 `ID`를 반환한다. 사용자 설정이나 엔진 상세 설정은 수집 범위에서 제외한다. |
| `/dsg/v1/inventory` | 엔진과 관측 일관성이 확인된 인벤토리. `--max-resources`에 지정한 1~32개의 범위 내에서 조회한다. |

브로커는 허용 경로 외 호출과 본문·업그레이드 요청을 거부한다. 요청 경로, 헤더, 본문을 엔진으로 전달하는 대신 미리 정의된 Docker 조회를 실행한다. 권한 또는 엔진 검증에 실패하면 연결을 닫거나 실패 응답을 반환한다. [서비스 권한과 복구](OPERATIONS.md#docker-조회-브로커)

Windows SCM 어댑터는 고정 로컬 named pipe에서 `/_ping`, `/info`, `/dsg/v1/inventory`를 제공한다. 시작 인수는 `--listen`, `--upstream`, `--engine-id`, `--service-name`, `--allow-sid`, `--upstream-sid`다. `--allow-sid`에는 쉼표로 구분한 사용자 SID를 지정한다. 엔진 파이프의 서버 토큰·소유자·허용 ACL과 엔진 ID를 확인한다. Windows 인벤토리는 확인된 `container_count`, `image_count`를 반환한다. 볼륨·중복·개별 리소스 상세는 미확인 `null`이며 응답 상태는 `complete=false`, `status=unknown`이다. 배포 상태는 플랫폼 시험 결과를 따른다.

### 로컬 대시보드

`dsg --root <프로젝트> dashboard [--port <포트>] [--lifetime <초>] [--docker-context <컨텍스트>]`는 `127.0.0.1`에서 읽기 전용 화면을 제공한다. 기본 포트는 자동으로 배정되고 기본 실행 시간은 600초다. `--json`은 `result.url`을 반환한다. Docker 집계는 컨텍스트를 지정한 경우에만 요청한다.

`GET /api/overview`는 `health`, `policy`, `docker`, `cleanup`의 집계 객체를 반환한다. 각 부분은 조회가 불가능하면 `null`이다. 자료가 불완전할 때 수치를 미확인으로 표시하며, 완전한 0건은 숫자로 표시한다. 정리 미리보기는 승인이나 적용 이력을 생성하지 않는다. 서비스는 루프백 Host 및 같은 출처 요청을 확인하고 연결을 한 번에 하나씩 처리한다. [운영 방법](OPERATIONS.md#로컬-대시보드)

## 계획 API (미구현)
- `dsg inspect <resource-id>`
- `dsg docker reuse --project <id>`
- `dsg lease acquire --project <id> --ttl 30`
- `dsg gc apply --plan <id> --approval <token>`
- `dsgd` local IPC methods: `health`, `policy.evaluate`, `resource.inventory`, `gc.plan`, `gc.apply`

## 공통 판단 응답(목표)
```json
{"decision":"deny","reason_code":"PROTECTED_VOLUME","message":"Persistent volume cannot be deleted","evidence_ids":["evt_001"],"suggested_action":"inspect"}
```

## 읽기 전용 MCP

`dsg --root <프로젝트> mcp`는 MCP `2025-06-18`의 stdio JSON-RPC 서버다. 클라이언트는 `initialize` 이후 `tools/list`와 `tools/call`을 호출한다. 요청은 줄 단위 JSON으로 최대 64 KiB이며, 조회는 시작 시 지정한 프로젝트와 정책으로 제한한다.

| 도구 | 입력과 반환 값 |
|---|---|
| `dsg_get_health`, `dsg_scan_project` | 인자 없이 프로젝트 예산 및 사용량 집계를 반환한다. |
| `dsg_inspect_docker` | 인자 없이 기본 컨텍스트의 인벤토리 건수와 조회 완전성을 반환한다. 리소스별 환경 변수와 설정값은 조회 범위에서 제외한다. |
| `dsg_check_command` | `{ "argv": ["docker", "volume", "ls"] }` 형태의 배열을 평가해 정책 결과를 반환한다. |
| `dsg_get_gc_plan` | 인자 없이 현재 정리 후보의 건수와 바이트 집계를 반환한다. 승인용 계획을 저장하는 명령은 `gc-plan`이다. |
| `dsg_get_policy` | 인자 없이 현재 적용되는 정책의 수치 한도와 보호 규칙 건수를 반환한다. |
| `dsg_get_execution_status` | 인자 없이 프로젝트의 lease 관측 상태와 시각을 조회한다. |
| `dsg_get_gc_approval_status` | `{ "plan_id": 1 }`을 받아 같은 프로젝트의 승인 기록, 정책 상태 및 만료를 조회한다. |

상태 도구의 macOS 실행은 파일 쓰기가 차단된 프로세스에서 SQLite를 읽는다. 정상 읽기에 필요한 WAL·SHM은 관리형 쓰기 경로에서 보존된다. 상태를 확인할 수 없거나 다른 프로젝트의 기록이면 조회 불가를 반환한다. `approved_recorded`는 승인 기록 존재를 나타낸다. 승인 발급·적용·실행은 관리형 CLI에서 수행한다.

### 오류 코드
`CONFIG_INVALID`, `PROJECT_UNKNOWN`, `RESOURCE_ACTIVE`, `RESOURCE_SHARED`, `RESOURCE_CHANGED`, `POLICY_DENY`, `INSUFFICIENT_SPACE`, `PLAN_EXPIRED`, `NEEDS_APPROVAL`, `ENGINE_UNAVAILABLE`.
