# Gemini CLI 연동

`dsg --root <프로젝트> agents install --agent gemini`는 `.gemini/settings.json`에 `BeforeTool`의 `run_shell_command` 훅을 추가한다. 설정에서 훅 실행이 허용되는지 확인한 뒤 신뢰한 프로젝트에서 사용한다. 차단 시 `decision: deny`와 `reason`을 반환한다. [Gemini CLI 훅 참조](https://geminicli.com/docs/hooks/reference/)

```sh
printf '%s\n' '{"hook_event_name":"BeforeTool","tool_name":"run_shell_command","tool_input":{"command":"docker volume prune"}}' | dsg hook --agent gemini
```
