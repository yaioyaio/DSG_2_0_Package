# DSG 2.0 프로젝트 분석과 DockerGuard 실측

2026-11-23 검토 초안. 앞서 진행한 DSG 패키지 분석과 DockerGuard 실측을 정리했다. 실행 순서와 완료 기준은 [전체 개발 계획 초안](IMPLEMENTATION-PLAN.md)에 있다.

## 제품 목적과 목표

Dev Storage Guardian은 개발자와 AI 에이전트가 사용하는 Docker, 빌드·의존성 캐시, 실행 프로세스, 테스트 증거의 수명을 프로젝트 단위로 관리한다. 호스트와 프로젝트 예산을 실행 전에 확인하고, 동일성이 확인된 자원을 재사용하며, 정리 전에 소유권과 사용 여부를 검증한다. 기본 동작은 조회와 정리 후보 계획이다. [제품 요구사항](../PRD.md) · [아키텍처](../ARCHITECTURE.md)

| 목표 | 판단 기준 |
|---|---|
| 중복 생성 방지 | 관리형 경로에서 동일 프로젝트를 반복 실행했을 때 불필요한 컨테이너 증가 0건 |
| 재사용 정확성 | 버전, 구성, 마운트, 상태가 달라진 자원을 잘못 재사용한 결과 0건 |
| 데이터 보호와 추적 | 사용자 원본과 영속 볼륨 자동 삭제 0건, 파괴적 작업의 감사 기록률 100% |
| 용량 예측 | 논리 사용량과 예상 회수량을 구분하고, 통제된 환경에서 계획과 실제 회수량의 차이를 측정 |

