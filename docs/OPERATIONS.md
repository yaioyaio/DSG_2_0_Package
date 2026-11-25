# 운영 및 설치/진단 매뉴얼

## 요구 환경
POC는 Python 3.10+, 설치 패키지 불필요. Docker 스캔에는 Docker CLI 및 Engine 접근 필요.

## 설치 없이 실행
```
cd poc
python3 -m unittest discover -s tests -v
python3 -m dsg init --root /path/to/sample
python3 -m dsg scan --root /path/to/sample
python3 -m dsg docker-scan
```

## 프로젝트 적용 지침
1. 실제 프로젝트는 **스캔/GC 계획까지만** 사용하여 보고서를 확인한다.
2. 디렉터리 감시 결과에 `node_modules`, `.git`, 영속 데이터가 들어오면 설정을 수정하고 관리 대상으로 지정하지 않는다.
3. `.dsg.json`을 코드 저장소에 포함할지 팀 정책으로 결정한다.
4. Host 여유 공간 정책은 안전 여유 공간이므로 CI 환경에서는 별도 설정 필요.
5. Docker 결과가 실제 호스트 사용량과 다르면 Docker Desktop VM reclaimable/allocated를 분리한다.

## Rust 관리형 실행

`dsg init`으로 프로젝트를 등록한 뒤 `dsg exec --timeout 60 -- cargo check --offline --jobs 1`과 같이 직접 argv를 넘긴다. macOS·Linux의 프로세스 그룹에 실행 시간을 제한하고 프로세스별 CPU·파일 크기·열린 파일 상한을 적용한다. 일반 텍스트 모드는 자식의 터미널 출력을 전달하고 JSON 모드는 결과 구조만 전달한다. macOS에서는 관측한 이탈 자손의 PID와 시작 시각을 기록한다. Linux 실행은 기존 프로세스 그룹 경계로 관리한다. Windows 관리형 실행은 미지원이다.

활성 또는 종료 미확인 lease가 있으면 같은 프로젝트의 실행을 보류한다. 만료된 실행은 OS 잠금이 풀리고 기록된 프로세스 그룹이 종료됐음이 확인돼야 회수된다. macOS에서는 기록된 프로세스 세대가 모두 종료돼야 한다. 관측 시작 뒤 완료 표지가 기록되지 않은 lease는 보존한다. 프로세스가 관측 사이에 이탈하면 기록에 포함되지 않을 수 있다. 실행과 재사용 결과는 비밀값과 argv 원문을 제외한 감사 코드로 기록한다.

## 정리와 완료 이벤트

