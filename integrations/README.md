# AI CLI Hook 통합 예시

`poc/dsg/__main__.py`의 `hook --agent`는 stdin JSON에서 `tool_input.command`를 분석하여 기본적인 위험 명령을 거부합니다. 예제는 세 CLI의 모든 도구 타입을 처리하지 않으며, **현재 설치된 CLI에서 계약 테스트를 거치기 전 자동 설치하지 마십시오**.

```bash
cd poc
printf '%s\n' '{"tool_name":"Bash","tool_input":{"command":"docker volume prune"}}' | python3 -m dsg hook --agent codex
printf '%s\n' '{"tool_name":"run_shell_command","tool_input":{"command":"docker volume prune"}}' | python3 -m dsg hook --agent gemini
printf '%s\n' '{"tool_name":"Bash","tool_input":{"command":"docker volume prune"}}' | python3 -m dsg hook --agent claude
```

실제 훅 설치에는 절대 경로로 Python 인터프리터 및 POC 코드를 지정해야 합니다. 프롬프트 규칙·Hook는 권한 관리 대체가 아닙니다.
