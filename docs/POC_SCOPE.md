# Python POC 범위와 Rust 제품 현황

## Python POC 범위

| 항목 | Python POC에서 확인한 범위 | 제품 설계의 목표 |
|---|---|---|
| Python 명령형 POC | 구현 + 테스트 | Rust 제품으로 이식 |
| 안전한 대상 폴더 스캔 | 최상위 알려진 폴더만 | 다중 프로젝트/Provider 감지 |
| 프로젝트 예산 | 스캔 경로 합계 기반 | 실제 전체 디스크·공유 사용량 |
| Docker | 읽기 전용 CLI 조회 | API 소유권 추적, Compose 중복·재사용 |
| GC | TTL 계획/제한적 적용 | 활성·공유 리소스 확인, 격리·복구 |
| Run wrapper | 기본 정책 및 여유 공간 체크 | 프로세스 트리/동시성/lease |
| Hooks | 세 CLI 형식의 차단 응답 예제 | 제품별 설치·호환 테스트 |
| Skills | 지침 문서 | 정식 등록/버전 관리 |
| MCP | 설계 문서 | 실행 가능한 서버 |

독립형 HTML 아티팩트는 시뮬레이션입니다. Python POC의 실제 명령과 안전 한계는 실행 안내를 따릅니다. [Python POC 안내](../poc/README.md)

## Rust 제품 현황

- 로컬 CLI는 등록·정책·용량 스캔·SQLite 감사, 정리 미리보기, 승인 기반 파일 정리와 격리 복구를 제공합니다. Docker는 읽기 전용 인벤토리, 관리형 Compose 재사용 확인, 이미지 정리 계획과 재검증을 제공합니다. [API 명세](API_SPEC.md)
- 관리형 실행에는 프로젝트 lease와 정형 빌드·테스트 Provider가 있습니다. 에이전트 연동에는 세 CLI용 훅과 설치·제거, 읽기 전용 MCP가 있습니다. 조회 데이터에 연결된 로컬 웹 대시보드는 `dsg dashboard`로 제공합니다. [운영 매뉴얼](OPERATIONS.md)
- Docker 조회 브로커는 macOS 샌드박스·launchd와 Linux 제한 UID·systemd에서 검증됐습니다. Linux 검증에는 실제 Docker 엔진을 사용했습니다. Windows ARM64에서는 실제 SCM에서 합성 상류로 계정 경계와 재시작을 검증했습니다. Windows 실제 Docker 엔진 연동과 macOS 배포 계정 분리의 검증이 남아 있습니다. [핸드오프](HANDOFF.md)

전체 제품 범위와 항목별 수용 상태는 백로그와 작업 대조표를 따릅니다. [개발 백로그](BACKLOG.md) · [작업 대조표](TASK.md)
