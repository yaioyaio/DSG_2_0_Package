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

## 승인 후 전용 Docker 응답 미수신 실측

- DSG-05·07, WBS 1.4·2.3·5.1, 브랜치 `feature/20261124-docker-control-recovery`, 기준 HEAD `eec330b`. 보호된 전용 엔진의 승인 증거와 같은 세대·스냅샷·전체 이미지 ID를 대조했다. 초기 기록은 적용 직전 `preserved`였고 삭제 전송·시도 표지가 없었다. 재개 원장은 프로젝트 등록만 기록한 상태에서 한 차례 멈췄다. 독립 조회 5건에서 전송 오류 2건과 일치 3건을 확인했고 이미지와 원본 기록을 보존했다. [전송 전 보존 원장](../dist/dsg-dedicated-engine-1bc6e90aa15d/UNSENT_PREFLIGHT_RESULT.json) · [전송 재확인](../dist/dsg-dedicated-engine-1bc6e90aa15d/RESPONSE_LOSS_PREFLIGHT_RECHECK.json)
- 배타 잠금 아래 원본 원장을 읽기 전용으로 재검증하고, 동일 대상과 등록만 된 재개 원장을 사용했다. 읽기 전용 관찰에만 전송 오류의 제한된 재시도를 적용했다. 실제 요청은 1건이고 클라이언트가 읽은 응답은 0바이트였다. 독립 조회는 합성 이미지 1→0을 확인했다. 재개 등록부는 `recovery_required`, 회수량 0 B, 영수증 조회 1건, 추가 게스트 조회·변경 각 0건으로 기록했다. 기존·경쟁 승인 적용을 거절했다. 지정 시험 1건과 GC 적용 회귀 9건이 통과했다. [응답 미수신 실측 집계](../dist/dsg-dedicated-engine-1bc6e90aa15d/RESPONSE_LOSS_RESULT.json)
- 독립 SQLite 검사에서 원본과 재개 원장 모두 `quick_check=ok`이고 `GC_APPLIED`는 각각 0건이었다. 초기 실패 원장의 lease 기록 1건은 감사 자료로 보존했고 재개 원장에 열린 lease는 0건이다. 전용 시험 서비스를 정지하고 임시 저장소를 회수했다. Ubuntu VM은 일시정지 상태로 돌아왔다. 보조 워크트리 생성·회수·잔여는 각각 0개다.
- 코어 쓰기 요청은 보호된 정책, 커널 연결·메시지 자격과 송신 pidfd, 제어기·엔진·역할 세대 및 정확한 요청 범위를 대조한다. Ubuntu ARM64의 독립 격리 시험에서 기존 4건과 신규 6건을 통과했다. 운영 어댑터에 연결한 변경 요청과 실제 캐시 회수는 0건이다. [Linux 권한 확인 시험](../dist/dsg-auth-gate-c95a42aa0b72/QUALIFICATION.json)
- 최신 macOS에서 Python 14건, Rust 119건과 통합 회귀가 통과했다. 수동 전용 엔진 시험 3건은 기본 실행에서 제외했다.
- 다음은 운영 어댑터에서 요청 검증과 SQLite 승인 소비·적용 의도·영수증을 같은 배타 경계에 연결하는 작업이다. BuildKit 단일 레코드 회수는 별칭·솔버·lease의 비대상 변경까지 차단하는 경로와 새 승인 대상이 필요하다. 실제 호스트 전원 장애 시험도 운영 자료를 분리한 검증 환경이 필요하다. [작업 대조표](TASK.md) · [테스트 계획](TEST_PLAN.md)

## 전용 Docker 변경 어댑터와 WAL 재생

- DSG-05·07, WBS 1.4·2.3·5.1, 브랜치 `feature/20261124-docker-exclusive-adapter`, 기준 HEAD `1acb9e5`. 보조 워크트리 생성·회수·잔여는 각각 0개다. [작업 대조표](TASK.md)
- macOS의 합성 SQLite 등록부에서 승인 소비와 적용 의도를 커밋하는 실제 WAL을 복제했다. WAL 부재 및 마지막 프레임 소실은 소비·의도 부재였고 완전 WAL은 둘을 함께 재생했다. 새 lease로 복구하면서 변경 호출 0건과 `recovery_required`, 회수량 0 B를 확인했다. 물리 호스트와 기존 VM에 전원 차단을 가하지 않았다. [macOS WAL 재생](../dist/dsg-wal-qualification-1acb9e5/QUALIFICATION.json)
- Linux ARM64 어댑터는 커널이 검증한 요청, 보호 등록부, 동일 프로젝트의 활성 lease를 받는다. 배타 슬롯 안에서 현재 정책·피어·대상을 재확인하고 승인 소비와 적용 의도를 커밋한 뒤에만 변경을 요청한다. 같은 엔진의 소켓·세대·정책이 바뀌어도 미확정 의도를 유지하며 현재 실행자에게 정확한 저장 영수증의 조회만 허용한다. [코어 어댑터](../rust-scaffold/crates/dsg-core/src/docker_gc_apply/dispatch.rs)
- Ubuntu Parallels ARM64의 전용 격리 환경에서 새 어댑터 3건과 커널·GC·복구 회귀 20건을 통과했다. 변경 대역은 별도 SQLite 연결로 선행 의도 커밋과 타 프로젝트 슬롯 획득 거절을 확인했다. 권한 변경 시 무변경, 승인 재사용 거절, 배포 회전 뒤의 정확 영수증 정산을 확인했다. 실제 Docker·BuildKit 변경 요청은 0건이다. 임시 자료 회수와 VM 일시정지 복귀를 확인했다. [Ubuntu 격리 실측](../dist/dsg-exclusive-dispatch-44a3dedceecb/QUALIFICATION.json)
- macOS Python 14건, Rust 120건 및 통합 회귀를 통과했다. Linux ARM64 운영 코어의 교차 컴파일도 통과했다. 다음은 배포 출처와 프로젝트 자원 소유권을 입증한 전용 변경 백엔드를 연결하는 작업이다. v0.35.1 공개 BuildKit API의 `Prune`은 비대상에도 영향을 줄 수 있어, 단일 레코드 회수는 좁은 변경 경로와 새 대상 승인이 필요하다. 실제 호스트 전원 장애와 운영 어댑터·실제 캐시 회수는 미완료다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목) · [시험 계획](TEST_PLAN.md)

## 전용 Docker 백엔드 소유권 사전 검증

