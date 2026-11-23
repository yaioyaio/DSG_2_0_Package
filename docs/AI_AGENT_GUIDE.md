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

## Hook 위치
- Claude: `.claude/settings.json` 또는 사용자 설정
- Codex: 공식 Hooks 설정 형식 참조(버전별 확인)
- Gemini: `.gemini/settings.json`

예시 어댑터는 `integrations/`에 있으나 자동 설치는 미지원. `poc`의 Hook 예제는 미검증 CLI 호환 부분이 있으므로 운영 환경에 바로 설치하지 마십시오.
