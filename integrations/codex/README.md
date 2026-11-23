# Codex CLI 연동

현재 문서 기준 Codex PreToolUse 차단 응답: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"..."}}`.

공식: https://learn.chatgpt.com/docs/hooks

샘플 입력: `{"tool_name":"Bash","tool_input":{"command":"docker volume prune"}}`

각 Hook은 다른 매칭 Hook과 병행 실행될 수 있습니다. DSG 정책과 잠금은 Hook 순서에 의존하지 않도록 구현하십시오. 자동 설치는 미구현입니다.