- 2026-11-25, DSG-03·05·07 및 WBS 1.4·2.3·5.1, `feature/20261125-docker-backend-ownership`, 기준 HEAD `a136f2d`. 보조 워크트리 생성·회수·잔여는 각각 0개다. [작업 대조표](TASK.md)
- Ubuntu Parallels의 전용 Moby, containerd 및 별도 임시 승인자·실행자·제어기 피어를 실행했다. 빈 전용 저장소에 합성 이미지 한 건을 적재했다. 보호된 배포 정책 원본 해시와 프로젝트 등록부 지문·전체 ID, 커널 피어의 실행 파일과 세대, 제어기가 연 저장소 디렉터리를 대조했다. 독립 Moby 조회의 데이터 경로도 일치했다. 현재 엔진의 지정 소켓 리스너 한 개와 TCP 리스너 0개를 확인했고, 비특권 두 역할의 직접 연결을 거절했다. 불일치 9건 거절, SQLite 등록부 불변, 이미지 1→1, 변경 요청 0건을 확인했다. [Moby 실측](../dist/dsg-backend-ownership-5fbb83f5a031/QUALIFICATION.json)
- 별도 관측에서 데몬이 보유한 Unix 리스너 세 곳을 분류했다. 대상 API 소켓 한 곳과 경로상 metrics·libnetwork 내부 소켓 두 곳이 모두 격리된 root 전용 저장소 안에 있었다. 이 관측은 시험 엔진의 외부 접근 범위를 확인한다. 엔진 내부 변경의 직렬화와 영속 정산은 추가 검증이 필요하다. [리스너 실측](../dist/dsg-backend-sockets-b151d4d4d829/QUALIFICATION.json)
- Parallels 스크립트 전달 두 건이 종료 255로 중단됐다. 검사된 압축 파일 전달과 봉인한 실행 메모리로 바꿔 실측을 통과했다. 전용 서비스·tmpfs·전송 파일을 회수하고 기존 Ubuntu VM을 일시정지로 복귀시켰다. macOS의 Python 14건, Rust 120건과 통합 회귀, Ubuntu ARM64의 소유권 단위 시험 3건을 통과했다. [Linux 단위 실측](../dist/dsg-backend-unit-2b0490cc66b4/QUALIFICATION.json)
- 제품에는 읽기 전용 점검을 연결했고 운영 변경 게이트는 보존 상태를 유지한다. 운영 쓰기에는 엔진 내부에서 같은 배타 경계로 보호되는 변경과 영속 영수증을 검증해야 한다. BuildKit 실제 회수 요청은 0건이다. v0.35.1 공개 API의 `Prune`은 비대상 lease·워커 GC·솔버 메타데이터에 영향을 줄 수 있다. 별도 정확 ID API의 개발 범위 승인, 비대상 변경 0건 검증, 새 전체 ID 및 출처 증거에 대한 별도 적용 승인 순으로 재개한다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목) · [BuildKit 시험 계약](TEST_PLAN.md#현재-제품의-linux-arm64-별도-buildkit-네이티브-조회)
- 실제 물리 호스트 전원 장애는 전용 물리 시험기와 독립 저장장치를 확인하지 못해 실행을 보류했다. 이전 합성 WAL 세 경우의 근거를 보존한다. [WAL 재생 실측](../dist/dsg-wal-qualification-1acb9e5/QUALIFICATION.json)

## 운영 변경 경계와 BuildKit 단일 ID 경로

- 2026-11-25, DSG-03·05·07 및 WBS 1.4·2.3·5.1. `feature/20261125-exact-docker-reclaim`, 기준 HEAD `515ee81`. 사용자 승인 범위는 운영 어댑터의 승인 소비·대상 변경·영수증 복구 연결, BuildKit 단일 ID API 개발, 외부 영향 없는 격리 시험이다. 실제 캐시 회수는 현재 세대의 새 대상 출처·전체 ID·증거를 제출하고 별도 승인을 받은 경우에만 실행한다. [WBS](WBS.md) · [시험 기준](TEST_PLAN.md)
- Linux ARM64 Rust 디스패치는 보호된 프로젝트·자원 배정을 확인하고, 백엔드가 확보한 쓰기 잠금 안에서 SQLite 승인 소비와 적용 의도 커밋을 진행한다. 합성 SQLite 백엔드에서는 독립 연결의 쓰기 경쟁을 차단했고, 응답 유실 뒤 등록부와 백엔드를 다시 열어 정확한 영수증으로 정산했다. 추가 변경은 0건이다. 마지막 배정을 회수한 뒤에도 같은 프로젝트·저장소의 이전 영수증만 조회할 수 있게 검증했다. 관련 Ubuntu Parallels 회귀 29건, macOS Python 14건·Rust 120건 및 통합 회귀를 통과했다. [Rust Linux 실측](../dist/dsg-exact-rust-5059ce755957/QUALIFICATION.json)
- 공식 BuildKit v0.35.1 커밋에 적용하는 Go 오버레이를 검증했다. 별도 합성 bbolt 시험은 대상 메타데이터 1→0과 승인 소비·영수증의 동시 커밋을 확인했다. 같은 워커의 비대상과 다른 워커의 동일 ID 변경은 각각 0건이었다. 만료·재사용·참조가 생긴 대상과 실패한 영수증 기록을 보존하고, 정산 재시도는 기존 영수증만 읽었다. macOS에서 최상위 시험 5건과 그 하위 24건을 통과했다. [macOS BuildKit 실측](../dist/dsg-exact-buildkit-macos-v0351/QUALIFICATION.json)
- Ubuntu Parallels ARM64에서 합성 회귀 5건과 공식 네이티브 snapshotter 통합 시험 1건을 통과했다. 두 독립 저장소에서 별칭, 스냅샷, lease와 비대상 데이터베이스의 불변을 확인했다. 표준 워커의 적용 요청은 게이트에서 거절됐고 엔진 변경과 `Prune` 호출은 각각 0건이다. 실운영 Docker·BuildKit 데몬은 시작하지 않았다. 시험 유닛은 종료됐고, 최초 정리 래퍼 실패 뒤 검증한 전송 파일과 빈 임시 디렉터리를 회수했다. VM은 일시정지로 복귀했고 보조 워크트리 생성·회수·잔여는 각 0개다. [Ubuntu BuildKit 실측](../dist/dsg-buildkit-exact-native-be0e2c1ab945/QUALIFICATION.json)
- 운영 쓰기 연결은 엔진 내부 제어기, 솔버 및 containerd의 참조·저장소 작성자가 공유하는 변경 경계와 내구 영수증을 갖춰야 한다. 실제 backing을 가진 BuildKit 캐시의 논리적 제거와 물리 할당 회수량은 미검증이다. 새 대상 승인은 독립 영역과 비대상 무변경을 검증한 후 새 자원의 출처·전체 ID·증거 해시로 받는다. 물리 호스트 전원 장애는 격리된 시험기와 저장장치가 필요하다. [남은 수용 기준](BACKLOG.md#환경승인-조건이-남은-항목)

## 전용 BuildKit 워커 공통 변경 잠금

- 2026-11-25, DSG-03·05·07 및 WBS 1.4·2.3·5.1, `feature/20261125-dedicated-worker-fence`, 기준 HEAD `803aff3`. 현재 승인 범위는 보호된 전용 워커의 공통 잠금 연결, 비대상 불변과 중단 복구 검증, 새 실제 대상의 읽기 검증이다. 실제 캐시 변경은 통과한 새 증거에 대해 별도 승인을 받는다. [작업 대조표](TASK.md) · [시험 계획](TEST_PLAN.md)
- 공식 v0.35.1 소스에서 빌드한 전용 OCI/native 데몬을 Ubuntu Parallels ARM64에서 실제 실행했다. 보호된 디렉터리 잠금이 선점되면 초기화 전에 거절하고, 실행 중에는 유지보수와 오프라인 점검을 막았다. 다른 저장소는 잠글 수 있었고 데몬 종료 후 같은 inode를 재획득했다. 등록 저장소의 전용 플래그 없는 시작을 거절했다. [Ubuntu 실측](../dist/dsg-dedicated-worker-dbae7c58d25d/QUALIFICATION.json)
- 합성 bbolt에서 커밋 직전·직후 자식 프로세스를 실제 종료했다. 직전에는 대상·미소비 승인이 보존되고 영수증이 없었다. 직후에는 합성 대상 제거·승인 소비·영수증이 함께 복구됐으며 재적용은 거절됐다. 같은 워커의 비대상과 독립 워커의 동일 ID 변경은 각각 0건이다. macOS Python 14건과 Go 최상위 7건·하위 26건, Ubuntu Go 최상위 9건을 통과했다. [macOS 실측](../dist/dsg-dedicated-worker-macos-803aff3/QUALIFICATION.json) · [장애 시험](../integrations/buildkit-exact/cache/dsg_worker_boundary_test.go)
- 새 빈 워커에서 버전 고정 scratch LLB와 독립 OCI 계층으로 합성 캐시 출처를 확인했다. 공식 정확 ID 조회와 오프라인 레코드가 `n2jrxpx0117w6as6s9jx2euly`로 일치했다. 오프라인 조회 앞뒤 DB 4개가 그대로였다. 증거 SHA-256은 `77e32b77cb4c4b70dc176b093aafa98facaa76900768d655329229dc9467d427`다. 네이티브 스냅샷·blob 결합이 있어 보존으로 판정했고 실제 삭제와 prune 요청은 0건이다. 시험 저장소는 회수했고 VM은 일시정지로 돌아갔다. 이 종료된 용도의 ID로 실제 승인을 요청하지 않는다. [정확 ID 증거](../dist/dsg-dedicated-worker-dbae7c58d25d/QUALIFICATION.json)
- 네이티브 시험에서 확인한 빈 별칭 표지를 공식 저장 형식에 맞춰 처리했다. 전송 종료 255는 해시를 확인하는 분할 전송으로 복구했다. 다음은 잔존 실행기·마운트 및 독립 작성자 배제, 단일 대상의 메타데이터·솔버·lease·snapshot·blob 양방향 참조와 다중 저장소 중단 복구를 검증하는 일이다. 이를 통과한 새 전체 ID의 출처와 증거 해시에 실제 회수 승인을 별도로 받는다. 물리 전원 장애는 전용 물리 시험기 대기다. 보조 워크트리 생성·회수·잔여는 각각 0개다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## 전용 BuildKit 오프라인 정지와 다중 저장소 복구

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-offline-recovery`, 기준 HEAD `864eb51`. 승인 범위는 전용 워커의 정지 경계 실측, 정확 ID의 다중 저장소 읽기 증거, 합성 단일 대상 변경·중단 복구와 격리 물리 시험기 확인이다. 실제 네이티브 삭제는 사전 조건을 통과한 새 대상의 증거에 별도 승인이 필요하다. [작업 대조표](TASK.md)
- Ubuntu Parallels ARM64에서 별도 PID 네임스페이스의 실제 전용 데몬과 독립 ext4 loop 저장소를 검증했다. 원래 PID 네임스페이스 기동, 쓰기 가능 마운트, 외부 열린 FD, 추가 bind 마운트를 거절했다. 데몬 종료 후 읽기 전용 superblock에서 root의 쓰기 열기 실패와 네 정지 검사 통과를 확인했다. 조회 전후 DB 4개가 일치했다. loop·임시 파일을 회수했고 VM은 일시정지로 돌아갔다. [Ubuntu 실측](../dist/dsg-offline-recovery-d7b73b2013d5/QUALIFICATION.json)
- 새 합성 scratch LLB의 OCI 계층과 전체 ID `stlu53f1u0bnm67p0bnnx6clm`를 대조했다. 증거 SHA-256은 `37d0df256dd9223d6cd9be5362c806cc11760f9633d33c85370f7e9060e57488`다. 읽기 전용 직접 참조에서 솔버 결과 1개, blob에 연결된 lease 2개, 네이티브 스냅샷 자식 1개를 확인했다. 전이·GC 라벨·외부 참조와 물리 내용이 미검증이고 시험 세대를 정리했으므로 이 ID를 보존 실측으로 기록한다. 실제 삭제와 prune 요청은 각 0건이다. [전체 ID와 참조 실측](../dist/dsg-offline-recovery-d7b73b2013d5/QUALIFICATION.json)
- 독립된 합성 bbolt 저장소 5개에서 단계 의도 기록 직후, 개별 저장소 효과 직후, 저널 확인 직후를 포함한 6개 지점의 자식 프로세스 종료를 시험했다. 정확 증거로 정산했고 같은 단계 중복 변경과 비대상 변경은 각각 0건이었다. 승인·대상 선택자 경합, 중첩 증거, 만료, 예약 유실도 검사했다. macOS Python 14건과 Go 최상위 13건·하위 50건, Ubuntu Go 최상위 21건이 통과했다. 네이티브 쓰기 연결과 물리 전원 장애는 포함되지 않았다. [macOS 근거](../dist/dsg-offline-recovery-macos-final-864eb51/QUALIFICATION.json) · [복구 시험](../integrations/buildkit-exact/cache/dsg_recovery_test.go)
- 작업 중인 Mac과 Parallels 게스트를 확인했으며 격리된 물리 시험기·독립 저장장치·전원 제어 경로는 대기다. 다음은 전이 및 외부 참조를 포함한 정확 범위 증명과 쓰기 허용 전용 경계의 저장소별 변경·장애 복구 실측이다. 검증 후 활성 새 대상의 전체 ID·출처·증거 해시로 별도 승인을 받는다. 보조 워크트리 생성·회수·잔여는 각각 0개다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## BuildKit 전이 참조와 전용 네이티브 복구

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-native-recovery`, 기준 HEAD `667d98e`. 승인 범위는 솔버·GC 라벨의 전이와 외부 참조 조회, 전용 쓰기 경계의 시험용 객체 변경·장애 복구, 새 활성 대상의 사전 증거다. 실제 네이티브 회수에는 정확 대상을 특정한 별도 승인이 필요하다. [작업 대조표](TASK.md)
- 실제 Ubuntu 읽기 전용 전용 워커에서 정확 ID의 솔버 전이, 지원 GC 라벨 및 외부 참조 조회를 확인했다. 이력 DB를 포함한 5개 DB와 반복 증거가 일치했다. 이미지·컨테이너·ingest·이력의 지원 형식과 불명 형식은 별도 fixture에서 검증했다. [읽기 실측](../dist/dsg-native-recovery-1b54a76ab439/QUALIFICATION.json)
- 새 합성 캐시 ID `48np9t9kj7z2dewgxv3lknprs`의 출처는 독립 OCI 계층으로 검증한 격리 scratch LLB다. 증거 SHA-256은 `065347d32d2397374c3d7161faab190b5ca36d8e12f2da09c2af503d3fe1d3c5`다. 실제 솔버 결과 1개, GC 정방향 라벨 3개, 보존 루트 2개를 확인했다. 캐시 부모·별칭 전이와 네이티브 할당 내용은 미검증으로 남겼다. 이 시험 ID는 보존으로 판정한 뒤 세대를 정리했다. 실제 회수와 prune 요청은 각각 0건이다. [대상 증거](../dist/dsg-native-recovery-1b54a76ab439/QUALIFICATION.json)
- 별도의 쓰기 가능 ext4에서 다섯 저장소에 시험용 실제 변경을 가하고 커밋 직후 각 자식 프로세스를 종료했다. 공통 저널은 솔버 인덱스 보관과 보호 lease를 확인하고 중복 적용을 차단했다. 독립 시험은 네이티브 스냅샷 커밋, 내용 주소 blob 기록, 캐시 정책 retain의 단계 증거를 재개방 후 확인했다. 각 시험의 비대상 변경은 0건이다. 실제 삭제와 전체 회수 영수증은 발생하지 않았다. [쓰기·중단 실측](../dist/dsg-native-recovery-writable-ab1ea3f2191c/QUALIFICATION.json)
- macOS Python 14건, Go 최상위 13건·하위 50건을 통과했다. Ubuntu Go 회귀 27건과 별도 네이티브 시험 3건이 통과했다. 임시 loop와 시험 자료를 회수했으며 Ubuntu VM을 일시정지로 복귀시켰다. 보조 워크트리 생성·회수·잔여는 각 0개다. [macOS 근거](../dist/dsg-native-recovery-macos-667d98e/QUALIFICATION.json)
- 미완료 범위는 스냅샷·blob·캐시의 실제 회수 단계를 공통 저널에 연결하고, 캐시 전이와 실제 할당 관계를 포함한 정확 범위를 확인하는 일이다. 이 경계를 통과한 활성 새 대상에서만 별도 회수 승인을 요청한다. 격리 물리 시험기에 대한 기존 보류 상태도 유지한다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## BuildKit 다섯 단계 네이티브 저널과 할당 검증

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-end-to-end-recovery`, 기준 HEAD `23e0c21`. 남은 캐시 부모·별칭 전이와 네이티브 할당 검증, 한 저널의 다섯 저장소 변경·중단 복구를 이어서 진행한다. 실제 파괴적 회수는 유효한 새 대상의 증거에 대해 별도 승인을 적용한다. [작업 대조표](TASK.md)
- Ubuntu ARM64의 같은 쓰기 가능 ext4 시험 워커와 저널에서 솔버 인덱스 보관, 보호 lease, 네이티브 스냅샷 커밋, blob 기록, 캐시 retain을 순서대로 실행했다. 각 효과 커밋 직후 자식을 종료하고 이어서 저널 확인만 기록했다. 단계 중복 변경과 비대상 변경은 0건이었다. 다섯 단계의 완료는 `native-five-stage-trial-v1`로 확인했고 전체 삭제 영수증을 대입하면 거절됐다. [통합 실측](../dist/dsg-e2e-recovery-c306f911edac/QUALIFICATION.json) · [시험 코드](../integrations/buildkit-exact/cache/dsg_native_recovery_linux_test.go)
- 실제 읽기 전용 워커에서 캐시 부모·별칭 전이와 스냅샷·blob inode, 할당 블록·메타데이터 해시를 확인했다. 이 시험에서 생성한 정확한 파일과 blob의 바이트를 OCI 계층과 대조했다. 반복 조회가 일치했고 DB 5개가 그대로였다. 전체 ID `t526lj0a32xvomrnop2csn063`, 증거 SHA-256 `22b70011654473a251d7da505c68b618e2991bc9d575c21ea4dfe977371cb2c8`는 보존 루트 2개와 자식 스냅샷 1개가 남아 보존했다. [할당 증거](../dist/dsg-e2e-recovery-c306f911edac/QUALIFICATION.json)
- 내보내기 없이 별도로 생성한 scratch LLB의 전체 ID `gel62x0ude4d6t9pwldw0o0qq`에는 blob 결합과 솔버 결과가 없었다. 가변 별칭과 보존 루트 1개가 남아 보존했다. 두 시험 워커를 정리했고 삭제·prune 요청은 각 0건이다. 승인에 제출할 활성 대상은 0건이다. [후보 선별 실측](../dist/dsg-e2e-recovery-selection-169c44319405/QUALIFICATION.json)
- macOS Python 14건 및 Go 최상위 13건·하위 50건, Ubuntu Go 회귀 34건 및 별도 네이티브 시험 3건이 통과했다. 첫 Ubuntu 실행에서 native snapshotter의 경로 가정을 수정한 뒤 통합 실측을 통과했다. 임시 시험 저장소를 회수하고 VM을 일시정지로 복귀시켰다. [macOS 실측](../dist/dsg-e2e-recovery-macos-final-23e0c21/QUALIFICATION.json) · [Ubuntu 회귀 집계](../dist/dsg-e2e-recovery-c2f943171792/QUALIFICATION.json)
- 실제 회수에는 GC 보존 루트와 자식 스냅샷이 같은 대상의 전용 자원인지 증명하고, 실제 삭제 효과를 시험용 보존 단계와 구분하는 원장 연결이 필요하다. 검증을 통과한 새 활성 대상에 대해서만 별도 승인을 받는다. 격리 물리 시험기의 전원 장애 검증은 기존 보류 상태다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)


## BuildKit 정확 소유권과 네이티브 삭제 복구 시험

- 2026-11-25, DSG-03·05, WBS 1.4·2.3. `feature/20261125-buildkit-exact-ownership`, 기준 HEAD `e1bd1c3`. 판정 오버레이와 격리된 시험용 저장소만 검증했다. 소유 형태 fixture에서 레코드·변형 lease와 읽기 전용 자식 view, lease가 해제된 view를 확인했다. 외부 lease·캐시 연결·저장소 별칭·역방향 인덱스 결손·외부 콘텐츠 참조·자식의 별도 보존 루트·예상 밖 전용 디렉터리·미지원 lease 항목의 8가지 경우는 보존했다. [소유권 시험](../integrations/buildkit-exact/cache/dsg_graph_ownership_linux_test.go)
- Ubuntu Parallels ARM64에서 BuildKit v0.35.1 전용 데몬을 실행하고 정지 후 읽기 전용 ext4에서 전체 ID `zh7oonwj0wc5j6nmw0vj23fzv`를 관측했다. 대상 증거 SHA-256은 `4b7560dcd7e2ea09b4e488668f7ca0c2f44c9e5c9e9929d31aac188a8e698c85`다. 소유한 lease 2개와 lease가 해제된 읽기 전용 자식 view 1개를 확인했다. view 전용 할당은 없었고 스냅샷과 OCI에서 기대한 생성 파일을 대조했다. 반복 확인에서 DB 5개는 불변이었다. 유일한 유입 콘텐츠 참조의 지문과 네이티브 매니페스트 바이트를 독립 OCI 아카이브와 대조했다. 매니페스트 SHA-256은 `3e72d8b61a56699b9fc6895f7bfea8490f089aa79c928aa4a28394d621cdb0a8`다. 이 참조로 전체 범위의 배타성이 `false`(`foreign_or_unresolved_retention_edge`)여서 보존했다. 실제 캐시 삭제와 prune은 각각 0건이고 승인 가능한 활성 후보도 0개다. 시험 세대를 정리하고 VM을 일시정지로 복귀시켰다. [Ubuntu 실측](../dist/dsg-exact-ownership-8d9e8b065edd/QUALIFICATION.json)
- 별도 쓰기 가능 시험 저장소에서 공통 저널로 네이티브 스냅샷 1개와 blob 1개를 실제 삭제했다. 효과 전과 효과 후의 자식 종료를 주입했다. 효과가 확인된 복구는 저널 확인만 기록했고 삭제 재호출·비대상 변경·운영 회수 영수증은 각각 0건이었다. 효과 전 예상 객체 누락과 효과 후 불완전한 상태는 보존했다. Ubuntu Go 회귀 최상위 37건과 네이티브 최상위 4건, macOS Python 14건과 Go 최상위 13건·하위 50건이 통과했다. 초기 Ubuntu 관측에서 범용 상태 조회가 `ENOENT`를 구분하지 못하는 문제를 발견했다. 디렉터리 FD 기준 `statx`에서 `ENOENT`만 부재로 인정하도록 수정한 뒤 통과했다. [삭제 복구 시험](../integrations/buildkit-exact/cache/dsg_native_deletion_linux_test.go) · [Ubuntu 실측](../dist/dsg-exact-ownership-8d9e8b065edd/QUALIFICATION.json) · [macOS 실측](../dist/dsg-exact-ownership-macos-final-e1bd1c3/QUALIFICATION.json)
- 재개 시 워커에 매니페스트를 남기지 않는 `GetRemotes(true)` 기반 시험 exporter로 blob만 내보내 독립 대조하거나, 해당 참조의 워커 내부 소유권을 입증한다. 전체 레코드 회수에 참여하는 저장소를 연결하고 각각의 실제 효과와 영수증을 검증한 뒤 활성 새 대상의 증거를 별도 승인에 제출한다. 실제 호스트 전원 장애는 격리 물리 환경의 별도 검증으로 남는다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## BuildKit 매니페스트 없는 캐시와 전체 레코드 회수 시험

- 2026-11-25, DSG-03·05, WBS 1.4·2.3. `feature/20261125-buildkit-exact-reclamation`, 기준 HEAD `e918bde`. Ubuntu Parallels ARM64에서 BuildKit 네이티브 `CacheManager`의 `GetRemotes(true)`로 단일 레이어를 만들고 독립적으로 레이어 해시·tar·생성 데이터 바이트를 대조했다. 생성 프로세스 종료 후 전용 ext4를 읽기 전용으로 열어 실제 형식의 메타데이터를 확인했다. 반복 관측이 일치했고 DB 5개는 불변이었다. 전체 ID `76mdgzwt6p8v243o4531fty9p`, 관측 증거 SHA-256 `1a07f0063fa4bb1408774af0fdfca40fa1a57b36f8aef274f7af02478ea51582`, 레이어 SHA-256 `ed68f9b14ca719b3c4d7841bf6148f5caedfdfc79fd0c012a5d24ced7e1605d6`다. 소유 lease 2개, 자식 view 1개, 콘텐츠 노드 1개, 외부 매니페스트 참조 0건으로 소유권 확인·배타 판정이 모두 참이었다. 표준 OCI 내보내기로 유입 매니페스트 참조가 생기는 경우의 보존 판정도 재확인했다. [캐시 생성 및 관측 시험](../integrations/buildkit-exact/cache/dsg_manifestless_candidate_linux_test.go) · [Ubuntu 실측](../dist/dsg-exact-reclamation-c24288576c86/QUALIFICATION.json)
- 같은 생성기로 폐기 가능한 대상과 비대상 피어를 준비해 한 저널에서 솔버, lease·네임스페이스 메타데이터, 자식 snapshot view, snapshot, blob, cache의 6단계를 실행했다. 각 단계에서 효과 직전 6건과 직후 6건의 자식 종료를 주입했다. 출처 증거를 묶고 독립 네이티브 API로 최종 상태를 조회했다. 논리적 시험 레코드 1건을 회수했고 이미 반영된 효과의 재실행·비대상 변경·운영 회수 영수증은 각각 0건이었다. Ubuntu Go 회귀 37건, 네이티브 시험 5건, 엄격한 체크포인트 점검 1건과 macOS Python 14건, Go 최상위 13건·하위 50건이 통과했다. [전체 레코드 복구 시험](../integrations/buildkit-exact/cache/dsg_native_reclamation_linux_test.go) · [Ubuntu 실측](../dist/dsg-exact-reclamation-c24288576c86/QUALIFICATION.json) · [macOS 실측](../dist/dsg-exact-reclamation-macos-final-e918bde/QUALIFICATION.json)
- 사용자 캐시 DELETE와 prune은 각각 0건이다. 후보 마운트를 해제하고 시험 VM을 일시정지로 복귀시켰다. 별도 승인을 요청할 활성 대상은 0개다. 다음은 새 활성 대상의 증거와 별도 승인을 같은 오프라인 마운트의 배타 변경 경계에서 대조하고 운영 승인·변경·영수증 경로를 연결하는 작업이다. WBS 1.4·2.3은 부분 상태다. 물리 호스트 전원 차단은 현재 Mac 시험 범위에서 제외했으며 격리 장비를 확보한 뒤 별도로 검증한다. [백로그](BACKLOG.md#환경승인-조건이-남은-항목)

## BuildKit 동일 마운트 승인과 회수 예행시험

- 2026-11-25, DSG-03·05, WBS 1.4·2.3. `feature/20261125-buildkit-same-mount-approval`, 기준 HEAD `6ae93a4`. Ubuntu Parallels ARM64의 전용 ext4에서 보호된 비공개 실행기와 마운트 보유 프로세스가 같은 마운트와 출처, DB 5개의 해시를 승인 증거에 결속했다. 승인 부재, 보유 증거 해시 불일치, 승인 재사용을 각각 거절했다. 폐기 가능한 대상 1건에서 네이티브 6단계 논리 회수와 재개방 후 동일한 영수증을 확인했다. 역방향 참조 증거로 비대상 피어의 불변을 확인했다. 물리 회수량은 판정하지 않았다. [예행시험 사전 증거](../dist/dsg-same-mount-c781b7dbb024/REHEARSAL-READY.json) · [예행시험 결과](../dist/dsg-same-mount-c781b7dbb024/REHEARSAL.json)
- 초기 예행시험은 실제 효과 직후 최초 응답을 받지 못했다. 원장을 다시 열어 영수증만 조회해 정산했고 시험 자원을 정리했다. 읽기 전용 복귀 장애를 고정된 containerd의 fs-verity 디스크립터 누수로 좁혔다. 준비 스크립트에서 해당 디스크립터를 닫도록 수정한 뒤 정상 예행시험을 통과했다. macOS Python 14건과 Go 최상위 13건·하위 50건이 통과했다. [영수증 조회 복구 결과](../dist/dsg-same-mount-f50586013b0d/RECOVERED-REHEARSAL.json) · [준비기](../scripts/prepare-buildkit-exact.py) · [macOS 회귀](../dist/dsg-same-mount-macos-verified-6ae93a4/QUALIFICATION.json)
- Ubuntu에서 Go 회귀 39건, 네이티브 시험 5건과 매니페스트 없는 레코드의 엄격한 체크포인트 점검이 통과했다. 새 회귀는 일회 승인과 바이너리 역방향 피어 인덱스 변경을 검사했다. [Ubuntu 회귀](../dist/dsg-same-mount-regression-2effb1cd80c6/QUALIFICATION.json)
- 별도 승인에 제출한 활성 대상의 전체 ID는 `6qukfyt54j5o35na8uumic0xo`, 마운트 보유 증거 SHA-256은 `bcb4213b57480ade6afc12847004a1b568f00ac803d16662d900757759fc0d56`, 출처 SHA-256은 `df7680aa2371da67b642385548e3ccb2fb8b2ab1ce4f57612ac940844c7d1eff`다. BuildKit 네이티브 `CacheManager`의 `GetRemotes(true)`로 생성한 합성 레코드와 보호 피어를 검증했다. 전용 읽기 전용 마운트를 유지하고 VM을 일시정지했다. 기록 시점에 별도 승인 대기였고 해당 후보 적용·prune은 각각 0건이다. 호스트 유효 시각은 2026-11-25 12:30:37 KST이며 게스트가 벽시계와 부팅 후 경과 시간을 독립 검증한다. 만료 또는 마운트 보유 상태 변경 시 새 대상을 다시 검증하고 승인받는다. [별도 승인 사전 증거](../dist/dsg-same-mount-c7883af0a86e/PREAPPROVAL.json)
- WBS 1.4·2.3은 부분 상태다. 보호된 비공개 운영 경로의 예행시험을 통과했다. 표준 BuildKit 공개 API 연결, 마운트 보유 프로세스·승인 원장·VM 유실, 물리 호스트 전원 차단은 별도 검증 범위다. [시험 계획](TEST_PLAN.md#buildkit-동일-마운트-승인과-회수-예행시험)

## BuildKit 동일 마운트의 단일 승인 회수

- 2026-11-25, DSG-03·05, WBS 1.4·2.3. `feature/20261125-buildkit-single-approved-reclamation`, 기준 HEAD `2c3a81c`. 기존 대상 `6qukfyt54j5o35na8uumic0xo`는 호스트 유효 기간이 만료돼 적용 0건으로 시험 세대만 정리했다. 추가 질문 없이 실행하라는 사용자 지시는 앞서 지정한 전용 ext4 범위에서 새로 생성한 대체 합성 캐시 1건의 실행 승인으로 해석했다. 소스 동작은 변경하지 않았고 재빌드 바이너리 SHA-256은 앞서 검증한 바이너리와 일치했다. [만료 정리](../dist/dsg-same-mount-c7883af0a86e/EXPIRED-RETIRED.json) · [실행 승인 기록](../dist/dsg-same-mount-a4a90d9c6a36/EXECUTION-AUTHORIZATION.json)
- 새 대상의 전체 ID는 `elxf8w8fbyg2imsif9cbs13ib`, 마운트 보유 증거 SHA-256은 `833e1b338add4d20f2606da7402bd2e197941a0a8c1b4574b4bd71686ac41598`, 출처 SHA-256은 `7a22467e4d13cda64003c062f58900411f364438b6ea6988dbfb95b37fd72179`다. 적용 전후 동일 마운트를 확인했고 네이티브 6단계 실제 적용 1건과 논리 회수 1건을 확인했다. 독립 사후 조건 6개로 비대상 피어의 보존을 검증했다. 솔버·메타데이터·containerd·스냅샷 DB 4개는 실제 적용에서 변경됐고 이력 DB는 불변이었다. 물리 회수량은 미측정(`null`)이다. [사전 증거](../dist/dsg-same-mount-a4a90d9c6a36/PREAPPROVAL.json) · [적용 결과](../dist/dsg-same-mount-a4a90d9c6a36/AUTHORIZED-RESULT.json)
- 재개방한 기존 영수증의 정규화 SHA-256은 `de3ef3e6bc675e8a34c6869f7863e2a3d241d95f49c4e0ebb386a9e8048f90c9`로 적용 영수증과 일치했다. 영수증 재조회 중 변경 호출은 0건이었고 DB 5개, 네이티브 저널, 승인 원장은 바이트 단위로 일치했다. 마운트 보유 프로세스, 전용 ext4·loop 및 시험 디렉터리를 회수하고 VM을 일시정지했다. 사용자·시스템 캐시 변경과 prune은 각각 0건이다. WBS 1.4·2.3은 부분 상태이며 공개 API·운영 연결, 마운트 보유 프로세스·승인 원장·전체 VM 유실, 격리 물리 장비의 전원 장애 검증이 남았다. [적용 결과](../dist/dsg-same-mount-a4a90d9c6a36/AUTHORIZED-RESULT.json)


## BuildKit 공개 API와 승인 원장 복구

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-controller-recovery`, 기준 `52c6b72`. Linux ARM64 운영 제어기는 커널이 확인한 승인자와 실행자를 분리하고 고정된 네이티브 실행 파일을 호출한다. Ubuntu Parallels에서 역할·요청 불일치 6건과 승인 전 적용을 거절했다. 전용 ext4의 합성 레코드 `jfvzvv2e9zylkhzlt9nx3npy6` 한 건만 승인·적용했다. 승인 재사용 거절, 동일 영수증, 영수증 회수 중 변경 재호출 0건을 확인했다. 논리 회수 1건, 물리 회수량 미계측이다. 시험 서비스를 정리하고 VM을 원래 일시정지 상태로 돌렸다. [공개 API](API_SPEC.md#linux-arm64-buildkit-단일-대상-api) · [사전 증거](../dist/dsg-controller-runtime-5b7ca8e3454a/REHEARSAL-READY.json) · [적용 및 복구 실측](../dist/dsg-controller-runtime-5b7ca8e3454a/AUTHORIZED-RESULT.json)
- 같은 시험 마운트에서 원장 부재·바이트 동일 복사본 교체·절단·소비 전 상태로 되감기와 체크포인트 부재·부분 기록을 각각 주입했다. 적용과 실시간 영수증 조회를 모두 거절했고, 네이티브 DB 5개와 저널의 사후 해시가 유지됐다. 원본 원장과 체크포인트를 복원해 적용 직후 영수증과 동일한 결과를 얻었다. [원장 시험 코드](../integrations/buildkit-exact/cache/dsg_ledger_recovery_linux_test.go) · [실측](../dist/dsg-controller-runtime-5b7ca8e3454a/AUTHORIZED-RESULT.json)
- Mac의 별도 격리 QEMU Linux ARM64에서 네트워크와 쓰기 가능한 블록 장치 없이 2회 부팅했다. 첫 부팅의 휘발 표식 유실과 부팅 세대 변경, 이전 적용·실시간 영수증 조회 거절, 사전 고정한 호스트 아카이브 영수증 검증을 확인했다. 새 변경 호출은 0건이었다. 이 시험에서 기존 Parallels VM의 저장소를 변경하거나 전원을 차단하지 않았다. [호스트 고정값](../dist/dsg-controller-runtime-5b7ca8e3454a/OFF-VM-PINS.json) · [VM 실측](../dist/dsg-controller-runtime-5b7ca8e3454a/VM-LOSS.json) · [영수증 검증기](../scripts/check-buildkit-settled-receipt.py)
- 실측 Linux BuildKit 회귀 43건, QEMU의 Linux 전용 Rust 제어기 1건, macOS Rust 120건, Python 기준선 14건과 호스트 영수증 검사 2건이 통과했다. BuildKit 오버레이 54개는 준비 스크립트 결과·시험 바이너리 원본·저장소 사이에서 바이트가 일치했다. [Linux Rust 실측](../dist/dsg-controller-runtime-5b7ca8e3454a/LINUX-RUST-TEST.json) · [시험 기준](TEST_PLAN.md#buildkit-공개-api와-승인-원장-vm-유실-시험)
- 남은 운영 검증은 실제 배포 환경의 정책·소켓 서비스와 복구 보관 위치 확정, 격리 장비의 물리 전원 차단이다. 같은 세대의 원장과 체크포인트를 함께 되감는 전체 스냅샷, 호스트 아카이브와 독립 고정값의 동시 유실에는 이번 복구 증거를 적용할 수 없다. 새 운영 대상에는 프로젝트 소유권과 독점 변경 경계, 전체 ID·출처·증거를 대조한다. [대조표](TASK.md) · [백로그](BACKLOG.md)

## BuildKit 운영 사전 점검과 복구 회귀

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-recovery-qualification`, 기준 `28ba0a3`. **운영 사전 점검**은 Ubuntu Parallels Linux ARM64의 읽기 전용 systemd 기본 질의에 성공했다. BuildKit exact 서비스, 관련 빌더, keeper 및 등록은 각각 0개였고 `dsgd`는 미설치였다. 변경·배포 요청은 0건이며 VM은 시작과 종료 모두 일시정지 상태였다. 실제 운영 역할·정책·독점 마운트는 미검증이다. BuildKit 상시 등록 자산과 전용 읽기 전용 프로브는 미구현이며 기존 Docker 프로브와 별도로 추적한다. [운영 사전 점검](../dist/dsg-recovery-qualification-28ba0a3/OPERATING-PREFLIGHT.json)
- **복구 회귀**는 기존 예행 아카이브의 정상 검증과 macOS Python 검사기의 정상·아카이브 유실·핀 유실·동시 유실 4개 분기를 다룬다. 세 유실 조합에서 보존 판정, 변경 0건, 대체 파일 생성 0건을 확인했다. macOS Python 영수증 검사 3건과 기준선 14건이 통과했다. 아카이브와 핀은 별도 `0600` 파일이며 같은 `0700` 디렉터리와 파일시스템에 있다. Ubuntu Linux ARM64에서 고정 BuildKit 빌드의 Go 회귀 4건과 하위 분기 12건이 통과했다. 두 유효 단계는 로컬 검증을 각각 통과하며, 별도 보관한 완료값으로 구분할 수 있었다. 시험 자원을 회수하고 VM을 원래 일시정지 상태로 복귀시켰다. 원장과 체크포인트를 함께 되감을 수 있으면 로컬 비교만으로 최신 여부를 증명할 수 없다. 실제 동시 롤백과 재승인은 시험 범위에 포함하지 않았다. [호스트 회귀](../dist/dsg-recovery-qualification-28ba0a3/HOST-RECEIPT-REGRESSION.json) · [Linux 회귀](../dist/dsg-recovery-qualification-28ba0a3/LINUX-LEDGER-REGRESSION.json) · [Linux 시험 코드](../integrations/buildkit-exact/cache/dsg_ledger_recovery_linux_test.go#L100)
- 다음에는 BuildKit exact 상시 서비스 배포 자산과 보호 정책을 마련하고 별도 장애 영역의 영수증 및 독립 핀을 실제 운영 호스트에서 실측한다. 물리 전원 차단은 운영 중인 Mac과 별개로 지정된 격리 장비가 없어 제외했다. 기존 QEMU 증거는 VM 유실 검증에 해당한다. [시험 계획](TEST_PLAN.md#buildkit-운영-사전-점검과-복구-회귀)


## BuildKit 상시 소켓과 별도 원장 증거 실측

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-operating-service`, 기준 `6bee21e`. Ubuntu Parallels Linux ARM64, systemd 255에서 전용 서비스와 보호된 등록을 임시 배치했다. 실제 비루트 계정 2개를 사용해 권한·요청 불일치 6건, 전용 그룹 밖 소켓 연결, 두 역할의 워커·원장 직접 쓰기를 차단했다. 승인 전 적용과 재적용을 거절했다. 전용 ext4의 합성 레코드 `mj5k1xhprldwab9l5i07ks844` 한 건에 적용 1회, 동일 영수증 재조회 중 변경 0건을 확인했다. [서비스 자산](../integrations/systemd/dsg-buildkit-exact.service) · [운영 실측](../dist/dsg-buildkit-operating-runtime-bc17a618d978/OPERATING-RESULT-VERIFIED.json)
- 승인 원장·로컬 체크포인트·별도 증거를 기록하고 재대조한 뒤 다음 단계로 진행한다. 실제 적용 이후 같은 세대·동일 inode에서 원장과 체크포인트를 승인 직후 상태로 함께 되감는 경우와 별도 증거 변조를 각각 실측했다. 적용·실시간 영수증 조회 모두 종료 5였고, 네이티브 DB 5개와 저널은 불변이었다. 원본 복원 뒤 같은 영수증을 반환했다. [원장 코드](../integrations/buildkit-exact/cache/dsg_ledger_recovery_linux.go) · [실측](../dist/dsg-buildkit-operating-runtime-bc17a618d978/OPERATING-RESULT-VERIFIED.json)
- Ubuntu ext4에 영수증을 동기화하고 macOS 호스트에 독립 검증 해시를 남겼다. 서비스를 종료하고 VM을 일시정지·재개한 뒤 같은 영수증을 확인했다. 보관 자료 접근의 정상·단독 유실 두 가지·동시 유실을 모사했고 유실 분기는 모두 `preserve`, 변경 0건이었다. 실제 보관본과 해시는 보존했다. 두 영역은 같은 물리 Mac의 장애 영향을 공유한다. [보관 검증](../dist/dsg-buildkit-operating-runtime-bc17a618d978/STORAGE-MATRIX.json) · [호스트 해시](../dist/dsg-buildkit-operating-runtime-bc17a618d978/OFF-VM-PINS.json)
- Linux BuildKit 소켓 실측과 Go 회귀 43건, 별도 디스크 경계의 Go 4건·하위 12건, macOS Rust 120건, Python 기준선 14건 및 영수증 검사 4건을 통과했다. keeper를 종료하면 리스너가 함께 닫혔다. 시험 운영 계정·서비스·워커를 정리했고 Ubuntu는 기존 일시정지로 복귀했다. [원장 회귀](../dist/dsg-buildkit-operating-verification-6bee21e/LINUX-LEDGER-REGRESSION.json) · [시험 계획](TEST_PLAN.md#buildkit-상시-소켓과-별도-원장-증거-실측)
- 실제 운영 배치는 대상 프로젝트의 전용 워커 등록과 독점 변경권을 대상 호스트에서 재확인한 뒤 판정한다. 격리된 물리 시험 장비가 없어 실제 호스트 전원 차단은 제외했다. 원장·체크포인트와 별도 증거까지 함께 되감기거나 아카이브·독립 해시까지 함께 잃으면 최신성 및 과거 완료를 확정할 수 없다. [운영 절차](OPERATIONS.md#buildkit-단일-대상-운영) · [잔여 기준](BACKLOG.md#buildkit-운영-배포와-복구-검증-잔여)


## 프로젝트 전용 워커와 전체 VM 스냅샷 실측

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-project-enrollment`, 기준 `d3ea856`. Ubuntu Parallels ARM64의 새 전용 BuildKit v0.35.1 워커에 현재 프로젝트의 추적 `README.md` 3,762 B를 사용했다. 커밋·원본 SHA-256·LLB·OCI 레이어를 워커 전체 ID `jwdnmw42xkm3yyeshgqfeds9s`와 레코드 `syvhxtim4f18lgubr7ejmf3yv`에 결속했다. 새 저장소 등록, 실행 중 외부 잠금·오프라인 검사 거절, 등록 플래그 없는 데몬 기동 거절, 격리 비루트 역할 2개의 직접 접근 거절을 확인했다. 작업 후 네이티브 DB 4개의 해시는 같았고 회수·prune은 0건이었다. [등록 검사기](../scripts/check-dedicated-buildkit-fence.py) · [등록 실측](../dist/dsg-buildkit-project-verification-d3ea856/PROJECT-WORKER-VERIFICATION.json)
- 맥의 별도 QEMU에서 CPU·RAM·장치 상태와 ext4를 포함한 qcow2를 실제로 저장하고 복원했다. VM 상태는 83,736,271 B였다. 부팅 식별자와 PID 1은 같고 RAM 단계 및 합성 원장·체크포인트·별도 장치의 증거가 모두 과거 값으로 돌아갔다. 복원된 게스트 값끼리는 일치했고 스냅샷 밖에 보관한 최신값과의 차이를 외부에서 탐지했다. 생산 승인·네이티브 엔진·변경 호출은 이 시험에 사용하지 않았다. 실제 운영 코드의 전체 스냅샷 차단을 입증하려면 스냅샷 밖의 최신성 증거를 운영 승인 경계에서 확인해야 한다. [전체 스냅샷 실측](../dist/dsg-buildkit-project-verification-d3ea856/VM-FULL-SNAPSHOT.json) · [외부 최신값](../dist/dsg-buildkit-project-verification-d3ea856/OFF-VM-LATEST.json)
- 로컬 Ubuntu 및 연결 저장소의 배포 메타데이터에서 지정 운영 대상이나 보호된 상시 등록을 확인하지 못했다. 기존 격리 예행의 ext4 영수증과 macOS 사전 해시는 재대조에 성공했다. 이번 워커 등록은 현재 DSG 소스에 대한 읽기 전용 결속이며, 운영 캡슐의 생성·적용 및 새 운영 영수증 보관은 확인하지 않았다. 기존 승인 예행의 네이티브 경로는 합성 저장소와 고정 payload를 검증한다. [운영 대상 사전 점검](../dist/dsg-buildkit-project-verification-d3ea856/OPERATING-TARGET-PREFLIGHT.json) · [운영 범위](OPERATIONS.md#buildkit-단일-대상-운영)
- 전용 워커와 임시 자원을 회수하고 Ubuntu를 원래 일시정지 상태로 돌렸다. macOS Rust 120건, Python 프로젝트 등록 회귀 7건, 영수증 검사 4건, Python 기준선 14건을 통과했다. 실제 전원 차단은 별도의 지정 격리 물리 장비에서 수행한다. 다음은 지정 운영 대상에서 실제 워커 레이아웃의 읽기 전용 캡슐을 검증하고, 운영 승인 경계의 외부 최신성 확인을 검증하는 작업이다. [작업 대조표](TASK.md) · [백로그](BACKLOG.md#buildkit-운영-배포와-복구-검증-잔여)


## 기존 워커 등록과 VM 밖 승인 검증

- 2026-11-25, DSG-03·05·07, WBS 1.4·2.3·5.1. `feature/20261125-buildkit-external-freshness`, 기준 `e09e333`. Ubuntu Parallels ARM64에서 현재 프로젝트의 기록을 생성한 전용 BuildKit을 정지했다. 그 기존 저장소에서 독립 발급 SHA-256, 저장소 식별, 워커 `lkjmdi0x50eirl261wfcmnkrv`, 캐시 레코드 `v8gb1ya4wr04pbmabm6rb2275`와 결합 해시를 읽기 전용 대조했다. 워커와 DB 5개의 바이트가 유지됐다. 소켓 파일이 남은 입력을 거절했고, 시험 데몬 종료와 연결 거부를 확인해 해당 파일만 정리한 뒤 진행했다. 초기화 과정의 임시 파일은 별도 경로에 격리했다. 비루트 UID 두 개의 직접 접근 거절을 확인했고 실제 운영 계정·배타 변경 권한·새 영수증 경계는 미검증이다. [기존 저장소 실측](../dist/dsg-buildkit-freshness-verification-e09e333/EXISTING-STORE-REGISTRATION-ACCEPTED.json) · [독립 발급 해시](../dist/dsg-buildkit-freshness-verification-e09e333/INDEPENDENT-PROJECT-ISSUANCE-ACCEPTED.json)
- 공개 네이티브 `apply`는 배타 잠금과 읽기 전용 증거 확인 후, 로컬 승인 소비 및 쓰기 전환 전에 VM 밖 권한 서버에 범위와 승인 체크포인트를 보낸다. 외부의 한 번 사용 기록을 파일·디렉터리에 동기화한 성공 응답을 받은 뒤 같은 체크포인트에서만 소비한다. Ubuntu의 별도 합성 승인에서 외부 소켓 부재와 명시 거절을 각각 실측했다. 명시 거절은 실제 공개 경로가 전송한 범위·현재 체크포인트를 서버에서 대조했다. 원장·체크포인트·별도 증거·네이티브 DB 5개·읽기 전용 마운트가 유지됐고 변경 호출은 0건이었다. [네이티브 적용 차단](../dist/dsg-buildkit-freshness-native-bb291657c762/NATIVE-GATE-VERIFICATION.json)
- 별도 QEMU에서 요청 전 전체 스냅샷을 저장하고, 실제 Go 클라이언트가 제시한 최초 확인을 macOS 외부 권한 서버에서 한 번만 소비했다. 같은 부팅에서 RAM·qcow2를 되감은 뒤 같은 범위의 재요청은 보존됐다. 외부 소비 기록의 inode와 내용은 유지됐고 변경 호출은 0건이었다. 게스트 네트워크 장치 없이 보호 Unix 소켓과 제어된 직렬 중계로 시험했다. [QEMU 외부 승인 실측](../dist/dsg-buildkit-freshness-verification-e09e333/QEMU-EXTERNAL-FRESHNESS.json)
- Ubuntu Go 회귀 6건·하위 21건, macOS Rust 120건, Python 기준선 14건, 읽기 전용 등록 11건, 외부 권한 서버 6건, 이전 등록 7건, 영수증 4건이 통과했다. 이전 격리 영수증도 독립 핀으로 재검증했다. 전용 시험 자원을 회수하고 Ubuntu를 기존 일시정지 상태로 돌렸다. [검증 기준](TEST_PLAN.md#기존-워커-등록과-vm-밖-승인-검증)
- 현재 확인 범위에서 지정 운영 호스트와 계정·별도 물리 전원 장비는 확인되지 않았다. 운영 배치에서 실제 워커의 계정·독점 변경 허가·영수증 저장을 검증한다. 이미 성공 응답을 받은 뒤의 RAM 복원과 외부 권한 상태 자체의 롤백은 승인 경계 밖에서 차단해야 하며 실물 전원 차단도 별도다. [운영 절차](OPERATIONS.md#buildkit-단일-대상-운영) · [잔여 기준](BACKLOG.md#buildkit-운영-배포와-복구-검증-잔여)


## 승인 응답 이후 VM 복원과 외부 실행

- 2026-11-25, DSG-05·07, WBS 1.4·5.1. `feature/20261125-buildkit-external-execution`, 기준 `46d8826`. 별도 외부 실행기에 사전 등록한 작업과 영속 실행 기록을 두고, 최초 실행 표식과 `pending` 기록을 동기화한 뒤 범위가 고정된 실행 파일을 호출한다. 정산 응답에는 검증·저장된 영수증만 포함한다. 상태가 불명확하거나 최초 실행 표식과 맞지 않으면 재호출을 차단한다. [외부 실행기](../scripts/buildkit-external-executor.py)
- 별도 QEMU의 RAM과 qcow2를 승인 응답 수신 직후 및 완료 영수증 수신 직후로 각각 복원했다. 외부 승인 저장소도 사용 전 상태로 바꾸어 같은 합성 범위로 다시 승인 응답을 받았다. 게스트에는 워커 저장소와 네이티브 실행 파일이 없었다. 외부 실행 기록과 표식은 유지됐고, 재요청 2건은 이전과 바이트가 같은 영수증을 반환했다. 독립 호스트 시험 파일에 발생한 합성 변경은 1건이었다. [사후 복원 실측](../dist/dsg-buildkit-external-execution-46d8826/qualification/POST-RESPONSE-VM-RESTORES.json)
- Ubuntu Parallels ARM64에서는 보호된 작업과 SHA-256으로 고정한 실행 파일을 실제 외부 실행기 CLI로 호출했다. 실행 파일은 격리 파일에만 한 줄을 쓰는 대역이다. 범위 불일치와 최초 실행 표식을 남겨둔 채 실행 원장을 `ready`로 되감은 경우를 차단했다. 정산 원본 복원 뒤 동일 영수증을 반환했고 합성 변경은 1건이었다. 시험 파일을 정리하고 VM을 원래 일시정지 상태로 복귀시켰다. [Linux CLI 실측](../dist/dsg-buildkit-external-execution-46d8826/LINUX-EXTERNAL-EXECUTION-CLI.json)
- 지정 운영 호스트·역할과 상시 배치는 현재 확인 범위에서 찾지 못했다. 기존 예행 영수증과 Mac에 사전 보관한 해시는 다시 일치했다. 실제 운영 계정·독점 변경권·영수증 보관을 지정 환경에서 검증하고 기존 역할 통제에 외부 실행을 연결해야 한다. 격리 물리 장비의 실제 전원 차단과 외부 실행 기록·최초 실행 표식까지 함께 되감는 경우는 별도다. 두 저장소가 같은 물리 Mac을 공유한다. [운영·전원 사전 점검](../dist/dsg-buildkit-external-execution-46d8826/OPERATING-AND-POWER-PREFLIGHT.json)
- macOS Rust 120건, 외부 실행기 회귀 11건, 기존 외부 승인 6건, 영수증 검사 4건과 Python 기준선 14건이 통과했다. 외부 실행기의 정상·불일치 입력·응답 유실·동시 요청·원장 손상 및 부분 롤백을 확인했다. [검증 기준](TEST_PLAN.md#승인-응답-이후-vm-복원과-외부-실행)

## 실행 원장 동시 롤백과 독립 예약

- 2026-11-25, DSG-05·07, WBS 1.4·5.1. `feature/20261125-buildkit-independent-execution-fence`, 기준 `b12b208`. 외부 실행기는 최초 실행 표식과 `pending`을 저장한 뒤, 별도 예약 권한 서비스에서 등록·승인·적용 의도·증거·작업 SHA-256에 묶인 일회성 확인을 받아야 변경을 호출한다. 소켓 부재·거절·불완전 응답은 보존 처리한다. 정산된 작업은 원장과 표식이 맞으면 예약 서비스 없이 저장된 영수증을 반환한다. [API 명세](API_SPEC.md#독립-실행-예약) · [시험 기준](TEST_PLAN.md#실행-원장-동시-롤백과-독립-예약)
- Ubuntu Parallels ARM64에서 실제 외부 실행기 CLI와 SHA-256으로 고정한 합성 실행 파일을 구동했다. 최초 실행 뒤 원장을 동일 inode에서 과거 `ready`로 복구하고 최초 실행 표식도 제거했다. 독립 예약을 보관한 VM 밖 macOS에서 유효한 예약 요청 2건 중 최초 1건만 허용했다. 합성 파일 변경 1건, 재변경 0건이며 독립 예약 표식은 보존됐다. 정산 원장과 표식을 복원하고 소켓을 제거한 후 같은 영수증을 바이트 단위로 대조했다. 실제 Docker와 BuildKit 변경은 실행하지 않았다. 시험 자원을 회수하고 Ubuntu는 일시정지 상태로 돌렸다. [Linux·macOS 실측](../dist/dsg-buildkit-independent-fence-b12b208/qualification/LINUX-JOINT-ROLLBACK.json)
- 외부 실행기 회귀 15건, 권한 서비스 6건, 정산 영수증 4건, Python 기준선 14건이 통과했다. 지정 운영 호스트·계정·서비스는 확인 범위에서 발견하지 못했다. Ubuntu 시험 영수증은 VM 밖에 기존 보존한 해시와 일치했다. 실제 운영 배치에서는 기존 승인자·실행자 권한을 연결하고 운영 워커의 독점 변경권과 독립 영수증 보관을 확인한다. [운영 절차](OPERATIONS.md#buildkit-단일-대상-운영) · [운영·전원 사전 점검](../dist/dsg-buildkit-independent-fence-b12b208/qualification/OPERATING-AND-POWER-PREFLIGHT.json)
- 격리 물리 전원 시험 장비는 현재 환경에서 확인되지 않았다. 시험 VM과 권한 서비스는 같은 물리 Mac을 사용했다. 독립 예약 상태까지 함께 되감거나 외부 실행기 자체를 예약 수신 직후의 RAM으로 되돌리는 경우는 별도 검증이 남아 있다.

## 로컬 예상 회수량과 실제 할당량 실측

- 2026-11-25, DSG-05, WBS 1.4·2.3. feature/20261125-local-reclaim-measurement, 기준 70cadd3. SHA-256으로 확인한 기존 macOS ARM64 설치본의 dsg exec 자동 정리로 전용 합성 Cargo 산출물을 한 건 회수했다. 예상 16,777,216 B, 닫힌 시험 파일의 실제 APFS 할당 16,777,216 B, 정리 뒤 같은 경로의 파일 부재와 제품 논리 회수 16,777,216 B를 확인했다. 보호 시험 파일의 해시가 유지됐다. Mac 호스트 볼륨의 가용량 관측 차이는 +16,945,152 B이며 해당 파일 하나에 귀속하지 않는다. 시험용 설치와 경로는 회수했다. 이 macOS 시험에서는 Docker와 BuildKit을 구동하지 않았다. [macOS 실측](../dist/dsg-local-reclaim-measurement-70cadd3/MACOS-LOCAL-RECLAIM.json)
- 기존 Ubuntu Parallels 단일 승인 회수의 사전·실행 승인·적용 결과에서 회수 전후 할당량이 없고 두 영수증의 PhysicalBytes가 null임을 재확인했다. 기존 논리 회수 및 복구 결과를 유지한다. Linux ARM64의 요청 경계와 ext4 계측 도구를 읽기 전용 점검했고, 기존 VM을 일시정지 상태로 복귀시켰다. 새 대상의 게스트 할당 변화와 Mac 호스트의 변화는 개별 수치로 판정한다. [기존 승인 실측](#buildkit-동일-마운트의-단일-승인-회수)
- 기존 Ubuntu Parallels의 전용 ext4에서 새 합성 캐시 전체 ID b2t6sycko8qck4vloqpi7y0ni와 출처·보유 증거 SHA-256을 확인했다. 적용 요청과 prune은 0건이다. 대상 파일 두 개의 사전 할당 합계는 8,192 B, 전용 ext4의 사전 순할당은 393,216 B다. 마운트는 읽기 전용이며 VM은 일시정지로 돌렸다. ext4는 게스트 tmpfs의 loop를 사용해 Mac 볼륨·VM 이미지의 변화는 별도 관측값이다. Mac과 게스트의 시계는 달라 별도 승인 시 두 유효 기간과 같은 마운트·대상을 다시 확인한다. [새 대상 사전 계측](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-PREAPPROVAL.json)

## BuildKit 계측 승인 전 비휘발 원장 증거 확인

- 2026-11-25, DSG-05, WBS 1.4·2.3. feature/20261125-buildkit-reclaim-measurement, 기준 85d7cbe. 앞서 승인된 대상 b2t6sycko8qck4vloqpi7y0ni의 네이티브 승인 호출은 종료 5로 거절됐다. 당시 준비한 원장 증거가 tmpfs였으며, 네이티브 검사기는 비휘발 증거를 요구한다. 대상 ext4 할당 전후 393,216 B, 읽기 전용 유지, 적용·prune 각각 0건을 확인한 뒤 이전 전용 시험 자원을 정리했다. [차단 증거](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-REJECTED.json) · [비휘발성 검사](../integrations/buildkit-exact/cache/dsg_ledger_recovery_linux.go#L46)
- 기존 Ubuntu Parallels 안에서 디스크 파일을 사용하는 별도 ext4 원장 증거를 준비했다. 승인 원장 및 대상 저장소와 다른 장치이고, 증거의 원본 식별자와 체크포인트를 보호한 상태다. 새 합성 대상 yw90nonhug3mo7kb2z5a8dcua의 출처·보유 증거, 두 파일 할당 합계 8,192 B와 읽기 전용 사전 상태를 확인했다. 새 대상 변경 요청과 prune은 각각 0건이다. 별도 대상 승인 후 동일 마운트 검증과 한 번의 적용을 진행한다. VM은 일시정지 상태다. [새 대상 사전 증거](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-PREAPPROVAL-PERSISTENT.json)

## Ubuntu Parallels BuildKit 예상·실제 회수량 대조

- 2026-11-26, DSG-05, WBS 1.4·2.3. feature/20261126-buildkit-measured-reclaim, 기준 6a5c555. 대상 rx9iummme06rexq9s02ijeerr의 출처·보유 증거와 동일한 디스크 기반 ext4 마운트를 재확인했다. 게스트의 대상 저장소와 비휘발 원장 증거는 서로 다른 전용 loop 장치였다. 별도 승인된 정확 ID에만 네이티브 적용을 한 번 호출했고 prune은 0건이었다. [사전 증거](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-PREAPPROVAL-20261126.json) · [읽기 전용 보유 검증](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-CUSTODY-20261126.json)
- 예상 대상 데이터 블록 8,192 B와 실제로 사라진 스냅샷 파일·blob의 사전 할당 합계 8,192 B가 일치했다. 같은 ext4 전체의 순할당은 389,120 B에서 417,792 B로 28,672 B 증가했다. 전체 저장소 기준 순회수는 -28,672 B, 대상 예상 회수량과 순회수의 차이는 36,864 B다. 파일별 추가 할당 원인은 측정 항목에 포함되지 않았다. Parallels 디스크 파일의 할당 변화는 0 B, Mac 볼륨 가용량 변화는 +450,560 B였다. Mac 변화는 볼륨 관측값으로 보존한다. [전후 계측](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-MEASURED.json)
- 논리 레코드 1건을 회수했고 비대상 스냅샷·blob을 같은 해시와 inode로 확인했다. 마운트의 읽기 전용 복귀와 동일 영수증 재조회, 재조회 중 DB 5개 불변을 확인했다. 네이티브 영수증의 PhysicalBytes는 null이며 독립 계측값은 위 결과에 보관했다. 전용 시험 자원을 정리했고 VM은 기존 일시정지 상태로 복귀했다. 로컬 단일 대상의 예상·실제 회수량 대조를 완료했다. [최초 영수증 해시](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-FIRST-RECEIPT-20261126.json)

## 회수량 차이 분석과 현재 소스의 로컬 검증

- 2026-11-26, DSG-05·08, WBS 1.4·2.3·6.1. feature/20261126-local-recovery-and-packages, 기준 e452d73. 기존 Ubuntu BuildKit 계측에서 4 KiB 블록 기준 대상 파일 2개가 해제되고 ext4의 전체 할당은 7개 늘었다. 대상 파일 밖의 경로와 파일시스템 메타데이터를 합친 순증은 9개, 36,864 B다. 코드에서 같은 ext4의 네이티브 삭제 저널에 단계·호출·영수증을 저장하고 solver·cache·containerd의 bbolt 트랜잭션과 스냅샷·blob 제거를 수행함을 확인했다. 경로별 전후 계측값이 없어 어떤 파일의 페이지나 디렉터리 블록이 얼마나 늘었는지는 확정할 수 없다. 영수증 재조회 중 DB 불변은 적용 뒤의 결과다. 분석에 새 회수는 사용하지 않았다. [회수량 분석](../dist/dsg-local-qualification-13fdb79/ALLOCATION-GAP-ANALYSIS.json) · [저널 구현](../integrations/buildkit-exact/cache/dsg_same_mount_native_linux.go#L167)
- macOS의 현재 설치본으로 실제 로컬 Docker 컨텍스트 dockerguard-test에서 기존 이미지 계획과 재확인을 실행했다. 참조가 있는 이미지의 계획은 보존, 재확인은 종료 5였고 엔진·이미지·컨테이너·볼륨의 전후 목록이 일치했다. 별도 합성 CLI 입력은 적격 후보의 재확인 성공과 참조 추가 후 거절을 확인했다. 합성 조회는 읽기 요청 27건, 두 시험의 변경 요청은 0건이다. Linux ARM64에서는 배타 디스패치 합성 회귀 5건이 통과했다. 공개 이미지 적용 명령과 실제 Moby 디스패치 연결은 구현되지 않았다. [실제 Docker 확인](../dist/dsg-local-qualification-13fdb79/MACOS-AND-DOCKER-PUBLIC-QUALIFICATION.json) · [정상 후보와 참조 변경 회귀](../dist/dsg-local-qualification-13fdb79/DOCKER-PUBLIC-ELIGIBLE-FIXTURE.json) · [Linux 회귀](../dist/dsg-local-qualification-13fdb79/LINUX-CURRENT-BUILD.json)
- Rust 트리 13fdb79와 동일한 Cargo.lock에서 macOS ARM64, Linux ARM64 musl, Windows ARM64 패키지를 만들었다. 세 아카이브와 체크섬, 실행 파일 두 개의 아키텍처를 각각 대조했다. Mac과 기존 Ubuntu Parallels에서는 이전 후보 설치, 같은 버전의 현행 후보로 교체, 진단과 두 프로그램 실행, 읽기 전용 대시보드 HTTP, 제거와 시험용 사용자 파일 보존까지 통과했다. 설치본의 검증에서는 Docker 변경 호출이 0건이었다. Ubuntu VM은 기존 일시정지 상태로 복귀했다. 고정 Linux 빌드 컨테이너와 임시 빌드 경로를 회수했다. [세 아카이브 대조](../dist/dsg-local-qualification-13fdb79/NATIVE-PACKAGE-INTEGRITY.json) · [Mac 설치 검증](../dist/dsg-local-qualification-13fdb79/MACOS-AND-DOCKER-PUBLIC-QUALIFICATION.json) · [Ubuntu 설치 검증](../dist/dsg-local-qualification-13fdb79/UBUNTU-ARM64-CURRENT-RUNTIME.json)
- 같은 트리의 Windows ARM64 후보는 기존 Windows 11 Parallels의 로컬 NTFS에서 이전 후보 설치, 같은 버전 교체, 진단과 두 프로그램 실행, 읽기 전용 대시보드 HTTP, 제거와 사용자 파일 보존을 통과했다. 초기 게스트 명령 전송 문제는 읽기 전용 tar 시험으로 전송을 확인한 뒤 네이티브 tar에 압축 입력을 전달하는 경로로 해결했다. 시험 패킷·설치 디렉터리를 회수했고 Windows VM은 일시정지 상태로 돌아갔다. Python 기준선 14건과 Linux 배타 디스패치 회귀 5건이 통과했다. 로컬 검증에 사용한 Rust 빌드 이미지는 시험 엔진의 캐시에 남아 있다. 이미지 보고 크기는 1,346,811,366 B이고 이 이미지를 사용하는 컨테이너는 0개다. [Windows 검증](../dist/dsg-local-qualification-13fdb79/WINDOWS-ARM64-CURRENT-RUNTIME.json) · [빌드 캐시 확인](../dist/dsg-local-qualification-13fdb79/LINUX-BUILDER-CACHE.json)

## 로컬 BuildKit 경로별 할당과 Moby 적용 경계

- 2026-11-26 · DSG-05·07 · WBS 1.4·2.3·5.1 · `feature/20261126-local-allocation-attribution` · 기준 `418410a`. 기존 Go 네이티브 회귀에 선택형 ext4 할당 계측을 연결했다. 하나의 사설 loop 마운트를 동기화한 뒤 단계별 스냅샷 9개를 마운트 밖에 보관했다. 같은 마운트의 기준 전후는 397,312 B와 417,792 B다. `deletion-qualification.db` +16,384 B, `solver.db` +8,192 B, `metadata_v2.db` +8,192 B, 대상 스냅샷 파일·blob 각 −4,096 B, 대상 스냅샷 디렉터리 −4,096 B로 순증 20,480 B와 일치한다. 미귀속 잔차는 0 B다. 기존 승인 회수의 대상 외 순증 36,864 B는 경로별 스냅샷이 없어 소급 귀속할 수 없다. [단계별 증거](../dist/dsg-local-file-allocation/QUALIFICATION.json) · [검증 절차](TEST_PLAN.md#로컬-예상-회수량과-실제-할당량)
- macOS·Ubuntu 회귀 각 10건과 Ubuntu ARM64의 네이티브 6단계 시험이 통과했다. 자식 중단 12회, 비대상 데이터 변경·복구 재호출·prune·새 등록 대상 적용 각각 0건을 확인했다. Ubuntu 게스트에 전송한 시험 파일과 마운트를 회수하고 기존 일시정지 상태로 돌렸다. [실측과 회수](../dist/dsg-local-file-allocation/QUALIFICATION.json)
- 공개 이미지 적용은 실제 Moby 배타 백엔드가 필요하다. Moby 29.1.3 vfs 소스에서 전체 ID를 사용한 표준 DELETE도 특정 조건에서 단일 태그를 함께 제거함을 확인했다. Rust 디스패치의 배타 구현은 합성 시험용이다. 이번 확인의 Docker 실행·요청은 0건이다. 차기 범위는 Ubuntu Parallels의 전용 엔진 소스와 빌드 도구, 같은 락에서의 참조 확인·승인 소비·단일 변경·영수증 기록이다. 이를 연결한 뒤 경쟁 참조 변경과 응답 유실 복구를 검증한다. [백로그](BACKLOG.md#로컬-순할당-계측과-이미지-적용) · [Moby 29.1.3 삭제 구현](https://github.com/moby/moby/blob/docker-v29.1.3/daemon/images/image_delete.go#L162-L185)

## BuildKit 승인 경로의 회수량 차이 재현

- 2026-11-26 · DSG-05 · WBS 1.4·2.3 · `feature/20261126-local-same-mount-allocation` · 기준 `acacdc3`. Ubuntu Parallels에서 이전과 같은 BuildKit 네이티브 승인→적용→영수증 실행기를 사설 ext4 및 별도의 비휘발 원장 증거에 연결했다. 이 실행은 새 합성 레코드의 예행이다. Mac에서 정확한 범위·체크포인트의 시험 승인 요청을 각 예행마다 한 번 소비했다. 두 예행 모두 대상 두 파일에서 8,192 B를 회수했고, prune은 0건, 비대상 데이터는 동일했다. 적용 후 읽기 전용 복귀와 새 프로세스의 동일 영수증 재조회, 승인·재조회 구간의 대상 마운트 변화 0 B를 확인했다. 기존 등록 캐시 및 Docker 변경 요청은 0건이다. 두 시험 자원을 회수하고 Ubuntu VM을 일시정지로 복귀했다. [재현 검증 절차](TEST_PLAN.md#로컬-예상-회수량과-실제-할당량)
- 동일 시작값의 재현은 ext4 389,120 → 417,792 B와 예상–실제 차이 36,864 B에 일치했다. `deletion-qualification.db` +24,576 B, `solver.db` +8,192 B, `metadata_v2.db` +8,192 B, 스냅샷 디렉터리 −4,096 B로 대상 외 순증 36,864 B를 설명한다. 대상 데이터 −8,192 B를 합친 순증은 28,672 B, 미귀속 잔차는 0 B다. [동일 기준 재현](../dist/dsg-local-same-mount-allocation/MATCHED-QUALIFICATION.json) · [이전 승인 적용](../dist/dsg-local-reclaim-measurement-70cadd3/BUILDKIT-MEASURED.json)
- 시작값이 393,216 B인 예행도 같은 종료값 417,792 B에 도달했다. 두 예행에서 시작 할당량이 달랐던 파일은 `metadata_v2.db`다. 시작 할당량은 각각 40,960 B와 36,864 B, 종료 할당량은 모두 45,056 B였다. 따라서 두 예행의 예상–실제 차이 32,768 B와 36,864 B 사이의 4,096 B도 설명된다. [비교 예행](../dist/dsg-local-same-mount-allocation/QUALIFICATION.json)

## 로컬 전용 Moby 공개 이미지 적용

- 2026-11-26 · DSG-05·07 · WBS 1.4·5.1 · `feature/20261126-local-moby-image-apply` · 기준 `4b360a8`. 보호 등록부의 역할·승인·의도와 Moby 29.1.3 classic VFS 전용 이미지 저장소를 연결했다. 일반 서버 기동을 차단하는 표식과 사설 ext4를 확인한다. 제어기 실행 파일·PID를 고정하고 같은 소켓에서 준비와 커밋을 처리한다. SQLite 승인을 동기화한 뒤에만 엔진이 정확 ID의 시도 표식을 쓰고 변경을 시작한다. 결과는 디스크 영수증과 정확한 의도를 재대조해 정산한다. [공개 계약](API_SPEC.md#로컬-전용-moby-이미지-적용)
- Ubuntu Parallels의 새 합성 저장소에서 정상 적용과 영수증 동기화 직후 응답 유실을 각각 시험했다. 각 회차 이미지 2→1, 변경 시도와 영수증 각 1건, 비대상 내용·inode 보존을 확인했다. 실제 Docker CLI 태그·컨테이너 생성 요청, 잘못된 두 역할 요청, 같은 저장소의 표준 기동과 적용 재사용은 거절됐다. 시험용 응답 유실 바이너리에서 `recovery_required`를 얻고 정확한 영수증으로 정산했다. 복구 중 삭제 재호출과 prune은 각각 0건이다. 기존 등록 자원은 변경 0건이며, 시험 마운트·프로세스·임시 복사본을 회수한 뒤 VM을 일시정지로 복귀했다. [Ubuntu 실측](../dist/dsg-local-moby/QUALIFICATION.json)
- 공통 레이어와 각 이미지의 전용 레이어를 가진 별도 세 번째 시험에서 대상의 전용 레이어 1개를 제거했다. 공통 레이어와 비대상 이미지·레이어의 inode, 내용 해시 및 수정 시각은 동일했다. 시험 마운트와 프로세스를 회수하고 VM을 일시정지로 복귀했다. [공유 레이어 실측](../dist/dsg-local-moby/LAYERED-QUALIFICATION.json)
- Linux ARM64 제어기·승인·CLI 회귀 24건과 macOS 등록 회귀 120건이 통과했다. 논리 삭제를 정산하는 이 경로는 물리 회수량을 측정하지 않아 회수량을 0 B로 기록한다. 다음 로컬 작업은 승인된 전용 저장소에서 실제 이미지 회수량을 계측하고, 제품 패키지 교체 뒤 같은 읽기·쓰기 경계를 재검증하는 것이다. [Linux 빌드](../dist/dsg-local-moby/RUST-QUALIFICATION.json) · [macOS 회귀](../dist/dsg-local-moby/MACOS-REGRESSION.json)

## 전용 Moby 이미지 순회수량과 로컬 패키지 재검증

- 2026-11-26 · DSG-05·07·08 · WBS 1.4·5.1·6.1 · `feature/20261126-local-image-accounting-packages` · 기준 `cebad8e`, Rust 트리 `2bccd4b`. Ubuntu Parallels 사설 4 KiB ext4에서 Moby 레이어 예상값 8,192 B와 적용 전후 430,080 → 389,120 B를 대조했다. 대상 전용 VFS의 파일·상위 데이터 사본·디렉터리 20,480 B, 이미지 메타데이터 4,096 B와 레이어 메타데이터 24,576 B가 해제됐다. 영수증 원장 8,192 B를 차감한 실제 순회수는 40,960 B, 예상과의 차이는 32,768 B, 계측 잔차는 0 B다. 공유·피어 할당과 내용이 유지됐고 승인 및 재적용 시도 중 할당 변화는 각각 0 B였다. ext4와 시험 프로세스를 회수했다. 제품 영수증은 독립 계측 대신 논리 정산을 기록한다. [Ubuntu 이미지 계측](../dist/dsg-local-image-packages-2bccd4b/IMAGE-ALLOCATION.json)
- 세 ARM64 패키지를 같은 Rust 트리와 잠금 파일로 만들고 아카이브 및 파일별 SHA-256, 실행 파일 아키텍처를 확인했다. macOS와 Ubuntu·Windows Parallels에서 이전 설치, 같은 버전 교체, 두 실행 파일과 진단, 읽기 전용 대시보드, 제거·사용자 파일 보존을 통과했다. Ubuntu는 설치본과 바이트가 같은 보호 경로 사본을 역할에 사용해 계획·승인·단일 적용 및 ext4 40,960 B 순회수를 재현했다. macOS는 실제 로컬 Docker 조회에서 보존, Windows는 미확인 엔진에서 보존을 확인했다. 두 OS에서 이미지에 대한 승인·적용과 Linux 쓰기 명령이 거절됐으며 Docker 변경 요청은 0건이었다. 두 VM을 기존 일시정지 상태로 돌리고 임시 저장소와 전송 자료를 회수했다. [현재 패키지와 설치본](RELEASE.md#현재-소스의-로컬-후보-2026-11-26)
- Linux ARM64 디스패치·권한·CLI 24건과 할당 회귀 13건을 통과했다. 다음 작업은 제품에서 Moby 추정치·논리 처리 결과·파일시스템 순회수량을 각각 표시할 범위를 정하고, 보호된 로컬 측정과 정확한 적용 의도를 연결하는 것이다. [Linux 빌드](../dist/dsg-local-image-packages-2bccd4b/RUST-QUALIFICATION.json) · [회귀](../scripts/test-buildkit-allocation.py)
