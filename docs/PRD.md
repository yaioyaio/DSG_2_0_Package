# PRD — Dev Storage Guardian 2.0

## 제품 비전
AI 코딩 과정에서 Docker·빌드·실행·테스트·캐시가 무한히 증가하는 문제를 **생성 전 통제·재사용·안전한 수명 관리**로 해결한다.

## 대상 사용자
- macOS/Windows/Linux 개발자 및 모노레포 운영자
- Claude Code CLI, Codex CLI, Gemini CLI 사용 팀
- 다수의 Next.js, Spring Boot, Rust, Python, Go, Electron, Tauri 프로젝트 운영자

## 문제 정의
P-001 무분별한 Docker 이미지/컨테이너/볼륨 생성. P-002 대형 빌드 캐시 및 출력물 누적. P-003 E2E trace/video 증가. P-004 다중 에이전트 중복 명령. P-005 무분별한 청소로 DB 데이터 유실 위험.

## 제품 범위
- MVP: 프로젝트 등록, 대상 디렉터리 스캔, 용량 측정, 정책 검증, Docker 읽기 전용 인벤토리, GC 후보 계획, 관리형 명령 실행, 감사 이력.
- V1: Docker Compose 상태 재사용, 프로세스 lease, pnpm/Gradle/Cargo/uv/Playwright provider, OS 서비스.
- V2: Docker API Proxy 및 접근 권한 통제, 확장 Plugin SDK, MCP, 대시보드.

## 비기능 요구
NFR-001 기본 동작 read-only / dry-run. NFR-002 영속 볼륨·사용자 원본 자동 삭제 0건. NFR-003 오프라인 핵심 동작. NFR-004 macOS·Windows·Linux 지원(플랫폼별 테스트 필요). NFR-005 프로젝트별 정책 및 전역 상위 안전 정책.

## KPI
- 동일 프로젝트 반복 실행 시 불필요한 추가 컨테이너 수 0(통제 경로).
- 잘못된 재사용으로 인한 잘못된 배포/빌드 결과 0.
- 등록된 생성 산출물 사용량과 GC 계획의 오차를 측정·개선.
- 모든 파괴적 작업에 감사 기록 100%.
