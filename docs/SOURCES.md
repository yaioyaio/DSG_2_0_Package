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

## DockerGuard 조사 기준

DSG 읽기 전용 Provider를 구현할 때 `ddd-studio-dockerguard`의 `40dbd20`에서 `crates/guard-dedup/src/create.rs`, `tests/scenarios.rs`, `tests/no_false_block.rs`와 레코드 모델을 대조했다. Compose 인스턴스·설정 해시, 프로젝트 경계, 마운트·이미지 불일치 및 비정상 상태를 합성 fixture로 재작성했다. 원본 라이선스 선언은 `MIT OR Apache-2.0`이다. 실측과 재사용 조건은 [프로젝트 분석](draft/PROJECT-ANALYSIS.md)에서 확인할 수 있다.

## 로컬 전용 Moby

`docker-v29.1.3` 태그의 커밋 `fbf3ed25f893e6ce21336f1101590e40a13934f4`와 아카이브 SHA-256 `a3999adc2099b493d924fe7e1d07dfb4c7cb2887ea5c84c4de95554ee9ac8a07`을 고정한다. 오버레이는 이미지·참조·레이어 구현을 해당 소스에서 빌드하며 표준 데몬 초기화 진입 전 전용 모드를 분리한다. [Moby 소스](https://github.com/moby/moby/tree/fbf3ed25f893e6ce21336f1101590e40a13934f4) · [준비 도구](../scripts/prepare-moby-exact.py)
