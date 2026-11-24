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

동일한 격리 컨텍스트에서 Buildx 플러그인이 준비되어 있으면 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 sh scripts/test-rust.sh`로 명시한 엔진의 기본 Buildx 빌더를 실측한다. 명령은 사용자 Buildx 설정 대신 임시 설정을 사용한다. 동일 엔진 ID, 항목 수와 각 상태 수의 범위, 보존 판정을 확인한다. 별도 BuildKit 읽기 시험은 Linux ARM64 Ubuntu Parallels에서 임시 전용 소켓과 공식 v0.35.1 클라이언트를 사용한다. [조회 계약](API_SPEC.md#docker-읽기-전용-인벤토리)의 제품 명령으로 빈 캐시와 시험 캐시, 잘못된 PID·클라이언트 해시·소켓 링크 거절을 확인한다. 소유권과 GC는 별도 수용 기준이다.

관리형 실행 검증에는 임시 프로젝트의 작은 Cargo 빌드와 시간 초과 뒤 재실행, lease 경합·회수 및 등록부 마이그레이션이 포함된다. 사전에 digest가 있는 공개 fixture 이미지를 보유한 격리 엔진에서 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 sh scripts/test-rust.sh`로 건강한 컨테이너 하나를 확인한다. 반복 `up`의 동일 ID와 추가 컨테이너 0개를 확인한다. 구성 변경 시험에서는 원래 인스턴스를 회수하고 새 인스턴스에서 과거 기대값을 거절한다. 회수 직전에 동일 엔진과 시험 프로젝트·서비스·라벨·전체 컨테이너 ID를 대조한다.

같은 Compose 시험에서 엔진 ID와 단일 시험 컨테이너의 ID·라벨을 대조한 뒤 컨테이너의 `/dev/shm`에 상태 표식을 쓴다. 구성·이미지·마운트가 같은 컨테이너가 `unhealthy`가 된 경우 재사용은 `not_healthy`와 종료 코드 5를 반환해야 한다. 시험은 기존의 격리 컨테이너 회수 경로를 사용한다.

이미지·`tmpfs` 실측에는 같은 명령에 `DSG_TEST_COMPOSE_IMAGE`를 추가하고 값으로 격리 엔진에 준비된 공식 `docker.io/library/alpine@sha256:…` 참조를 지정한다. 두 번째 컨테이너는 원래 이미지와 ID·digest가 다른 이미지와 64 KiB `tmpfs`를 사용한다. 실제 이미지와 마운트를 조회한 뒤 구성·이미지·마운트 기대값을 개별 대조해 두 관측에서 각각 `configuration_changed`, `image_changed`, `mount_changed`와 종료 코드 5를 확인한다. 관측과 같은 `tmpfs`를 기대값에 넣어도 두 관측은 `unknown/unverified_mount`와 종료 코드 5를 반환해야 한다.

