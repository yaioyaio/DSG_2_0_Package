# DSG 2.0 개발 핸드오프

## 2026-11-23 · DSG-01 착수

- 작업 브랜치 `feature/20261123-dsg-development`. 기준 `a8649b2`. 원격 `main`과 `develop`이 같은 초기 커밋을 가리킨다. GitHub Actions 비활성 상태를 API로 확인했다.
- 원격 기준선에는 `README.md` 한 개가 추적됐다. 패키지 소스·문서 44개가 미추적 상태였으며, 로컬 전용 `.DS_Store` 한 개는 전역 제외 규칙에 걸려 있었다. 프로젝트 자체의 Git/Docker 제외 파일을 추가했다.
- 이번 구현 경로는 `poc/dsg/core.py`와 `poc/tests/test_core.py`다. 이전 계획의 보호 경계 재검사까지 포함하며, 실제 프로젝트의 정리 적용과 Docker 동작 검증은 다음 해당 단계에서 수행한다.
- 서브에이전트는 `poc/dsg/core.py`를 담당한다. 새 워크트리 0개, 회수 0개, 잔여 보조 0개다. 지휘자가 회귀 테스트와 Git 최종 검증을 담당한다.
- 재개 시 승인 범위는 [백로그](BACKLOG.md), 테스트 수용 기준은 [실행 계획](draft/IMPLEMENTATION-PLAN.md#첫-작업)과 [테스트 계획](TEST_PLAN.md)에서 먼저 대조한다.

### DSG-01 검증 결과

- Git 제외 fixture에서 금지 경로 12개 차단, 예제와 산출물 경로 7개 유지. `.gitignore`의 프로젝트 규칙 79개에 대응하는 Docker 컨텍스트 패턴을 대조했다. 이 단계의 Docker 컨텍스트 검증은 규칙 비교로 수행했다.
- `PYTHONDONTWRITEBYTECODE=1 sh scripts/test.sh`에서 macOS Python 3.12 단위 테스트 14개(기존 8개, 신규 보호 회귀 6개)와 `doctor`·`scan`이 통과했다. 위험한 계획의 적용 테스트는 삭제 함수를 모의 처리해 대상과 앞선 정상 후보가 보존되는 것을 확인했다. Docker 및 실제 정리 삭제 실행은 0회다.
- Python POC 적용의 검사·삭제 경합과 Linux 동일 장치 bind mount 감지는 여전히 제한적이다. 실제 프로젝트 적용은 승인 기반 GC 및 플랫폼 검증 게이트에서 다룬다.
- 다음은 DSG-02다. [Rust ADR](ADR.md), [API 명세](API_SPEC.md), [데이터 모델](ERD.md), [아키텍처](ARCHITECTURE.md)의 Foundation 범위를 대조하고 Python 보호 회귀를 Rust 검증 입력으로 재사용한다.

DSG-01의 `d2214db` 기준선을 PR #1로 `develop`, PR #2로 `main`에 반영했다. 두 브랜치와 feature의 로컬·원격 파일 차이 15쌍은 모두 0이었다.

## 2026-11-23 · DSG-02 Rust 기반과 CLI UX

- 브랜치 `feature/20261123-rust-foundation`, 기준 `558914a`. [단계별 계획](PLAN.md#phase-1--foundation), WBS 1.1~1.4, ADR-001·004, API·ERD에 따라 등록, 정책·스캔, 정리 계획과 감사까지만 구현했다.
- 지휘자는 `policy.rs`와 CLI·테스트 러너를, 서브에이전트들은 `filesystem.rs`와 `registry.rs`를 담당했다. 보조 워크트리 생성 0개, 회수 0개, 잔여 0개다.
- `cargo fmt --manifest-path rust-scaffold/Cargo.toml --all -- --check`, `sh scripts/test-rust.sh` 통과. macOS에서 Rust 테스트 18개와 Python/Rust 동일 fixture 대조를 통과했다. `dist` 후보의 사용량이 일치했고 보호 경로가 보존됐다. Docker 실행과 실제 정리 삭제는 0회다.
- Rust CLI는 파일 정보가 불완전할 때 그 상태를 표시한다. Windows 파일 식별자, Unix 동일 장치 하위 마운트와 경로 교체 경쟁은 실제 자동 정리 검증의 선행 과제로 남긴다.
- 다음은 DSG-03이다. [DockerGuard 실측](draft/PROJECT-ANALYSIS.md#dockerguard-실측)의 모델·중복 판정·라이선스를 확인하고 읽기 전용 Provider와 격리 fixture를 구현한다.

DSG-02의 `bfbd69b`를 PR #3으로 `develop`, PR #4로 `main`에 반영했다. 여섯 참조의 15개 파일 비교가 일치했다.

## 2026-11-23 · DSG-03 읽기 전용 Docker Provider

- 브랜치 `feature/20261123-docker-inventory`, 기준 `65ea6de`. 공식 계획 Phase 2와 WBS 2.1·2.2 중 조회·후보 판정까지 적용했다. 소유권 확인이 부족한 bind, healthcheck 없음, 이미지 digest 없음은 재사용 미확인이다.
- 모델 및 고정 Docker 명령 어댑터를 서로 다른 서브에이전트에 위임했다. 지휘자가 CLI, 실측, Git을 담당했다. 보조 워크트리 생성·회수·잔여는 모두 0개다.
- `DSG_TEST_DOCKER_CONTEXT=dockerguard-test sh scripts/test-rust.sh`에서 Rust 테스트 34개, Python/Rust 보호 회귀 대조, 이미 기동된 격리 Docker 엔진 읽기 전용 조회를 통과했다. 컨테이너 7개, 이미지 ID 5개, 볼륨 0개, 중복 후보 0묶음. Docker 이미지 목록 6행은 같은 ID의 태그를 포함했고 고유 ID는 5개였다. Docker 데몬 시작과 자원 변경은 0회다.
- 다음은 DSG-04다. [실행 계획](draft/IMPLEMENTATION-PLAN.md#단계별-산출물과-통과-기준)의 관리형 실행·lease·Compose 재사용을 구현한다. 격리 엔진에서 전후 개수와 자원 건강 상태를 확인하고 실제 정리 활성화는 DSG-05에 둔다.

DSG-03의 `4bc8890`을 PR #5로 `develop`, PR #6으로 `main`에 반영했다. 로컬·원격 여섯 참조와 15개의 파일 diff가 모두 일치했다.

## 2026-11-23 · DSG-04 관리형 실행과 재사용

- 브랜치 `feature/20261123-managed-execution`, 기준 `cee771c`. Phase 3·WBS 2.2/3.1/3.2 중 `exec`, Cargo 시험 경로, SQLite v2 lease, Compose 재사용 재확인을 구현했다. OS·Provider 확대는 각 후속 범위에서 검증한다.
- `cargo fmt --manifest-path rust-scaffold/Cargo.toml --all -- --check`와 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 sh scripts/test-rust.sh` 통과. macOS Rust 테스트 46개, Python/Rust parity, 실제 Cargo fixture 검사, 타임아웃 뒤 동일 프로젝트의 실행 재획득 확인.
- 격리 엔진에서 fixture 1개를 임시 실행했다. 동일한 건강한 컨테이너를 두 번 반환했고, 설정 해시가 바뀐 요청은 거절했다. 재사용으로 늘어난 컨테이너 0개. 라벨로 만든 fixture 1개만 회수해 잔여 0개를 확인했다. Docker 데몬은 새로 시작하지 않았다.
- 슬롯·실행기·재사용 검증은 소유 경로를 분리해 위임했다. 보조 워크트리 생성·회수·잔여 모두 0개다. Linux·Windows 런타임과 그룹 이탈 프로세스는 미검증이다.
- 다음은 DSG-05다. 계획 승인과 경로 재검증을 등록부에 연결하고 승인된 재생성 산출물만 대상으로 종료·Git 완료 정리 경로를 검증한다.

DSG-04의 `4bd24d9`를 PR #7로 `develop`, PR #8로 `main`에 반영했다. 로컬·원격 여섯 참조의 파일 비교 15개가 모두 일치했다.

## 2026-11-23 · DSG-05 승인 기반 정리와 자동 회수

- 브랜치 `feature/20261123-approved-cleanup`, 기준 `5f1d661`. 레지스트리 v3, 정리 경계, 자동 정리기를 분리해 위임하고 지휘자가 CLI·Git 완료 진입점과 인수 검증을 담당한다.
- 승인·회수 검증은 합성 프로젝트 및 임시 Git 저장소에만 적용한다. 최초 컴파일에서 SQLite가 `u64` 바인딩을 거절해 범위 확인 후 `i64`로 저장하도록 수정했다.
- macOS에서 `cargo fmt --manifest-path rust-scaffold/Cargo.toml --all -- --check`와 `sh scripts/test-rust.sh` 통과. Rust 58개와 Python/Rust parity, 관리형 실행, 승인 GC, 자동 회수 통합 fixture를 검증했다.
- 보호 경로 1개와 계획 이후 변경된 경로 1개를 보존했다. 승인된 산출물 1개 23 B를 회수했고 잘못된 승인과 재사용을 거부했다. 정리 적용과 lease 해제를 감사 기록에서 각각 확인했다.
- 등록 Cargo 대역의 성공 산출물 1개 7 B와 임시 Git 보조 워크트리 1개 157 B를 회수했다. 실패·중단 산출물 각 1개, Git 브랜치와 주 작업 디렉터리는 보존했다. 자동 정리기의 macOS 자식 프로세스는 `RLIMIT_DATA` 대신 50 ms 간격으로 RSS를 확인했다. Linux의 메모리 제한 경로는 미실측이다.
- 실제 프로젝트 데이터, Docker 자원·데몬 변경은 0회다. 프로젝트의 보조 워크트리 생성·회수·잔여는 0개다. 다음은 DSG-06 훅 계약과 읽기 전용 MCP다.

DSG-05의 `1161b75`를 PR #9로 `develop`, PR #10으로 `main`에 반영했다. 여섯 참조 파일 비교 15개가 일치했고 주 작업 폴더는 `develop`으로 전환했다.

## 2026-11-23 · DSG-06 에이전트 연동

- 브랜치 `feature/20261123-agent-integration`, 기준 `9651e33`. Phase 4와 WBS 4.1·4.2 중 훅·설치 제거·공통 스킬·읽기 전용 MCP를 구현했다. 훅 프로토콜, 설정 편집, MCP를 별도 파일로 위임했고 지휘자가 진입점, 운영 문서와 통합 검증을 맡았다. 보조 워크트리 생성·회수·잔여 0개다.
- macOS에서 Rust 테스트 71개, Python/Rust 보호 회귀, 관리형 실행·승인 GC·자동 회수, 프로젝트 훅 설치·제거, 읽기 전용 MCP를 검증했다. 정리 후보 조회 후 상태 디렉터리가 생기지 않았고 변경 요청을 거절했다.
- Claude Code 2.1.330, Codex CLI 0.170.0, Gemini CLI 0.70.0으로 합성 모델 응답을 보내 실제 CLI의 허용·거부·훅 오류 사례 9개를 검증했다. 임시 설정과 대역 명령을 사용했다. 초기 검증에서 Codex/Gemini는 프로젝트 신뢰 설정이 필요했고, Claude는 쓰기 제한 안에 자체 임시 위치가 필요했다. 세 CLI는 정상 훅에서 보호 명령을 모두 차단했다. 훅 오류 시 Claude는 차단했고 Codex·Gemini는 검증용 대역 명령을 실행했다.
- 사용자 설정, 외부 모델 서비스 및 실제 Docker 엔진 사용은 0회다. Windows 훅 설치는 미지원이다. 당시 미실측이던 Linux 실제 CLI와 macOS MCP 실행·승인 상태 조회는 이후 확인했다. 승인 발급·적용을 MCP로 제공하는 확장은 사용 사례 확정 대기다. [Linux 실제 CLI](#linux-실제-에이전트-시험) · [설치본 MCP](#8단계-현재-후보-제품화-검증) [에이전트 운영 범위](../integrations/README.md) · [검증 재현](TEST_PLAN.md#에이전트-연동-검증)
- 다음은 DSG-07이다. 허용된 조회의 프록시 전달, 프로세스 권한 분리, 장애 후 서비스 복구를 격리된 환경의 순서로 검증한다.

DSG-06의 `a62830b`를 PR #11로 `develop`, PR #12로 `main`에 반영했다. 여섯 참조 파일 비교 15개가 일치했다. Git 완료 정리기를 호출한 뒤 주 작업 폴더를 `develop`으로 전환했다.

## 2026-11-23 · DSG-07 OS 서비스와 강제 통제

- 브랜치 `feature/20261123-os-enforcement`, 기준 `8f8dff8`. Phase 5·WBS 5.1·REQ-012에 따라 허용 조회만 수행하는 `dsgd` Unix 소켓 브로커, 서비스 예시, 통합 검증을 구현했다. HTTP·Unix 소켓 모듈을 분리해 위임했고 지휘자가 고정 Docker 클라이언트·서비스 구성·실측을 맡았다. 보조 워크트리 생성·회수·잔여 0개다.
- macOS arm64에서 실제 Docker CLI를 임시 복제해 합성 Unix 엔진에 연결했다. 미허용 UID와 변경·상세 조회 요청의 엔진 전달 0건, 같은 엔진 복구 후 조회 성공, 다른 엔진 ID에서 `503`, 브로커 중복 시작 거절, 충돌 후 소유 소켓 회수와 종료 시 소켓 정리를 확인했다. 경로별 macOS 샌드박스의 직접 소켓 접근 거절과 브로커 조회 성공도 확인했다.
- 임시 GUI `launchd` 서비스에서 등록, 강제 종료, 한 번의 수동 재시작, 해제까지 실행했다. 등록된 서비스에는 고유한 임시 레이블을 사용했다. 호스트 공용 Docker 설치 상위 디렉터리의 쓰기 권한 검사에 걸려 임시 전용 복제본을 사용했다. 재시작 검증의 대기는 `launchd` 시험 서비스의 재시작 간격을 명시해 해결했다.
- `DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`에서 Rust 테스트 78개와 기존 보호·실행·회수·에이전트 통합 회귀가 통과했다. 소유 기록과 이름만 같은 원본 파일 세 형태를 보존했다. 지휘자가 만든 임시 빌드와 Gemini 검증 번들 2개를 회수했다. 논리 크기는 합계 319,083,788 B였다.
- 실제 Docker 엔진과 배포용 계정·서비스 설정 변경은 0회다. 실제 엔진·별도 제한 계정의 런타임 접근 경계, Linux systemd 및 Windows SCM·파이프 구현/검증이 남았다. 공식 Phase 5 전체 수용 판정은 남은 권한·OS 실측에 따르며, 조건을 충족한 플랫폼에서 제품화 검증을 진행한다. [운영 권한과 복구](OPERATIONS.md#docker-조회-브로커) · [플랫폼 검증](TEST_PLAN.md#브로커와-서비스-검증)

### 재개 후 플랫폼 실측

- `DSG_TEST_LIVE_ENFORCEMENT=1 DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`에서 Rust 테스트 82개와 macOS 통합 검증을 통과했다. 실제 `dockerguard-test` 엔진에서 동일 UID의 제한 프로세스는 시험용 Unix 릴레이 소켓과 엔진의 루프백 TCP를 직접 사용할 수 없었고, 브로커 조회는 성공했다. 변경 요청 전달은 0건이다. 배포 계정 분리 실측은 별도 조건이다.
- Windows SCM 서비스와 고정 named pipe 어댑터를 추가했다. MSVC ARM64·x64 교차 컴파일 및 링크를 확인했다. Windows 동작과 배포 자격은 실제 SCM·계정·엔진 테스트 전까지 미검증으로 표시한다.
- Linux ARM64의 musl 링크에서 발견한 `renameat2` 라이브러리 차이를 커널 호출로 해결했다. 별도의 QEMU 게스트에서 `systemd`가 실행됐다. 첫 Docker 엔진 기동 종료는 새 게스트의 임시 유닛 진단으로 좁힌다. 기존 시스템 VM·Docker 설정과 데이터는 보존했다.
- 계획의 플랫폼별 수용 조건에 따라 실제 검증을 마친 macOS 로컬 제품화에 진입한다. 다른 플랫폼의 지원 범위와 전체 목표의 완료 판정은 각 실측이 끝난 뒤 기록한다.

DSG-07의 `6ffe700`을 PR #13으로 `develop`, PR #14로 `main`에 반영했다. 여섯 Git 참조의 파일 비교 15개가 일치했고 등록된 Git 정리 경로를 거쳐 `develop`으로 복귀했다.

- Linux ARM64 Ubuntu 24.04 QEMU 게스트에서 Docker 29.10.0과 `systemd`를 실행했다. 가상 링크는 외부 및 호스트 연결이 차단됐다. Docker TCP 리스너·컨테이너·이미지 0개를 확인했다. 비권한 UID의 원본 소켓 접근은 거부, 브로커 읽기 전용 조회는 성공, 미허용 UID·경로의 엔진 전달 0건이었다. SIGKILL 뒤 재시작 1회와 정상 중지 시 소켓 회수, 임시 유닛 3개 및 tmpfs 회수를 확인했다.
- 게스트 네트워크 대기 및 Docker의 읽기 전용 런타임 디렉터리 문제는 외부 통신 없는 가상 링크와 엔진 서비스 안의 전용 `/run/docker` tmpfs로 해결했다. 기존 사용자 VM은 시험에 사용하지 않았다.

## 2026-11-23 · DSG-08 웹 대시보드와 제품화

- 브랜치 `feature/20261123-dashboard-product`, 기준 `7a8f675`. Phase 6·WBS 6.1, 실행 계획의 플랫폼별 제품화 범위에 맞춰 대시보드 화면과 배포·설치 구현을 별도 위임했다. 지휘자는 로컬 API와 경계·수명 주기 검증을 맡았다. 보조 워크트리 생성·회수·잔여 0개다.
- 대시보드는 루프백 GET으로 프로젝트 예산, Docker 요약, 정리 후보와 정책을 표시한다. Rust API·CSP·출처 제한, 쓰기 요청 거부, 상태 DB·원본 보존을 확인했다. JS fixture에서 0건, 미확인, 새로고침, 조회 실패 상태가 통과했다. 브라우저 연결 호스트는 사용 불가여서 화면 렌더링은 미실측이다.
- `PYTHONDONTWRITEBYTECODE=1 sh scripts/test.sh`에서 Python 테스트 14개, `DSG_TEST_LIVE_ENFORCEMENT=1 DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`에서 Rust 테스트 82개와 대시보드·패키지 통합 검증이 통과했다. macOS ARM64 실제 배포 아카이브를 임시 prefix에 설치해 대시보드를 실행하고 로컬 서명을 검증한 뒤 제거했다.
- Linux ARM64 Ubuntu 게스트에서 패키지 생성 재현성, 설치, 업그레이드, 진단·제거와 실제 배포 아카이브의 대시보드 실행을 검증했다. 첫 제품 시험에서 ISO 파일명과 실패 판정의 문제를 찾아 이름을 보존하는 전송과 별도 실패 전파 스크립트로 교정했다. 이후 실행 제한이 걸린 prefix를 실행 가능 임시 경로로 바꿔 배포본 E2E를 통과했다.
- `dist/`에 macOS ARM64와 Linux ARM64 musl의 `0.1.0` 아카이브 및 SHA-256 파일을 보존했다. macOS는 ad-hoc 서명 검증, Linux는 해시 검증을 적용했다. Git과 Docker 문맥에서 해당 산출물을 제외한다.
- 두 대상의 `dsg`·`dsgd`를 깨끗한 별도 Cargo 대상 디렉터리에서 다시 최적화 빌드했다. 처음 빌드한 각 바이너리와 바이트가 일치했다.
- Windows ARM64·x64의 교차 빌드는 실제 SCM/파이프·설치 검증과 구분한다. OS 실행 환경, Windows 인벤토리 지원 및 승인된 공인 릴리스 서명과 브라우저 렌더링 확인은 전체 Goal의 남은 항목이다. [제품 운영](RELEASE.md)

DSG-08의 `f6397b2`를 PR #15로 `develop`, PR #16으로 `main`에 반영했다. 여섯 Git 참조의 15개 파일 비교가 일치했고 등록된 정리 경로를 거쳐 `develop`으로 복귀했다.

### Windows·브라우저 보완

- 작업 브랜치 `feature/20261123-windows-qualification`, 기준 `79319fa`. Windows 브로커가 인증한 엔진의 컨테이너·이미지 건수를 제공하도록 했다. 볼륨·중복·개별 상세는 미확인이다. Windows용 로컬 패키지 설치 수명 주기와 SCM 검증 진입점을 도입했다. 설치 경로의 상태 파일·릴리스·스테이징 디렉터리를 Git과 Docker 문맥에서 제외했다. 보조 워크트리 생성·회수·잔여는 0개다.
- Windows ARM64·x64 최적화 교차 빌드와 두 `0.1.0-rc.1` 후보 아카이브의 동일 입력 해시·PE 대상·파일 무결성 검증이 통과했다. 공식 PowerShell의 파서에서 SCM 검증 스크립트의 구문이 통과했다. 기존 Windows VM을 재개하거나 평가판을 설치하지 않았으므로 SCM·별도 계정·실제 엔진과 Windows 설치 수명 주기는 미실측이다.
- `DSG_TEST_WEBKIT=1 DSG_TEST_LIVE_ENFORCEMENT=1 DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`로 Rust 85개와 통합 검증이 통과했다. 비공개 WebKit 테스트 뷰에서 1060px·390px의 실제 문서 배치, 스타일과 조회 데이터를 검증했다. 사용자 브라우저는 열지 않았다.
- 재개 시 사용 승인을 받은 Windows 게스트와 엔진을 확인한 뒤 SCM 및 패키지 수명 주기를 실행한다. macOS의 배포 계정 분리와 공인 서명은 각각 승인된 계정·서명 환경을 확인한 뒤 검증한다. [Windows 실측 진입점](TEST_PLAN.md#브로커와-서비스-검증) · [제품 운영](RELEASE.md)

### Parallels Windows ARM64 실측과 서명 검증

이번 검증 범위는 macOS 호스트와 기존 Parallels Windows ARM64 게스트에서 실행 가능한 작업이다. 개발 브랜치 병합의 근거는 이 범위에서 통과한 검증이다. 플랫폼별 공식 배포 자격과 검증 현황은 릴리스 문서에서 관리한다. [플랫폼별 검증 현황](RELEASE.md)

- 기존 Parallels Windows 11 ARM64 게스트를 재개했다. 공식 ARM64 임베디드 Python과 릴리스 후보를 해시 확인 후 임시 폴더로 옮겼다. 동일 입력 재생성, 설치·진단·실행·업그레이드·제거, 동시 설치 충돌, 파일 변조·링크·대체 스트림 거절 및 잠긴 파일의 제거 재시도를 통과했다. 설치한 ARM64 CLI의 대시보드 HTTP 테스트도 통과했다. 패키지 검증용 임시 파일을 회수했다.
- Windows SDK 경로에 SignTool이 있다. 이 게스트의 Docker 명령과 표준 로컬 엔진 파이프는 확인되지 않았다. ARM Windows용 Docker Desktop은 WSL 2가 필요하며, Parallels는 Apple Silicon의 Windows ARM 게스트에서 해당 중첩 가상화를 지원하지 않는다고 명시한다. [Parallels 제한](https://kb.parallels.com/en/129497) · [Docker 요구사항](https://docs.docker.com/desktop/setup/install/windows-install/)
- 실제 SCM 실측에서 허용 계정의 파이프 연결이 속성 읽기 권한 누락으로 실패했다. 허용 SID에 `FILE_READ_ATTRIBUTES`만 추가한 `0.1.0-rc.2`에서 LocalService의 제한 조회, NetworkService의 연결 거부, 원본 파이프 직접 연결 거부, 정상 종료·강제 종료 뒤 재시작 및 임시 서비스·작업 회수를 확인했다. 상류는 합성 파이프를 사용했다. ARM64·x64 후보 아카이브와 SHA-256 파일을 `dist/`에 보존했다. [Windows 파이프 권한](OPERATIONS.md#docker-조회-브로커) · [Windows 접근 권한 기준](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- `0.1.0-rc.2`의 대시보드에서 연결 뒤 요청이 늦게 도착할 때 WinSock `10053`을 네이티브 재현했다. 수락한 소켓을 제한 시간이 있는 블로킹 I/O로 설정한 `0.1.0-rc.3`에서 같은 테스트와 대시보드 접근 정책, 설치 수명 주기 및 임시 경로 회수가 통과했다. ARM64의 `dsgd.exe`는 SCM을 통과한 `rc.2`와 해시가 일치한다. x64의 `rc.3`는 교차 링크와 아카이브 검증을 통과했다. Windows 게스트를 원래의 일시정지 상태로 돌렸다. [WinSock 연결 수락](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-accept)
- `ddd-tools`의 Windows Authenticode·타임스탬프, macOS Developer ID·공증 및 Linux APT 검증 절차를 대조했다. `ddd-studio-rsie`가 아카이브와 정규 매니페스트를 각각 서명하는 방식을 참조해 DSG 분리 서명 검증을 구현했다. macOS에서 임시 Ed25519 키로 정상 서명, 12가지 거부 입력, 서명 옵션을 사용하는 설치·업그레이드 및 거부 시 이전 버전 보존을 확인했다. DSG 공식 게시자 자격과 공인 서명은 검증 대기다. [서명 조건과 검증 명령](RELEASE.md#배포-파일의-분리-서명-검증)
- Windows 게스트가 실행 중일 때 macOS 통합 러너의 합성 Git 회수는 사용 여부 확인 실패로 보존 동작을 반환했다. 게스트 일시정지 후 단독 실행한 `sh scripts/test-rust.sh`는 Rust 85개와 통합 검증을 통과했다. 합성 Git 보조 워크트리 1개·157바이트를 회수했고 로컬·원격 참조와 주 작업 폴더를 보존했다. 작업용 저장소의 보조 워크트리는 생성 0개다.

## macOS 및 기존 Parallels 계획 보완

- 브랜치 `feature/20261123-mac-plan-completion`, 기준 `53476a4`. 자격증명 및 런타임 산출물 제외 정책을 확인했다. 보조 워크트리 0개다. 남은 기준은 [백로그](BACKLOG.md#macos-및-기존-parallels-보완)에 둔다.
- DSG-01·02의 Python 14개와 CLI 진단, Python/Rust 보호 대조, 잘못된 정책 거절, 5 GiB 희소 파일의 논리 크기를 검증했다. 기본 POC 러너에서 테스트용 바이트코드가 작업 트리에 생기지 않도록 했다.
- DSG-03에 로컬 컨텍스트 이름 탐색과 지정한 엔진의 내장 빌더 집계를 추가했다. Rust 및 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test sh scripts/test-rust.sh` 통합 검증이 통과했다. 실측은 컨테이너 7개, 이미지 5개, 볼륨 0개, 내장 빌더 캐시 208개와 활성 0개다. 캐시는 소유권 미확인으로 보존했다.
- DSG-04에서 실제 Compose `up`을 반복해 동일 ID 한 개를 확인했고, 구성 변경 후 이전 기대값을 거절했다. Cargo·Node·pnpm·Gradle·uv·Go의 독립 프로젝트와 Playwright 1.65.0 테스트 실행, 성공·실패 trace 보존 및 해당 lease의 감사 기록을 확인했다. Gradle의 파일 디스크립터 상한은 해당 Provider에만 1024개로 조정했다.
- DSG-05에서 기존 격리 엔진 이미지의 보존 판정을 확인했다. 후보 검사와 재검증은 합성 이미지로 확인했으며 실제 Docker 자원은 이 이미지 계획으로 변경되지 않았다. Playwright 보관 기간을 1초로 설정한 시험에서 만료 성공 trace 1개를 회수하고 미설정 실행과 기대 실패 trace를 보존했다.
- DSG-06의 macOS MCP에서 실행 상태와 승인 기록을 세션 프로젝트에 한정해 조회했고 상태 파일 변경 0건을 확인했다. 승인 발급과 적용은 관리형 CLI에 둔다.
- DSG-07의 macOS 시험용 Unix 릴레이와 기존 Docker 엔진의 TCP 직접 접근 차단, 브로커 조회, 임시 launchd의 비정상 종료·재시작·해제를 재검증했다. 훅 오류와 무관하게 Docker 접근을 통제하는 기준은 OS가 적용한 경계다. 호스트의 비대화형 관리자 접근을 사용할 수 없어 별도 macOS 배포 계정 설정과 LaunchDaemon 검증은 미실측이다.
- DSG-08에서 macOS ARM64의 `0.1.1-rc.1`을 각각 새 빌드 대상에서 두 번 생성해 아카이브 SHA-256이 일치했다. 보존 중인 `0.1.0` 실물에서 새 후보로 업그레이드하고 CLI 버전, 로컬 서명, 제거 후 기존 파일 보존을 확인했다. Azure나 Apple 등 공식 게시자 서명은 별도 승인 환경이 필요하다.
- 최종 macOS 통합에서 독립적으로 수집한 실제 Docker 건수가 API, WebKit 1060px·390px 화면에 모두 동일했다. 임시 관리형 감독자를 강제 종료한 시험에서 만료 전 lease를 보존하고 만료·자식 종료 확인 뒤 같은 프로젝트가 다시 실행됐다.
- 전체 로컬 러너에서 Python 14개와 Rust 98개, Docker·Compose·Provider·TTL·MCP·launchd·WebKit 및 로컬 Ed25519 검증을 통과했다. 소스 트리 `35a30aa`로 만든 `0.1.1-rc.1` ARM64 후보와 SHA-256 파일을 `dist/`에 보존했다. macOS와 Windows용 아카이브는 각각 이름에 대상이 들어 있다.
- 기존 Parallels Windows ARM64에서 같은 버전의 실제 설치·업그레이드·제거, 대시보드 HTTP 및 SCM 시작·정상/강제 종료 후 재시작을 통과했다. SCM 검증은 합성 파이프와 별도 SID 두 개를 사용했다. Windows 검증용 임시 자료와 등록 자원을 회수하고 게스트를 원래 일시정지 상태로 되돌렸다. 분리된 운영 계정, 실제 Windows Docker, 공인 게시자 서명은 각 전제 조건에서 확인한다.

## 외부 서명 요청과 macOS 게시자 검증

- `feature/20261124-release-signing`, 기준 `66bef38`. PLAN의 패키지 배포와 WBS 6.1의 서명 검증에 `ddd-studio-rsie` `718ca48`의 원문 분리 서명, `ddd-tools` `3e30ff9`의 플랫폼별 승인 게시자 확인 절차를 반영했다. `prepare-release-signature.py`는 원문 매니페스트를 내보내고 검증한 서명 응답을 조립한다. macOS에서는 승인된 Team ID를 지정해 최종 아카이브에 든 두 실행 파일을 점검한다. [릴리스 절차](RELEASE.md#오프라인-서명-요청과-조립)
- 서명 작업 디렉터리 `dist/dsg-signing/`의 기존 제외 규칙을 확인하고 Apple `.p8` 및 추가 키 보관 파일 형식을 Git과 Docker 문맥에서 제외했다.
- `python3 -B scripts/check-release-signature.py --openssl <OpenSSL 3 절대 경로>`에서 임시 키로 생성한 서명 두 개의 조립과 검증, 승인 해시·서명 오류 거부, 출력 충돌 보존을 확인했다. 두 번째 실행 파일의 서명 거부가 반영되는지는 `codesign` 응답을 대역한 회귀로 확인했다.
- macOS ARM64의 실제 `0.1.1-rc.1` 아카이브와 임시 Ed25519 키로 준비·조립·검증, 두 실행 파일의 ad-hoc 검사 및 CLI 실행, 패키지 재생성과 설치·업그레이드·제거를 통과했다. Developer ID 검증을 요구하면 실제 ad-hoc 후보를 서명 파일 생성과 설치 전에 거부했고, 업그레이드에서는 기존 설치 상태를 보존했다. 후보 해시는 검증 전후에 일치했고 모든 시험 자료는 임시 경로에서 회수했다.
- DSG용 Apple Developer ID와 공증, Windows 공인 Authenticode·타임스탬프는 승인된 환경에서의 성공 검증이 남아 있다. 이번 작업에서 Docker 데몬과 Parallels 게스트의 상태를 변경하지 않았다. [플랫폼별 공인 서명](RELEASE.md#플랫폼별-공인-서명)

## WBS 작업과 산출물 대조

- 2026-11-24, `feature/20261124-plan-reconciliation`, 기준 `f316175`. DSG-01~08의 구현·검증을 WBS 14개와 일대일로 연결했다. PLAN·WBS의 계획 본문, 요구사항 13개와 수용 기준은 보존했다. 백로그의 승인 범위와 전체 플랫폼 수용 상태, README·POC의 실행 범위, ADR의 초기 제안과 구현 채택 근거를 맞췄다. [작업 대조표](TASK.md) · [백로그](BACKLOG.md) · [요구사항](REQUIREMENTS.md) · [설계 결정](ADR.md)
- macOS에서 `sh scripts/test.sh`의 Python 14개와 CLI 진단·스캔, `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_WEBKIT=1 sh scripts/test-rust.sh`의 Rust 98개 및 통합 러너가 통과했다. 기존 지정 엔진에서 컨테이너 7개, 이미지 5개, 볼륨·중복 후보 0개를 읽었고, 내장 빌더 캐시 208개·활성 0개와 기존 이미지의 보존 판정을 확인했다. API와 두 WebKit 화면의 실제 Docker 집계가 일치했다. Docker 데몬을 새로 시작하거나 실제 Docker 자원을 변경하지 않았다. Parallels 게스트는 실행하지 않았다.
- `dist/`에 보존한 macOS ARM64·Windows ARM64 0.1.1-rc.1, Linux ARM64 0.1.0, Windows x64 0.1.0-rc.3 아카이브의 대상·버전·SHA-256·멤버 해시를 대조했다. 당시 플랫폼별 설치·서비스 시험 결과는 기존 기록을 유지했다. 보조 워크트리는 생성·회수·잔여 0개다. [플랫폼별 결과](RELEASE.md)
- 재개 시 WBS 2.3의 소유권과 안전한 회수 경계, 실제 Docker 이미지 적용 경로, macOS 운영 계정 및 Windows 실제 엔진 같은 플랫폼별 잔여 수용 기준을 작업 대조표에서 먼저 확인한다. 훅 오류의 운영 기준과 MCP·Provider SDK 확장의 승인 선행 조건도 대조표에 기록했다. [잔여 조건](TASK.md#수용-기준-확정이-남은-항목) · [출시 게이트](TEST_PLAN.md#출시-게이트)

## DSG-02 Windows NTFS 파일 식별 보완

- `feature/20261124-windows-foundation`, 기준 `1708cea`. WBS 1.3의 Windows 스캔·계획에서 읽기 전용 NTFS 핸들로 파일·볼륨 ID와 링크 수를 확인한다. 프로젝트 루트까지의 경로와 검사 중인 디렉터리를 핸들로 유지한다. 하위 파일 ID와 링크 수를 메타데이터 다이제스트에 반영한다. 링크·junction·다중 하드 링크·공유 충돌로 확인할 수 없는 후보는 보존한다. Windows의 실제 GC 적용은 별도 범위다. [코드](../rust-scaffold/crates/dsg-core/src/filesystem/windows.rs) · [작업 대조표](TASK.md)
- macOS에서 Python 14개와 Rust 98개 및 통합 러너, `cargo fmt` 검증을 통과했다. Windows ARM64 MSVC로 교차 링크한 Rust 시험 바이너리를 해시 비교 후 기존 Parallels Windows 11 ARM64에서 실행했다. `filesystem::tests --test-threads=1`의 9개가 통과했다. 그중 Windows 전용 5개가 디렉터리 교체 차단과 검사 종료 뒤 정상 교체, 동일 시간·크기 하위 파일 교체, 하드 링크, junction, 공유 충돌을 재현했다. [재현 절차](TEST_PLAN.md#rust-foundation-검증)
- PowerShell 실행 경로와 공유 파일 전달을 분리해 확인했다. 디렉터리 교체 시험에서 출력 핸들만 해제한 경우 루트 핸들이 보호를 유지했다. 루트 검사를 종료한 뒤 교체를 재검증했다. 호스트·게스트 임시 시험 자료를 회수하고 게스트를 원래 일시정지 상태로 돌렸다. Docker 자원 변경과 보조 워크트리 생성은 0회다.

## DSG-03 기본 Buildx 빌더 조회 검증

- `feature/20261124-buildx-observation`, 기준 `c3707b0`. WBS 2.3의 기본 Buildx 조회를 `docker-buildx-cache`로 추가했다. 빈 전용 Buildx 설정과 명시 컨텍스트, 조회 전후 엔진 ID 대조, 수치 범위 검증, 하위 프로세스 정리를 적용했다. 조회가 실패하면 수치를 미확인으로 남기고 보존한다. [명령과 결과](API_SPEC.md#docker-읽기-전용-인벤토리)
- `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 sh scripts/test-rust.sh`에서 Rust 102개와 통합 검증을 통과했다. 실제 기본 Buildx 캐시는 총 208개, 회수 가능 표시 208개, 공유 8개, 변경 가능 167개다. 각 수치는 해당 플래그의 집계로 일부 항목은 둘 이상의 상태에 해당한다. 실제 Docker 자원과 캐시는 변경하지 않았다.
- 첫 통합 시 기존 워크트리 정리 fixture가 실패했다. 보존한 배포 바이너리의 같은 fixture를 단독으로 실행해 통과했고 후속 전체 검증도 통과했다. 현재 격리 컨텍스트에서는 실행 중인 별도 BuildKit 빌더를 확인하지 못했다. 별도 빌더의 조회, 소유권과 GC 수용 기준은 남아 있다. 이번 검증에서 VM 실행과 보조 워크트리 생성은 0회다.

## DSG-04 Next.js 앱과 Compose health 실측

- `feature/20261124-nextjs-provider-proof`, 기준 `4c33e8a`. WBS 3.1의 Next.js 16.4.6을 실제 `pnpm` Provider로 빌드했다. `BUILD_ID`, 임의 표식이 들어간 사전 렌더 HTML, 루프백 `next start` 응답, 종료와 해당 프로젝트의 서로 다른 빌드·실행 lease 두 건 및 감사 기록을 확인했다.
- WBS 2.2의 실제 Compose fixture에서 반복 실행 뒤 확인한 동일 컨테이너에 전용 `/dev/shm` 표식을 썼다. 같은 ID·구성·이미지·마운트에서 health가 `unhealthy`로 바뀌면 재사용을 `not_healthy`로 거절했다. 기존 구성 변경 거절과 라벨·ID로 확인한 시험 컨테이너 회수도 통과했다. 실제 이미지·마운트 변경 실측은 남아 있다.
- macOS `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_COMPOSE_FIXTURE=1 DSG_TEST_BUILD_PROVIDERS=1` 및 `DSG_TEST_NEXT_MODULES`를 지정한 `sh scripts/test-rust.sh`에서 Rust 102개와 통합 검증을 통과했다. 첫 시험에서 임시 홈의 pnpm 버전 관리자 shim과 공유 의존성 갱신 문제가 확인됐다. 독립 실행 pnpm, 파일 수·크기를 제한한 임시 복사본, 오프라인 설정으로 재검증했고 의존성 스테이지와 fixture를 회수했다. Docker Engine과 VM을 새로 시작하지 않았다. 보조 워크트리 생성 0개다. [검증 절차](TEST_PLAN.md#rust-foundation-검증)

## DSG-05 격리 직후 강제 종료와 감사 복구

- `feature/20261124-gc-crash-recovery`, 기준 `777f9a7`. WBS 1.4에서 임시 파일의 승인 적용 의도 커밋과 격리 이동 뒤 시험 자식만 `SIGKILL`했다. 새 등록부에서 만료 lease와 적용 의도를 되찾아 실제 복원 경로를 적용했다. 복원 파일의 ID·SHA-256이 일치하고, 회수 0바이트와 중복 복원 거절, SQLite 무결성·외래키 위반 0건, `GC_APPLY_INTENT`·`GC_PRESERVED` 각 1건을 확인했다. [시험 코드](../rust-scaffold/crates/dsg-core/src/gc.rs)
- macOS `sh scripts/test-rust.sh`에서 Rust 103개와 통합 러너가 통과했다. 실제 사용자 데이터·Docker 자원 변경과 보조 워크트리 생성은 0회다. 호스트 전원 장애와 실제 Docker 이미지·Buildx 캐시 회수는 별도 수용 조건으로 남는다.

## DSG-06~08 현재 소스의 macOS 자격 검증

- `feature/20261124-macos-qualification`, 기준 `b7d94d6`, Rust 소스 트리 `681be1efa0ed4136f47fb6d4c5c6a7122b503519`. 최적화 빌드한 현재 바이너리의 임시 설치본으로 `DSG_TEST_AGENT_NATIVE=all`의 실제 Claude·Codex·Gemini 9개 사례와 MCP 대조를 통과했다. 네트워크와 쓰기를 제한한 시험에서 합성 응답과 대역 도구를 사용했다. [에이전트 검증](TEST_PLAN.md#에이전트-연동-검증)
- 현재 `dsgd`의 임시 GUI launchd 등록·재시작·해제와 시험용 Unix 릴레이와 기존 격리 Docker의 루프백 TCP 직접 접근 차단 및 브로커 조회가 통과했다. 별도 macOS 운영 계정·공식 LaunchDaemon은 관리자 경로가 필요하다. 기존 Parallels Ubuntu ARM64의 기본 게스트 명령을 확인하고 일시정지로 복귀했다. 당시 남은 Linux 관리형 실행은 기존 Ubuntu 게스트에서 후속 검증했다. Windows 실제 Docker 엔진은 별도 수용 조건이다. [현재 Ubuntu 실측](#별도-buildkit-읽기-전용-실측) [플랫폼 검증](TEST_PLAN.md#브로커와-서비스-검증)
- macOS `0.1.1-rc.1`을 독립 대상 디렉터리에서 두 번 최적화 빌드했다. `tar.gz` SHA-256이 일치했고 보존된 `0.1.0`에서 업그레이드·실행·제거를 통과했다. 기존 Docker 엔진에서 독립 조회한 컨테이너 7개, 이미지 ID 5개, 볼륨 0개가 현재 패키지의 API·WebKit 1060px·390px 화면에 일치했다. 첫 대시보드 호출은 실행 래퍼의 컨텍스트 변수가 누락돼 실패했고, 동일 패키지를 명시 컨텍스트로 재검증해 통과했다. `dist/dsg-macos-qualification-681be1e/`에 패키지·SHA-256·검증 요약을 보존했다. 임시 설치·서비스와 보조 워크트리 잔여는 0개다. [제품 검증](RELEASE.md)

## 최신 계획과 산출물 정합성 검증

- 2026-11-24, `feature/20261124-latest-plan-evidence`, 기준 `fb67f38`, Rust 소스 트리 `681be1e`. WBS 14개와 DSG-01~08을 대조하고 구현 산출물과 실측·시험 근거를 별도 열로 정리했다. PLAN·WBS, 실행 초안의 단계별 산출물·수용 기준 및 요구사항 13개의 기준을 보존했다. 문서의 1:1 대조 결과와 구현·시험의 범위별 수용 판정은 [작업 대조표](TASK.md)에 기록한다.
- 이번 macOS 회귀에서 `sh scripts/test.sh`의 Python 14개와 진단·스캔, `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 DSG_TEST_WEBKIT=1 sh scripts/test-rust.sh`의 Rust 103개 및 통합 검증을 통과했다. 파일 격리 직후 강제 종료 복구, 기존 격리 엔진 인벤토리 7/5/0 및 중복 후보 0건, 기본 Buildx 캐시 208개 조회·보존, API와 WebKit 1060px·390px 집계 일치를 확인했다. [시험 절차](TEST_PLAN.md)
- 보존 아카이브 4개의 대상·버전·아카이브 및 파일별 SHA-256을 대조했다. 최신 macOS 아카이브는 `QUALIFICATION.json`의 해시와 현재 Rust 소스 트리에 일치했다. 이전 Linux·Windows 패키지 수명 주기와 Windows NTFS·SCM은 각 절의 기존 실측을 인용한다. 이번에는 VM을 실행하지 않았으며 Docker 데몬 시작·실제 Docker 자원 변경·보조 워크트리 생성은 0회다. [후보별 근거](RELEASE.md)
- 재개 항목은 WBS 2.3의 별도 빌더 조회·캐시 소유권·안전한 회수다. 실제 Compose 이미지·마운트 변경, Docker 이미지 적용 구현과 승인, 관리형 실행·OS 서비스·패키지의 플랫폼별 잔여 검증을 [백로그](BACKLOG.md)와 [출시 게이트](TEST_PLAN.md#출시-게이트)에 연결했다.

## macOS 및 기존 Parallels 잔여 수용 기준 재검증

- 2026-11-24, `feature/20261124-docker-remaining-qualification`, 기준 `c20d807`, Rust 소스 트리 `681be1e`. 공식 PLAN·WBS 및 실행 계획의 선행 관계를 확인했다. Compose 시험과 플랫폼별 검토를 나눠 맡기고 지휘자가 실제 환경과 최종 검증을 맡았다. 보조 워크트리 생성·회수·잔여는 각각 0개다.
- WBS 2.3은 지정 `dockerguard-test` 엔진에서 기본 Buildx 조회·보존 시험을 다시 통과했다. 별도 BuildKit 컨테이너 및 시험용 BuildKit 이미지가 0개였다. 별도 빌더의 조회에는 엔진과 단일 노드에 귀속된 로컬 엔드포인트가 필요하다. 빌더 이름만 지정한 조회는 로컬 격리 엔진 경계를 입증하지 못한다. 캐시 소유권과 회수는 별도의 구현 및 검증 조건으로 기록했다. [Buildx 및 이미지 적용 조건](TEST_PLAN.md#출시-게이트)
- WBS 2.2의 실제 Compose 시험에 공개 Alpine ARM64 다이제스트를 두 번째 입력으로 고정했다. `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 DSG_TEST_COMPOSE_FIXTURE=1`과 `DSG_TEST_COMPOSE_IMAGE`로 Rust 103개와 통합 시험이 통과했다. 반복 실행은 동일 컨테이너 1개를 유지했다. 구성·health·이미지·64 KiB `tmpfs` 변경을 모두 거절했고 일치하는 `tmpfs`는 미확인으로 표시했다. 시험 컨테이너 회수 후 기존 컨테이너 7개가 남았다. 이미지 6개에는 시험용 Alpine 1개가 포함된다. 볼륨과 중복 후보는 각각 0개였다. [시험용 이미지 지정](TEST_PLAN.md#rust-foundation-검증)
- WBS 6.1의 현재 Windows ARM64 바이너리를 `cargo xwin`으로 `--release --locked --offline --jobs 1` 교차 링크했다. 같은 입력으로 두 번 패키징한 해시가 일치했다. 기존 Windows ARM64의 로컬 NTFS에서 이전 `0.1.1-rc.1` 실물을 설치하고 현재 후보로 교체했다. 아카이브·설치 파일별 해시, `doctor` 상태와 경로, 이전 릴리스 회수, 두 실행 파일, 대시보드 HTTP와 제거 후 사용자 파일 보존을 확인했다. PowerShell 접속 시간과 출력 디코딩을 보완한 재시도에서 통과했다. 임시 경로를 회수하고 게스트를 원래 일시정지 상태로 복귀시켰다. [후보와 해시](RELEASE.md)
- WBS 3.2·6.1의 Linux ARM64 현재 후보는 macOS에서 임시 Rust 대상 표준 라이브러리와 Zig·Rust LLD로 교차 링크했다. ELF 정적 링크와 동일 입력 두 번의 패키지 해시 일치를 확인했다. 기존 Ubuntu Parallels에서는 ARM64 명령과 Docker·runc·Node 실행 파일 존재를 확인했다. Cargo·Rust·별도 BuildKit 도구가 기본 실행 경로에 없었다. 기본 공유와 기존 가상 인터페이스의 직접 전송 경로를 확보하지 못해 게스트에서 제품을 실행하지 못했다. 게스트를 원래 일시정지 상태로 돌렸다.
- 기존 격리 Docker 엔진에서 권한을 제거한 읽기 전용 Linux ARM64 컨테이너와 전용 tmpfs를 사용하고 Docker에 `--network none`을 요청했다. 이전 `0.1.0` 실물 설치, `0.1.1-rc.1`로 업그레이드, 두 실행 파일 실행, 성공·타임아웃(124)·같은 프로젝트 lease 재획득, 대시보드 HTTP, 제거·사용자 파일 보존을 확인했다. 첫 정리에서 시험 래퍼의 이름 변수가 덮어써져 소유 컨테이너를 따로 확인해 회수했다. 이름을 분리한 재검증은 자동 회수까지 종료 0으로 통과했다. [후보와 해시](RELEASE.md)

## 명명 볼륨과 Ubuntu Parallels 실측

- 2026-11-24, `feature/20261124-remaining-runtime-gates`, 기준 `b6d2cb2`. WBS 2.2·2.3의 선행인 실제 Docker 경계와 WBS 3.2·6.1의 플랫폼 검증 조건을 기존 PLAN·WBS와 대조했다. 보조 워크트리 생성·회수·잔여는 각각 0개다.
- WBS 2.2는 지정 격리 엔진에 빈 시험 볼륨 A·B 두 개를 준비했다. `external`, `read_only`, `nocopy` 투영과 실제 Docker 마운트 이름을 확인했다. A를 사용하는 Compose 반복 뒤 같은 컨테이너 ID와 재사용 성공을 두 번 확인했다. B로 교체한 뒤 현재 구성 해시에서 A를 기대하면 `mount_changed`와 종료 5, B를 기대하면 재사용 성공이었다. 네이티브 엔진에서 소유 시험 컨테이너를 회수하고 두 시험 볼륨은 미사용 상태로 보존했다. 전체 등록 러너는 Rust 103개와 통합 시험을 통과했다. 인벤토리는 컨테이너 7·이미지 7·볼륨 2·중복 후보 0개였다. [입력 및 시험 절차](TEST_PLAN.md#rust-foundation-검증)
- WBS 3.2·6.1은 기존 Ubuntu ARM64에서 `prlctl exec`의 표준입력 바이트 왕복을 확인해 전송 제약을 해결했다. 두 아카이브와 시험 스크립트는 게스트 로컬 임시 폴더에 길이와 SHA-256을 재검증한 뒤 기록했다. Rust 트리 `681be1e`의 현재 `0.1.1-rc.1` Linux 후보로 이전 `0.1.0` 실물 업그레이드, `dsg`·`dsgd` 실행, 성공·타임아웃 124·lease 재획득, 대시보드 HTTP, 제거 후 시험용 사용자 파일 보존까지 통과했다. 시험 파일을 회수하고 VM을 원래 일시정지 상태로 복귀시켰다. [후보와 해시](RELEASE.md)

### 별도 BuildKit 읽기 전용 실측

- 공식 BuildKit v0.35.1 Linux ARM64 릴리스의 아카이브와 두 실행 파일에 대해 SHA-256을 확인했다. 기존 Ubuntu Parallels의 시험 프로세스에 전용 네트워크·마운트 네임스페이스와 전용 소켓을 적용했다. 공식 `buildctl du`의 초기 집계는 0개였다. 외부 접속 없이 `FROM scratch`와 `COPY`만 사용하는 시험 빌드 후 두 번의 집계가 4개로 일치했다.
- Rust 트리 `ada5a09`의 Linux ARM64 제품 `buildkit-local-cache`는 공식 명령의 0개·4개와 각각 일치했고 보존 판정을 반환했다. 예상 PID 불일치, 다른 클라이언트 해시, 소켓 심볼릭 링크는 모두 종료 5와 수치 미확인이었다. 허용되지 않은 클라이언트의 실행 흔적은 0개였다. 클라이언트 출력 형식의 개행 인코딩을 바로잡은 뒤 실측했고, 시험 프로세스·소켓·임시 저장소를 회수했다. 실제 공유 캐시·이미지의 삭제를 적용할 수용 기준은 남아 있다. [조회 계약](API_SPEC.md#docker-읽기-전용-인벤토리)
- `ada5a09`의 Linux ARM64 패키지는 동일 정적 링크 입력 두 번의 SHA-256이 일치했다. 기존 Ubuntu 게스트에서 표준입력 전송과 SHA-256 재검증, 이전 `0.1.0` 실물 설치·현재 후보 업그레이드, 관리형 실행·타임아웃 124·lease 재획득, 대시보드 HTTP 및 제거 후 사용자 시험 파일 보존을 다시 통과했다. 시험 자료 회수와 VM의 일시정지 복귀를 확인했다. [현재 Linux 후보](RELEASE.md)

### Linux 실제 에이전트 시험

- macOS에 연결된 시험 엔진에서 컨테이너에 `--network none`을 요청했으나 커널 인터페이스 목록에 루프백 외 인터페이스가 9개 존재했다. 기본 IPv4 라우트는 0개였다. Linux 시험기의 루프백 전용 조건이 실패해 이 경로에서는 해당 CLI를 실행하지 않았다. 시험 컨테이너를 회수했다.
- 기존 Ubuntu Parallels에서는 별도 네트워크·마운트·PID 네임스페이스, 읽기 전용 루트, 임시 합성 계정, 실행 가능한 전용 tmpfs, 비권한 UID와 유효 capability 0, `NoNewPrivs=1`, 기본 라우트·Docker 소켓 부재, 루프백 응답을 확인한 경로를 사용했다. Gemini `0.71.0`과 Claude `2.1.331`의 실제 허용·정책 거부·훅 오류를 각각 통과했다. Codex `0.167.1`에서는 CLI 단계가 실패했고, macOS 실측과 같은 공식 `0.172.0`을 준비한 뒤 세 경우가 통과했다. Linux 시험에서 정상 훅은 세 CLI에서 차단했고 훅 오류 시 Claude는 대역 실행을 차단했다. Codex·Gemini는 대역을 실행했다. 공식 바이너리와 합성 응답을 사용했고 CLI와 시험 파일을 회수했다. [Linux 시험 조건](TEST_PLAN.md#에이전트-연동-검증)

## 8단계 현재 후보 제품화 검증

- 2026-11-24, DSG-08·WBS 6.1. 브랜치 `feature/20261124-stage8-release-qualification`, 기준 `a6989a3`, Rust 트리 `ada5a09f9d59e00b8c2aa70a5143464d01450f2c`. 현재 후보는 `0.1.1-rc.1`이다. 보조 worktree 생성·회수·잔여는 각각 0개다. 새 시험 스크립트와 문서 변경 경로는 작업 대조표와 테스트 계획에 연결했다. [작업 대조표](TASK.md) · [테스트 계획](TEST_PLAN.md#대시보드와-릴리스-검증)
- macOS ARM64는 `scripts/build-macos-release.py`로 독립 임시 빌드 두 번의 아카이브 SHA-256 일치를 확인했다. `0.1.0` 실물을 설치하고 현재 후보로 업그레이드한 뒤 롤백·재업그레이드·제거했다. 진단, 두 실행 파일의 SHA-256과 실행, 설치 제거 후 사용자 시험 파일 보존을 통과했다. 설치본의 읽기 전용 MCP와 루프백 대시보드 시험도 통과했다. 독립 Docker CLI의 집계와 Compose 라벨 및 제품 스캔을 대조해 컨테이너 7개, 고유 이미지 ID 8개, 명명 볼륨 2개, 중복 그룹 0개가 API 및 WebKit 1060px·390px에 일치함을 확인했다.
- Windows ARM64·x64는 `cargo xwin`으로 `--release --locked --offline --jobs 1` 교차 링크했다. 각 대상의 동일 입력 두 번 패키징, PE 대상 및 호스트 패키지 회귀를 통과했다. 기존 Parallels Windows ARM64의 로컬 NTFS에서 `scripts/check-windows-arm64-release.py`와 기존 패키지·대시보드 시험을 실행했다. 이전 `681be1e` 실물에서 같은 `0.1.1-rc.1` 표기의 현재 ARM64 후보로 교체해 두 실행 파일의 SHA-256·실행, 진단, 대시보드 HTTP, 제거 후 사용자 시험 파일 보존을 확인했다. x64 실행 파일 두 개와 대시보드 HTTP를 에뮬레이션으로 확인했다. 설치기는 ARM64에서 x64 패키지의 새 설치와 기존 ARM64 설치의 업그레이드를 거절했고, 설치 파일과 사용자 파일을 보존했다. 게스트 임시 자료를 회수하고 VM을 원래 일시정지 상태로 복귀시켰다.
- macOS의 `sh scripts/test.sh`와 `sh scripts/test-rust.sh`는 각각 종료 코드 0이었다. Python 14개와 Rust 103개 및 기본 등록 통합 시험을 통과했다.
- 현재 후보와 이전 실물, SHA-256 및 검증 요약은 릴리스 문서에 연결했다. Linux ARM64는 같은 Rust 트리의 기존 Ubuntu 실측을 인용했다. 기존 Windows SCM 및 실제 Docker의 수용 조건은 WBS 5.1에서, 공인 서명은 릴리스 문서에서 추적한다. 승인형 MCP와 Provider SDK는 사용 사례 확정 후 진행한다. [후보와 해시](RELEASE.md) · [Linux 실측](#별도-buildkit-읽기-전용-실측) · [남은 수용 조건](BACKLOG.md#환경승인-조건이-남은-항목)

## Parallels x64 설치기 경계 재검증

- 2026-11-24, DSG-08·WBS 6.1. 브랜치 `feature/20261124-parallels-x64-boundary`, 기준 HEAD `26eba10`. Rust 트리 `ada5a09`의 `0.1.1-rc.1` 후보와 패키지 SHA-256을 그대로 사용했다. 제품과 계획 변경은 0건이다.
- 로컬 Parallels Desktop 27.1.0의 등록 VM은 `win-11`, `ubuntu`, `macOS` 세 개였다. 기존 `win-11` ARM64 게스트에서 공식 포터블 Python 3.13.9의 ARM64판과 에뮬레이션 x64판을 사용했다. 두 프로세스에서 공식 설치기의 x64 새 설치·업그레이드는 각각 종료 코드 1이었다. 새 설치 시 설치 경로에 사용자 시험 파일만 남았고, 업그레이드 거절 뒤 기존 ARM64 설치 포인터와 실행 파일 SHA-256, 사용자 시험 파일이 일치했다.
- ARM64 설치 수명 주기와 x64 설치기 경계 E2E는 종료 코드 0이었다. 시험용 임시 자료를 회수하고 `win-11`을 원래의 일시정지 상태로 복귀시켰다. 신규 VM과 게스트 시험 자료 잔여는 0개다. [시험 방법](TEST_PLAN.md#대시보드와-릴리스-검증)
- 2026-11-24, `feature/20261124-record-parallels-scope`, 기준 HEAD `0eee430`. 기존 통과 근거로 DSG-08·WBS 6.1의 macOS 호스트와 기존 Parallels Ubuntu ARM64·Windows ARM64 게스트에서 수행 가능한 패키지 검증을 종료하고 백로그에 반영했다. 등록된 Windows 게스트가 ARM64이고 Windows x64 OS VM이 없어 x64 OS에서의 설치·업그레이드·제거를 이번 범위에서 제외했다. 공식 출시 기준의 Windows x64 수명 주기는 미검증으로 남는다. 이번 문서 정리에서 VM 실행과 시험 재실행은 0건이다. [범위 제외 기록](BACKLOG.md#환경승인-조건이-남은-항목) · [공식 출시 게이트](TEST_PLAN.md#출시-게이트)

## 전체 8단계 계획과 산출물 재대조

- 2026-11-24, `feature/20261124-plan-evidence-alignment`, 기준 HEAD `9a084ae`, Rust 트리 `ada5a09`. 공식 PLAN·WBS, 실행 계획과 착수 실측을 대조했다. WBS 14개와 실행 8단계의 산출물·시험·남은 기준을 연결했다. 변경은 작업 대조표, 백로그, 핸드오프, 요구사항과 시험 계획의 현황, 착수 분석의 참조 및 패키지 매니페스트에 적용했다. 승인된 계획과 제품 코드를 유지했다. [작업 대조표](TASK.md) · [백로그](BACKLOG.md)
- macOS에서 `sh scripts/test.sh`로 Python 14개, `DSG_TEST_DOCKER_CONTEXT=dockerguard-test DSG_TEST_BUILDX=1 DSG_TEST_WEBKIT=1 sh scripts/test-rust.sh`로 Rust 103개와 등록 통합 시험을 재실행했다. 둘 다 종료 코드 0이다. 지정 엔진의 컨테이너 7개, 고유 이미지 ID 8개, 명명 볼륨 2개, 중복 그룹 0개를 확인했다. 내장 빌더 208개 중 활성 0개, 기본 Buildx 208개 중 회수 가능 표시 208개·공유 8개·변경 가능 167개를 확인했고 모두 보존했다. 이미지 계획은 보존을 판정했고 WebKit 1060px·390px 검증도 통과했다. [시험 조건](TEST_PLAN.md#rust-foundation-검증) · [대시보드 시험](TEST_PLAN.md#대시보드와-릴리스-검증)
- Rust 트리 `ada5a09`의 `0.1.1-rc.1` 보존 후보 4개는 대상·버전과 아카이브·구성 파일 SHA-256이 각 배포 매니페스트, SHA-256 파일, 검증 요약 및 릴리스 기록과 일치했다. 플랫폼별 설치 수명 주기와 실제 CLI·서비스 근거는 각 후보·환경의 기존 실측을 적용했다. 이번 회귀에서 VM 또는 Docker 데몬 시작과 실제 Docker 자원 변경은 0건이다. 보조 워크트리 생성·회수·잔여는 각각 0개다. [배포 파일과 검증 범위](RELEASE.md)
- 재개 시 Compose 구성 자체 정규화·재빌드 필요 판정과 캐시 소유권 검증을 해당 WBS의 선행 작업과 대조한다. 이미지 삭제 적용, 그룹 경계, 승인형 확장, 각 OS 운영 실측의 잔여 조건을 유지한다. Windows x64 OS 설치 수명 주기는 기존 Mac·Parallels 범위에서 제외했으며 공식 출시 상태는 미검증이다. [재개 조건과 환경 제외](BACKLOG.md#환경승인-조건이-남은-항목) · [출시 게이트](TEST_PLAN.md#출시-게이트)

## 현재 시스템에서 8단계 재검증

- 2026-11-24, `feature/20261124-verify-current-system-evidence`, 기준 HEAD `3508d07`, Rust 트리 `ada5a09`. WBS 14개 행, 실행 8단계, 요구사항 13개를 제품 진입점과 기존 실측에 다시 연결했다. 변경은 작업 대조표·백로그·핸드오프, 테스트 계획과 요구사항의 현황, 패키지 매니페스트에 적용했다. 공식 PLAN·WBS와 실행 계획의 통과 기준, 제품 코드와 패키지는 유지했다. [작업 대조표](TASK.md)
- macOS의 `sh scripts/test.sh`에서 Python 14개가 통과했다. `sh scripts/test-rust.sh`에 `DSG_TEST_DOCKER_CONTEXT=dockerguard-test`와 `DSG_TEST_BUILDX`·`DSG_TEST_WEBKIT`·`DSG_TEST_BUILD_PROVIDERS`·`DSG_TEST_LAUNCHD`·`DSG_TEST_LIVE_ENFORCEMENT`의 값 `1`, `DSG_TEST_AGENT_NATIVE=all` 및 로컬 Node·Gemini 번들 경로를 지정했다. 종료 코드 0으로 Rust 103개, 등록된 통합 시험과 WebKit 1060px·390px를 통과했다. 현재 사용자 권한에서 임시 모드 000 디렉터리의 열거가 실제 거부됨을 확인했다. Docker 집계는 컨테이너 7개, 고유 이미지 ID 8개, 명명 볼륨 2개, 중복 0건이었다. 빌더 캐시 208개를 보존했고 선택한 기존 이미지 1개의 보존 판정을 확인했다. [시험 절차](TEST_PLAN.md#rust-foundation-검증)
- 등록 작업 6개인 Cargo·Node·pnpm·Gradle·uv·Go를 로컬 도구와 최소 임시 프로젝트에서 다시 실행했다. Playwright·Next.js 확장, 실제 Compose 변경과 Parallels 시험은 앞선 실측을 적용한다. 현재 macOS의 Claude 2.1.331·Codex 0.172.0·Gemini 0.67.0은 합성 모델 응답과 대역 명령으로 9개 사례를 통과했다. 정상 거부에서 대역 실행 0건이었다. 훅 오류 시 Claude는 대역 실행을 막고 Codex·Gemini는 실행했다. 읽기 전용 MCP 회귀도 통과했다. [Provider 근거](TASK.md) · [실제 CLI 시험](TEST_PLAN.md#에이전트-연동-검증)
- 현재 소스에서 임시 GUI launchd 등록, 단일 재시작과 해제를 재검증했다. 동일 UID 샌드박스에서 시험용 Unix 릴레이와 기존 엔진의 루프백 TCP 직접 접근을 차단하고, 브로커 경유 조회와 변경 요청의 상류 전달 0건을 확인했다. 비대화형 관리자 실행 경로는 확인되지 않았다. 현재 후보 아카이브 4개에서 버전·대상과 아카이브 SHA-256이 검증 요약·릴리스 기록과 일치했다. 내부 파일 SHA-256은 패키지 매니페스트와 일치했다. [브로커 시험](TEST_PLAN.md#브로커와-서비스-검증) · [후보와 해시](RELEASE.md)
- 현재 Parallels 인벤토리는 일시정지 2개, 정지 1개였다. 이번 검증에서 VM과 Docker 데몬을 시작한 건수, 실제 Docker 자원을 변경한 건수, 보조 워크트리 생성·회수·잔여는 모두 0이다. 독립 운영 계정, Windows 실제 엔진, Windows x64 운영체제 시험의 제외 이유는 백로그에 기록했다. 재개 항목은 공식 WBS에 연결한 구현·검증 잔여를 선행 순서로 확인한다. [환경 제외와 재개 조건](BACKLOG.md#현재-시스템에서-제외한-검증)

## Compose 캐시 관리형 실행 및 승인 정리 보완

- 2026-11-24, `feature/20261124-compose-cache-managed-recovery`, 기준 HEAD `911ae2d`. 이번 변경의 Rust 트리는 `a44f0ca6f668bf6effb8d48815d20740c9418c34`다. 순서는 WBS 2.2의 Compose 정규화, WBS 2.3의 캐시 판정, WBS 3.2의 자손 추적, WBS 1.4·2.3의 승인 적용, WBS 6.1의 후보 검증이다. 별도 작업 트리 생성·회수·잔여는 각 0개다. [작업 대조표](TASK.md)
- Compose 코어 3건과 합성 Docker CLI 통합 1건을 통과했다. 비민감 서비스 투영에서 동일·재생성 필요·재빌드 필요·미확인을 구분하고, 기존 CLI 계약과 기존 컨테이너의 두 번 확인을 유지했다. Buildx 파서·어댑터 7건을 통과했다. 지정 엔진에서 실제 기본 Buildx 208개를 보존 사유로 분류했고 회수 후보는 0개였다. 별도 Linux ARM64 BuildKit의 현재 변경은 공통 파서로 검증했으며 실제 빌더는 과거 Ubuntu 실측이 근거다. [API 명세](API_SPEC.md) · [시험 조건](TEST_PLAN.md#rust-foundation-검증)
- 러너·레지스트리·macOS 프로세스 식별 22건이 통과했다. macOS의 소유 시험 자손은 원래 그룹을 이탈했다. 정상 리더 종료와 감독자 강제 종료에서 만료된 lease가 자손 생존 중 보존됐고 자손 자진 종료 뒤 재획득 및 회수 감사 각 1건을 확인했다. 시험 DB에서만 만료 시각을 조정했다. 관측 누락이 확인되거나 관측 기록이 중단되면 lease를 보존한다. [검증 방법](TEST_PLAN.md#rust-foundation-검증)
- Docker 이미지·캐시 제어 대역 7건, 레지스트리 17건, 이미지 GC 4건이 통과했다. 등록 프로젝트·정책·엔드포인트·자원 종류와 세대 및 스냅샷을 일회 승인에 결합한다. 적용 중 배타 슬롯 확보와 단일 호출을 확인했다. 임시 SQLite 대역에서 의도와 변경·영수증 커밋 후 중단을 재현했다. 동일 영수증은 읽기 전용으로 정산하고 근거 부재는 복구 필요 상태로 보존했다. 공개 Docker 이미지 계획과 재확인의 적용 상태는 제어권 미확인 보존이다. [대역 경계](TEST_PLAN.md#rust-foundation-검증)
- macOS 통합 회귀에서 Python 14건, Rust 117건, 위 자손 시험, 등록 작업 6건, 실제 CLI 훅 9건, 임시 GUI launchd, 기존 엔진의 조회 경계와 WebKit 1060px·390px가 통과했다. 기존 지정 엔진 집계는 컨테이너 7개, 고유 이미지 8개, 명명 볼륨 2개, 중복 0건이었다. VM·Docker 데몬 시작과 실제 Docker 자원 변경은 0건이다. [테스트 계획](TEST_PLAN.md)
- 새 macOS ARM64 `0.1.1-rc.1` 후보는 독립 빌드 두 번의 아카이브 SHA-256이 `67c1fb2045dba5709c4e1b4858c5cb9c922732fedc53b5f3f32cccd50f29c7f4`로 일치했다. 이전 `0.1.0` 실물 설치, 업그레이드, 롤백, 재업그레이드, 제거 및 사용자 파일 보존을 통과했다. 설치본의 두 바이너리 실행, Docker 집계와 API·WebKit 1060px·390px도 통과했다. 아카이브·해시·시험 요약은 `dist/dsg-macos-qualification-a44f0ca/`에 보존했다. Linux·Windows 후보는 기존 실측 소스에 해당한다. [후보 증거](RELEASE.md)
- 남은 항목은 증명된 프로젝트별 전용 제어기에 운영 적용을 연결하는 일과 실제 운영 적용 검증이다. 전체 Compose 설정 해석, 관측 사이에 이탈한 프로세스, 새 소스의 Linux BuildKit 네이티브 확인은 별도다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## 전용 Docker 엔진 단일 적용 준비

- 2026-11-24, DSG-05·WBS 1.4·2.3, `feature/20261124-dedicated-docker-approval`, 실측 기록 기준 HEAD `dc568d3`. 기존 Ubuntu Parallels ARM64의 관리자 권한, cgroup v2, 필수 도구를 확인했다. 사전 프로브는 최초 인자 전달에서 종료 코드 2를 반환했고 표준입력 전달로 재시도해 통과했다. VM을 재개해 전용 임시 Docker 데몬과 containerd를 실제 시작했다. 합산 CPU 1코어·메모리 512 MiB·PID 128개·실행 120초와 독립 tmpfs 128 MiB·inode 4,096개 상한을 확인했다. [사전 계약](TEST_PLAN.md#전용-docker-엔진-단일-이미지-적용-사전-계약)
- Unix 소켓 상대 프로세스 확인, `nobody`의 `EACCES`, 외부 마운트 네임스페이스에서 소켓 숨김, TCP 리스너 0개를 확인했다. 초기 무자원 상태에서 태그와 레이어가 없는 합성 이미지 1개를 적재하고 전체 이미지 ID와 출처 해시를 대조했다. 삭제 요청은 0건이다. 엔진이 시험 중 활성 상태임을 확인한 뒤 VM을 원래 일시정지 상태로 복귀시켰다. 엔진 지문·세대·스냅샷·전체 이미지 ID와 검증 수치는 증거에 보존했다. [실측 증거](../dist/dsg-dedicated-engine-cf83a703d2fd/PREAPPROVAL.json)
- 같은 등록부의 미확정 이미지·캐시 의도를 엔드포인트·종류·전체 ID로 찾아 계획 저장·승인·적용 의도 커밋에서 차단한다. 뒤늦게 확인된 영수증으로 기존 의도를 정산한 뒤에만 같은 대상의 새 적용을 허용한다. 제어 대역 8건, 등록부 17건, 이미지 계획 4건을 통과했다. macOS Python 14건, Rust 118건과 등록 통합 시험에서 기존 Docker·Buildx 조회 및 선택 이미지 보존이 통과했다. 별도 생성한 작업 트리는 0개다.
## 승인 후 전용 Docker 엔진 단일 이미지 적용

- 2026-11-24, DSG-05·WBS 1.4·2.3, `feature/20261124-dedicated-docker-approval`, 기준 HEAD `75acf97`과 전용 시험 보조 코드. 승인된 전용 Moby 엔진에서 사전 기록과 엔진·세대·스냅샷·전체 이미지 ID가 일치했다. 적용 전 독립 관측의 삭제 요청은 0건이었다. 기본 실행에서 제외된 Rust 시험 `controlled_docker_live_approved_receipt` 1건의 명시 실행은 종료 코드 0, 기존 Docker GC 제어 대역 시험은 8건 통과다. [전용 게스트 실행기](../scripts/check-dedicated-docker-apply.py) · [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs) · [실측 집계](../dist/dsg-dedicated-engine-cf83a703d2fd/LIVE_RESULT.json)
- 실제 삭제 1회로 합성 이미지 수가 1개에서 0개로 줄었다. 강제 삭제와 prune은 사용하지 않았고 기록한 회수량은 0 B다. 물리 디스크 회수량은 미계측이다. 종료 후 시험 서비스가 비활성화됐고 전용 임시 디렉터리가 회수됐으며 Ubuntu Parallels ARM64가 원래 일시정지 상태로 복귀했다. 보조 워크트리 생성·회수·잔여는 각각 0개다. [실측 집계](../dist/dsg-dedicated-engine-cf83a703d2fd/LIVE_RESULT.json)
- 정산 전 중단은 Rust 시험 코드에서 모사했다. SQLite 등록부 재열기 후 보존 영수증으로 정산해 `GC_APPLY_INTENT`·`GC_APPLIED` 각 1건, 재적용·재정산 거절, 열린 lease 0건 및 `quick_check=ok`를 확인했다. 호스트 등록부의 합성 소유자와 실제 게스트 검증 소유자 지문의 대응은 시험 전용이다. [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs) · [실측 집계](../dist/dsg-dedicated-engine-cf83a703d2fd/LIVE_RESULT.json)
- 재개 시 운영 프로젝트 소유권과 독점 제어를 확인하고 공개 운영 어댑터 연결과 별도 대상 승인을 검증한다. 일반 공유 Docker 영역과 실제 캐시 적용, 삭제 응답 유실과 전원 장애 복구는 각각 별도 범위다. 동일 시험을 다시 수행할 때 유효 기간이 끝났거나 세대가 바뀌면 재실측과 새 승인이 필요하다. [시험 계약](TEST_PLAN.md#전용-docker-엔진-단일-이미지-적용-사전-계약) · [현재 백로그](BACKLOG.md#환경승인-조건이-남은-항목)
- 같은 수정에서 자동 회귀 Python 14건, Rust 118건과 기본 통합 검증을 통과했다. 실제 삭제 시험은 자동 회귀에서 제외된 1건으로, 승인 대상의 수동 지정 실행에서 한 번 통과했다. 의도 없이 적용 모드를 호출한 오프라인 게이트는 게스트 접근 전에 거절했다. [테스트 계획](TEST_PLAN.md#전용-docker-엔진-단일-이미지-적용-사전-계약)

## 2026-11-24 · 전용 Docker 영수증 유실과 BuildKit 네이티브 조회

- DSG-03·05·07 및 WBS 1.4·2.3·5.1. 작업 브랜치는 `feature/20261124-docker-receipt-loss-buildkit`, 기준 HEAD는 `e8b0e84`다. [작업 대조표](TASK.md) · [환경과 승인 조건](BACKLOG.md#환경승인-조건이-남은-항목)
- WBS 1.4는 실제 전용 Moby의 단일 DELETE 성공 응답 확인과 관측 기록 동기화 뒤, 영수증 기록 전 중단을 주입하는 시험을 추가했다. 기존 Ubuntu Parallels에서 독립 저장소·전용 소켓과 제한 계정의 접근 거부를 검증했다. 초기 무자원 엔진에 합성 이미지 1개를 준비하고 전체 이미지 ID와 증거 해시, 삭제 요청 0건을 확인했다. VM은 원래 일시정지 상태로 복귀했고 당시 승인 대기 중에는 120초 상한이 있는 격리 시험만 보존했다. 증거 불일치·승인 부재는 게스트 접근 전 거절했다. [유실 시험 계약](TEST_PLAN.md#전용-docker-엔진-삭제-응답-확인-후-영수증-유실-시험) · [사전 증거](../dist/dsg-dedicated-engine-93292d9f465f/PREAPPROVAL.json)
- WBS 2.3의 전용 BuildKit v0.35.1을 기존 Ubuntu Parallels ARM64에서 실제 실행했다. 현재 제품과 독립 공식 조회의 빈 집계 0개 및 시험 빌드 후 집계 4개가 일치했다. 4개는 모두 변경 가능이며 보존했고 회수 후보는 0개다. 잘못된 PID·공식 도구 해시·소켓 링크는 거절했고 별도 prune은 0회다. 읽기 전용 게스트 제약 아래 BuildKit 추적 소켓과 제품 임시 HOME의 경로를 전용 tmpfs에 배치해 재실행했다. 데몬·시험 저장소와 임시 경로 회수 및 VM의 일시정지 복귀를 확인했다. [네이티브 시험 계약](TEST_PLAN.md#현재-제품의-linux-arm64-별도-buildkit-네이티브-조회) · [실측 집계](../dist/dsg-buildkit-native-1a31748b483d/QUALIFICATION.json)
- WBS 5.1 연결은 현재 Linux ARM64 제품과 임시 root Unix 프로토콜 대역으로 검증했다. 전용 선언 1건은 통과했고 공유 선언과 다른 PID 2건은 거절했다. 변경 요청은 0건이었다. 운영 적용에는 권한 제어기의 신뢰, 요청별 승인, 프로젝트 자원의 출처·소유권, 배타 변경 경계와 프로젝트 소유 캐시의 실제 적용 증거가 필요하다. [프로브 계약](API_SPEC.md#로컬-docker-제어-서비스-읽기-확인) · [실측 집계](../dist/dsg-buildkit-native-1a31748b483d/QUALIFICATION.json)
- macOS에서 Python 14건, Rust 118건 및 통합 회귀가 통과했다. 새 영수증 유실 시험을 포함한 전용 엔진 수동 시험 2건은 자동 회귀에서 제외했다. 보조 워크트리 생성·회수·잔여는 각각 0개다.

## 승인 후 전용 Docker 엔진 영수증 기록 전 중단 검증

- DSG-05·WBS 1.4, 브랜치 `feature/20261124-docker-receipt-loss-buildkit`, 기준 HEAD `8a2ecfa`. 승인된 사전 증거 해시와 엔진·세대·스냅샷·전체 이미지 ID를 독립 관측으로 재확인했다. 적용 직전 삭제 요청은 0건이었다. [사전 증거](../dist/dsg-dedicated-engine-93292d9f465f/PREAPPROVAL.json)
- 실제 전용 Moby에 요청 1건을 보내 HTTP 200과 합성 이미지 1→0을 확인했다. Rust 시험에서 응답 관측 기록을 동기화한 후 영수증 기록 직전 중단을 주입했다. 등록부 재열기 후 `recovery_required`, 회수량 0 B, 영수증 부재를 확인했다. 복구 중 로컬 영수증 조회 1건과 추가 게스트 관측·변경 각 0건을 검증했고, 기존 승인 재사용과 경쟁 승인 적용을 거절했다. 지정 실측 1건과 Docker GC 적용 회귀 8건이 통과했다. [Rust 시험](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/live_fixture.rs) · [실측 집계](../dist/dsg-dedicated-engine-93292d9f465f/RECEIPT_LOSS_RESULT.json)
- 실행기가 표준출력의 결과 표식을 수집하지 못해 보존한 응답 관측 기록과 SQLite 등록부를 읽기 전용으로 재검증했다. `quick_check=ok`, 열린 lease 0건, `GC_APPLY_INTENT`·`GC_RECOVERY_REQUIRED` 각 1건과 `GC_APPLIED` 0건이었다. 결과를 수집하는 동안 Docker 호출은 없었다. 전용 시험 서비스 정지, 전용 임시 디렉터리 회수 및 VM의 일시정지 복귀를 확인했다. 보조 워크트리 생성·회수·잔여는 각각 0개다.
- 다음은 운영 제어기의 배포 출처와 승인·실행 권한을 기록한 뒤 전용 엔진 및 프로젝트 소유 캐시에서 읽기 전용 소유권·독점 변경 경계를 확인하는 작업이다. 캐시의 실제 회수는 대상 증거와 별도 승인이 필요하다. DELETE 응답 미수신과 전원 장애 시나리오도 별도 검증으로 남는다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## 2026-11-24 · Docker 제어 경계와 복구 재개

- DSG-03·05·07, WBS 1.4·2.3·5.1, 브랜치 `feature/20261124-docker-control-recovery`, 기준 HEAD `a1ba1e6`. 보조 워크트리 생성·회수·잔여는 각각 0개다. [작업 대조표](TASK.md)
- WBS 1.4의 macOS 회귀에서 캐시 제어 대역의 소유 자식을 정산 전에 실제 SIGKILL로 종료했다. 영수증 유무의 각 경로에서 새 SQLite 연결과 lease로 복구했으며 경쟁 적용과 대상 재호출을 차단했다. 실제 호스트 전원 장애는 별도다. [회귀 계약](TEST_PLAN.md#정산-전-자식-프로세스-강제-종료-회귀)
- 전용 Docker 응답 미수신 시험은 `SHUT_RD`로 수신을 차단한 뒤 응답 0바이트와 독립 연결의 대상 삭제 효과를 대조한다. 이전 실측은 HTTP 200을 수신했다. 초기 무자원 Moby에 새 합성 이미지 1개를 올려 전체 ID·엔진·세대·스냅샷을 확인했다. 삭제 요청은 0건이며 VM은 일시정지 상태로 돌아갔다. 새 정확 증거에 대한 별도 승인과 수동 시험이 남았다. [사전 증거](../dist/dsg-dedicated-engine-1bc6e90aa15d/PREAPPROVAL.json) [시험 계약](TEST_PLAN.md#전용-docker-엔진-삭제-응답-미수신-시험)
- WBS 2.3은 Ubuntu Parallels의 전용 BuildKit v0.35.1에서 같은 워커·소켓 세대의 전체 집계 4→5와 기존 4개 레코드의 불변을 확인했다. 독립 OCI 산출물과 새 ID `gwblmsybluazy85n7e72b8pj3`를 대조했다. 공식 정확 ID 조회는 6,144 B의 미사용·비공유·불변·무참조 레코드 1개만 반환했다. 표준 prune의 이미지 lease 취소·워커 GC·미참조 캐시 메타데이터 정리 범위를 고려해 요청 0건, 보존으로 판정했다. 데몬·임시 저장소를 정리했으며 이 ID는 종료된 시험의 증거다. [BuildKit 실측 집계](../dist/dsg-buildkit-native-69f9692b4210/QUALIFICATION.json) [네이티브 시험 계약](TEST_PLAN.md#현재-제품의-linux-arm64-별도-buildkit-네이티브-조회)
- WBS 5.1의 보호된 배포 확인은 현재 제품과 Ubuntu Parallels의 실제 전용 Moby에서 시험했다. 제어기와 구분한 두 임시 비root 역할의 피어·실행 파일·소켓·세대가 일치했다. 같은 UID·틀린 역할 UID·엔진 PID·세대·실행 파일 해시·쓰기 가능한 정책을 거절했다. 두 역할의 직접 엔진 연결도 각각 거절됐고 TCP 리스너와 변경 요청은 0건이었다. 게스트 시험 바이너리 실행은 검증한 ELF의 봉인 memfd로 해결했다. 임시 역할과 전송 파일을 정리했다. [배포 확인 실측](../dist/dsg-dedicated-engine-1bc6e90aa15d/CONTROLLER_ADMISSION_RECHECK.json) 다음 운영 적용의 선행 조건은 권한 제어기의 신뢰 가능한 배포 출처, 프로젝트 자원별 출처·소유권, 요청별 승인, 배타 변경과 공개 쓰기 어댑터 검증이다. [명령 계약](API_SPEC.md#보호된-docker-제어기-배포-확인)
- macOS의 Python 14건, Rust 119건과 통합 회귀가 통과했다. 수동 전용 엔진 시험은 기본 실행에서 3건 제외했다. 새 시험의 빈 입력과 이전에 소비한 사전 증거는 VM 접속 전에 거절했다.
