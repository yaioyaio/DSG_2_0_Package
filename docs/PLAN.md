# 단계별 개발 계획

## Phase 1 — Foundation
- 프로젝트 식별/등록, 정책 스키마, 파일스캔, SQLite, 감사 로깅, GC dry-run, 기초 CLI.
- 우선 POC 코드를 Rust로 이식하며 동작 동등성 테스트.
- Exit: 알 수 없는 경로 삭제 0건, 회귀 테스트 통과.

## Phase 2 — Docker
- Engine Context 탐색; Compose 구성 정규화; 이미지·컨테이너·빌더 리소스 그래프.
- 중복 판정의 false positive 테스트. Volume 자동 삭제 금지 유지.
- Exit: 재사용/재빌드 필요 판정의 정확성 검증.

## Phase 3 — Build / Runtime / Test
- pnpm/Next, Gradle, Cargo, uv, Go, Playwright Provider.
- 실행 lease, 테스트 worker 제어, 산출물 TTL, 빌드 유지 규칙.

## Phase 4 — Agent Integrations
- Claude, Codex, Gemini 공식 Hook Adapter; 설치/제거; 공통 Skills; MCP Read-only.

## Phase 5 — Enforcement
- Docker API Proxy+접근 제한, 정책 우회 테스트, crash recovery, OS daemon.

## Phase 6 — Productization
- Cross-platform 배포, 코드서명/릴리스, 설치/제거, UI, 설치 진단, 문서.

각 단계는 보안 위협 모델과 OS별 실제 검증 뒤 다음 단계로 전환.

실행 순서는 [전체 개발 계획 초안](draft/IMPLEMENTATION-PLAN.md), 각 WBS의 산출물·구현·실측 차이는 [작업 대조표](TASK.md)에 기록한다. 플랫폼별로 후속 검증을 진행한 경우 해당 플랫폼과 시험 범위를 명시한다. 전체 단계의 완료는 공식 계획의 수용 기준을 모두 확인한 뒤 판정한다. [출시 게이트](TEST_PLAN.md#출시-게이트)
