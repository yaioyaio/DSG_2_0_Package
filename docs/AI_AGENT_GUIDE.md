# Claude Code / Codex CLI / Gemini CLI 공용 개발 지침

## SSOT
PRD → REQUIREMENTS → ARCHITECTURE → API_SPEC → TASK → TEST_PLAN → 구현 → 증거.

## 에이전트 동작 규칙
- 빌드 전 디스크 여유/프로젝트 출력 확인.
- 같은 Docker Compose 프로젝트 반복 생성 금지; 상태 및 구성 검증.
- 무조건 `clean build` 금지. 캐시 재사용 우선.
- 임의의 `docker system prune -a --volumes`, `docker compose down -v` 금지.
- E2E 실패 증거만 보관(사용자 요구 시 예외).
- Hook 설치 전 백업, 실제 CLI에서 샘플 입력 테스트.
- 명령 거부 시 우회 명령을 만들지 말고 정책과 사용자 의도를 확인.
- 대량 파일 스캔/삭제는 사용자 승인 및 dry-run 선행.
- 기능 구현과 테스트 증거는 문서상의 REQ ID와 연결.

## 훅과 읽기 전용 도구

프로젝트 설정에 Rust `dsg` 훅을 등록하고, 사용 중인 CLI의 훅 신뢰 설정과 버전을 확인한다. 실행을 차단당한 명령은 정책을 검토한다. 상태·정책·정리 후보를 조회할 때는 지정 프로젝트의 stdio MCP를 사용할 수 있다. [에이전트 설치와 제거](../integrations/README.md) · [MCP 도구 명세](API_SPEC.md#읽기-전용-mcp)