위 수치는 제품의 수용 목표다. 현재 검증 범위는 아래에 기록했다. [PRD의 KPI](../PRD.md#kpi) · [출시 게이트](../TEST_PLAN.md#출시-게이트)

## 현재 패키지에서 확인한 범위

| 항목 | 확인 결과 |
|---|---|
| Python POC | 프로젝트 설정, 지정 폴더 스캔, 디스크 예산 확인, 명령 분류, GC 계획, SQLite 감사 기록 및 제한적 실행 코드가 있다. `sh scripts/test.sh`로 단위 테스트 8개와 `doctor`·`scan`을 확인했다. |
| Rust 스캐폴드 | `dsg-core`와 `dsg-cli`가 있고 명령 분류 예제가 들어 있다. 임시 복사본에서 `cargo test --workspace --offline --jobs 1`로 두 크레이트를 빌드해 테스트 1개가 통과했다. |
| HTML 및 에이전트 연동 | HTML은 독립형 설계 시뮬레이션이다. 에이전트 Hook은 샘플 단계다. Docker 재사용, 영속 lease, 데몬·프록시·MCP는 DSG의 구현 계획에 있다. |
| Git 기준선 | 최초 분석 당시 DSG 경로는 Git 메타데이터가 없는 패키지였다. Git 연결 뒤의 기준선과 착수 상태는 [핸드오프](../HANDOFF.md)에 기록한다. |

[POC 범위](../POC_SCOPE.md) · [POC 실행 CLI](../../poc/dsg/__main__.py) · [Rust 명령 분류 예제](../../rust-scaffold/crates/dsg-core/src/lib.rs)

**우선 해결할 결함은 정리 후보의 보호 경계다.** 임시 데이터로 검증한 `gc-plan`에서 보호 목록을 프로젝트 설정으로 비우면 최상위 `data`가 후보에 올랐다. 기본 보호 목록을 사용해도 `build/data`가 있을 때 `build` 전체가 후보에 올랐다. 계획 생성까지만 검증했다. 적용 코드는 후보를 재귀 삭제하므로, 프로젝트 설정으로 공통 보호 정책을 완화할 수 없게 하고 보호 자료가 포함된 상위 경로를 후보에서 제외해야 한다. 후보 적용 기능은 대상의 활성·공유 상태, 경로 변경, 잠금, 감사 복구까지 확인한 뒤 활성화한다. [Python GC 구현](../../poc/dsg/core.py) · [보호·승인 요구사항](../REQUIREMENTS.md)

## DockerGuard 실측

별도 저장소 `ddd-studio-dockerguard`의 `develop` 커밋 `40dbd20`을 기준으로 추적 파일 220개, Rust 파일 144개·59,147줄, 제품 크레이트 14개, 실험 크레이트 3개를 확인했다. Rust 줄 수에는 테스트가 포함된다. Cargo Nextest로 `guard-model`, `guard-dedup`, `guard-reuse`, `guard-storage`의 관련 단위 테스트와 `scenarios`, `no_false_block`을 실행해 **125개가 모두 통과**했다. 빌드 및 테스트 동시성은 각각 1로 제한했고, 오프라인으로 실행했다. 테스트 뒤 Git 작업 트리는 깨끗했고 기존 `target`은 약 121 MiB 커졌다. Docker Engine·서비스를 새로 시작하거나 실제 자원을 변경하는 시험은 수행 범위에 포함하지 않았다. [DockerGuard 워크스페이스](../../../ddd-platform/ddd-studio-dockerguard/Cargo.toml) · [오차단 방지 테스트](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-dedup/tests/no_false_block.rs)

| 순서 | 재사용 후보 | DSG 적용 조건 |
|---|---|---|
| 1 | **읽기 전용 인벤토리와 중복 판정.** 조회 모델, 허용 필드 변환, Compose 프로젝트·서비스·인스턴스 번호·설정 해시 비교, 오차단 방지 테스트 | 엔진 식별자와 데이터 소유권을 정책에 묶는다. 중복 후보를 보여 준 뒤, 실제 재사용 전에 최신 health, 이미지 ID·digest, 마운트와 참조를 확인한다. 조회가 불완전하면 `UNKNOWN`으로 보존한다. [변환 모델](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-discovery/src/convert.rs) · [Compose 중복 판정](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-dedup/src/create.rs) |
| 2 | **이미지 정리 계획과 직전 재검증.** 소유·참조 근거에 따라 후보를 생성하고 적용 직전에 태그·digest·정지 컨테이너 참조까지 재조회 | DSG의 계획 만료, 사용자 승인, lease, 감사와 복구 흐름에 연결한다. 소유권 미확인 이미지와 실제 dry-run이 없는 엔진 공용 BuildKit prune은 자동 정리에서 제외한다. [정리 계획](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-storage/src/plan.rs) · [직전 재검증](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-storage/src/engine.rs) |
| 3 | **관리형 실행과 운영 테스트.** 자식 프로세스 종료 상태 전달, Compose 설정 해시 조회, 프록시 경쟁 조건 테스트 | DSG에 실행 시간·자원 상한, 프로세스 트리 추적, 영속 lease가 필요하다. 프록시 장애 시 직접 Engine으로 가는 동작은 DSG 강제 경로에서 제외한다. 빌드 입력 해시는 보호 경로 제외와 명시적 읽기 상한을 적용한 뒤 검토한다. [자식 실행](../../../ddd-platform/ddd-studio-dockerguard/apps/guard-cli/src/child.rs) · [Compose 경로](../../../ddd-platform/ddd-studio-dockerguard/apps/guard-cli/src/compose.rs) · [프록시 테스트](../../../ddd-platform/ddd-studio-dockerguard/crates/guard-proxy/tests/engine.rs) |

DockerGuard의 라이선스는 `MIT OR Apache-2.0`으로 선언돼 있다. 실제 이식 때 가져오는 파일과 원본 커밋을 기록하고 해당 라이선스와 고지를 보존한다. 이식 뒤에는 읽기 전용 인벤토리부터 DSG 자체 테스트로 검증한다. [라이선스 선언](../../../ddd-platform/ddd-studio-dockerguard/Cargo.toml) · [MIT 고지](../../../ddd-platform/ddd-studio-dockerguard/LICENSE-MIT) · [Apache 고지](../../../ddd-platform/ddd-studio-dockerguard/LICENSE-APACHE)

## 운영 지침 반영 내역

프로젝트 지침에 작업별 Codex 모델 위임, 날짜 기반 feature 브랜치, 금지 파일 제외 및 여섯 Git 참조 비교, 워크트리 회수와 인수인계, Markdown 파일명, 계획 범위 승인 규칙을 기록했다. 이번 검토에서는 개발 착수 시 백로그와 핸드오프를 만들고 작업 상태와 재개 지점을 관리한다는 규칙을 추가했다. 범위 차단 훅과 워크트리 정리기의 제품 구현은 별도 계획 항목이다. [프로젝트 지침](../../AGENTS.md)
