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

Docker 읽기 전용 단계에서는 Docker 명령 대역으로 컨텍스트 고정, 빈 목록과 미확인 상태의 구분, 중복 후보 및 변경 상황을 검증한다. 격리 엔진이 이미 동작 중이면 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test sh scripts/test-rust.sh`로 실제 조회를 추가 검증한다. 엔진 시작·리소스 생성·삭제는 이 조회 검증에서 수행하지 않는다.

관리형 실행 검증에는 임시 프로젝트의 작은 Cargo 빌드와 시간 초과 뒤 재실행, lease 경합·회수 및 등록부 마이그레이션이 포함된다. 사전에 digest가 있는 공개 fixture 이미지를 보유한 격리 엔진에서 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 sh scripts/test-rust.sh`로 건강한 컨테이너 하나를 확인한다. 검증하는 동안 동일 재사용 요청과 설정 해시 변경 결과, 추가 컨테이너 0개를 확인하고 fixture 라벨로 오늘 생성한 컨테이너만 회수한다.

`DSG_TEST_BUILD_PROVIDERS=1 sh scripts/test-rust.sh`는 로컬 도구와 의존성 없는 임시 프로젝트로 등록된 빌드·테스트 작업을 실행한다. `DSG_TEST_PLAYWRIGHT_MODULES`에 준비한 공식 Playwright의 `node_modules` 경로를 추가하면 실제 성공·실패 추적 파일의 존재와 길이, 서로 다른 출력 경로, 기본 프로젝트 출력 보존도 확인한다. 감사 검증에는 실행 결과 코드와 해당 lease 참조만 사용한다.

정리 회귀는 자체 생성한 임시 프로젝트만 대상으로 한다. 보호 디렉터리 보존, 기존 계획 이후 내부 파일 변경 감지, 틀린 승인 보존·일회용 승인 재실행 거절, 감사 기록과 회수량을 확인한다. 등록된 Cargo 대역의 성공·실패·중단 이벤트와 격리 Git 원격의 여섯 참조를 사용하는 워크트리 회수도 검사한다.

## 에이전트 연동 검증

`sh scripts/test-rust.sh`는 합성 프로젝트의 훅 설치·중복 방지·설정 보존·제거, 허용·거부·잘못된 JSON 응답과 MCP의 읽기 전용 목록·쓰기 거절을 확인한다. 정리 후보를 조회한 뒤 상태 폴더가 생기지 않아야 한다.

macOS에서 실제 CLI를 검증할 때는 `DSG_TEST_AGENT_NATIVE=all`을 설정한다. Claude와 Codex 명령을 `PATH`에 두고 `DSG_TEST_NODE`와 `DSG_TEST_GEMINI_ENTRY`에 Node 실행 파일 및 Gemini CLI 번들의 절대 경로를 지정한다. 단일 CLI 검증은 `all` 대신 `claude`, `codex`, `gemini`를 지정한다. Claude와 Codex는 루프백 대역 응답을, Gemini는 합성 응답 재생을 사용한다. 쓰기와 네트워크를 임시 경계에 제한하고 CLI별로 허용, 정책 거부, 훅 실행 오류에서 대역 명령이 실행됐는지 확인한다.

## 브로커와 서비스 검증

macOS·Linux에서 Docker CLI를 사용할 수 있으면 `sh scripts/test-rust.sh`가 임시 Unix 소켓의 합성 엔진만 조회한다. 허용 UID만 응답을 받고, 미허용 UID와 변경·상세 조회 요청은 엔진에 전달되지 않아야 한다. 엔진이 중단되거나 ID가 바뀌면 `503`을 반환하고 같은 엔진을 복구한 뒤 다시 조회해야 한다. 충돌한 프로세스의 수신 소켓 처리와 종료 시 정리도 검사한다.

Linux 검증용 소켓은 기본적으로 사용자 홈의 임시 디렉터리에 생성한다. `DSG_TEST_RUNTIME_DIR`로 상위 경로를 지정할 수 있다. Unix 소켓 경로 길이 제한을 고려해 그룹·기타 쓰기 권한이 없고 충분히 짧은 절대 경로를 사용한다.