수동 정리는 먼저 [승인 기반 명령](API_SPEC.md#승인-기반-정리)으로 후보와 소유권을 확인한다. 프로젝트 경계, 보호 파일, 상태 변경, 실행 중인 작업의 증거가 불확실하면 경로를 보존한다. 격리 단계가 남아 있으면 원본 경로와 격리 자료를 대조한 뒤 복구한다.

공개 Docker 이미지 계획과 재확인에서 `apply_gate=preserve_unverified_control`이 표시되면 후보를 보존한다. Docker 자원 변경을 검증하려면 단일 대상을 원자적으로 제어하고 그 결과 영수증을 보존하는 백엔드가 필요하다. 내부 제어 대역의 복구는 적용 의도에 일치하는 영수증만 읽어 판정하며, 결과를 확인할 수 없으면 `recovery_required`로 남긴다. 공개 파일 GC 명령과 MCP 도구는 파일 정리 범위에서 사용한다. [적용 계약](API_SPEC.md#승인-기반-정리)

- 등록된 Cargo 실행은 `--auto-clean-cargo`로 전용 산출물 위치를 선택한다. 종료 이벤트별로 정리 결과를 기록한다. 실패 근거와 종료를 확인하지 못한 산출물은 보존한다.
- 보조 워크트리는 동시 쓰기 충돌을 피해야 할 때만 `dsg worktree-create`로 등록해 만든다. 등록된 Git 완료 경로는 `DSG_CLI=<검증된 dsg 경로> sh scripts/git-completion.sh <feature> <상태 디렉터리>`다. 여섯 참조 파일 일치, 사용·잠금·미반영 자료 검사를 통과하면 정상 Git 회수 후 주 작업 폴더가 `develop`으로 돌아간다.
- 위 진입점을 호출한 실행과 Git 완료 이벤트에 자동화가 적용된다. 회수 바이트는 검사한 논리 크기다. 회수할 수 없는 경로는 보존 결과를 확인한다.
- Playwright 추적 파일은 기본적으로 보존한다. 신뢰하는 테스트를 실행할 때 `dsg exec --provider playwright --trace-ttl-seconds <1~604800> -- test`로 성공 trace의 보관 기간을 정할 수 있다. 모든 테스트가 첫 시도에 예상대로 통과하고 실행 트리 종료와 감사 기록이 확인된 경우만 등록한다. 같은 프로젝트의 다음 정책 적용 종료 이벤트에서 만료 파일을 한 개까지 재검증해 회수한다. 기대 실패 및 교체·사용·소유 여부가 미확인인 파일은 보존한다.

## 에이전트 연동 운영

CLI 훅 설치 후 신뢰 설정과 실행 파일 위치를 확인한다. CLI가 훅을 로드하지 못하면 해당 에이전트에서 보호 명령을 실행하지 않고 설치 상태부터 점검한다. Claude Code의 등록은 훅 실행 실패를 차단하도록 설정한다. Codex와 Gemini는 훅 실행 오류가 발생하면 자체 권한 처리로 진행할 수 있다. 통제된 Docker 접근이 필요한 운영 환경의 수용 기준은 별도의 OS 권한 경계다. [에이전트별 설정 및 검증](../integrations/README.md)

## Docker 조회 브로커

전용 서비스 계정과 제한된 클라이언트 UID를 구분한다. 서비스만 접근 가능한 Docker 소켓과 명시한 엔진 ID가 필요하다. 운영자가 서비스 소유의 비공개 `0700` 업스트림 디렉터리와 비어 있는 `0700` Docker 환경 디렉터리를 제공한다. 수신 소켓은 별도의 서비스 소유 디렉터리에 둔다. 서비스 실행 파일과 소켓 디렉터리의 모든 상위 경로에 대해 그룹·기타 쓰기 권한, 링크 및 부여된 ACL을 확인한다.

엔진의 다른 Unix 소켓, TCP 포트, Docker 그룹이나 동등한 관리자 권한에서 같은 클라이언트가 직접 접근할 수 있는지 확인한다. 확인한 경계 안에서만 강제 통제라고 표시한다. 진단 시 허용 UID로 `GET /_ping`과 `GET /dsg/v1/inventory`를 호출한다. 엔진 장애나 예상 ID 불일치 시 브로커는 `503`을 반환한다. 확인되지 않은 다른 엔진으로 자동 전환하는 대신 같은 경계의 설정·접근을 복구한다.

macOS LaunchDaemon 예시는 비정상 종료 상태를 보존하고 관리자가 확인한 뒤 수동으로 다시 시작하도록 설정한다. Linux systemd 예시는 60초에 두 번의 시작 시도까지만 허용한다. 예시의 계정·클라이언트 UID·엔진 ID와 경로를 검토한 뒤 대상 호스트에서 검증한다. 수신 경로에 외부 파일이나 소유 증거가 없는 소켓이 있으면 그대로 보존한다. [macOS 서비스 예시](../integrations/launchd/LOCAL.DSG.DSGD.plist) · [Linux 서비스 예시](../integrations/systemd/dsgd.service)

Windows SCM 어댑터의 시험 구성은 `LocalSystem` 서비스와 전용 로컬 named pipe를 사용한다. 제한 클라이언트는 데이터 읽기·쓰기, 속성 읽기와 동기화 권한 `0x00100083`으로 브로커 파이프를 연다. 파이프 인스턴스 생성과 속성 쓰기 권한은 `LocalSystem`에만 있다. upstream 서버 계정 SID와 파이프 소유자 SID가 일치하고 제한 계정이 upstream ACL에서 제외돼야 한다. 운영 배포 전에 해당 호스트에서 실제 계정과 SCM 수명 주기를 검증한다. [Windows 플랫폼 검증](TEST_PLAN.md#브로커와-서비스-검증)

## BuildKit 단일 대상 운영

1. 관리자는 Linux ARM64의 전용 BuildKit 등록에 맞춰 검증한 `dsg`를 `/opt/dsg/bin/dsg`에 설치하고 `dsg-buildkit-access` 그룹과 승인·실행용 비루트 계정 두 개를 준비한다. 등록 캡슐의 root 소유 0400 `controller-policy.json`에 버전 1, `approver_uid`, `executor_uid`, 네이티브 실행 파일의 `runtime_sha256`과 전체 `scope`를 기록한다. 두 계정에만 필요한 소켓 그룹 접근을 부여한다. 승인자는 `approve`, 실행자는 `apply`와 `receipt`를 사용한다.
2. root 소유 0700 `/etc/dsg-buildkit-exact`에 root 소유 0400 `operating.json`을 둔다. 객체에는 `version: 1`, `registration`, 보유 증거의 `keeper` 객체를 그대로 넣는다. 서비스는 시작할 때 이 등록의 부팅 지문, 프로세스 세대와 마운트·네트워크 네임스페이스를 확인한다. 보유 프로세스가 종료되면 서비스가 종료되므로 등록 증거를 확인한 뒤 관리자가 시작한다.
3. 초기 등록에 앞서 `/var/lib/dsg-buildkit-witness`를 로컬 승인 원장·작업 저장소와 다른 비휘발 장치에 준비한다. 증거 루트와 등록 ID별 하위 디렉터리는 root 소유 0700으로 둔다. 초기 등록은 `/var/lib/dsg-buildkit-witness/<등록 ID>/ledger.sha256`을 root 소유 0600으로 만들고 장치 번호와 inode를 등록 봉인에 기록한다. 메모리 파일시스템의 증거는 거절된다. 운영 중 원장·체크포인트·별도 증거가 누락되거나 서로 다르면 자료를 보존하고 원인을 조사한다. 파일 교체나 남은 사본에서의 증거 재생성으로 재개하지 않는다.
4. 승인된 대상 호스트에서 [systemd unit](../integrations/systemd/dsg-buildkit-exact.service)을 설치하고 `systemctl daemon-reload`, `systemctl start dsg-buildkit-exact.service`를 실행한다. `/run/dsg-buildkit-exact`의 `root:dsg-buildkit-access` 소유와 0750 권한, `control.sock`의 같은 소유와 0660 권한을 확인한다. unit은 자동 재시작을 사용하지 않는다. 배포 전 실제 두 계정으로 각 역할과 교차 요청 거절을 확인한다. 별도 BuildKit·Docker 소켓과 TCP 리스너, 그룹·ACL·관리자 권한을 통해 대상 변경 경계를 우회할 수 있는지도 점검한다. [요청과 응답](API_SPEC.md#linux-arm64-buildkit-단일-대상-api)

### 외부 최초 적용 확인 서비스

공개 `apply`를 열기 전에 워커 VM의 메모리·디스크 스냅샷에 포함되지 않는 비휘발 저장 위치에 외부 확인 서비스를 배치한다. 서비스 계정 소유 0700 상태 디렉터리에 같은 계정 소유 0400 `expected.json`을 미리 만든다. 값은 승인 직후의 신뢰할 수 있는 기록에서 가져와 `version: 1`, `registration`, `scope_sha256`, `checkpoint_sha256` 네 필드에 고정한다. `scope_sha256`의 입력은 등록 ID, 보유 증거 SHA-256, 적용 의도 SHA-256, 승인 SHA-256을 순서대로 둔 ASCII 줄 네 개이며 마지막 줄도 개행으로 끝난다. [전송 계약](API_SPEC.md#외부-최초-적용-확인)

워커 측의 루트 릴레이는 보호된 상위 경로, 루트 소유 0700 `/run/dsg-buildkit-freshness`, 루트 소유 0600 `authority.sock`을 사용한다. 전송할 때 요청 쓰기 방향의 EOF와 응답 종료를 보존한다. 외부 확인 스크립트에는 `--state-dir`과 함께 같은 상태 디렉터리 안의 `--socket` 또는 요청 하나를 읽는 `--stdio`를 지정한다. 최초 요청이 일치하면 외부 서비스가 `claimed` 표식과 상태 디렉터리를 동기화한 뒤 응답한다. 외부 상태가 유실되거나 응답을 받지 못하면 기존 상태와 표식을 보존한다. VM 안의 원장 또는 복원본으로 외부 확인 상태를 다시 초기화하면 안 된다. [외부 확인 실행기](../scripts/buildkit-freshness-authority.py)

격리 QEMU 검증에서는 요청 전 전체 VM 스냅샷을 복원한 뒤, 루트 릴레이와 macOS 호스트 외부 확인 서비스를 사용해 동일 Go 클라이언트의 재호출이 보존으로 끝나는 것을 확인했다. 외부 최초 소비는 1건, 네이티브 변경은 0건이었다. 성공 응답을 수신한 이후의 RAM 스냅샷을 복원하거나 외부 확인 상태 자체를 롤백하는 상황에서는 적용 경계 밖에서 스냅샷 복원 세대와 중복 VM 실행을 차단해야 한다.

### 프로젝트 등록 준비

전용 워커 검사기의 프로젝트 옵션은 DSG의 추적 `README.md`와 고정 커밋·해시를 새 빈 격리 워커의 캐시 기록에 결속한다. 워커를 정지한 뒤 Linux ARM64의 전용 마운트·네트워크 네임스페이스에서 기존 발급 증거와 저장소의 결속을 읽기 전용으로 재확인할 수 있다. 루트 소유 0700 `/run/dsg-project-enrollment-<hex>/stage`의 보호된 전용 tmpfs 위치에 기존 워커 저장소와 설정을 사용하고, 이전 `cache_read_only_evidence` 객체를 루트 소유 0400 `project-issuance.json`으로 제공한다. 기존 발급 파일의 SHA-256과 패치된 `buildkitd`의 SHA-256은 별도로 확보한 값으로 고정한다. [등록 계약](API_SPEC.md#격리된-프로젝트-전용-워커-검증)

```sh
python3 -IB scripts/check-buildkit-project-registration.py \
  "$workspace" "$retained_buildkitd_sha256" "$retained_issuance_sha256"
```

엄격하게 등록한 워커는 `--require-quiescence`로 검사한다. 이 경우 정지한 실행 세대와 읽기 전용 저장소, 마운트 및 열린 핸들 검사를 모두 통과해야 한다. 읽기 전용 등록 결과의 운영 배포, 운영 계정, 배타 변경, 정확 변경 승인, 영수증 보관 플래그는 모두 `false`다. 지정 운영 호스트에서는 실제 계정과 접근 제한, 배타 변경 경계, 운영 등록 캡슐, 별도 보관 지점을 검증해야 한다.

### 영수증과 검증 해시 보관

운영 실측으로 확정한 영수증 아카이브와 그 SHA-256을 서로 다른 장애 영역에 보관한다. 예를 들어 VM 안에 아카이브를 두고 VM 저장소와 독립적으로 접근 가능한 호스트 경로에 이미 산출한 검증 SHA-256을 보관한다. 복구에는 별도로 보관한 아카이브 SHA-256, 보유 증거 SHA-256, 전체 레코드 ID를 사용한다. VM 원장 증거 `ledger.sha256`은 승인 원장 상태의 검증 파일이며, 보관 아카이브에는 별도의 검증 SHA-256을 사용한다. 관리자 한 명이나 동일 물리 호스트의 장애가 두 저장소에 영향을 줄 수 있으므로 운영자는 전원·물리 매체·관리 권한 공유 범위를 확인한다.

```sh
python3 -IB scripts/check-buildkit-settled-receipt.py \
  --archive "$archive_path" --archive-sha256 "$retained_archive_sha256" \
  --custody-sha256 "$retained_custody_sha256" --record-id "$retained_record_id"
```

아카이브, 검증 해시 각각의 접근 차단과 동시 차단에서 `preserve`, 양쪽을 다시 읽을 수 있는 상태에서 `archived_settled_receipt`를 확인한다. 검증 해시가 유실되면 아카이브에서 다시 계산해 대체하지 않는다. 이 검증의 범위는 격리된 실측 환경의 저장 경로와 읽기 검증이며, 물리 호스트 상실이나 운영 배포의 장애 대응은 대상 환경에서 별도로 확인한다. [아카이브 계약](API_SPEC.md#보관된-운영-영수증-검증)

## 로컬 대시보드

`dsg --root <프로젝트> dashboard`가 출력한 주소를 브라우저에서 연다. 정리 후보 화면은 파일 변경 없이 즉시 조회한 집계를 제공한다. Docker 요약을 사용하려면 명시한 격리·운영 컨텍스트에 대한 자신의 기존 권한을 적용한다. 재조회는 화면의 새로고침 버튼을 사용한다. 터미널에서 `Ctrl+C`로 종료하거나 실행 시간 상한을 사용한다. 같은 호스트에서 연결 가능한 프로세스에 집계가 제공되므로 로컬 사용자 경계를 확인한다. [API와 수명 주기](API_SPEC.md#로컬-대시보드)

## 응급 정리 안내
대규모 prune 대신 Docker의 `docker system df -v`, `docker buildx du`, `docker ps -a`, `docker volume ls`로 먼저 조사. 영속 데이터 보호 확인 후 대상 범위에 국한해 수동 정리.

## 롤백
POC state는 `~/.dsg-poc`(또는 `DSG_STATE_HOME`)에 있습니다. 로그/계획만 들어 있으며 작업 파일 삭제와 별개입니다. `.dsg.json`은 사용자가 직접 삭제할 수 있습니다.
