# Codex CLI 연동

`dsg --root <프로젝트> agents install --agent codex`는 `.codex/hooks.json`에 `PreToolUse`의 `Bash` 훅을 추가한다. 프로젝트와 훅 등록을 검토한 뒤 Codex에서 신뢰를 승인한다. 변경된 명령은 다시 검토한다. DSG는 일치하는 다른 훅의 실행 순서에 의존하지 않는다. [Codex Hooks 문서](https://developers.openai.com/codex/hooks)

```sh
printf '%s\n' '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"docker volume prune"}}' | dsg hook --agent codex
```
