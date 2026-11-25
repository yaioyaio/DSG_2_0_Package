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

`dsg --json docker-buildx-cache --context <context>`는 지정 엔진에 기본 제공되는 Buildx 빌더의 `total_count`, `reclaimable_count`, `shared_count`, `mutable_count`를 읽는다. 이 명령은 빈 임시 Buildx 설정을 사용하고 조회 전후에 엔진 ID를 대조한다. Buildx의 회수 가능 표시는 캐시 상태이며 프로젝트 소유권이나 삭제 승인과 별개다. 이 명령은 항상 `disposition=preserve`와 `candidate_count=0`을 반환한다. 조회 실패·초과 시 상태 집계와 `preserved_by_reason`은 `null`, 종료 코드는 5다. [Docker Buildx 사용량 명령](https://docs.docker.com/reference/cli/docker/buildx/du/)

Linux ARM64의 `dsg --json buildkit-local-cache --socket <절대 Unix 소켓> --expected-pid <데몬 PID> --buildctl <절대 실행 경로>`는 이미 실행 중인 별도 BuildKit의 사용량만 읽는다. 소켓이 속한 디렉터리는 현재 UID 소유의 권한 0700이어야 한다. 같은 PID 네임스페이스의 상대 PID·UID와 소켓 식별자를 조회 전후에 확인한다. 실행 가능 상태 및 SHA-256을 검사한 공식 v0.35.1 `buildctl`만 `du`에 사용한다. 성공 응답의 `counts`에 `total`, `reclaimable`, `shared`, `mutable`과 `candidate_count=0`, `preserved_by_reason`을 제공한다. 소유권 판정은 `unknown_ownership`이며 `disposition=preserve`를 유지한다. 검증이나 조회 실패는 `status=unknown`, `counts=null`, 종료 5로 반환한다. [공식 BuildKit 릴리스](https://github.com/moby/buildkit/releases/tag/v0.35.1) · [실측 근거](HANDOFF.md#별도-buildkit-읽기-전용-실측)

두 조회에서 회수 가능·공유·변경 가능 상태의 집계는 서로 겹칠 수 있다. `preserved_by_reason`은 각 레코드에 사유 하나를 부여한다. 회수 가능 표시가 없으면 `active`, 다음으로 공유면 `shared`, 변경 가능이면 `mutable`, 나머지는 `unknown_ownership`에 집계한다. 정상적인 빈 응답의 상태와 보존 사유는 모두 0으로 집계한다. 중복 ID, 마지막 줄이 잘린 출력과 한도 초과는 부분 수치를 폐기하고 조회 실패로 처리한다. 캐시 ID는 중복 확인에만 사용한다.

공식 v0.35.1에서 준비한 DSG 전용 Linux 데몬은 미리 만든 빈 0700 root에서 `buildkitd --dsg-dedicated-worker --root <절대경로> --oci-worker=true --oci-worker-snapshotter=native --containerd-worker=false`로 최초 등록한다. 외부 snapshotter는 거절한다. `buildkitd --dsg-inspect-dedicated-worker --root <절대경로>`는 같은 디렉터리 inode를 잠근 상태에서 오프라인 DB만 읽는다. 결과는 `worker`, `solver_tx`, `cache_tx`, `containerd_tx`, `snapshot_tx`다. native 스냅샷 DB가 생성되기 전에는 `snapshot_tx=null`이다. 데몬이나 다른 도구가 잠금을 보유했거나 등록·소유권이 확인되지 않으면 거절한다. 오프라인 명령에 `--dsg-record-id <25자리 전체 ID>`를 함께 주면 정확한 레코드의 존재와 스냅샷·가변 별칭·blob 결합, 해당 결합의 SHA-256을 반환한다. 이 명령은 캐시 회수 승인을 발급하지 않는다. [전용 데몬과 조회 구현](../integrations/buildkit-exact/cache/dsg_dedicated_daemon_linux.go)

전용 모드와 오프라인 조회에 `--dsg-require-quiescence`를 주면 등록한 Linux PID 네임스페이스 세대와 로컬 ext4 저장소를 고정한다. 기동은 별도 PID 네임스페이스의 PID 1에서만 등록한다. 조회는 그 세대가 사라지고 ext4 전체가 읽기 전용인 시점에만 시작한다. 관련 마운트·태스크·열린 핸들을 읽기 전후에 점검하며, 가시성을 확인할 수 없는 경우는 거절한다. 엄격 조회의 `quiescence`에 네 경계의 검사 결과를 제공한다. [정지 점검 구현](../integrations/buildkit-exact/cache/dsg_quiescence_linux.go)

정확 ID 조회는 하나의 전용 잠금에서 기존 네 저장소와 존재하는 빌드 이력 DB의 읽기 전용 트랜잭션을 고정한다. `graph`는 캐시, 솔버, containerd, 스냅샷, blob에서 확인한 직접 연결과 `references_sha256`, 미확인 범위 및 보존 판정을 반환한다. `transitive`는 솔버 양방향 인덱스와 지원하는 GC 라벨의 도달 범위 및 미해결 자원을 집계한다. `external`은 이미지·컨테이너·ingest·빌드 이력과 외부 캐시 메타데이터의 지원 형식을 집계한다. `transitive.cache_records`는 캐시 부모·자식과 양방향 별칭 경로의 제한된 탐색 결과다. `native_allocations`에는 확인된 inode, 메타데이터 SHA-256과 할당 블록을 제공한다. 일반 조회에서 파일 내용과 확장 속성은 미확인으로 기록하고 보존 판정을 유지한다. [참조 대조 구현](../integrations/buildkit-exact/cache/dsg_graph_linux.go)

`graph.retention_ownership`은 `checked`, `exclusive`, `reason`과 검증한 lease·자식 스냅샷 수를 반환한다. `owned_roots`와 `owned_child_snapshots`는 구성 요소별 판정이다. 스냅샷의 양방향 인덱스, 같은 네임스페이스의 정확한 lease 리소스, 뷰의 전용 디렉터리 부재를 대조한다. 단일 로컬 content 객체에서 대상 blob으로 들어오는 참조를 확인한 경우 `incoming_content_key_sha256`으로 해당 객체 키의 SHA-256을 반환한다. 이 참조가 있으면 전체 폐쇄 범위를 `exclusive=false`로 판정한다. 응답은 읽기 증거이며 변경 권한을 부여하지 않는다. [소유권 판정](../integrations/buildkit-exact/cache/dsg_graph_ownership_linux.go)

### 로컬 Docker 제어 서비스 읽기 확인

Linux ARM64의 `dsg --json docker-controller-probe --socket <절대 Unix 소켓> --expected-pid <서비스 PID>`는 로컬 Unix 소켓과 상대 프로세스가 선언한 프로토콜을 확인한다. 경로 구성 요소는 심볼릭 링크 없이 열고, 상위 경로의 root 소유와 그룹·기타 쓰기 제한, 소켓 부모의 0700 권한, root 소유 단일 링크 소켓의 0600 권한을 확인한다. 예상 PID와 root 상대 자격을 대조하고, pidfd로 고정한 프로세스의 생존·시작 시각과 소켓 식별자를 요청 전후에 확인한다. 연결에는 2초 기한과 512바이트 응답 한도를 적용한다.

프로토콜 1의 `probe` 요청에 `dedicated` 및 `read_only` 선언이 정확히 돌아오면 `status=verified`, `declared_boundary=dedicated`, `endpoint`, `generation`과 종료 코드 0을 반환한다. 실패 시 `status=unknown`, `failure`와 종료 코드 5를 반환한다. 두 경우 모두 `mutation_authorized=false`, `apply_gate=preserve_unverified_control`이다. 운영 적용에는 별도로 신뢰할 수 있는 권한 제어기, 요청별 승인, 프로젝트 자원의 출처와 소유권, 엔진 또는 빌더의 배타 변경 경계가 필요하다. [프로브 구현](../rust-scaffold/crates/dsg-core/src/docker_control_local.rs)

### 보호된 Docker 제어기 배포 확인

Linux ARM64의 `dsg --json docker-controller-admission --policy <절대 경로>/docker-controller-admission.json`은 배포 정책과 현재 로컬 피어를 읽기 전용으로 대조한다. 정책은 root 소유의 심볼릭 링크 없는 보호 경로에 둔 단일 링크 일반 파일이어야 하며 권한은 0444, 크기 상한은 8 KiB다. 버전 1 정책에 현재 `boot_id`와 `controller`, `engine`, `approver`, `executor`를 지정한다. 각 역할은 `socket`, `pid`, `uid`, `start_ticks`, `executable`, 소문자 16진수 `executable_sha256`을 받는다. 제어기와 엔진은 root 피어이고 승인자와 실행자는 서로 다른 비root UID다. 네 PID와 소켓은 각각 구분된다.

운영 쓰기 어댑터가 사용할 Linux ARM64 코어의 `with_authorized_write_request`는 보호된 제어기 프로세스에서 `SOCK_SEQPACKET` 연결 한 건을 받는다. 리스너 준비 시 `SO_PASSCRED`와 `SO_PASSPIDFD`를 설정하며 Linux 6.5 이상을 요구한다. 버전 1 메시지의 최대 크기는 512바이트다. 승인자는 프로젝트·계획 번호가 매인 `approve`만, 실행자는 같은 범위의 `apply`와 프로젝트·의도 ID가 매인 `recover`를 요청할 수 있다. 요청 JSON의 역할·UID 선언, 연결 피어와 메시지 송신자의 불일치, 종료된 피어, 전달 FD와 잘린 메시지는 거절한다.

디스패치에 전달하는 값은 보호 정책 지문, 제어기·엔진·요청 역할의 세대, 정확한 요청에 묶인다. 후속 어댑터는 같은 배타 변경 경계에서 승인과 의도를 재검증하고, 정책·역할 교체 뒤에도 미확정 의도의 잠금을 같은 엔진에 유지해야 한다. 공개 CLI와 `dsgd`는 이 진입점에 운영 쓰기 요청을 전달하지 않는다. [코어 구현](../rust-scaffold/crates/dsg-core/src/docker_control_local/admission/write_request.rs)

`DockerGcDispatch`는 배타 변경 경계를 소유한 제어기에 연결하는 코어 어댑터다. 커널 검증을 통과한 요청, root의 보호된 등록부, 같은 프로젝트의 활성 lease를 요구한다. 배타 슬롯 아래 대상 상태, 현재 프로젝트 정책, 네 배포 피어의 세대를 확인한다. 보호된 승인 행을 한 번 소비하면서 적용 의도와 감사를 커밋한 뒤에만 백엔드에 대상 변경을 요청한다. 복구는 같은 프로젝트의 전체 의도 ID와 안정적인 엔진 자원 범위를 대조하고 영수증만 조회한다. 활성 정책·프로세스 세대가 바뀌어도 이전 의도는 같은 자원 범위에 남는다. 실제 삭제는 소유권과 영속 영수증을 갖춘 전용 백엔드를 조립한 운영 제어기만 연결할 수 있다. [어댑터 구현](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/dispatch.rs)

Linux ARM64 제어기는 승인 정책과 같은 디렉터리의 보호된 소유 등록을 고정한다. 승인·적용은 현재 프로젝트 소유 지문, 자원 종류·전체 ID와 저장소를 대조한다. 백엔드가 대상의 쓰기 잠금을 잡은 상태에서 정확한 관측을 반환해야 SQLite 원장이 승인 한 건을 소비하고 적용 의도를 커밋한다. 이 커밋 뒤에도 같은 변경 잠금 아래에서만 백엔드가 대상 변경과 영속 영수증을 마칠 수 있다. 응답 유실은 `recovery_required`와 0 B로 남긴다. 복구는 현재 프로젝트의 이전 의도와 동일한 안정 저장소를 대조하고 정확한 영수증만 읽는다. [Rust 제어기](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/dispatch.rs)

Unix 피어 자격증명, pidfd, 프로세스 시작 시각, 실행 파일 식별자와 해시, 소켓 식별자를 대조하고 읽기 프로브는 제어기에만 보낸다. 성공 시 `status=verified`, 제어기와 엔진의 `endpoint`·`generation`, `role_separation=verified`, `engine_boundary=protected_unix_socket`과 종료 코드 0을 반환한다. 불일치는 `status=unknown`, `failure`와 종료 코드 5를 반환한다. 두 경우 모두 `mutation_authorized=false`, `apply_gate=preserve_unverified_control`을 유지한다. 운영 쓰기에는 제어기 배포 출처·신뢰, 프로젝트 자원 출처·소유권, 요청별 승인, 배타 변경 경계와 공개 운영 어댑터 검증이 필요하다. [구현](../rust-scaffold/crates/dsg-core/src/docker_control_local/admission.rs)

### 전용 Docker 백엔드 읽기 전용 점검

Linux ARM64에서 `dsg --state-dir <절대경로> --json docker-backend-preflight --policy <배포정책> --ownership <소유등록> --project <등록번호> --kind <image|build_cache> --resource <전체ID>`로 점검한다. `docker-backend-ownership.json`은 root 소유의 보호 경로에 권한 0444로 둔다. 버전 1에는 배포 정책 원본의 SHA-256인 `admission_policy_sha256`과 `controller_executable_sha256`, `engine_executable_sha256`, `store_directory`, `store_id`, `controller_store_fd`, `resources`를 기록한다. 자원 항목마다 `project_id`, `project_owner_sha256`, `kind`, `id`를 대조한다. root가 보유한 저장소 디렉터리의 고정 표지는 `docker-backend-store-id`에 64자리 소문자 16진수와 끝 개행으로 기록한다. SQLite의 보호 상태는 쓰기 보조 파일이 없는 시점에 읽기 전용으로 확인한다.

성공은 보호된 자원 배정, 제어기가 열고 있는 저장소 디렉터리, 현재 엔진의 보호된 Unix 소켓과 안정 저장소 범위를 보고한다. 불일치는 종료 코드 5로 보존한다. 결과에서 `exclusive_mutation=unverified`, `mutation_authorized=false`를 유지한다. 실제 엔진에 적용하려면 해당 엔진 내부의 독점 변경 경로와 영속 영수증을 별도로 검증한다. [점검 구현](../rust-scaffold/crates/dsg-core/src/docker_control_local/admission/backend_ownership.rs)

### 관리형 실행과 Compose 재사용

`dsg exec --timeout 1~120 -- <argv...>`는 등록된 프로젝트의 정책과 호스트·프로젝트 예산을 확인한 뒤 직접 실행한다. 관리형 실행은 프로젝트별 lease 하나와 동일 상태 저장소의 전역 두 슬롯으로 제한한다. 조회가 불완전하거나 정책상 추가 확인이 필요하면 기존 판단 형식으로 반환한다. 성공 0, 정책 거부 3, 승인 필요 4, 제어 미확인 5, 시간 초과 124, 정상 중단 130을 사용한다. 자식이 실패하면 자식의 종료 코드를 반환한다. `--json`에서는 명령과 자식 원문 출력 없이 `run.outcome`, `run.exit_code`, `run.tree_confirmed_gone`과 `lease_released`를 제공한다.

macOS에서는 관리형 실행 중 확인한 리더와 자손을 PID와 시작 시각으로 등록한다. 등록된 자손은 별도 프로세스 그룹으로 이동해도 같은 프로세스 세대로 추적한다. 만료된 lease를 회수하려면 완료된 관측과 등록된 모든 세대의 종료를 확인해야 한다. 같은 PID가 다른 시작 시각으로 재사용되면 이전 세대는 종료된 것으로 본다. 진위가 불명확하거나 관측이 미완료이면 lease를 보존한다. Linux는 리더와 프로세스 그룹의 종료를 확인하는 기존 경로를 사용한다. macOS의 주기적 관측은 두 관측 사이에 생성돼 추적 중인 계보에서 빠져나간 자손을 놓칠 수 있다.

`dsg exec --provider <cargo|node|pnpm|gradle|uv|go|playwright> -- <작업>`는 등록된 정형 작업을 실행한다. Cargo는 `build`, `check`, `test`, pnpm·Gradle·Go는 `build`, `test`, Node·uv·Playwright는 `test`를 받는다. 하나의 작업만 받으며 각 도구의 병렬 실행 옵션을 제한한다. pnpm이 호출한 프로젝트 스크립트 내부의 작업자는 프로젝트 설정을 따른다. Playwright는 프로젝트에 준비된 실행기를 사용하고 실행마다 독립된 상태 폴더에 성공·실패 추적 자료를 저장한다. 응답 `evidence_path`는 상태 디렉터리 기준 상대 경로다. 트리 종료가 확인되면 Provider·작업·결과 코드가 해당 lease 감사 이벤트에 기록된다.

`dsg docker-reuse --context <context> --expectation-json <JSON>`은 `engine_id`, `project`, `service`, `number`, `config_hash`, `image_id`, `image_digest`, `mounts`를 받는다. 같은 실행 중인 정상 컨테이너가 두 번 모두 유일한 후보면 `verified=true`와 `assessment.container_id`를 반환한다. 확인할 수 없으면 종료 코드 5와 판정 사유를 반환한다. 이미 실행 중인 컨테이너 식별자를 전달하는 경로다.

`--compose-projection-stdin`을 추가하면 최대 32 KiB의 비민감 JSON을 표준 입력으로 받는다. 최상위 필드는 `baseline_config_hash`, `baseline`, `desired`다. 각 구성에는 `service`와 필수 필드인 `build_inputs_sha256`이 있으며 빌드 입력이 없을 때는 `null`, 있을 때는 64자리 16진수 SHA-256을 넣는다. `service`는 레지스트리를 명시하고 digest로 고정한 `image`, `platform`(`linux/amd64` 또는 `linux/arm64`), `init`, `read_only`, `volumes`를 받는다. 각 명명 볼륨 항목은 `type: "volume"`, `source`, `target`, `read_only`, `volume.nocopy`를 포함한다. 호출자는 기준 투영이 관측된 Compose 구성 해시에 대응하도록 준비한다. DSG는 기준 해시·이미지 digest·마운트를 기존 기대값과 대조하고 두 번의 Docker 조회로 기존 재사용 조건과 이미지 플랫폼을 확인한다. 입력 범위는 이 JSON의 필드로 한정된다.

`--json` 응답은 `verified`, 기존 재사용 판정을 담는 `baseline`, 투영 비교 결과인 `compose`를 제공한다. `compose.status`가 `same`이면 `verified=true`와 종료 코드 0을 반환한다. 변경이 확인되면 `changed`와 종료 코드 5를 반환한다. 원하는 구성에 빌드 입력 해시가 있고 해시나 플랫폼이 바뀌면 `action=rebuild`를, 그 밖의 서비스 또는 빌드 입력 변경에는 `action=recreate`를 반환한다. 비교할 수 없으면 `unknown`과 종료 코드 5를 반환한다. `reason`은 입력 문제(`invalid_projection`, `input_too_large`, `input_unavailable`), 기대값과의 불일치(`baseline_mismatch`), Docker 관측에서 기준을 확인할 수 없는 경우(`baseline_unverified`)를 구분한다. 조회 전 입력 오류나 불일치는 `baseline=null`로 표시한다. 이 경로는 판정까지만 수행하며 옵션을 생략하면 기존 명령 계약을 그대로 사용한다.

### 승인 기반 정리

등록된 프로젝트에서 `gc-own --candidate <이름> --confirm-generated`로 직접 하위의 생성 폴더에 소유권을 기록한다. `gc-plan` 결과에서 후보를 확인한 뒤 `gc-approve --plan <번호> --candidate <이름> --confirm`으로 한 후보에 대한 일회용 승인을 발급한다. 승인 유효 기간은 10분 이내이며 계획 만료 시각을 넘지 않는다.

`gc-apply --plan <번호> --approval-stdin`에 승인값을 표준 입력으로 전달한다. 적용은 활성 lease, 소유권, 파일 식별자와 트리 메타데이터를 재확인하며, 입력 승인 원문은 감사 기록과 명령 인자로 보관하지 않는다. 결과는 `applied`, `preserved`, `recovery_required`로 구분한다. 복구가 필요하면 `gc-recover --plan <번호>`가 일치하는 격리 자료를 확인해 빈 원래 경로로 복원을 시도한다.

`gc-plan --docker-context <context> --docker-engine <ID> --docker-image <sha256:전체ID>`는 파일 정리 계획과 별도로 `plan.docker_images`에 이미지 후보 또는 보존 사유를 기록한다. 후보는 선택한 엔진, 프로젝트의 소유 표식 `io.dsg.gc.owner`(정규 경로의 SHA-256), 전체 이미지 ID가 일치하며 태그·digest·컨테이너 참조·활성 lease가 없어야 한다. 관측 한도 초과와 이미지 마운트의 참조 불명은 보존한다. `docker-image-recheck --plan <번호>`는 유효기간과 정책을 확인하고 같은 엔진에서 후보 지문을 비교한다. 공개 이미지 경로의 계획과 재확인은 적용 게이트를 기본값인 `preserve_unverified_control`로 반환한다.

내부 Docker GC 서비스의 제어 대역은 이미지 또는 빌드 캐시 한 건을 프로젝트 ID, 프로젝트 루트 SHA-256, 정책 digest, 엔드포인트 지문, 세대 지문, 자원 종류, 전체 ID, 관측 스냅샷에 묶는다. 승인에는 계획과 만료 시각이 연결되며 적용은 레지스트리 슬롯을 배타적으로 확보하고 일회 승인을 소비하면서 적용 의도를 커밋한다. 제어 백엔드는 동일한 상태를 확인하는 경계에서 단일 대상 변경과 결과 영수증의 기록을 원자적으로 처리해야 한다. 복구에서는 적용 의도와 정확히 일치하는 영수증을 조회해 결과를 확정한다. 영수증이 없거나 불일치하면 `recovery_required`로 처리하며 변경을 재실행하지 않는다. 이 내부 경로는 공개 파일 GC 명령과 MCP 도구에서 분리돼 있다.

제어 경계 밖에서 삭제 결과의 관측 자료만 확인돼도 복구는 영수증을 별도로 조회한다. 유효한 영수증이 없으면 삭제 완료를 추정하지 않고 `recovery_required`와 정산 0바이트를 기록한다. 적용에 사용한 승인은 소비 상태로 보존되며, 같은 계획의 재적용과 복구 대기 중인 동일 자원에 대한 경쟁 적용을 차단한다.

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

## Linux ARM64 BuildKit 단일 대상 API

`dsg buildkit-exact-controller <등록 ID>`는 등록된 보유 프로세스의 격리 네임스페이스에서 수락된 로컬 AF_UNIX `SOCK_SEQPACKET` 연결을 표준 입력(fd 0)으로 받아 요청 하나를 처리한다. 등록 ID는 소문자 16진수 12–64자다. 패킷은 최대 4 KiB의 JSON 객체이며 `version: 1`, `operation`, `scope`를 받는다. `scope`는 `project`, `worker`, `record_id`, `generation`, `origin`, `metadata_digest`, `approval`, `intent`, `custody_sha256`의 전체 값을 요구한다. 프로젝트·세대·출처·메타데이터·승인·적용 의도·보유 증거는 각각 SHA-256 값이며, 워커와 레코드는 전체 ID다. 커널의 연결 자격과 메시지 자격·pidfd가 같은 생존 프로세스를 가리켜야 한다. 루트 관리자가 설치한 정책의 서로 다른 비루트 UID가 각각 승인과 실행을 담당한다. 예외 요청은 변경 전에 거절된다.

| 요청 | 역할 | 정상 응답 |
|---|---|---|
| `approve` | 승인자 | `{"status":"approved","receipt":null}` |
| `apply` | 실행자 | `{"status":"applied","receipt":{…}}`. 승인·세대·같은 마운트의 현재 증거를 재확인하고 외부 최초 적용 확인과 로컬 승인 소비를 거쳐 변경한다. |
| `receipt` | 실행자 | `{"status":"settled","receipt":{…}}`. 저장된 변경의 영수증을 확인하며 네이티브 변경을 다시 호출하지 않는다. 논리 회수와 물리 회수량을 별도로 표시한다. |

요청 제어기는 SHA-256이 고정된 네이티브 `dsg-buildkit-exact`에 메서드와 등록 ID를 전달한다. 관리자가 보호한 등록에서 승인·출처·실행 파일·보유 프로세스·같은 마운트와 원장 체크포인트를 독립적으로 검증한다. 클라이언트가 경로, 임의 명령, UID 또는 승인 원문을 지정할 수 없다. 요청 제어기의 권한 거절은 `{"status":"preserved","reason":"controller_admission"}`과 종료 5, 네이티브 보존 판정은 본문 없이 종료 5다. [기존 요청 제어기 실측](HANDOFF.md#buildkit-공개-api와-승인-원장-복구)

### 운영 서비스 소켓

Linux ARM64에서 `/opt/dsg/bin/dsg buildkit-exact-service`는 `/run/dsg-buildkit-exact/control.sock`으로 위 패킷 계약을 제공한다. 배포 unit은 `root:dsg-buildkit-access`로 실행하며 런타임 디렉터리는 0750, 소켓은 0660이다. 보호된 `/etc/dsg-buildkit-exact/operating.json`은 `version: 1`, `registration`, `keeper`를 받는다. `keeper`는 보유 증거의 같은 객체이며 `pid`, `start_ticks`, `boot_sha256`, `mount_namespace`, `network_namespace`를 포함한다. 서비스는 등록된 보유 프로세스와 두 네임스페이스를 고정하고 수락한 연결마다 자식 요청 제어기만 해당 네임스페이스에 진입시킨다. 그룹은 연결을 허용하며, 요청별 권한은 등록 캡슐의 `controller-policy.json`이 지정한 두 비루트 UID와 커널 자격을 대조해 판정한다. [배포와 접근 점검](OPERATIONS.md#buildkit-단일-대상-운영)

운영 등록은 로컬 승인 원장, 로컬 체크포인트, 별도 장치에 등록한 원장 증거를 대조한다. 각 원장 단계는 로컬 DB 커밋, 체크포인트 `fsync`, 별도 원장 증거 `fsync` 순서로 기록한 뒤 후속 효과를 허용한다. 세 자료가 누락되거나 불일치하면 적용과 실시간 영수증 조회를 보존으로 처리한다. 이때 남은 파일에서 원장 증거를 재생성하지 않는다.

### 외부 최초 적용 확인

공개 `apply`는 읽기 전용 마운트에서 로컬 증거를 대조한 후 `/run/dsg-buildkit-freshness/authority.sock`에 최초 적용을 요청한다. 네이티브 실행기는 루트 소유 0700 디렉터리, 루트 소유 0600 Unix 스트림 소켓, 커널 피어 UID 0을 요구한다. 로컬 소켓의 상대는 워커 VM 스냅샷 밖에 상태를 보관하는 외부 확인 서비스에 요청을 전달한다. [운영 배치](OPERATIONS.md#외부-최초-적용-확인-서비스)

요청은 ASCII `DSG-FRESHNESS-1 <registration> <scope_sha256> <checkpoint_sha256> <nonce>` 한 줄과 개행이며, 클라이언트는 쓰기 방향을 닫아 EOF를 보낸다. 범위 해시는 마지막 개행을 포함한 ASCII `registration\ncustody_sha256\nintent\napproval\n`의 SHA-256이다. 체크포인트 해시는 승인 상태의 로컬 체크포인트와 일치해야 한다. 외부 서비스는 사전에 등록한 버전 1의 네 필드 `version`, `registration`, `scope_sha256`, `checkpoint_sha256`을 확인한다. 외부 서비스가 최초 소비 표식 파일과 부모 디렉터리를 각각 `fsync`한 뒤, 같은 nonce의 `DSG-FRESHNESS-1 CLAIMED <nonce>` 한 줄을 반환하고 연결을 닫는다. 소켓과 `--stdio` 전송 모두 EOF까지의 단일 요청을 읽는다. [외부 서비스 구현](../scripts/buildkit-freshness-authority.py)

동일 체크포인트에서 로컬 승인을 소비하고 동기화한 뒤에만 마운트를 쓰기 가능으로 전환한다. 외부 상태·응답의 유실, 재요청, 타임아웃, 범위 불일치 또는 로컬 체크포인트 변경은 적용을 보존으로 처리한다. 새 적용은 기존 외부 표식을 보존한 상태에서 별도 등록과 승인을 거친다.

### 스냅샷 밖 단일 실행

스냅샷을 사용하는 호출 환경에서는 스냅샷 밖의 실행 호스트가 전용 워커의 쓰기 권한과 네이티브 실행을 보유한다. 승인·실행 요청은 기존의 역할·범위 검사를 거친다. 외부 실행기의 표준 입출력은 인증을 끝낸 실행 요청 하나를 전달하기 위한 관리 전송이다. 워커 VM 내부의 네이티브 `apply`를 그대로 사용하면 외부 최초 적용 확인 응답 이후의 RAM 복원에 영향을 받는다. [배치 조건](OPERATIONS.md#스냅샷-밖-전용-실행기)

요청은 ASCII `DSG-EXECUTION-1 <registration> <job_sha256>` 한 줄과 LF, EOF로 끝난다. 등록 ID는 소문자 16진수 12–64자다. 작업 해시는 실행 호스트에 미리 보관한 `job.json`의 원본 바이트를 SHA-256으로 계산한 소문자 16진수 64자다. `job.json`은 `version: 1`, `registration`, 전체 `scope`, `operator_sha256`을 포함한다. 호출자가 실행 파일, 인자, 상태 경로 또는 적용 범위를 요청에 넣을 수 없다.

실행기는 잠근 `execution-state.json`의 작업 해시와 `ready` 상태를 확인한다. 해당 파일의 장치와 inode에 연결한 `execution-claimed`를 배타 생성해 파일과 상위 디렉터리를 동기화하고, 같은 상태 파일을 `pending`으로 기록하고 동기화한 뒤에만 해시가 고정된 네이티브 `apply`를 시작한다. 네이티브 경로는 외부 최초 적용 확인과 로컬 승인 소비를 수행한다. 네이티브 성공 영수증의 범위, 논리 레코드 1건과 `PhysicalBytes: null`을 확인한 뒤 `settled` 상태와 영수증을 저장하고 동기화한다.

성공은 `{"status":"settled","receipt":{…}}`와 종료 0이다. 동일 요청에서 이미 완료한 상태와 시작 표식이 일치하면 저장한 영수증을 반환한다. `pending`, 잠금 경합, 타임아웃, 상태 파일 유실, 완료 상태의 시작 표식 유실 또는 불일치는 `{"status":"preserve"}`와 종료 1로 처리한다. 전송이 끊긴 경우 보존 응답도 유실될 수 있다. 실행기는 확인이 끝나지 않은 변경을 자동으로 재시도하지 않는다. [외부 실행기](../scripts/buildkit-external-executor.py)

### 보관된 운영 영수증 검증

`scripts/check-buildkit-settled-receipt.py`는 `--archive`, `--archive-sha256`, `--custody-sha256`, `--record-id`를 받는다. 운영 실측 아카이브는 `purpose: operating_controller`와 `attempt.postconditions.completion_store_hashes`의 `checkpoint`, `ledger_witness`를 포함한다. 두 필드는 같은 64자리 소문자 SHA-256이어야 한다. 기존 원장·저널·네이티브 저장소 해시, 적용과 읽기 전용 복구의 동일 영수증, 같은 읽기 전용 마운트 및 DB 사후조건도 함께 검사한다. 이 계약의 입력은 운영 응답을 포함해 별도로 보관한 아카이브다.

검사 성공은 `status: archived_settled_receipt`, `purpose: operating_controller`, `record_id`, `receipt_sha256`, `mutation_calls: 0`과 종료 0을 반환한다. 아카이브나 독립 입력이 누락되거나 일치하지 않으면 `{"status":"preserve","mutation_calls":0}`과 종료 1을 반환한다. 검증에는 별도 장애 영역에서 보관한 기존 아카이브 SHA-256을 사용한다. [보관 절차](OPERATIONS.md#buildkit-단일-대상-운영)

### 격리된 프로젝트 전용 워커 검증

Linux ARM64의 `scripts/check-dedicated-buildkit-fence.py <workspace> <daemon_sha256> <client_sha256>`는 기존 격리 시험에 프로젝트 입력을 추가할 수 있다. `--project-source <workspace/README.md>`, `--project-source-sha256 <64자리 SHA-256>`, `--project-id dsg`, `--project-revision <40자리 Git 커밋>`은 함께 지정한다. 입력은 동일 파일시스템의 단일 링크를 가진 루트 소유 0400 일반 파일이어야 한다. 최대 16 KiB를 읽고 경로·파일 식별자의 안정성과 내용 해시를 검증한다. 호출자가 고정한 Git 리비전은 검증 증거에 기록한다.

격리된 native 워커에 입력을 빌드하고 LLB, OCI 레이어 및 정확 ID의 오프라인 조회 증거를 반환한다. `cache_read_only_evidence.project_registration`은 발급 당시의 프로젝트 식별자와 입력 해시를 기록한다. [발급 스크립트](../scripts/check-dedicated-buildkit-fence.py)

`scripts/check-buildkit-project-registration.py WORKSPACE DAEMON_SHA256 ISSUANCE_SHA256 [--require-quiescence]`는 Linux ARM64의 보호된 격리 작업 위치에서 기존에 정지한 워커를 조회한다. 루트 소유 0400 `project-issuance.json`에는 기존 `cache_read_only_evidence` 객체를 넣고, 독립 보관한 파일 원본 바이트의 SHA-256을 인자로 전달한다. 스크립트는 발급 증거의 내부 해시, 실행 파일 해시, 워커·레코드의 전체 ID, 네이티브 결합 해시, 조회 전후 저장소 식별자와 DB 해시를 대조한다. 기본 조회는 전용 워커의 배타 잠금을 확인한다. `--require-quiescence`는 엄격 등록, 읽기 전용 파일시스템, 실행 세대 종료, 마운트 및 외부 열린 핸들의 네이티브 검사를 추가로 요구한다.

일치하면 `schema: dsg-buildkit-project-read-only-registration-v1`과 `disposition: preserve`의 증거를 반환한다. `production_deployment_verified`, `production_operating_accounts_verified`, `production_exclusive_mutation_verified`, `exact_mutation_authorized`, `receipt_storage_verified`는 모두 `false`다. 운영 서비스 등록은 지정 호스트에서 운영 계정, 배타 변경 경계, 운영 등록 캡슐과 영수증 보관 지점을 확인해야 한다. [등록 검증기](../scripts/check-buildkit-project-registration.py)

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
