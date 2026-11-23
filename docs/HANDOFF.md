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
- 성공 Cargo 산출물 1개 7 B와 임시 Git 보조 워크트리 1개 157 B를 회수했다. 실패·중단 산출물 각 1개, Git 브랜치와 주 작업 디렉터리는 보존했다. macOS에서 설정할 수 없는 `RLIMIT_DATA`는 50 ms 간격의 자식 RSS 확인으로 대체했다. Linux의 메모리 제한 경로는 미실측이다.
- 실제 프로젝트 데이터, Docker 자원·데몬 변경은 0회다. 프로젝트의 보조 워크트리 생성·회수·잔여는 0개다. 다음은 DSG-06 훅 계약과 읽기 전용 MCP다.

DSG-05의 `1161b75`를 PR #9로 `develop`, PR #10으로 `main`에 반영했다. 여섯 참조 파일 비교 15개가 일치했고 주 작업 폴더는 `develop`으로 전환했다.

## 2026-11-23 · DSG-06 에이전트 연동

- 브랜치 `feature/20261123-agent-integration`, 기준 `9651e33`. Phase 4와 WBS 4.1·4.2 중 훅·설치 제거·공통 스킬·읽기 전용 MCP를 구현했다. 훅 프로토콜, 설정 편집, MCP를 별도 파일로 위임했고 지휘자가 진입점, 운영 문서와 통합 검증을 맡았다. 보조 워크트리 생성·회수·잔여 0개다.
- macOS에서 Rust 테스트 71개, Python/Rust 보호 회귀, 관리형 실행·승인 GC·자동 회수, 프로젝트 훅 설치·제거, 읽기 전용 MCP를 검증했다. 정리 후보 조회 후 상태 디렉터리가 생기지 않았고 변경 요청을 거절했다.
- Claude Code 2.1.330, Codex CLI 0.170.0, Gemini CLI 0.70.0으로 합성 모델 응답을 보내 실제 CLI의 허용·거부·훅 오류 사례 9개를 검증했다. 임시 설정과 대역 명령을 사용했다. 초기 검증에서 Codex/Gemini는 프로젝트 신뢰 설정이 필요했고, Claude는 쓰기 제한 안에 자체 임시 위치가 필요했다. 세 CLI는 정상 훅에서 보호 명령을 모두 차단했다. 훅 오류 시 Claude는 차단했고 Codex·Gemini는 검증용 대역 명령을 실행했다.
- 사용자 설정, 외부 모델 서비스 및 실제 Docker 엔진 사용은 0회다. Windows 훅 설치는 미지원이며 Linux에서 실제 CLI는 미실측이다. MCP 실행 상태와 승인 도구는 후속 범위다. [에이전트 운영 범위](../integrations/README.md) · [검증 재현](TEST_PLAN.md#에이전트-연동-검증)
- 다음은 DSG-07이다. 허용된 조회의 프록시 전달, 프로세스 권한 분리, 장애 후 서비스 복구를 격리된 환경의 순서로 검증한다.

DSG-06의 `a62830b`를 PR #11로 `develop`, PR #12로 `main`에 반영했다. 여섯 참조 파일 비교 15개가 일치했다. Git 완료 정리기를 호출한 뒤 주 작업 폴더를 `develop`으로 전환했다.

## 2026-11-23 · DSG-07 OS 서비스와 강제 통제

- 브랜치 `feature/20261123-os-enforcement`, 기준 `8f8dff8`. Phase 5·WBS 5.1·REQ-012에 따라 허용 조회만 수행하는 `dsgd` Unix 소켓 브로커, 서비스 예시, 통합 검증을 구현했다. HTTP·Unix 소켓 모듈을 분리해 위임했고 지휘자가 고정 Docker 클라이언트·서비스 구성·실측을 맡았다. 보조 워크트리 생성·회수·잔여 0개다.
- macOS arm64에서 실제 Docker CLI를 임시 복제해 합성 Unix 엔진에 연결했다. 미허용 UID와 변경·상세 조회 요청의 엔진 전달 0건, 같은 엔진 복구 후 조회 성공, 다른 엔진 ID에서 `503`, 브로커 중복 시작 거절, 충돌 후 소유 소켓 회수와 종료 시 소켓 정리를 확인했다. 경로별 macOS 샌드박스의 직접 소켓 접근 거절과 브로커 조회 성공도 확인했다.
- 임시 GUI `launchd` 서비스에서 등록, 강제 종료, 한 번의 수동 재시작, 해제까지 실행했다. 등록된 서비스에는 고유한 임시 레이블을 사용했다. 호스트 공용 Docker 설치 상위 디렉터리의 쓰기 권한 검사에 걸려 임시 전용 복제본을 사용했다. 재시작 검증의 대기는 `launchd` 시험 서비스의 재시작 간격을 명시해 해결했다.
- `DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`에서 Rust 테스트 78개와 기존 보호·실행·회수·에이전트 통합 회귀가 통과했다. 소유 기록과 이름만 같은 원본 파일 세 형태를 보존했다. 지휘자가 만든 임시 빌드와 Gemini 검증 번들 2개를 회수했다. 논리 크기는 합계 319,083,788 B였다.
- 실제 Docker 엔진과 배포용 계정·서비스 설정 변경은 0회다. 실제 엔진·별도 제한 계정의 런타임 접근 경계, Linux systemd 및 Windows SCM·파이프 구현/검증이 남았다. 해당 환경에서 통과한 뒤 7단계를 닫고 제품화에 진입한다. [운영 권한과 복구](OPERATIONS.md#docker-조회-브로커) · [플랫폼 검증](TEST_PLAN.md#브로커와-서비스-검증)

### 재개 후 플랫폼 실측

- `DSG_TEST_LIVE_ENFORCEMENT=1 DSG_TEST_LAUNCHD=1 sh scripts/test-rust.sh`에서 Rust 테스트 82개와 macOS 통합 검증을 통과했다. 실제 `dockerguard-test` 엔진에서 동일 UID의 제한 프로세스는 원본 Unix 소켓 및 TCP를 직접 사용할 수 없었고, 브로커 조회는 성공했다. 변경 요청 전달은 0건이다. 배포 계정 분리 실측은 별도 조건이다.
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

- 기존 Parallels Windows 11 ARM64 게스트를 재개했다. 공식 ARM64 임베디드 Python과 릴리스 후보를 해시 확인 후 임시 폴더로 옮겼다. 동일 입력 재생성, 설치·진단·실행·업그레이드·제거, 동시 설치 충돌, 파일 변조·링크·대체 스트림 거절 및 잠긴 파일의 제거 재시도를 통과했다. 설치한 ARM64 CLI의 대시보드 HTTP 테스트도 통과했다. 패키지 검증용 임시 파일을 회수했다.
- Windows SDK 경로에 SignTool이 있다. 이 게스트의 Docker 명령과 표준 로컬 엔진 파이프는 확인되지 않았다. ARM Windows용 Docker Desktop은 WSL 2가 필요하며, Parallels는 Apple Silicon의 Windows ARM 게스트에서 해당 중첩 가상화를 지원하지 않는다고 명시한다. [Parallels 제한](https://kb.parallels.com/en/129497) · [Docker 요구사항](https://docs.docker.com/desktop/setup/install/windows-install/)
- 실제 SCM 실측에서 허용 계정의 파이프 연결이 속성 읽기 권한 누락으로 실패했다. 허용 SID에 `FILE_READ_ATTRIBUTES`만 추가한 `0.1.0-rc.2`에서 LocalService의 제한 조회, NetworkService의 연결 거부, 원본 파이프 직접 연결 거부, 정상 종료·강제 종료 뒤 재시작 및 임시 서비스·작업 회수를 확인했다. 상류는 합성 파이프를 사용했다. ARM64·x64 후보 아카이브와 SHA-256 파일을 `dist/`에 보존했다. [Windows 파이프 권한](OPERATIONS.md#docker-조회-브로커) · [Windows 접근 권한 기준](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- `0.1.0-rc.2`의 대시보드에서 연결 뒤 요청이 늦게 도착할 때 WinSock `10053`을 네이티브 재현했다. 수락한 소켓을 제한 시간이 있는 블로킹 I/O로 설정한 `0.1.0-rc.3`에서 같은 테스트와 대시보드 접근 정책, 설치 수명 주기 및 임시 경로 회수가 통과했다. ARM64의 `dsgd.exe`는 SCM을 통과한 `rc.2`와 해시가 일치한다. x64의 `rc.3`는 교차 링크와 아카이브 검증을 통과했다. Windows 게스트를 원래의 일시정지 상태로 돌렸다. [WinSock 연결 수락](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-accept)
- `ddd-tools`의 Windows Authenticode·타임스탬프, macOS Developer ID·공증 및 Linux APT 검증 절차를 대조했다. `ddd-studio-rsie`가 아카이브와 정규 매니페스트를 각각 서명하는 방식을 참조해 DSG 분리 서명 검증을 구현했다. macOS에서 임시 Ed25519 키로 정상 서명, 12가지 거부 입력, 서명 옵션을 사용하는 설치·업그레이드 및 거부 시 이전 버전 보존을 확인했다. DSG 공식 게시자 자격과 공인 서명은 검증 대기다. [서명 조건과 검증 명령](RELEASE.md#배포-파일의-분리-서명-검증)
- Windows 게스트가 실행 중일 때 macOS 통합 러너의 합성 Git 회수는 사용 여부 확인 실패로 보존 동작을 반환했다. 게스트 일시정지 후 단독 실행한 `sh scripts/test-rust.sh`는 Rust 85개와 통합 검증을 통과했다. 합성 Git 보조 워크트리 1개·157바이트를 회수했고 로컬·원격 참조와 주 작업 폴더를 보존했다. 작업용 저장소의 보조 워크트리는 생성 0개다.
