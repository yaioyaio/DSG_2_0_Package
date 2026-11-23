# 개발 TASK / TODO 체크리스트

## 현재 POC 확인
- [x] `dsg init` 기초 정책
- [x] `dsg doctor` 진단
- [x] 생성 디렉터리 스캔 및 크기 합계
- [x] Docker 읽기 전용 CLI 검사
- [x] GC Plan 생성 및 보존
- [x] 명시적 이중 승인 + 파일 상태 재검증
- [x] 제한적 명령 실행 전 용량 검증
- [x] Python unit tests

## Rust Core
- [ ] 정책 스키마/상위 정책 병합
- [ ] 정확한 파일시스템 사용량 및 소유권
- [ ] process lease 구현
- [ ] GC 원자성/격리/복구
- [ ] Docker Engine Provider/Compose Provider
- [ ] Build Providers
- [ ] Mac launchd / Windows Service / Linux systemd

## AI 통합
- [ ] Claude CLI actual smoke test
- [ ] Codex CLI actual smoke test
- [ ] Gemini CLI actual smoke test
- [ ] MCP server contract
- [ ] Skills 설치/제거
- [ ] 직접 Docker 소켓 우회 방지
