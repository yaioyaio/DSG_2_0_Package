# 운영 및 설치/진단 매뉴얼

## 요구 환경
POC는 Python 3.10+, 설치 패키지 불필요. Docker 스캔에는 Docker CLI 및 Engine 접근 필요.

## 설치 없이 실행
```
cd poc
python3 -m unittest discover -s tests -v
python3 -m dsg init --root /path/to/sample
python3 -m dsg scan --root /path/to/sample
python3 -m dsg docker-scan
```

## 프로젝트 적용 지침
1. 실제 프로젝트는 **스캔/GC 계획까지만** 사용하여 보고서를 확인한다.
2. 디렉터리 감시 결과에 `node_modules`, `.git`, 영속 데이터가 들어오면 설정을 수정하고 관리 대상으로 지정하지 않는다.
3. `.dsg.json`을 코드 저장소에 포함할지 팀 정책으로 결정한다.
4. Host 여유 공간 정책은 안전 여유 공간이므로 CI 환경에서는 별도 설정 필요.
5. Docker 결과가 실제 호스트 사용량과 다르면 Docker Desktop VM reclaimable/allocated를 분리한다.

## Rust 관리형 실행

`dsg init`으로 프로젝트를 등록한 뒤 `dsg exec --timeout 60 -- cargo check --offline --jobs 1`과 같이 직접 argv를 넘긴다. macOS·Linux의 프로세스 그룹에 실행 시간을 제한하고 프로세스별 CPU·파일 크기·열린 파일 상한을 적용한다. 일반 텍스트 모드는 자식의 터미널 출력을 전달하고 JSON 모드는 결과 구조만 전달한다. 자체적으로 그룹을 이탈한 프로세스 및 Windows 런타임 제어는 플랫폼 검증 단계에서 다룬다.

활성 또는 종료 미확인 lease가 있으면 같은 프로젝트의 실행을 보류한다. 만료된 실행은 OS 잠금이 풀리고 기록된 프로세스 그룹이 종료됐음이 확인돼야 회수된다. 실행과 재사용 결과는 비밀값과 argv 원문을 제외한 감사 코드로 기록한다.

## 정리와 완료 이벤트

수동 정리는 먼저 [승인 기반 명령](API_SPEC.md#승인-기반-정리)으로 후보와 소유권을 확인한다. 프로젝트 경계, 보호 파일, 상태 변경, 실행 중인 작업의 증거가 불확실하면 경로를 보존한다. 격리 단계가 남아 있으면 원본 경로와 격리 자료를 대조한 뒤 복구한다.

- 등록된 Cargo 실행은 `--auto-clean-cargo`로 전용 산출물 위치를 선택한다. 종료 이벤트별로 정리 결과를 기록한다. 실패 근거와 종료를 확인하지 못한 산출물은 보존한다.
- 보조 워크트리는 동시 쓰기 충돌을 피해야 할 때만 `dsg worktree-create`로 등록해 만든다. 등록된 Git 완료 경로는 `DSG_CLI=<검증된 dsg 경로> sh scripts/git-completion.sh <feature> <상태 디렉터리>`다. 여섯 참조 파일 일치, 사용·잠금·미반영 자료 검사를 통과하면 정상 Git 회수 후 주 작업 폴더가 `develop`으로 돌아간다.
- 위 진입점을 호출한 실행과 Git 완료 이벤트에 자동화가 적용된다. 회수 바이트는 검사한 논리 크기다. 회수할 수 없는 경로는 보존 결과를 확인한다.

## 에이전트 연동 운영

CLI 훅 설치 후 신뢰 설정과 실행 파일 위치를 확인한다. CLI가 훅을 로드하지 못하면 해당 에이전트에서 보호 명령을 실행하지 않고 설치 상태부터 점검한다. Claude Code의 등록은 훅 실행 실패를 차단하도록 설정한다. Codex와 Gemini는 훅 실행 오류가 발생하면 자체 권한 처리로 진행할 수 있다. 통제된 Docker 접근이 필요한 운영 환경의 수용 기준은 별도의 OS 권한 경계다. [에이전트별 설정 및 검증](../integrations/README.md)

## 응급 정리 안내
대규모 prune 대신 Docker의 `docker system df -v`, `docker buildx du`, `docker ps -a`, `docker volume ls`로 먼저 조사. 영속 데이터 보호 확인 후 대상 범위에 국한해 수동 정리.

## 롤백
POC state는 `~/.dsg-poc`(또는 `DSG_STATE_HOME`)에 있습니다. 로그/계획만 들어 있으며 작업 파일 삭제와 별개입니다. `.dsg.json`은 사용자가 직접 삭제할 수 있습니다.
