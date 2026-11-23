# Dev Storage Guardian (DSG) 2.0

**배포 내용:** 이전 대화 설계 보고서 Markdown 보존본, 독립형 HTML 아티팩트(정책 슬라이더·JSON 복사·체크리스트), 실제 실행 가능한 **Python 표준 라이브러리 POC**, Rust 2-크레이트 스캐폴드, 개발 계획·품질·테스트·CLI 연동·Skill 문서.

## 가장 중요한 구분

- `original/DSG_2_0_ORIGINAL_REPORT.md`: 기존 대화의 설계 설명·표·코드·체크리스트를 Markdown으로 변환한 원문 보존용 문서입니다. 채팅 전용 UI는 HTML로 분리했으며, 일부 서식은 Markdown으로 바꾸었습니다.
- `artifact/DSG_2_0_Interactive_Artifact.html`: 브라우저에서 파일로 열어 사용할 수 있는 독립형 인터랙티브 시뮬레이션. **로컬 디스크 변경 없음**.
- `poc/`: **실제 동작** 진단/스캔/Dry-run/정책 차단 예제/관리형 실행; Python 3.10+ 필요.
- `rust-scaffold/`: Rust CLI와 코어 워크스페이스. 프로젝트 등록, 정책, 용량 스캔과 정리 미리보기를 제공하며 SQLite에 등록과 명령 판단·계획 감사 기록을 저장합니다.
- `docs/`: 개발 실무 문서. POC가 제공하지 않는 Daemon, Docker Proxy, MCP, 실제 중복 판정 기능은 미래 개발 범위입니다.
- `integrations/`, `skills/`: CLI Hook 샘플 및 Skill 지침; **자동 설치되지 않음**. 실제 적용 전 해당 버전의 공식 Hook 문서 확인.

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

**이 패키지는 기존 개인 작업 파일, DB 볼륨, Docker 이미지 등을 자동 삭제하지 않습니다.** 실제 프로젝트 적용 전 docs/OPERATIONS.md 및 SECURITY.md를 읽으십시오.

## 개발 구현 순서

1. docs/PRD.md → docs/REQUIREMENTS.md
2. docs/ARCHITECTURE.md → docs/ADR.md → docs/ERD.md → docs/API_SPEC.md
3. docs/PLAN.md → docs/WBS.md → docs/TASK.md
4. docs/TEST_PLAN.md → docs/SECURITY.md
5. docs/AI_AGENT_GUIDE.md → docs/OPERATIONS.md
6. Python POC 테스트 후 Rust 대체 구현 착수