macOS는 샌드박스에서 원본 소켓 접근이 거부되고 브로커 조회는 성공해야 한다. `DSG_TEST_LAUNCHD=1`로 실행하면 현재 GUI 사용자 도메인에 임시 테스트 서비스를 등록하고 한 번 종료·재시작한 뒤 해제한다. 실제 엔진 또는 배포용 서비스 계정 경계 확인에는 운영 전용 격리 엔진과 제한 계정이 필요하다. Linux systemd와 Windows SCM은 각각 해당 호스트에서 확인한다.

- macOS `DSG_TEST_LIVE_ENFORCEMENT=1 sh scripts/test-rust.sh`는 기존 `dockerguard-test` 루프백 엔진의 `/info`만 조회한다. 샌드박스의 Unix·TCP 직접 접속 거부와 브로커 조회를 확인한다. 실행 프로세스들의 UID는 같다.
- Linux `python3 -IB scripts/check-linux-service.py --disposable-guest --dockerd <경로> --docker <경로> <dsgd>`를 폐기 가능한 systemd 게스트에서 실행한다. 검증용 Docker 엔진과 런타임은 읽기 전용 미디어로 공급한다. 별도 제한 UID, 엔진·릴레이 직접 접근 거부, 변경 요청 전달 0건, broker 충돌 이후 단일 재시작과 정상 중지 및 등록 자원 회수를 확인한다.
- Windows 교차 빌드는 컴파일·링크를 검증한다. 실제 Windows 시험은 폐기 가능한 관리자 게스트의 `scripts/check-windows-service.ps1`에서 `-DisposableGuest`, `-DsgdExe`, `-UpstreamPipe`, `-UpstreamSid`, `-EngineId`, `-EngineKind Real|Fixture`를 명시한다. 임시 SCM 서비스와 작업으로 정상·강제 종료 후 재시작, 허용·거부 SID, 직접 엔진 접근 거부, 인벤토리와 임시 자원 회수를 검증한다. `EngineKind`는 운영자가 확인한 시험 환경을 구분한다.
- 실제 Windows의 LocalSystem에서 `scripts/check-windows-fixture.ps1 -DisposableGuest -DsgdExe <경로>`를 실행하면 한정된 합성 파이프를 같은 SCM 검증기에 연결한다. 실제 Docker 연동 수용 기준은 별도의 `Real` 실행에서 확인한다.

## 대시보드와 릴리스 검증

`sh scripts/test-rust.sh`는 임시 프로젝트에서 대시보드의 읽기 전용 응답, 정적 파일, 출처 검사와 쓰기 경로 거절을 확인한다. 연결 뒤 요청 전송을 짧게 늦춰도 응답해야 한다. 확인 뒤 상태 저장소가 생성되지 않고 테스트 파일이 남아야 한다. macOS와 Linux에서는 패키지 두 번 생성 결과의 일치, 설치된 바이너리 실행, 업그레이드, 진단·제거와 변조·링크 거절을 확인한다. macOS fixture에는 로컬 ad-hoc 서명 검증이 포함된다.

macOS에서 `DSG_TEST_WEBKIT=1 sh scripts/test-rust.sh`는 임시 WebKit 뷰를 띄워 1060px·390px 레이아웃, 스타일 적용, 실측 정리 후보와 Docker 미확인 상태를 확인한다. 사용자 브라우저 프로필과 저장소는 사용 범위에서 제외한다.

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

- Docker 내장 빌더 집계와 기존 이미지 보존은 조회·계획 검증에 해당한다. WBS 2.3은 별도 Buildx 빌더, 캐시 소유권, 안전한 정리 범위의 증거가 필요하다. 이미지 적용 검증에는 승인된 적용 경로와 단독 제어를 확인한 격리 엔진이 필요하다.
- macOS Compose 실험은 동일 컨테이너 재사용과 구성 해시 변경 거절을 실측했다. 이미지·마운트·health 차이는 합성 입력으로 대조했다. pnpm 시험은 일반 프로젝트 스크립트 실행을 검증하며 Next.js 프로젝트 시험을 별도로 추적한다.
- 프로세스 강제 종료와 재시작 근거는 [핸드오프](HANDOFF.md)에 기록한다. 전원 장애 복구와 전체 플랫폼의 감사 무손실은 독립된 출시 검증으로 추적한다.
