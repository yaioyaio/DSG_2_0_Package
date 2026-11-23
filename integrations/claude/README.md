# Claude Code CLI 연동

PreToolUse Hook을 이용할 수 있습니다. Claude의 Hook 입력은 JSON stdin이며, deny 응답은 hookSpecificOutput의 permissionDecision=deny입니다. 먼저 공식 문서(`https://code.claude.com/docs/en/hooks`)에서 현재 버전의 정확한 설정과 결과 형식을 확인하십시오.

예시 검사:
```
echo '{"tool_name":"Bash","tool_input":{"command":"docker volume prune"}}' | python3 -m dsg hook --agent claude
```

글로벌 설정 자동 수정 및 설치 기능은 미구현입니다. `.claude/settings.json`에 Hook을 직접 연결하기 전에 백업하십시오.
