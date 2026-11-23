# CLI / IPC / MCP API 명세

## CLI POC에서 실제 가능한 명령
```
python3 -m dsg init --root PATH
python3 -m dsg doctor --root PATH
python3 -m dsg scan --root PATH
python3 -m dsg docker-scan
python3 -m dsg check -- docker volume prune
python3 -m dsg gc-plan --root PATH
python3 -m dsg gc-apply PLAN_ID --i-understand-delete
python3 -m dsg exec --root PATH -- python3 -V
python3 -m dsg hook --agent codex
```

## 계획 API (미구현)
- `dsg inspect <resource-id>`
- `dsg docker reuse --project <id>`
- `dsg lease acquire --project <id> --ttl 30`
- `dsg gc apply --plan <id> --approval <token>`
- `dsg agents install --agent claude|codex|gemini`
- `dsgd` local IPC methods: `health`, `policy.evaluate`, `resource.inventory`, `gc.plan`, `gc.apply`

## 공통 판단 응답(목표)
```json
{"decision":"deny","reason_code":"PROTECTED_VOLUME","message":"Persistent volume cannot be deleted","evidence_ids":["evt_001"],"suggested_action":"inspect"}
```

## MCP 설계 (미구현)
`dsg_get_health`, `dsg_scan_project`, `dsg_inspect_docker`, `dsg_check_command`, `dsg_get_gc_plan`, `dsg_get_execution_status`, `dsg_get_policy`.

### 오류 코드
`CONFIG_INVALID`, `PROJECT_UNKNOWN`, `RESOURCE_ACTIVE`, `RESOURCE_SHARED`, `RESOURCE_CHANGED`, `POLICY_DENY`, `INSUFFICIENT_SPACE`, `PLAN_EXPIRED`, `NEEDS_APPROVAL`, `ENGINE_UNAVAILABLE`.
