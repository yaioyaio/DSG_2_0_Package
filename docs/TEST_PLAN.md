# Test Plan / Quality Gates

## 테스트 레벨
1. Unit: config validation, risk detection, path trust boundary, size walker, state transition.
2. Integration: SQLite WAL concurrency, Docker Engine fixtures, cache Provider contracts.
3. E2E: macOS/Windows/Linux and CLI hook actual invocations.
4. Fault injection: SIGKILL, power loss, malformed state, symlink swaps, concurrent GC.
5. Security: directory traversal, malicious project config, Docker socket bypass, symlink race, platform-specific junction.

## POC 실행
```
cd poc
python3 -m unittest discover -s tests -v
python3 -m dsg doctor --root .
python3 -m dsg check -- docker volume prune
python3 -m dsg gc-plan --root .
```

## Rust Foundation 검증

Rust 의존성을 받은 뒤 저장소 루트에서 `sh scripts/test-rust.sh`를 실행한다. Rust 빌드와 테스트의 동시성은 각각 1이며 임시 Cargo 대상 폴더를 사용한다. macOS·Linux 셸에서 Python 3이 필요하다.

러너는 워크스페이스 테스트를 실행한 뒤 동일한 합성 프로젝트를 Python POC와 Rust CLI로 검사한다. 보호 이름을 포함한 하위 트리가 보존되고, 정상 후보의 상대 경로와 바이트 수가 일치해야 한다. SQLite 등록·중복·감사 기록은 임시 상태 저장소에서 검증한다.

잘못된 필드 타입과 미정의 필드가 들어간 합성 정책은 `CONFIG_INVALID`로 거절해야 한다. 임시 희소 파일은 디스크 블록을 거의 사용하지 않으며 스캔에서 논리 크기 5 GiB를 반환해야 한다.

Windows 스캔·계획은 로컬 NTFS에서 파일·볼륨 ID를 핸들로 확인한다. macOS에서 `cargo xwin test --manifest-path rust-scaffold/Cargo.toml -p dsg-core --lib --target aarch64-pc-windows-msvc --no-run --locked --offline --jobs 1`로 시험 실행 파일을 빌드하고, 전달 시 해시를 대조한 뒤 기존 Parallels Windows ARM64에서 해당 파일에 `filesystem::tests --test-threads=1`을 전달한다. 검사 중 디렉터리 교체 차단과 검사 종료 후 정상 교체, 동일 시간·크기의 하위 파일 교체 감지, 하드 링크·junction·공유 위반 경로 제외와 원본 보존을 확인한다. 이 시험의 정리 경로는 dry-run이다.