명명 볼륨 교체는 `DSG_TEST_COMPOSE_IMAGE` 없이 기본 Compose 시험 명령에 `DSG_TEST_COMPOSE_VOLUMES`와 `DSG_TEST_COMPOSE_VOLUME_ENGINE_ID`를 추가해 실행한다. 값은 각각 `dsg-reuse-volume-<12자리 소문자 16진수>-a,dsg-reuse-volume-<같은 값>-b` 형식인 외부 볼륨 두 개와 확인한 격리 엔진 ID다. 두 볼륨은 옵션 없는 로컬 볼륨이어야 하며 `dsg.fixture.reuse-volume` 라벨 값이 이름에 포함된 12자리 값과 일치해야 한다. 볼륨 A를 읽기 전용·`nocopy`로 연결하고 같은 컨테이너의 재사용을 두 번 확인한다. 실제 B로 교체한 뒤 현재 구성 해시와 이미지 정보를 두 요청에 똑같이 사용한다. 볼륨 A를 기대하면 `mount_changed`와 종료 코드 5, B를 기대하면 재사용 성공을 반환해야 한다. 시험 후 소유 컨테이너를 회수하고 외부 볼륨 두 개가 미연결 상태로 보존됐는지 대조한다. [실측](HANDOFF.md#명명-볼륨과-ubuntu-parallels-실측)

`DSG_TEST_BUILD_PROVIDERS=1 sh scripts/test-rust.sh`는 로컬 도구와 의존성 없는 임시 프로젝트로 등록된 빌드·테스트 작업을 실행한다. `DSG_TEST_PLAYWRIGHT_MODULES`에 준비한 공식 Playwright의 `node_modules` 경로를 추가하면 실제 성공·실패 추적 파일의 존재와 길이, 서로 다른 출력 경로, 기본 프로젝트 출력 보존도 확인한다. 감사 검증에는 실행 결과 코드와 해당 lease 참조만 사용한다.

실제 Next.js 앱의 통합 검증에는 `DSG_TEST_BUILD_PROVIDERS=1`과 `DSG_TEST_NEXT_MODULES`를 함께 지정한다. 두 번째 값은 Next.js 16.4.6, React·React DOM 19.3.1과 실행 환경의 SWC가 준비된 `node_modules`의 절대 경로다. 의존성은 크기와 파일 수를 제한한 임시 복사본을 사용한다. `PATH`에는 임시 홈에서 바로 실행할 수 있는 Node와 pnpm을 둔다. 단독 검증은 `DSG_TEST_NEXT_MODULES`를 지정해 `python3 -IB scripts/check-build-providers.py <dsg> --next-only`로 실행한다. pnpm Provider의 `next build --webpack`, `BUILD_ID`, 사전 렌더 HTML, 관리형 `exec`의 `127.0.0.1` 응답·종료 및 두 lease의 감사 기록을 확인한다.

정리 회귀는 자체 생성한 임시 프로젝트만 대상으로 한다. 보호 디렉터리 보존, 기존 계획 이후 내부 파일 변경 감지, 틀린 승인 보존·일회용 승인 재실행 거절, 감사 기록과 회수량을 확인한다. 등록된 Cargo 대역의 성공·실패·중단 이벤트와 격리 Git 원격의 여섯 참조를 사용하는 워크트리 회수도 검사한다.

`crash_after_quarantine_recovers_file_and_committed_audit`는 임시 프로젝트에서 정상 승인 적용 의도를 커밋하고 파일을 격리한 자식 프로세스만 강제 종료한다. 새 등록부를 열어 만료 lease를 회수하고 실제 복원 함수를 실행한다. 파일 ID·SHA-256 유지, 회수량 0바이트, 재복원 거절, SQLite 무결성 및 적용 의도·보존 이벤트 각 1건이 통과 기준이다. 호스트 전원 장애는 별도 출시 시험이다.

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
- 영속 DB/사용자 데이터 유실 0건.
- 관리형 실행 경로에서 우회 0건(테스트 매트릭스 기준).
- 잘못된 이미지/서비스 재사용 0건.
- 감사 이벤트 무손실, 비정상 종료 후 회복.

WBS마다 구현과 검증 기록을 [작업 대조표](TASK.md)에서 대조한다. 성공 여부는 실행한 OS, 후보 버전, 실환경·시험 대역, 입출력 범위를 근거로 판정한다. 교차 링크는 컴파일·링크 증거이며, 합성 Docker 파이프는 해당 OS 서비스와 파이프 권한의 증거다. 대상 환경에서 실행하지 못한 항목은 별도의 검증으로 남긴다.

- Rust 트리 `ada5a09`의 `0.1.1-rc.1` 후보는 macOS ARM64와 기존 Parallels Windows ARM64에서 설치 수명 주기와 대시보드를 검증했다. Linux ARM64 후보는 앞선 Ubuntu Parallels 시험에서 이전 `0.1.0` 실물 업그레이드, 두 바이너리 실행, 대시보드 HTTP, 제거 후 사용자 시험 파일 보존, 관리형 타임아웃 124와 lease 재획득을 통과했다. Windows x64 후보는 ARM64 게스트 에뮬레이션과 설치기 대상 판정을 검증했다. WBS 6.1의 기존 macOS 호스트와 Parallels Ubuntu ARM64·Windows ARM64에서 수행 가능한 패키지 검증을 완료했다. 등록된 Windows VM이 ARM64이므로 Windows x64 운영체제 설치 수명 주기는 이번 범위에서 제외했다. 해당 시험의 공식 수용 기준은 WBS 6.1에서 관리한다. [패키지 근거](RELEASE.md) · [macOS·Windows 실측](HANDOFF.md#8단계-현재-후보-제품화-검증) · [Ubuntu 실측](HANDOFF.md#별도-buildkit-읽기-전용-실측)

- Docker 내장 빌더 총량·활성 집계, 기본 Buildx 빌더의 상태별 조회, 선택한 기존 이미지 1개 보존은 조회·계획 검증에 해당한다. Buildx 상태별 항목은 서로 겹칠 수 있다. WBS 2.3의 별도 BuildKit는 실제 4개를 조회하고 전부 보존했다. 프로젝트 캐시 소유권과 안전한 승인 회수의 증거가 필요하다. 이미지 적용 검증에는 승인된 적용 경로와 단독 제어를 확인한 격리 엔진이 필요하다.
- macOS에서 격리 엔진·Buildx·Compose 이미지 옵션을 포함한 `sh scripts/test-rust.sh`는 종료 코드 0으로 완료됐고 Rust 시험 103개를 통과했다. 시험기가 Compose를 반복 실행한 뒤 제품은 동일 ID 1개를 반환했다. 시험에서는 구성·health·실제 이미지·64 KiB `tmpfs` 변경 거절, 일치하는 `tmpfs`의 `unknown/unverified_mount` 판정, 소유 컨테이너 회수를 확인했다. 명명 볼륨 옵션으로 실행한 별도 전체 시험도 종료 코드 0과 Rust 시험 103개 통과를 확인했다. 볼륨 A의 재사용, B로 교체한 뒤 A 기대값의 거절과 B 기대값의 통과, 소유 컨테이너 회수 및 외부 볼륨 두 개 보존을 확인했다. 종료 후 인벤토리는 컨테이너 7개, 이미지 7개, 볼륨 2개, 중복 0건이었다. Compose 시험의 제품 진입점은 호출자가 준 기대 구성으로 기존 컨테이너를 관측한다. 공식 PLAN의 구성 자체 정규화와 재빌드 필요 판정은 현재 미구현이며 백로그에서 추적한다. pnpm 시험에는 일반 프로젝트 스크립트와 실제 Next.js 앱 빌드·루프백 응답을 포함한다. [명명 볼륨 실측](HANDOFF.md#명명-볼륨과-ubuntu-parallels-실측)
- 프로세스 강제 종료와 재시작 근거는 [핸드오프](HANDOFF.md)에 기록한다. 전원 장애 복구와 전체 플랫폼의 감사 무손실은 독립된 출시 검증으로 추적한다.
