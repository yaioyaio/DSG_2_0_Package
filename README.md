# Dev Storage Guardian (DSG) 2.0

패키지에는 설계 보고서 Markdown 보존본, 독립형 HTML 시뮬레이션, Python 표준 라이브러리 POC, Rust 제품 기반과 검증·운영 문서가 포함됩니다.

## 가장 중요한 구분

- `original/DSG_2_0_ORIGINAL_REPORT.md`: 기존 대화의 설계 설명·표·코드·체크리스트를 Markdown으로 변환한 원문 보존용 문서입니다. 채팅 전용 UI는 HTML로 분리했으며, 일부 서식은 Markdown으로 바꾸었습니다.
- `artifact/DSG_2_0_Interactive_Artifact.html`을 브라우저에서 열면 정책 슬라이더, JSON 복사, 체크리스트가 있는 독립형 시뮬레이션을 사용할 수 있습니다. 시뮬레이션은 로컬 디스크를 변경하지 않습니다.
- `poc/`: **실제 동작** 진단/스캔/Dry-run/정책 차단 예제/관리형 실행; Python 3.10+ 필요.
- `rust-scaffold/`에는 `dsg-core`와 실행 파일 `dsg`·`dsgd`를 빌드하는 `dsg-cli`가 있습니다. `dsg`는 프로젝트 등록, 정책·용량 검사, Docker 조회와 Compose 재사용 확인, lease를 사용하는 관리형 실행, 승인 기반 파일 정리, 읽기 전용 MCP와 로컬 대시보드를 제공합니다. 상태와 감사 기록은 SQLite에 저장합니다.
- `dsgd`의 읽기 전용 Docker 조회는 macOS·Linux의 Unix 소켓과 Windows SCM의 named pipe를 지원합니다. Linux 제한 UID·systemd는 실제 Docker 엔진으로 검증했고, Windows ARM64의 실제 SCM·계정 경계·재시작은 합성 상류로 검증했습니다. Windows 실제 Docker 엔진 연동과 macOS 배포 계정 분리의 검증이 남아 있습니다. [서비스 운영](docs/OPERATIONS.md#docker-조회-브로커) · [핸드오프](docs/HANDOFF.md)
- `docs/`에서 Python 검증 범위, 제품 구현과 남은 수용 기준을 확인할 수 있습니다. [POC 범위](docs/POC_SCOPE.md) · [개발 백로그](docs/BACKLOG.md) · [작업 대조표](docs/TASK.md)
- `integrations/`, `skills/`는 프로젝트별 훅 설치·제거, 스킬 절차와 서비스 구성 예시를 제공합니다. [에이전트 연동](integrations/README.md)

## 빠른 테스트 (macOS/Linux/Windows Python 환경)

```bash
cd poc
python3 -m unittest discover -s tests -v
python3 -m dsg doctor --root .
python3 -m dsg scan --root .
python3 -m dsg check -- docker volume prune
python3 -m dsg docker-scan
python3 -m dsg gc-plan --root .
```

Windows에서는 `python3` 대신 `py -3` 또는 `python`을 사용합니다.

Rust CLI의 테스트와 Python 대조 검증은 의존성을 받은 뒤 `sh scripts/test-rust.sh`로 실행합니다. CLI 명령과 JSON 출력 규약은 [API 명세](docs/API_SPEC.md#rust-cli-foundation)에 있습니다.

`dsg --root <프로젝트> dashboard`를 실행한 뒤 표시된 로컬 주소를 브라우저에서 열면 사용량, 정리 후보, 적용 정책을 조회할 수 있습니다. 설치 패키지와 플랫폼별 검증 범위는 [릴리스 안내](docs/RELEASE.md)에 있습니다.

자동 정리의 적용 대상은 등록된 재생성 산출물과 회수 조건을 통과한 보조 워크트리입니다. 개인 작업 파일과 DB 볼륨은 보호 대상이며, Docker 이미지 기능은 계획과 재검증까지 제공합니다. [운영 안내](docs/OPERATIONS.md) · [보안 가이드](docs/SECURITY.md)

## 개발 문서 읽는 순서

1. docs/PRD.md → docs/REQUIREMENTS.md
2. docs/ARCHITECTURE.md → docs/ADR.md → docs/ERD.md → docs/API_SPEC.md
3. docs/PLAN.md → docs/WBS.md → docs/TASK.md
4. docs/TEST_PLAN.md → docs/SECURITY.md
5. docs/AI_AGENT_GUIDE.md → docs/OPERATIONS.md
6. Python POC 안전 회귀와 Rust 제품 검증 결과 대조. [테스트 계획](docs/TEST_PLAN.md) · [핸드오프](docs/HANDOFF.md)
