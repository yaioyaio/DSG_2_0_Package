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

`dsg --json docker-scan --context <context> [--max-resources <1~64>]`는 컨텍스트를 명시해 엔진 ID, 컨테이너·이미지·볼륨, Compose 중복 후보를 조회한다. 허용된 필드만 수집하며 컨텍스트 환경변수로 대상이 바뀌지 않게 한다. 조회가 불완전하면 `complete=false`와 `issues`가 반환된다. 예산 안에서 조회 결과가 비어 있는 경우에는 완전한 0건으로 기록한다. 엔진 ID를 확인할 수 없으면 동일 구조의 JSON과 종료 코드 5를 반환한다.

중복 후보는 관측된 동일성에 관한 정보다. 실제 재사용에 필요한 엔진·구성 해시·이미지·마운트·health가 확인되지 않으면 판정을 미확인으로 남긴다. 마운트 대상 위치와 명명 볼륨을 비교한다. bind 소스는 수집 범위 밖이며, healthcheck가 없거나 이미지 digest가 없는 경우도 재사용 미확인이다.

## 계획 API (미구현)
- `dsg inspect <resource-id>`
- `dsg docker reuse --project <id>`
- `dsg lease acquire --project <id> --ttl 30`
- `dsg gc apply --plan <id> --approval <token>`
- `dsg agents install --agent claude|codex|gemini`
- `dsgd` local IPC methods: `health`, `policy.evaluate`, `resource.inventory`, `gc.plan`, `gc.apply`

## 공통 판단 응답(목표)
```json
{"decision":"deny","reason_code":"PROTECTED_VOLUME","message":"Persistent volume cannot be deleted","evidence_ids":["evt_001"],"suggested_action":"inspect"}
```

## MCP 설계 (미구현)
`dsg_get_health`, `dsg_scan_project`, `dsg_inspect_docker`, `dsg_check_command`, `dsg_get_gc_plan`, `dsg_get_execution_status`, `dsg_get_policy`.

### 오류 코드
`CONFIG_INVALID`, `PROJECT_UNKNOWN`, `RESOURCE_ACTIVE`, `RESOURCE_SHARED`, `RESOURCE_CHANGED`, `POLICY_DENY`, `INSUFFICIENT_SPACE`, `PLAN_EXPIRED`, `NEEDS_APPROVAL`, `ENGINE_UNAVAILABLE`.
