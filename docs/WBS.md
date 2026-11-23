# WBS — 구현 일정 추정용 작업 분해

| WBS | 산출물 | 선행 | 검증 |
|---|---|---|---|
| 1.1 | SSOT 정책/스키마 | 없음 | 스키마 검증 |
| 1.2 | 프로젝트 등록 및 상태 DB | 1.1 | DB 마이그레이션/중복 등록 |
| 1.3 | 파일 스캐너 및 크기 측정 | 1.1 | 링크/권한/대용량 테스트 |
| 1.4 | GC Planner 및 승인 workflow | 1.2,1.3 | 상태변경 시 삭제 거부 |
| 2.1 | Docker context/이미지 인벤토리 | 1.2 | Docker API fixture |
| 2.2 | Compose 재사용 판정 | 2.1 | 반복 up/변경 구성 테스트 |
| 2.3 | Buildx 캐시 Provider | 2.1 | GC 안전 범위 테스트 |
| 3.1 | Node/Gradle/Cargo Provider | 1.3 | 실제 스택 빌드 테스트 |
| 3.2 | Runtime lease | 1.2 | 동시 실행/비정상 종료 |
| 3.3 | Test evidence policy | 3.1 | 성공/실패 trace 보관 |
| 4.1 | 3 CLI Hook Adapter | 1.1 | JSON contract tests |
| 4.2 | Skills + MCP | 4.1 | 읽기 전용/승인 검증 |
| 5.1 | Proxy/OS 권한 경계 | 2.1,4.1 | 우회·권한 침해 시나리오 |
| 6.1 | 릴리스·서명·패키지 | 5.1 | 플랫폼 별 E2E |
