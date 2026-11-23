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