Docker 읽기 전용 단계에서는 Docker 명령 대역으로 컨텍스트 고정, 빈 목록과 미확인 상태의 구분, 중복 후보 및 변경 상황을 검증한다. 격리 엔진이 이미 동작 중이면 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test sh scripts/test-rust.sh`로 실제 조회를 추가 검증한다. 엔진 시작·리소스 생성·삭제는 이 조회 검증에서 수행하지 않는다.

동일한 격리 컨텍스트에서 Buildx 플러그인이 준비되어 있으면 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 sh scripts/test-rust.sh`로 명시한 엔진의 기본 Buildx 빌더를 실측한다. 명령은 사용자 Buildx 설정 대신 임시 설정을 사용한다. 동일 엔진 ID, 항목 수와 각 상태 수의 범위, `candidate_count=0`, `preserved_by_reason` 합계가 전체 항목 수와 일치하는지 확인한다. 별도 BuildKit 읽기 시험은 Linux ARM64 Ubuntu Parallels에서 임시 전용 소켓과 공식 v0.35.1 클라이언트를 사용한다. [조회 계약](API_SPEC.md#docker-읽기-전용-인벤토리)의 제품 명령으로 빈 캐시와 시험 캐시, 잘못된 PID·클라이언트 해시·소켓 링크 거절을 확인한다. 소유권과 GC는 별도 수용 기준이다.

2단계 파서·어댑터 대상 시험 7건이 통과했다. 정상 빈 집계, 중복 ID·잘린 출력의 부분 집계 폐기, 중복될 수 있는 원래 상태 집계와 항목당 하나인 보존 사유의 우선순위를 확인했다. 지정 엔진의 실제 Buildx 조회에서는 전체 208개, 회수 가능 표시 208개, 공유 8개, 변경 가능 167개, 회수 후보 0개를 확인했다. 별도 BuildKit 서버의 실측 근거는 기존 Ubuntu ARM64 Parallels 결과를 사용했다. [BuildKit 실측](HANDOFF.md#별도-buildkit-읽기-전용-실측)

관리형 실행 검증에는 임시 프로젝트의 작은 Cargo 빌드와 시간 초과 뒤 재실행, lease 경합·회수 및 등록부 마이그레이션이 포함된다.

3단계 대상 러너 4건, 레지스트리 17건, macOS 프로세스 식별 1건이 통과했다. 실제 macOS에서 `python3 -IB scripts/check-managed-execution.py <dsg> --macos-lease-only`로 소유한 이탈 자손을 등록한 뒤 두 경로를 확인했다. 첫 경로는 리더의 정상 종료 뒤 실행 감독자가 `RUN_CONTROL_UNVERIFIED`와 종료 코드 5를 반환하며 lease를 유지했다. 둘째 경로에서는 소유 실행 감독자만 SIGKILL로 종료했다. 두 경로 모두 리더 종료와 별도 그룹 자손의 생존을 확인한 뒤 같은 프로젝트 실행이 차단되는지 검사했다. 이어 임시 시험 SQLite의 해당 lease 행에서 획득 시각을 0, 만료 시각을 1로 조정해 만료를 재현했다. 제품 TTL과 heartbeat는 그대로 사용했다. 이후에도 자손이 살아 있으면 재실행이 차단됐고 자손의 자진 종료 뒤 lease를 재획득했다. 회수 감사 이벤트는 경로별 1건이다.

사전에 digest가 있는 공개 fixture 이미지를 보유한 격리 엔진에서 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 sh scripts/test-rust.sh`로 건강한 컨테이너 하나를 확인한다. 반복 `up`의 동일 ID와 추가 컨테이너 0개를 확인한다. 구성 변경 시험에서는 원래 인스턴스를 회수하고 새 인스턴스에서 과거 기대값을 거절한다. 회수 직전에 동일 엔진과 시험 프로젝트·서비스·라벨·전체 컨테이너 ID를 대조한다.

같은 Compose 시험에서 엔진 ID와 단일 시험 컨테이너의 ID·라벨을 대조한 뒤 컨테이너의 `/dev/shm`에 상태 표식을 쓴다. 구성·이미지·마운트가 같은 컨테이너가 `unhealthy`가 된 경우 재사용은 `not_healthy`와 종료 코드 5를 반환해야 한다. 시험은 기존의 격리 컨테이너 회수 경로를 사용한다.

`compose_projection` 대상 시험 4건이 통과했다. 코어 단위 시험 3건은 명명 볼륨 순서와 16진수 대소문자 정규화, 서비스 변경과 빌드 입력 변경의 구분, 지원 범위를 벗어나거나 불완전한 입력의 `unknown` 판정을 확인했다. 합성 Docker CLI 대역을 사용한 통합 시험 1건은 두 번의 조회, 옵션을 생략한 기존 JSON 계약, 동일 투영의 종료 코드 0, 재생성·재빌드 판정의 종료 코드 5를 확인했다. 컨테이너 교체·비정상 health·이미지 플랫폼 불일치에는 `baseline_unverified`와 종료 코드 5를 확인했다. 기준 해시 불일치와 알 수 없는 필드는 Docker 조회 0회로 종료하고, 오류 응답에 입력 원문이 포함되지 않는지 확인했다. 실제 격리 엔진 검증 기준은 아래에 별도로 명시한다.

이미지·`tmpfs` 실측에는 같은 명령에 `DSG_TEST_COMPOSE_IMAGE`를 추가하고 값으로 격리 엔진에 준비된 공식 `docker.io/library/alpine@sha256:…` 참조를 지정한다. 두 번째 컨테이너는 원래 이미지와 ID·digest가 다른 이미지와 64 KiB `tmpfs`를 사용한다. 실제 이미지와 마운트를 조회한 뒤 구성·이미지·마운트 기대값을 개별 대조해 두 관측에서 각각 `configuration_changed`, `image_changed`, `mount_changed`와 종료 코드 5를 확인한다. 관측과 같은 `tmpfs`를 기대값에 넣어도 두 관측은 `unknown/unverified_mount`와 종료 코드 5를 반환해야 한다.

명명 볼륨 교체는 `DSG_TEST_COMPOSE_IMAGE` 없이 기본 Compose 시험 명령에 `DSG_TEST_COMPOSE_VOLUMES`와 `DSG_TEST_COMPOSE_VOLUME_ENGINE_ID`를 추가해 실행한다. 값은 각각 `dsg-reuse-volume-<12자리 소문자 16진수>-a,dsg-reuse-volume-<같은 값>-b` 형식인 외부 볼륨 두 개와 확인한 격리 엔진 ID다. 두 볼륨은 옵션 없는 로컬 볼륨이어야 하며 `dsg.fixture.reuse-volume` 라벨 값이 이름에 포함된 12자리 값과 일치해야 한다. 볼륨 A를 읽기 전용·`nocopy`로 연결하고 같은 컨테이너의 재사용을 두 번 확인한다. 실제 B로 교체한 뒤 현재 구성 해시와 이미지 정보를 두 요청에 똑같이 사용한다. 볼륨 A를 기대하면 `mount_changed`와 종료 코드 5, B를 기대하면 재사용 성공을 반환해야 한다. 시험 후 소유 컨테이너를 회수하고 외부 볼륨 두 개가 미연결 상태로 보존됐는지 대조한다. [실측](HANDOFF.md#명명-볼륨과-ubuntu-parallels-실측)

`DSG_TEST_BUILD_PROVIDERS=1 sh scripts/test-rust.sh`는 로컬 도구와 의존성 없는 임시 프로젝트로 등록된 빌드·테스트 작업을 실행한다. `DSG_TEST_PLAYWRIGHT_MODULES`에 준비한 공식 Playwright의 `node_modules` 경로를 추가하면 실제 성공·실패 추적 파일의 존재와 길이, 서로 다른 출력 경로, 기본 프로젝트 출력 보존도 확인한다. 감사 검증에는 실행 결과 코드와 해당 lease 참조만 사용한다.

실제 Next.js 앱의 통합 검증에는 `DSG_TEST_BUILD_PROVIDERS=1`과 `DSG_TEST_NEXT_MODULES`를 함께 지정한다. 두 번째 값은 Next.js 16.4.6, React·React DOM 19.3.1과 실행 환경의 SWC가 준비된 `node_modules`의 절대 경로다. 의존성은 크기와 파일 수를 제한한 임시 복사본을 사용한다. `PATH`에는 임시 홈에서 바로 실행할 수 있는 Node와 pnpm을 둔다. 단독 검증은 `DSG_TEST_NEXT_MODULES`를 지정해 `python3 -IB scripts/check-build-providers.py <dsg> --next-only`로 실행한다. pnpm Provider의 `next build --webpack`, `BUILD_ID`, 사전 렌더 HTML, 관리형 `exec`의 `127.0.0.1` 응답·종료 및 두 lease의 감사 기록을 확인한다.

정리 회귀는 자체 생성한 임시 프로젝트만 대상으로 한다. 보호 디렉터리 보존, 기존 계획 이후 내부 파일 변경 감지, 틀린 승인 보존·일회용 승인 재실행 거절, 감사 기록과 회수량을 확인한다. 등록된 Cargo 대역의 성공·실패·중단 이벤트와 격리 Git 원격의 여섯 참조를 사용하는 워크트리 회수도 검사한다.

내부 Docker GC의 제어 대역 7건, 레지스트리 회귀 17건, 이미지 GC 회귀 4건이 통과했다. 이미지·빌드 캐시 한 건 적용과 재사용 거절, 외부 입력 및 계획 범위 교차 거절, 레지스트리 간 프로젝트 번호 충돌, 동시 실행 슬롯 경합, 만료·정책·프로젝트·승인 불일치, 엔드포인트·세대·스냅샷·참조·사용 상태 변경, 영수증에 의한 복구를 검사한다. 중단 시험은 등록부의 적용 의도 커밋 뒤 모의 백엔드 SQLite에서 상태 변경과 영수증을 커밋하고 panic을 일으킨다. 레지스트리와 모의 백엔드를 다시 열어 일치하는 영수증의 결과를 확정하며, 영수증 누락 경로는 `recovery_required`로 보존하는지 확인한다. 두 경로 모두 변경 시도는 1회다. 전용 Moby의 실제 이미지 삭제와 Rust가 모사한 정산 전 중단은 아래 계약에서 확인한다. 실제 Docker 적용 중 OS 프로세스 종료는 별도 실측 범위다. [전용 엔진 실측](HANDOFF.md#승인-후-전용-docker-엔진-단일-이미지-적용)

`crash_after_quarantine_recovers_file_and_committed_audit`는 임시 프로젝트에서 정상 승인 적용 의도를 커밋하고 파일을 격리한 자식 프로세스만 강제 종료한다. 새 등록부를 열어 만료 lease를 회수하고 실제 복원 함수를 실행한다. 파일 ID·SHA-256 유지, 회수량 0바이트, 재복원 거절, SQLite 무결성 및 적용 의도·보존 이벤트 각 1건이 통과 기준이다. 호스트 전원 장애는 별도 출시 시험이다.

### 전용 Docker 엔진 단일 이미지 적용 사전 계약

WBS 1.4·2.3의 승인 적용 시험은 전용 Moby 이미지 1개를 대상으로 통과했다. 기존 Ubuntu Parallels ARM64의 관리자 권한, cgroup v2, 필수 도구 사전 점검을 통과했다. 시험 범위는 기존 Ubuntu의 전용 엔진과 합성 이미지 1개로 한정한다.

- 전용 Unix 소켓과 독립 저장소를 사용하고 다른 클라이언트의 접근이 차단됐는지 확인한다. 시험 시작 시 이미지·컨테이너·볼륨·빌드 캐시가 0개여야 한다. 실행 전에 시간·CPU·RSS·PID·디스크와 정리 상한을 수치로 확정하고, 격리와 상한이 확인되지 않으면 중단한다.
- 합성 이미지 1개의 생성 출처, 전체 이미지 ID, 불변 스냅샷, 엔진 지문을 기록한다. 별도 삭제 승인을 받은 뒤 엔진·스냅샷·대상을 직전에 다시 확인하고 지정 ID 1개에만 한 번 실제 적용한다.
- 실제 엔진의 삭제와 로컬 DB 기록은 단일 트랜잭션으로 묶을 수 없다. 중단 후 대상과 적용 의도에 정확히 일치하는 영수증이 있으면 읽기 전용으로 결과를 복구한다. 결과가 불확실하면 회수량 0 B와 `recovery_required`를 기록하고 동일 대상의 재승인과 재호출을 차단한다.

2026-11-24에는 전용 임시 Docker 데몬과 containerd에 합산 CPU 1코어·메모리 512 MiB·PID 128개·실행 120초를 적용하고, 독립 tmpfs에 128 MiB·inode 4,096개 상한을 적용했다. Unix 소켓 상대 프로세스와 `nobody`의 `EACCES`, 외부 마운트 네임스페이스에서 소켓 숨김, TCP 리스너 0개를 확인했다. 이미지·컨테이너·볼륨·캐시가 모두 0개인 상태에서 태그와 레이어가 없는 합성 이미지 1개를 적재한 뒤 전체 ID와 출처 해시를 대조했다. 삭제 요청은 0건이며 VM은 원래 일시정지 상태로 돌아갔다. [실측 증거](../dist/dsg-dedicated-engine-cf83a703d2fd/PREAPPROVAL.json)

승인 후 동일 엔진·세대·스냅샷·전체 이미지 ID를 대조했다. 적용 전 독립 관측에서 삭제 요청은 0건이었다. Rust의 기본 실행에서 제외되는 승인 전용 시험 `docker_gc_apply::live_fixture::controlled_docker_live_approved_receipt` 1건을 `--ignored --exact`로 실행해 종료 코드 0을 확인했다. 기존 Docker GC 제어 대역 8건도 통과했다. 전용 Moby의 이미지 수는 1개에서 0개로 줄었고, 삭제 요청은 1건이었다. 강제 삭제와 prune은 사용하지 않았다. [전용 게스트 실행기](../scripts/check-dedicated-docker-apply.py) · [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs) · [실측 집계](../dist/dsg-dedicated-engine-cf83a703d2fd/LIVE_RESULT.json)

Rust 시험에서 정산 전 중단을 모사했다. SQLite 등록부를 다시 열어 기록된 영수증과 적용 의도를 대조하고 `GC_APPLY_INTENT`·`GC_APPLIED` 각 1건, 재적용·재정산 거절, 열린 lease 0건, 무결성 점검 `ok`를 확인했다. 등록부의 회수량은 0 B이며 물리 디스크 회수량은 미계측이다. 종료 후 시험 서비스 비활성, 임시 디렉터리 회수 및 VM 일시정지 복귀를 확인했다. 호스트 등록부의 합성 소유자와 게스트에서 검증한 소유자 지문의 대응은 시험 전용이다. 공개 운영 어댑터, 일반 공유 Docker 영역, 실제 캐시 적용, 삭제 응답 유실 및 전원 장애 복구는 별도 검증이다. 다음 시험에서 유효 기간이 끝나거나 엔진 세대가 바뀌면 다시 실측하고 새 승인을 받는다. [전용 엔진 실측](HANDOFF.md#승인-후-전용-docker-엔진-단일-이미지-적용)

같은 등록부에서 엔드포인트·종류·전체 ID가 일치하는 적용 의도가 `applying` 또는 `recovery_required`이면 새 계획과 승인, 이미 발급한 승인의 적용을 차단한다. 세대·스냅샷 변경과 등록부 재열기 뒤에도 차단을 확인했다. 일치하는 영수증으로 정산한 뒤 제어 대역에서 새 승인과 적용이 가능했다. 제어 대역 8건, 등록부 회귀 17건, 이미지 계획 회귀 4건이 통과했다.

### 전용 Docker 엔진 삭제 응답 확인 후 영수증 유실 시험

WBS 1.4의 실측은 기존 Ubuntu ARM64의 독립 전용 Moby와 태그·레이어 없는 합성 이미지 1개에 한정한다. 별도 사전 증거에 저장한 시험 식별자, 엔진·엔드포인트·세대, 스냅샷, 전체 이미지 ID 및 증거 지문을 대조하고, 해당 증거에 대한 새 명시적 승인을 받은 뒤 적용한다. 앞선 정상 영수증 시험의 승인은 이 시험에 적용되지 않는다. 승인 없이 실행 케이스를 직접 호출해도 적용할 수 없어야 한다.

`docker_gc_apply::live_fixture::controlled_docker_live_receipt_loss`는 기본 실행에서 제외한다. 승인 전용 경로는 단일 DELETE의 HTTP 200과 이미지 1→0을 확인하고 삭제 응답 관측 기록을 동기화한 직후, 영수증 기록 전에 시험 중단을 주입한다. SQLite를 다시 열고 영수증 부재, 승인 소비, `recovery_required`와 정산 0바이트, 적용 의도·복구 필요 감사 각 1건을 확인한다. 복구 중 추가 변경 요청 0건, 동일 계획과 경쟁 계획의 적용 거절이 통과 조건이다. [게스트 실행기](../scripts/check-dedicated-docker-apply.py) · [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs)

승인된 실제 전용 Moby 시험 1건과 Docker 적용 회귀 8건이 통과했다. 영수증이 없는 등록부에서 복구 필요와 회수량 0 B, 추가 게스트 호출 0건을 확인했다. 읽기 전용 독립 검증에서 `quick_check=ok`, 열린 lease 0건과 적용 완료 감사 0건을 확인했다. [실측](HANDOFF.md#승인-후-전용-docker-엔진-영수증-기록-전-중단-검증)

### 전용 Docker 엔진 삭제 응답 미수신 시험

전용 Moby와 새 합성 이미지 1개에 대한 수동 시험이다. 시험 모드, 시험 식별자, 엔진·엔드포인트·세대, 스냅샷, 전체 이미지 ID 및 증거 지문을 현재 사전 증거와 대조하고, 그 증거에 대해 새 명시적 승인을 받은 뒤 실행한다. 이전의 HTTP 200 확인 후 영수증 유실 시험과 승인 범위를 구분한다.

클라이언트는 단일 DELETE 송신 전에 읽기 방향을 `SHUT_RD`로 닫고 실제 응답 수신 바이트 0을 확인한다. 독립 읽기 연결에서 대상 전체 ID의 이미지가 1개에서 0개로 바뀌는지 관측한다. 복구는 등록부를 다시 열어 영수증 부재, `recovery_required`, 회수량 0 B, 복구 중 재삭제 0건과 경쟁 적용 차단을 확인한다. 이 시험의 승인 적용 실측은 대기 상태다. [전용 엔진 실행기](../scripts/check-dedicated-docker-apply.py) · [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs)

### 정산 전 자식 프로세스 강제 종료 회귀

별도 SQLite를 사용하는 캐시 제어 대역에서 자식이 적용 의도를 커밋하고 대역 삭제를 수행한 뒤 등록부 정산 전에 정지하도록 한다. 부모는 자신이 시작한 자식만 실제 `SIGKILL`로 종료하고 종료 신호를 확인한다. 새 SQLite 연결에서 새 lease를 획득하고 경쟁 적용을 차단하며, 제어 대역의 영속 영수증이 있으면 `applied`와 회수량 17 B를, 없으면 `recovery_required`와 0 B를 확인한다. 두 경우 모두 감사 이벤트와 대상 재호출 0건을 검사했다. macOS에서 두 분기가 통과했다. 실제 호스트 전원 장애는 별도 시험이다. [회귀 코드](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/tests.rs)

### 현재 제품의 Linux ARM64 별도 BuildKit 네이티브 조회

WBS 2.3은 실제 제품 CLI와 공식 v0.35.1 `buildkitd`·`buildctl`을 전용 소켓에서 비교한다. 독립 `buildctl du` 집계와 제품 JSON을 빈 캐시와 시험 캐시에서 대조한다. 자동 GC를 끈 임시 빌더에서 외부 입력 없이 `FROM scratch`와 `COPY`로 시험 캐시를 만들고 산출물을 검증한다. 조회는 `candidate_count=0`과 보존 판정을 유지해야 한다. 잘못된 PID, 클라이언트 해시, 소켓 링크는 종료 5와 미확인으로 거절하고, 대체 클라이언트의 실행 흔적이 없어야 한다. 부정 시험 후 독립 집계를 다시 비교하며 prune 요청은 0건이다. 종료 시 시험 데몬과 임시 작업 경로의 회수를 확인한다. [네이티브 시험 실행기](../scripts/check-buildkit-native.py)

격리 단일 워커의 캐시 출처 검증은 기존 공식 전체 ID 목록을 저장하고, 독립 난수 페이로드의 직접 LLB 빌드와 OCI 레이어 해시를 확인한 뒤 전체 목록을 두 번 대조한다. 같은 워커·데몬 세대에서 변경되지 않은 기준 레코드와 새 캐시 ID 하나를 확인하고 공식 `du --filter id==<전체 ID>`로 그 레코드만 돌아오는지 확인한다. 표준 prune은 이미지 캐시 lease 취소, 워커 저장소 GC, 참조 없는 캐시 메타데이터 정리까지 도달할 수 있으므로 정확한 ID 조회가 통과해도 prune 0건과 후보 0개를 유지한다. [네이티브 시험 실행기](../scripts/check-buildkit-native.py) · [v0.35.1 제어기](https://github.com/moby/buildkit/blob/v0.35.1/control/control.go#L228-L266) · [v0.35.1 캐시 관리자](https://github.com/moby/buildkit/blob/v0.35.1/cache/manager.go#L1018-L1042)

현재 Linux ARM64 제품의 Ubuntu 시험에서 기준 4개 불변과 새 레코드 1개를 독립 OCI 산출물과 대조했다. 공식 정확 ID 조회가 1개를 반환했고 회수 후보·prune 요청은 0개였다. [BuildKit 실측 집계](../dist/dsg-buildkit-native-69f9692b4210/QUALIFICATION.json)

### Linux ARM64 로컬 제어 서비스 프로브

WBS 5.1의 로컬 읽기 확인은 보호된 root 소켓과 예상 프로세스의 프로토콜 선언을 검증한다. 등록한 서비스의 성공과 소켓 링크·권한·상대 PID·선언 불일치 거절을 확인하고, 성공과 실패 모두 `mutation_authorized=false`와 보존 게이트를 확인한다. 운영 제어기의 출처·권한·배타 변경 경계는 별도로 검증한다. [프로브 구현과 시험](../rust-scaffold/crates/dsg-core/src/docker_control_local.rs)

보호된 배포 확인은 root 제어기·엔진과 서로 다른 비root 승인자·실행자의 피어를 분리하고 정책의 부팅 ID, 프로세스 세대, 실행 파일 해시와 소켓을 검증한다. 불일치 시 종료 5, 성공·실패 모두 변경 권한은 false이며 운영 게이트는 보존을 유지한다. [배포 확인 계약](API_SPEC.md#보호된-docker-제어기-배포-확인)

## 에이전트 연동 검증

`sh scripts/test-rust.sh`는 합성 프로젝트의 훅 설치·중복 방지·설정 보존·제거, 허용·거부·잘못된 JSON 응답과 MCP의 읽기 전용 목록·쓰기 거절을 확인한다. 정리 후보를 조회한 뒤 상태 폴더가 생기지 않아야 한다.

macOS에서 실제 CLI를 검증할 때는 `DSG_TEST_AGENT_NATIVE=all`을 설정한다. Claude와 Codex 명령을 `PATH`에 두고 `DSG_TEST_NODE`와 `DSG_TEST_GEMINI_ENTRY`에 Node 실행 파일 및 Gemini CLI 번들의 절대 경로를 지정한다. 단일 CLI 검증은 `all` 대신 `claude`, `codex`, `gemini`를 지정한다. Claude와 Codex는 루프백 대역 응답을, Gemini는 합성 응답 재생을 사용한다. 쓰기와 네트워크를 임시 경계에 제한하고 CLI별로 허용, 정책 거부, 훅 실행 오류에서 대역 명령이 실행됐는지 확인한다.

Linux에서는 같은 실제 CLI 시험을 `DSG_TEST_AGENT_LINUX_CONTAINER=1`로 명시한 격리 프로세스에서 수행한다. 실행기는 읽기 전용 루트, 별도 실행 가능한 `/fixture` tmpfs, 비권한 UID와 유효 capability 0, `NoNewPrivs=1`, 루프백 전용 네트워크, 기본 라우트와 표준 Docker 소켓 부재를 확인한다. 이 조건을 입증하지 못하면 CLI 호출 전에 종료한다. Parallels Ubuntu에서는 별도 네트워크·마운트·PID 네임스페이스를 적용한 제한 실행 경로에서 동일 게이트를 실측했다.

## 브로커와 서비스 검증

macOS·Linux에서 Docker CLI를 사용할 수 있으면 `sh scripts/test-rust.sh`가 임시 Unix 소켓의 합성 엔진만 조회한다. 허용 UID만 응답을 받고, 미허용 UID와 변경·상세 조회 요청은 엔진에 전달되지 않아야 한다. 엔진이 중단되거나 ID가 바뀌면 `503`을 반환하고 같은 엔진을 복구한 뒤 다시 조회해야 한다. 충돌한 프로세스의 수신 소켓 처리와 종료 시 정리도 검사한다.

Linux 검증용 소켓은 기본적으로 사용자 홈의 임시 디렉터리에 생성한다. `DSG_TEST_RUNTIME_DIR`로 상위 경로를 지정할 수 있다. Unix 소켓 경로 길이 제한을 고려해 그룹·기타 쓰기 권한이 없고 충분히 짧은 절대 경로를 사용한다.

macOS는 샌드박스에서 시험용 Unix 릴레이와 지정 엔진의 루프백 TCP에 직접 접근하면 거부되고, 브로커 경유 조회는 성공해야 한다. `DSG_TEST_LAUNCHD=1`은 현재 GUI 사용자 도메인의 임시 테스트 서비스에서 등록·종료·재시작·해제를 확인한다. 기존 격리 엔진에서 동일 UID의 샌드박스 경계를 실측하는 절차는 아래에 있다. 운영 계정 분리와 공식 LaunchDaemon의 검증에는 승인된 제한 계정과 관리자 접근이 필요하다. Linux systemd와 Windows SCM은 각각 해당 호스트에서 확인한다.

- macOS `DSG_TEST_LIVE_ENFORCEMENT=1 sh scripts/test-rust.sh`는 기존 `dockerguard-test` 루프백 엔진을 조회한다. 시험용 Unix 릴레이는 `/_ping`·`/info`·`/version` 계열의 GET·HEAD만 허용한다. 동일 UID 샌드박스에서 릴레이 소켓과 실제 엔진의 루프백 TCP 직접 접속 차단, 브로커 경유 `/info` 응답 및 변경 요청의 상류 전달 0건을 확인한다.
- Linux `python3 -IB scripts/check-linux-service.py --disposable-guest --dockerd <경로> --docker <경로> <dsgd>`를 폐기 가능한 systemd 게스트에서 실행한다. 검증용 Docker 엔진과 런타임은 읽기 전용 미디어로 공급한다. 별도 제한 UID, 엔진·릴레이 직접 접근 거부, 변경 요청 전달 0건, broker 충돌 이후 단일 재시작과 정상 중지 및 등록 자원 회수를 확인한다.
- Windows 교차 빌드는 컴파일·링크를 검증한다. 실제 Windows 시험은 폐기 가능한 관리자 게스트의 `scripts/check-windows-service.ps1`에서 `-DisposableGuest`, `-DsgdExe`, `-UpstreamPipe`, `-UpstreamSid`, `-EngineId`, `-EngineKind Real|Fixture`를 명시한다. 임시 SCM 서비스와 작업으로 정상·강제 종료 후 재시작, 허용·거부 SID, 직접 엔진 접근 거부, 인벤토리와 임시 자원 회수를 검증한다. `EngineKind`는 운영자가 확인한 시험 환경을 구분한다.
- 실제 Windows의 LocalSystem에서 `scripts/check-windows-fixture.ps1 -DisposableGuest -DsgdExe <경로>`를 실행하면 한정된 합성 파이프를 같은 SCM 검증기에 연결한다. 실제 Docker 연동 수용 기준은 별도의 `Real` 실행에서 확인한다.

## 대시보드와 릴리스 검증

`sh scripts/test-rust.sh`는 임시 프로젝트에서 대시보드의 읽기 전용 응답, 정적 파일, 출처 검사와 쓰기 경로 거절을 확인한다. 연결 뒤 요청 전송을 짧게 늦춰도 응답해야 한다. 확인 뒤 상태 저장소가 생성되지 않고 테스트 파일이 남아야 한다. macOS와 Linux에서는 패키지 두 번 생성 결과의 일치, 설치된 바이너리 실행, 업그레이드, 진단·제거와 변조·링크 거절을 확인한다. macOS fixture에는 로컬 ad-hoc 서명 검증이 포함된다.

macOS에서 `DSG_TEST_WEBKIT=1 sh scripts/test-rust.sh`는 임시 WebKit 뷰를 띄워 1060px·390px 레이아웃, 스타일 적용, 실측 정리 후보와 Docker 미확인 상태를 확인한다. 사용자 브라우저 프로필과 저장소는 사용 범위에서 제외한다.

배포 후보는 임시 경로에 이전 실물을 설치하고 업그레이드한 뒤 진단 결과, 매니페스트와 설치 파일의 SHA-256, `dsg --version`과 `dsgd --help`, 대시보드, 제거 후 사용자 시험 파일 보존을 대조한다. macOS에서는 `0.1.0` 실물로 롤백한 뒤 현재 후보로 재업그레이드하는 경로도 확인한다.

macOS 설치본의 Docker 대시보드는 `scripts/check-dashboard.py <설치된-dsg>`로 확인한다. `DSG_TEST_DOCKER_CONTEXT=dockerguard-test`를 지정하고, `DSG_TEST_RENDERER`에는 `scripts/check-dashboard-rendering.swift`를 빌드한 시험 실행 파일의 경로를 지정한다. 독립 Docker CLI 조회와 제품 스캔으로 확인한 컨테이너·고유 이미지 ID·명명 볼륨·중복 그룹 수를 `DSG_TEST_DASHBOARD_COUNTS`의 `container_count`, `image_count`, `volume_count`, `duplicate_groups` JSON 필드로 전달한다. 시험은 이 수치를 API 및 WebKit 1060px·390px 화면과 대조한다. [현재 후보 검증](HANDOFF.md#8단계-현재-후보-제품화-검증)

Windows ARM64의 로컬 NTFS에서는 `python.exe -B scripts/check-windows-arm64-release.py --legacy-arm64 <이전-ARM64-아카이브> --current-arm64 <현재-ARM64-아카이브> --x64 <현재-x64-아카이브>`를 실행한다. 각 아카이브 옆에 대응하는 `.sha256` 파일을 두고 시험기와 같은 디렉터리에 `release-package.py`와 `check-dashboard.py`를 둔다. 시험은 ARM64 수명 주기와 x64 바이너리의 에뮬레이션 실행·대시보드 HTTP를 확인한다. x64 패키지의 새 설치와 기존 ARM64 설치에서의 업그레이드가 거절되고 설치 파일과 사용자 파일이 보존돼야 한다. 선택 옵션 `--x64-python <공식 포터블 x64 python.exe 절대 경로>`를 추가하면 `win-amd64`를 보고하는 x64 Python에서도 설치기 호출을 반복한다. 두 인터프리터에서 공식 설치기의 새 설치와 업그레이드는 각각 종료 코드 1이어야 하며, 설치 경로·기존 ARM64 설치 포인터와 실행 파일 SHA-256·사용자 시험 파일이 보존돼야 한다. Windows x64 호스트에서의 네이티브 설치 수명 주기는 별도로 검증한다.

`python3 -B scripts/check-release-signature.py --openssl <절대경로>`는 임시 Ed25519 키로 아카이브와 매니페스트의 서명을 검증한다. 키 핀 불일치, 서명 누락·변조, 패키지 파일 변조를 거부해야 한다. macOS·Linux에서는 불완전한 서명 옵션을 사용한 설치의 변경 0건, 틀린 서명의 업그레이드에서 기존 버전 보존, 올바른 서명의 설치·업그레이드 및 제거를 추가로 확인한다.

같은 러너에서 서명자에게 전달할 매니페스트가 내부 원문과 일치하는지, 원문 두 개의 서명을 조립한 JSON이 기존 검증기를 통과하는지 확인한다. 승인 해시 불일치, 잘못된 서명 파일 또는 길이, 이미 존재하는 다른 출력은 거부해야 한다. macOS 게시자 핀을 요청하면 macOS 대상과 두 실행 파일의 Developer ID 서명이 필요하며, 검증 실패 시 설치 경로가 변경되지 않아야 한다. 실제 공인 서명·공증의 성공 검증에는 DSG용 승인된 게시자와 서명 환경이 필요하다.

## POC 제한
- 정규식 명령 필터는 모든 셸 표현을 탐지하지 않음.
- 파일/디렉터리 변경 간 TOCTOU 경합이 존재하므로 실제 프로젝트의 삭제 자동화 비권장.
- Docker 실제 중복 생성 방지/다중 OS 통합 테스트 미수행.

## 출시 게이트

2026-11-24의 새 macOS ARM64 후보는 Rust 트리 `a44f0ca`에서 검증했다. Python 14건, Rust 117건과 통합 회귀, 독립 빌드 두 번의 해시 일치, 설치·업그레이드·롤백·재업그레이드·제거 및 설치본 대시보드까지 통과했다. Linux·Windows의 검증은 기존 소스에 해당한다. [새 후보 검증](HANDOFF.md#compose-캐시-관리형-실행-및-승인-정리-보완)
- 영속 DB/사용자 데이터 유실 0건.
- 관리형 실행 경로에서 우회 0건(테스트 매트릭스 기준).
- 잘못된 이미지/서비스 재사용 0건.
- 감사 이벤트 무손실, 비정상 종료 후 회복.

WBS마다 구현과 검증 기록을 [작업 대조표](TASK.md)에서 대조한다. 성공 여부는 실행한 OS, 후보 버전, 실환경·시험 대역, 입출력 범위를 근거로 판정한다. 교차 링크는 컴파일·링크 증거이며, 합성 Docker 파이프는 해당 OS 서비스와 파이프 권한의 증거다. 대상 환경에서 실행하지 못한 항목은 별도의 검증으로 남긴다.

- Rust 트리 `ada5a09`의 `0.1.1-rc.1` 후보는 macOS ARM64와 기존 Parallels Windows ARM64에서 설치 수명 주기와 대시보드를 검증했다. Linux ARM64 후보는 앞선 Ubuntu Parallels 시험에서 이전 `0.1.0` 실물 업그레이드, 두 바이너리 실행, 대시보드 HTTP, 제거 후 사용자 시험 파일 보존, 관리형 타임아웃 124와 lease 재획득을 통과했다. Windows x64 후보는 ARM64 게스트 에뮬레이션과 설치기 대상 판정을 검증했다. WBS 6.1의 기존 macOS 호스트와 Parallels Ubuntu ARM64·Windows ARM64에서 수행 가능한 패키지 검증을 완료했다. 등록된 Windows VM이 ARM64이므로 Windows x64 운영체제 설치 수명 주기는 이번 범위에서 제외했다. 해당 시험의 공식 수용 기준은 WBS 6.1에서 관리한다. [패키지 근거](RELEASE.md) · [macOS·Windows 실측](HANDOFF.md#8단계-현재-후보-제품화-검증) · [Ubuntu 실측](HANDOFF.md#별도-buildkit-읽기-전용-실측)

- Docker 내장 빌더 총량·활성 집계, 기본 Buildx 빌더의 상태별 조회, 선택한 기존 이미지 1개 보존은 조회·계획 검증에 해당한다. Buildx 상태별 항목은 서로 겹칠 수 있다. WBS 2.3의 별도 BuildKit는 실제 4개를 조회하고 전부 보존했다. 프로젝트 캐시 소유권과 안전한 승인 회수의 증거가 필요하다. 합성 이미지의 전용 Moby 적용을 확인했다. 운영 적용은 프로젝트 소유권·단독 제어와 별도 승인, 공개 어댑터 연결을 검증한다. [전용 엔진 실측](HANDOFF.md#승인-후-전용-docker-엔진-단일-이미지-적용)
- macOS에서 격리 엔진·Buildx·Compose 이미지 옵션을 포함한 `sh scripts/test-rust.sh`는 종료 코드 0으로 완료됐고 Rust 시험 103개를 통과했다. 시험기가 Compose를 반복 실행한 뒤 제품은 동일 ID 1개를 반환했다. 시험에서는 구성·health·실제 이미지·64 KiB `tmpfs` 변경 거절, 일치하는 `tmpfs`의 `unknown/unverified_mount` 판정, 소유 컨테이너 회수를 확인했다. 명명 볼륨 옵션으로 실행한 별도 전체 시험도 종료 코드 0과 Rust 시험 103개 통과를 확인했다. 볼륨 A의 재사용, B로 교체한 뒤 A 기대값의 거절과 B 기대값의 통과, 소유 컨테이너 회수 및 외부 볼륨 두 개 보존을 확인했다. 종료 후 인벤토리는 컨테이너 7개, 이미지 7개, 볼륨 2개, 중복 0건이었다. Compose 시험의 제품 진입점은 호출자가 준 기대 구성으로 기존 컨테이너를 관측한다. 구성 정규화와 재빌드 필요 판정의 1단계 구현은 비민감 JSON 투영을 대상으로 하며, 위의 합성 Docker CLI 대역 시험에서 확인했다. pnpm 시험에는 일반 프로젝트 스크립트와 실제 Next.js 앱 빌드·루프백 응답을 포함한다. [명명 볼륨 실측](HANDOFF.md#명명-볼륨과-ubuntu-parallels-실측)
- 프로세스 강제 종료와 재시작 근거는 [핸드오프](HANDOFF.md)에 기록한다. 전원 장애 복구와 전체 플랫폼의 감사 무손실은 독립된 출시 검증으로 추적한다.
