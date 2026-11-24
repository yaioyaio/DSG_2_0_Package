# Architecture Decision Records

초기 ADR의 승인 제안 상태를 보존하고, 현재 코드와 검증 기록에서 확인한 적용 범위를 함께 기록합니다.

## ADR-001 Rust 최종 구현 + Python 실행 POC

- 초기 상태는 승인 제안이었다.
- 이유: 크로스 플랫폼 단일 바이너리, 시스템 레벨 프로세스/파일 접근에 적합.
- 대안: Go, Python. POC는 Python stdlib로 먼저 검증.
- 결과: Python POC의 테스트를 Rust 포팅 회귀 기준으로 사용.
- 구현에서 채택. Rust 제품 기반을 두 크레이트로 구성하고 Python 안전 회귀를 Rust 결과와 대조했다. [워크스페이스](../rust-scaffold/Cargo.toml) · [DSG-02 검증 기록](HANDOFF.md#2026-11-23--dsg-02-rust-기반과-cli-ux)

## ADR-002 임의 파일 삭제 대신 Provider 기반 GC

- 초기 상태는 승인 제안이었다.
- 이유: 공유 캐시, 볼륨, Docker 레이어의 참조 관계가 상이함.
- 결과: 권한·연령·소유권·활성 상태·계획 이력을 요구.
- 구현에서 범위를 한정해 채택. 파일 정리는 승인·소유권·활성 lease를 재검증하고 격리 자료의 복구 경로를 제공한다. Docker 이미지 경로는 소유권과 참조 확인을 거쳐 계획과 재검증을 수행한다. [승인 기반 정리](API_SPEC.md#승인-기반-정리) · [DSG-05 검증 기록](HANDOFF.md#2026-11-23--dsg-05-승인-기반-정리와-자동-회수)

## ADR-003 Hook는 보조 통제, 실행 경계는 독립

- 초기 상태는 승인 제안이었다.
- 이유: 스크립트/다른 프로세스/직접 Docker API 우회 가능.
- 결과: Enforce 단계에는 프록시+권한 격리 필요.
- 구현에서 채택. CLI 훅은 정책을 평가하고, Docker 조회 브로커의 접근 권한은 OS가 적용한다. 강제 통제의 확인 범위는 플랫폼별 시험 환경을 따른다. [브로커 운영](OPERATIONS.md#docker-조회-브로커) · [DSG-07 검증 기록](HANDOFF.md#2026-11-23--dsg-07-os-서비스와-강제-통제)

## ADR-004 SQLite 로컬 상태 DB

- 초기 상태는 승인 제안이었다.
- 이유: 무서버/오프라인/트랜잭션. 잠금 충돌과 백업·복구 절차 필요.
- 구현에서 채택. 로컬 SQLite의 WAL, 마이그레이션과 트랜잭션을 사용해 프로젝트, lease, 정리 계획과 감사 상태를 저장한다. [등록부 구현](../rust-scaffold/crates/dsg-core/src/registry.rs) · [DSG-02 검증 기록](HANDOFF.md#2026-11-23--dsg-02-rust-기반과-cli-ux)
