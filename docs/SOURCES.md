# 공식 참조 및 재검증 체크리스트 (2026-11-19 조회)

- Docker Build GC: https://docs.docker.com/build/cache/garbage-collection/
- Docker Engine Prune: https://docs.docker.com/engine/manage-resources/pruning/
- Docker Authorization: https://docs.docker.com/engine/extend/plugins_authorization/
- Claude Hooks: https://code.claude.com/docs/en/hooks
- Codex Hooks: https://learn.chatgpt.com/docs/hooks
- Gemini Hooks: https://geminicli.com/docs/hooks/reference/
- Cargo Config: https://doc.rust-lang.org/stable/cargo/reference/config.html

## 확인 사항
- Codex `PreToolUse`에는 `hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: ...}` 형태를 사용.
- Gemini `BeforeTool`은 `{decision:"deny",reason:"..."}` 형태.
- Claude `PreToolUse`는 제품 버전에 맞춘 `hookSpecificOutput` 블록 사용.

사용하는 CLI 버전의 실제 `settings.json` 구성 및 JSON 이벤트를 확인하지 않고 Hook 예제를 자동 설치하지 마십시오. 별도 Plugin Store 확장과 AI Skill은 CLI 기능 이름이 같더라도 서로 호환된다는 보장은 없습니다.
