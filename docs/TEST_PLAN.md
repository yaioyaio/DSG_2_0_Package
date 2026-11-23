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

Docker 읽기 전용 단계에서는 Docker 명령 대역으로 컨텍스트 고정, 빈 목록과 미확인 상태의 구분, 중복 후보 및 변경 상황을 검증한다. 격리 엔진이 이미 동작 중이면 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test sh scripts/test-rust.sh`로 실제 조회를 추가 검증한다. 엔진 시작·리소스 생성·삭제는 이 조회 검증에서 수행하지 않는다.

관리형 실행 검증에는 임시 프로젝트의 작은 Cargo 빌드와 시간 초과 뒤 재실행, lease 경합·회수 및 등록부 마이그레이션이 포함된다. 사전에 digest가 있는 공개 fixture 이미지를 보유한 격리 엔진에서 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 sh scripts/test-rust.sh`로 건강한 컨테이너 하나를 확인한다. 검증하는 동안 동일 재사용 요청과 설정 해시 변경 결과, 추가 컨테이너 0개를 확인하고 fixture 라벨로 오늘 생성한 컨테이너만 회수한다.

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
- Windows 교차 빌드는 컴파일·링크를 검증한다. 실제 Windows 시험에서는 SCM 시작·정지·재시작, 허용·거부 SID, upstream 소유자·DACL 및 별도 제한 계정 경계를 확인한다.

## POC 제한
- 정규식 명령 필터는 모든 셸 표현을 탐지하지 않음.
- 파일/디렉터리 변경 간 TOCTOU 경합이 존재하므로 실제 프로젝트의 삭제 자동화 비권장.
- Docker 실제 중복 생성 방지/다중 OS 통합 테스트 미수행.

## 출시 게이트
- 영속 DB/사용자 데이터 유실 0건.
- 관리형 실행 경로에서 우회 0건(테스트 매트릭스 기준).
- 잘못된 이미지/서비스 재사용 0건.
- 감사 이벤트 무손실, 비정상 종료 후 회복.
