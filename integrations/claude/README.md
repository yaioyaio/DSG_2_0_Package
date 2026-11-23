# Claude Code CLI 연동

`dsg --root <프로젝트> agents install --agent claude`는 `.claude/settings.json`에 `PreToolUse`의 `Bash` 훅을 추가한다. 차단 시 `hookSpecificOutput.permissionDecision`에 `deny`를 반환한다. 실행 실패 시 차단하는 `onFailure: block`을 적용하려면 Claude Code 2.1.295 이상이 필요하다. [Claude Code 훅 참조](https://code.claude.com/docs/en/hooks#block-the-action-when-a-hook-fails)

```sh
printf '%s\n' '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"docker volume prune"}}' | dsg hook --agent claude
```
