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
