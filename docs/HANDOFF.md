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
