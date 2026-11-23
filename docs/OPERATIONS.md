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

## 응급 정리 안내
대규모 prune 대신 Docker의 `docker system df -v`, `docker buildx du`, `docker ps -a`, `docker volume ls`로 먼저 조사. 영속 데이터 보호 확인 후 대상 범위에 국한해 수동 정리.

## 롤백
POC state는 `~/.dsg-poc`(또는 `DSG_STATE_HOME`)에 있습니다. 로그/계획만 들어 있으며 작업 파일 삭제와 별개입니다. `.dsg.json`은 사용자가 직접 삭제할 수 있습니다.
