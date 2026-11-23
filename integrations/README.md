# AI CLI 훅 연동

macOS와 Linux에서 Rust 바이너리의 위치를 확정한 뒤 프로젝트에 설치한다. 기존 설정을 먼저 백업하고, 변경 후 사용 중인 CLI 버전에서 허용과 거부 입력을 확인한다.

```sh
dsg --root /path/to/project agents install --agent claude
dsg --root /path/to/project agents uninstall --agent claude
```

| CLI | 입력 이벤트와 도구 | 설치 파일 |
|---|---|---|
| [Claude Code](claude/README.md) | `PreToolUse` · `Bash` | `.claude/settings.json` |
| [Codex CLI](codex/README.md) | `PreToolUse` · `Bash` | `.codex/hooks.json` |
| [Gemini CLI](gemini/README.md) | `BeforeTool` · `run_shell_command` | `.gemini/settings.json` |

설치 항목은 `dsg` 실행 파일의 절대 경로를 참조한다. 바이너리를 이동하거나 교체하기 전에 기존 위치의 `dsg`로 설치 항목을 제거하고 새 위치에서 다시 설치한다. 사용자 설정에서 제공하던 다른 항목은 보존된다.

훅은 해당 CLI가 전달한 셸 도구의 리터럴 명령을 공통 기본 정책으로 검사한다. 차단 사유만 반환하며 명령의 원문이나 인자는 응답에 포함하지 않는다. 훅이 활성화된 도구에서만 검사하며, 작업 디렉터리·Docker 엔진에 대한 OS 권한 경계는 서비스 단계의 범위다. 프로젝트 설정을 쓰거나 신뢰를 승인할 수 있는 사용자를 운영자가 관리해야 한다.

공통 [빌드 가드](../skills/build-guard/SKILL.md), [디스크 진단](../skills/disk-health/SKILL.md), [Docker 재사용](../skills/docker-reuse/SKILL.md), [안전 정리](../skills/safe-cleanup/SKILL.md), [테스트 가드](../skills/test-guard/SKILL.md) 절차를 함께 사용한다. 에이전트가 상태를 조회할 때는 [읽기 전용 MCP](../docs/API_SPEC.md#읽기-전용-mcp)를 프로젝트별 stdio 서버로 등록할 수 있다.
