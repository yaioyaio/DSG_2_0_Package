# Codex / Claude Code / Gemini CLI용 단계별 프롬프트

## 초기 공통 프롬프트
> AGENTS.md, docs/PRD.md, REQUIREMENTS.md, ARCHITECTURE.md, SECURITY.md, TEST_PLAN.md를 먼저 읽어라. 이번 작업은 WBS 1.1만 수행한다. 범위 외 변경과 파괴적 명령은 금지한다. 변경 전에 구현 계획, 후에는 테스트 실행 결과·증거·미구현 항목을 보고하라. Docker를 새로 생성하기 전에 현재 상태와 소유권을 검사하라.

## Rust Port
> Python POC의 core.py 동작과 tests를 기준으로 Rust Core의 정책 스키마 및 파일스캐너를 작성하라. 동일 입력에서 동등한 결과를 제공하는 회귀 테스트를 작성하라. 파일 삭제는 구현하지 말고 GC plan만 구현하라.

## Docker Provider
> 이미지 태그만으로 재사용을 판정하지 말고 context/engine ID/config hash/volume/health를 포함한 상태 비교를 설계하라. Docker API는 초기 단계에서 read-only로 호출하라. 테스트 fixture로 잘못된 재사용(false positive)을 0건으로 확인하라.

## Agent Hooks
> Claude, Codex, Gemini 각 CLI의 현재 공식 문서에서 Hook 입력/출력 스키마를 확인하고 adapter별 contract test를 작성하라. 설치/제거는 사용자 백업 및 승인 후 진행하라. Shell wrapping 우회 가능성을 명시하라.
