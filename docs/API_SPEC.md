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

### 관리형 실행과 Compose 재사용

`dsg exec --timeout 1~120 -- <argv...>`는 등록된 프로젝트의 정책과 호스트·프로젝트 예산을 확인한 뒤 직접 실행한다. 관리형 실행은 프로젝트별 lease 하나와 동일 상태 저장소의 전역 두 슬롯으로 제한한다. 조회가 불완전하거나 정책상 추가 확인이 필요하면 기존 판단 형식으로 반환한다. 성공 0, 정책 거부 3, 승인 필요 4, 제어 미확인 5, 시간 초과 124, 정상 중단 130을 사용한다. 자식이 실패하면 자식의 종료 코드를 반환한다. `--json`에서는 명령과 자식 원문 출력 없이 `run.outcome`, `run.exit_code`, `run.tree_confirmed_gone`과 `lease_released`를 제공한다.

`dsg docker-reuse --context <context> --expectation-json <JSON>`은 `engine_id`, `project`, `service`, `number`, `config_hash`, `image_id`, `image_digest`, `mounts`를 받는다. 같은 실행 중인 정상 컨테이너가 두 번 모두 유일한 후보면 `verified=true`와 `assessment.container_id`를 반환한다. 확인할 수 없으면 종료 코드 5와 판정 사유를 반환한다. 이미 실행 중인 컨테이너 식별자를 전달하는 경로다.

### 승인 기반 정리

등록된 프로젝트에서 `gc-own --candidate <이름> --confirm-generated`로 직접 하위의 생성 폴더에 소유권을 기록한다. `gc-plan` 결과에서 후보를 확인한 뒤 `gc-approve --plan <번호> --candidate <이름> --confirm`으로 한 후보에 대한 일회용 승인을 발급한다. 승인 유효 기간은 10분 이내이며 계획 만료 시각을 넘지 않는다.

`gc-apply --plan <번호> --approval-stdin`에 승인값을 표준 입력으로 전달한다. 적용은 활성 lease, 소유권, 파일 식별자와 트리 메타데이터를 재확인하며, 입력 승인 원문은 감사 기록과 명령 인자로 보관하지 않는다. 결과는 `applied`, `preserved`, `recovery_required`로 구분한다. 복구가 필요하면 `gc-recover --plan <번호>`가 일치하는 격리 자료를 확인해 빈 원래 경로로 복원을 시도한다.

`exec --auto-clean-cargo -- cargo check …`는 자동 회수 대상인 전용 Cargo 산출물 디렉터리를 만든다. 실행 결과의 `cleanup`에서 회수·보존 건수와 확인된 바이트를 읽는다. Git 완료는 `scripts/git-completion.sh`로 전달한다. 이 경로는 여섯 참조의 파일 일치 후 사용자 원본과 브랜치를 보존하면서 등록된 보조 워크트리만 검사한다.

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
