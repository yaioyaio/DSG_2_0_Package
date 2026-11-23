# 기능 요구사항 명세 (SSOT)

| ID | 기능 | 우선순위 | 요구 결과 | POC |
|---|---|---|---|---|
| REQ-001 | 프로젝트 등록 | P0 | 명시 config 생성, 덮어쓰기 금지 | 구현 |
| REQ-002 | 기술/폴더 탐색 | P0 | 관리 대상만 스캔, 링크 회피 | 일부 구현 |
| REQ-003 | 호스트/프로젝트 예산 | P0 | 실행 전 최소 여유 공간 검증 | 구현(스캔 대상 기준) |
| REQ-004 | Docker Inventory | P0 | Docker CLI 읽기 전용 결과 | 구현(도구 설치 시) |
| REQ-005 | Docker 재사용 | P1 | Compose+config+health 동일성 | 미구현 |
| REQ-006 | 실행 전 정책 검증 | P0 | allow/warn/deny 응답 | 정규식 기반 POC |
| REQ-007 | GC Plan | P0 | 만료/용량/보호 상태 판단 | 일부 구현 |
| REQ-008 | GC Apply | P0 | 재검증 및 명시적 승인 필수 | 제한 구현 |
| REQ-009 | 세션 Lease | P1 | 동시 실행 소유권 관리 | 미구현 |
| REQ-010 | AI Hook | P1 | CLI별 정확한 JSON 입출력 | 3개 CLI의 셸 훅과 프로젝트 설치·제거 |
| REQ-011 | MCP | P2 | 읽기 전용 도구+승인 도구 | 읽기 전용 stdio 도구 6개. 실행 상태 조회와 승인 도구는 예정 |
| REQ-012 | Docker Proxy | P2 | 직접 소켓 우회 제한 | macOS 샌드박스·launchd 및 Linux 제한 UID·systemd 격리 검증. Windows SCM·파이프 교차 링크, 런타임 검증 대기 |
| REQ-013 | 확장 Provider | P2 | SDK/서명/격리 | 미구현 |

## 수용 기준
- REQ-007: Dry-run은 파일을 삭제하지 않는다.
- REQ-008: 변경된 경로/mtime/inode 감지 시 삭제 거절한다. 활동 중 프로세스 추적까지 완료되기 전 실사용 자동 삭제 금지.
- REQ-005: 태그만 동일하다는 이유로 재사용해서는 안 된다.
- REQ-010: 악의적 중첩 셸/직접 Docker API 우회는 Hook만으로 방지할 수 없음을 사용자에게 명시한다.
