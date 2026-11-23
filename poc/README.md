# DSG 2.0 실행 가능 Python POC

이 폴더는 **Python 3.10+ 표준 라이브러리만** 사용하는 진단·정리 계획·제한적 실행 래퍼 POC입니다. Rust 최종 제품을 대신하지 않으며, 시스템 전역 강제 차단 기능은 없습니다.

```bash
cd poc
python3 -m unittest discover -s tests -v
python3 -m dsg doctor --root .
python3 -m dsg init --root .
python3 -m dsg scan --root .
python3 -m dsg docker-scan
python3 -m dsg check -- docker volume prune
python3 -m dsg gc-plan --root .
python3 -m dsg exec --root . -- python3 -c 'print("controlled")'
```

`.dsg.json`은 프로젝트 루트에 만듭니다. 사용자 데이터를 보호하기 위해 `gc-apply`는 기본적으로 실행되지 않습니다. `gc-apply PLAN_ID --i-understand-delete`와 `DSG_POC_ALLOW_DELETE=YES` **두 조건**이 동시에 필요합니다. GC는 최상위 알려진 생성 디렉터리만 대상으로 하며, 오래된 파일 변경 검사를 합니다. 실 프로젝트에서는 자동 삭제를 권장하지 않습니다.

`hook --agent codex|claude|gemini`는 stdin JSON의 `tool_input.command`를 읽어 위험 명령에 차단 응답을 반환하는 최소 예제입니다. 설정 파일에 자동 설치되지 않으며 셸 우회/간접 실행 감지는 구현하지 않았습니다. 실제 사용 전 해당 CLI 버전별 Hook 스키마를 재검증해야 합니다.

**현재 미구현:** Rust daemon, Docker API proxy, 실제 중복 이미지/컨테이너 판정, BuildKit GC 강제, 전역 파일시스템 quota, MCP server, 외부 Plugin SDK, 동시 프로세스 lease 및 안전한 격리 GC. 문서상 계획이며 동작한다고 주장하지 않습니다.
