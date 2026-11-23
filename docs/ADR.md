# Architecture Decision Records

## ADR-001 Rust 최종 구현 + Python 실행 POC
- 상태: 승인 제안
- 이유: 크로스 플랫폼 단일 바이너리, 시스템 레벨 프로세스/파일 접근에 적합.
- 대안: Go, Python. POC는 Python stdlib로 먼저 검증.
- 결과: Python POC의 테스트를 Rust 포팅 회귀 기준으로 사용.

## ADR-002 임의 파일 삭제 대신 Provider 기반 GC
- 상태: 승인 제안
- 이유: 공유 캐시, 볼륨, Docker 레이어의 참조 관계가 상이함.
- 결과: 권한·연령·소유권·활성 상태·계획 이력을 요구.

## ADR-003 Hook는 보조 통제, 실행 경계는 독립
- 상태: 승인 제안
- 이유: 스크립트/다른 프로세스/직접 Docker API 우회 가능.
- 결과: Enforce 단계에는 프록시+권한 격리 필요.

## ADR-004 SQLite 로컬 상태 DB
- 상태: 승인 제안
- 이유: 무서버/오프라인/트랜잭션. 잠금 충돌과 백업·복구 절차 필요.
