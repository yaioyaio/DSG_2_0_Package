# DSG 로컬 제품 릴리스

로컬 제품의 사용 범위는 프로젝트 사용량·예산 확인, Docker 집계, 정리 미리보기다. 대시보드와 정리 승인 흐름은 분리돼 있다. 플랫폼별 패키지·서비스·서명 확인은 릴리스 근거에 각각 기록한다. [API 명세](API_SPEC.md#로컬-대시보드) · [검증 기록](HANDOFF.md)

| 대상 | 검증한 후보 | 검증 결과 |
|---|---|---|
| macOS ARM64 | 최신 `0.1.1-rc.1`, Rust 트리 `ada5a09` | 독립 빌드 두 번의 아카이브 SHA-256 일치. 실제 `0.1.0`에서 업그레이드·롤백·재업그레이드·제거, 설치본 진단과 대시보드 API·WebKit 확인 |
| Linux ARM64 musl | 최신 `0.1.1-rc.1`, Rust 트리 `ada5a09` | 기존 Ubuntu Parallels에서 이전 `0.1.0` 실물을 업그레이드했다. 두 실행 파일, 관리형 타임아웃 124와 lease 재획득, 대시보드 HTTP, 제거와 시험 파일 보존 및 별도 BuildKit 조회를 통과했다. `681be1e` 후보는 격리 Linux 엔진과 기존 Ubuntu VM에서도 검증했다. |
| Windows ARM64 | 최신 `0.1.1-rc.1`, Rust 트리 `ada5a09` | 기존 Parallels의 로컬 NTFS에서 이전 `681be1e` 실물 설치와 교체, 설치 파일의 해시·실행, 대시보드 HTTP·제거와 사용자 파일 보존 확인 |
| Windows x64 | 최신 `0.1.1-rc.1`, Rust 트리 `ada5a09` | 교차 링크·PE·아카이브 검증과 ARM64 게스트의 x64 에뮬레이션 실행·대시보드 HTTP 확인. ARM64 게스트의 네이티브 ARM64 Python과 에뮬레이션 x64 Python 각각에서 공식 설치기의 x64 설치·업그레이드가 종료 코드 1로 거절됐다. 설치 경로, 기존 ARM64 설치 포인터와 실행 파일 SHA-256, 사용자 시험 파일을 보존했다. 등록된 Windows VM이 ARM64이므로 Windows x64 운영체제 설치 수명 주기는 이번 범위에서 제외했다. [설치기 경계 재검증](HANDOFF.md#parallels-x64-설치기-경계-재검증) |

현재 후보의 Rust 소스 트리는 `ada5a09f9d59e00b8c2aa70a5143464d01450f2c`다. 아래 후보별 디렉터리에 아카이브, SHA-256 파일과 `QUALIFICATION.json` 검증 요약을 보존한다.

macOS ARM64 후보는 `dist/dsg-macos-qualification-ada5a09/`에 있다. 두 독립 빌드에서 SHA-256 `a367fc58fb16bf209dd4ec8259d83a0877de89a2eca33e9b2d32e626ce682730`이 일치했다. 설치본의 대시보드 API와 WebKit 1060px·390px 화면은 격리 Docker의 컨테이너 7개, 고유 이미지 ID 8개, 명명 볼륨 2개, 중복 그룹 0개와 일치했다. 기존 `681be1e` 후보와 당시 검증 근거는 `dist/dsg-macos-qualification-681be1e/`에 보존한다. [현재 후보 검증](HANDOFF.md#8단계-현재-후보-제품화-검증) · [이전 후보 검증](HANDOFF.md#dsg-0608-현재-소스의-macos-자격-검증)

Windows ARM64 로컬 NTFS의 파일시스템 시험 9개는 WBS 1.3의 별도 실측 기록이다. 패키지 수명 주기와 비교할 때는 위 표의 후보·소스 범위를 적용한다. [Windows NTFS 검증](HANDOFF.md#dsg-02-windows-ntfs-파일-식별-보완)

현재 Windows ARM64 후보는 `dist/dsg-windows-arm64-qualification-ada5a09/`에 보존하며, 아카이브 SHA-256은 `19ee6712ddbcfa0e7bc1f1da59ce39a4e5cad613f3a14fec4d6371d73dea6596`이다. 현재 Windows x64 후보는 `dist/dsg-windows-x64-qualification-ada5a09/`에 보존하며, 아카이브 SHA-256은 `667bd3ab4979f571c0b2e5293434026e0735786723f099e1f0a8333255b80470`이다. 두 대상 각각 동일한 교차 링크 입력으로 두 번 패키징한 아카이브의 해시가 일치했다. Windows ARM64의 이전 `681be1e` 후보와 현재 후보는 버전 문자열이 같으므로 아카이브·실행 파일의 해시, 설치 경로 교체와 이전 릴리스 회수로 업그레이드를 확인했다. 이전 ARM64 `681be1e` 후보와 x64 `0.1.0-rc.3` 실물도 보존한다. [현재 Windows 실측](HANDOFF.md#8단계-현재-후보-제품화-검증) · [이전 ARM64 실측](HANDOFF.md#macos-및-기존-parallels-잔여-수용-기준-재검증)

기존 Linux ARM64 후보와 SHA-256 파일은 `dist/dsg-linux-arm64-qualification-681be1e/`에 보존한다. 두 정적 musl ELF 바이너리로 두 번 패키징해 SHA-256 `4847b388b8d2d97bb2e8f44dfe364d5f9c5cda0eb563f2fa862fb657fbf03e65`가 일치했다. 읽기 전용 Linux ARM64 시험 컨테이너와 기존 Ubuntu Parallels에서 같은 후보를 검증했다. Ubuntu 게스트는 표준 입력으로 전달한 시험 코드와 아카이브의 SHA-256을 각각 대조한 뒤 설치 수명 주기와 관리형 실행을 확인했다. 게스트 시험 전용 임시 자료를 회수하고 일시 중단 상태로 복귀했으며 VM 설정 변경은 없었다. [Ubuntu Parallels 실측](HANDOFF.md#명명-볼륨과-ubuntu-parallels-실측)

최신 Linux ARM64 후보는 `dist/dsg-linux-arm64-qualification-ada5a09/`에 보존한다. 아카이브 SHA-256 `c8bea5e9d37927679a9a57812781a322a0d180abd937487fbadc09f9748202a8`가 동일 입력 두 번의 패키징에서 일치했다. Ubuntu Parallels에서 설치·업그레이드·제거와 제품 명령 `buildkit-local-cache`를 검증했다. [최신 Linux 실측](HANDOFF.md#별도-buildkit-읽기-전용-실측)

## 패키지 만들기

잠긴 의존성으로 대상 OS에 맞는 `dsg`와 `dsgd`를 빌드한다. 생성기는 입력 바이너리, `Cargo.lock`과 출처 고지를 담은 재현 가능한 `tar.gz` 및 SHA-256 파일을 만든다. `--adhoc`은 macOS 복사본에 로컬 서명을 적용하고 검증한다. Linux 패키지는 파일별 SHA-256과 아카이브 해시를 확인한다. 공인 배포 서명은 승인된 서명 환경에서 수행한다.

macOS 후보는 `scripts/build-macos-release.py`로 임시 빌드 대상을 사용해 만든다. 빌드 작업은 한 개씩 실행하고 Cargo 의존성은 lock 파일과 로컬 캐시를 사용한다. CLI의 컴파일 버전과 지정한 아카이브 버전이 일치해야 하며 아카이브와 SHA-256 파일은 새 이름을 사용한다.

```sh
mkdir -p dist
DSG_MACOS_BUILD_DIR=$(mktemp -d dist/dsg-macos-local.XXXXXXXX)
python3 -B scripts/build-macos-release.py --version 0.1.1-rc.1 \
  --output "$DSG_MACOS_BUILD_DIR/dsg-0.1.1-rc.1-aarch64-apple-darwin.tar.gz"
```

## 설치와 롤백

사용자 소유의 기존 디렉터리를 `--prefix`로 제공하고, `--sha256`에는 검증된 아카이브의 해시를 지정한다. `install` 뒤에 `<prefix>/dsg-current/dsg --version`으로 실행을 확인한다. `doctor`는 버전, 대상 및 서명 종류를 반환한다. `upgrade`는 같은 대상 패키지로 교체하고, `uninstall`은 검증한 설치 파일과 연결을 회수한다. 롤백은 보관한 이전 버전의 아카이브를 같은 `upgrade` 절차로 적용한다.

```sh
python3 -B scripts/release-package.py install --prefix "$DSG_PREFIX" \
  --archive "$DSG_ARCHIVE" --sha256 "$DSG_SHA256"
python3 -B scripts/release-package.py doctor --prefix "$DSG_PREFIX"
```

`dsg-current` 경로나 설치 폴더에 생성된 사용자 파일이 있거나 매니페스트와 실물이 다르면 실패를 반환한다. 진단 후 사용자 자료를 보호한 상태에서 원인을 해결한다. 서비스의 계정과 엔진 연결은 [운영 매뉴얼](OPERATIONS.md#docker-조회-브로커)을 따른다.

Windows 패키지는 `aarch64-pc-windows-msvc` 또는 `x86_64-pc-windows-msvc`의 `dsg.exe`와 `dsgd.exe`를 사용한다. `dsg-current`는 설치 상태 파일이다. `doctor`의 `path`가 바이너리 디렉터리다. 실행 중인 파일이 잠겨 정리를 완료하지 못하면 상태를 보존하고 `pending_cleanup`을 보고한다. 프로세스 종료 뒤 같은 명령으로 회수를 재시도한다. 외부 Authenticode 서명이 적용된 PE 입력은 바이트 그대로 보관하며 공식 배포 신뢰 검증은 승인된 환경에서 수행한다.

## 배포 파일의 분리 서명 검증

`scripts/verify-release-signature.py`는 동일한 Ed25519 공개 키로 아카이브와 내부 `MANIFEST.json`의 정확한 바이트를 각각 검증한다. 아카이브 SHA-256과 SPKI DER 공개 키 SHA-256은 신뢰하는 경로에서 미리 확인한다. `--openssl`에는 신뢰하는 OpenSSL 3 실행 파일의 절대 경로를 지정한다. 검증 결과에 `authority: caller-pinned-key`가 표시된다. 공식 게시자 신원과 OS 배포 서명은 아래 플랫폼 절차로 검증한다.

```sh
python3 -B scripts/verify-release-signature.py \
  --archive "$DSG_ARCHIVE" --sha256 "$DSG_SHA256" \
  --signatures "$DSG_SIGNATURES" --public-key "$DSG_PUBLIC_KEY" \
  --public-key-sha256 "$DSG_PUBLIC_KEY_SHA256" --openssl "$DSG_OPENSSL"
```

`--signatures`, `--public-key`, `--public-key-sha256`, `--openssl` 네 옵션을 `install`과 `upgrade`에 함께 전달하면 설치 경로 변경 전에 서명을 확인한다. 옵션을 모두 생략하면 로컬 후보는 기존 SHA-256 검증으로 설치할 수 있다. 서명 파일은 정렬된 키와 끝 줄바꿈이 있는 JSON이며 필드는 `format`(1), `algorithm`(`Ed25519`), `key_id`(SPKI DER SHA-256), `archive_sha256`, `manifest_sha256`, `archive_signature_hex`, `manifest_signature_hex`다. 서명은 각각 64바이트를 소문자 16진수로 인코딩한다.

### 오프라인 서명 요청과 조립

플랫폼 서명이 끝난 실행 파일로 최종 아카이브를 만든 뒤 `prepare`로 내부 `MANIFEST.json` 원문을 내보낸다. 아래 명령의 출력 디렉터리는 운영자가 만든 기존 `dist/dsg-signing/`을 사용한다. 아카이브와 매니페스트 원문을 승인된 서명자에게 전달하고, 서명자가 같은 키로 각각 Pure Ed25519 서명한 64바이트 결과 두 개를 받는다. 승인 기록에서 확인한 대상·버전과 아카이브·매니페스트·공개 키의 SHA-256을 사용한다. `assemble`은 반환된 서명 두 개를 검사한 뒤에만 JSON을 생성하고, 내용이 다른 기존 출력은 보존한다. 키 보관과 실제 서명은 서명자가 담당한다.

```sh
python3 -B scripts/prepare-release-signature.py prepare \
  --archive "$DSG_ARCHIVE" --sha256 "$DSG_SHA256" \
  --manifest-output dist/dsg-signing/MANIFEST.json

python3 -B scripts/prepare-release-signature.py assemble \
  --archive "$DSG_ARCHIVE" --sha256 "$DSG_SHA256" \
  --manifest-sha256 "$DSG_MANIFEST_SHA256" \
  --archive-signature dist/dsg-signing/archive.sig \
  --manifest-signature dist/dsg-signing/manifest.sig \
  --public-key "$DSG_PUBLIC_KEY" --public-key-sha256 "$DSG_PUBLIC_KEY_SHA256" \
  --openssl "$DSG_OPENSSL" --output dist/dsg-signing/signatures.json
```

## 플랫폼별 공인 서명

| 대상 | 공인 배포물 검증 | 참조 |
|---|---|---|
| macOS | 두 실행 파일에 Developer ID Application 서명, Hardened Runtime·보안 타임스탬프 적용. 승인된 Team ID와 공증 결과를 각각 검증 | `ddd-tools` `3e30ff9`의 `docs/references/DPG-P5-061-MACOS-PRODUCTION-SIGNED-INSTALLER-GUIDE.md` |
| Windows | `dsg.exe`와 `dsgd.exe` 각각에 SHA-256 Authenticode 서명과 RFC 3161 타임스탬프 적용. `signtool verify /pa /all`로 검증하고 승인 게시자·신뢰 체인·타임스탬프 확인 후 패키징 | `ddd-tools` `3e30ff9`의 `docs/references/DPG-P5-061-WINDOWS-PRODUCTION-SIGNED-INSTALLER-GUIDE.md` |
| Linux | DEB/APT 채택 시 `InRelease` 또는 `Release.gpg` 검증과 저장소별 `signed-by` 지정 | `ddd-tools` `3e30ff9`의 `docs/references/DPG-P5-061-LINUX-PRODUCTION-SIGNED-INSTALLER-GUIDE.md` |

macOS에서는 승인된 환경에서 서명한 두 실행 파일을 `release-package.py build`에 전달한다. 로컬 `--adhoc` 옵션을 생략하면 외부에서 서명된 바이트가 보존된다. macOS 호스트에서 `assemble`, `verify-release-signature.py`, `install`, `upgrade`에 분리 서명 검증 옵션과 `--macos-team-id "$DSG_MACOS_TEAM_ID"`를 함께 제공할 수 있다. 이 옵션은 최종 아카이브에서 꺼낸 두 실행 파일에 Apple Developer ID Application 체인과 승인된 Team ID를 요구한다. 검증기가 출력하는 `macos_developer_id_verified`는 이 검사 결과다. 공급자가 공증을 요구하면 같은 바이트의 두 실행 파일을 ZIP에 넣어 승인된 환경에서 Apple 공증 `Accepted`와 대상 호스트의 실행을 별도로 확인한다. DSG의 `tar.gz`에는 staple을 적용할 수 없다. [Apple 코드 서명 요구사항](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements/)

분리 서명은 `ddd-studio-rsie` `718ca48`의 `docs/RELEASE-TRUST.md`, `scripts/release_signature.py`, `contracts/release-signatures.schema.json`에서 아카이브와 매니페스트의 원문 서명 및 독립적인 공개 키 지정 원칙을 참조했다. DSG는 위 서명 파일 형식과 자체 승인 경로를 사용한다.

## 정책 변경 이력

- `0.1.1-rc.1`의 Docker 빌드 캐시는 소유권을 확인할 수 없어 보존한다. 이미지 후보는 사용자의 명시적 선택과 소유 표식, 참조 및 lease 검사를 거쳐 기록한다.
- 같은 버전의 Playwright trace 보관 기간은 운영자가 지정한다. 실제·예상 결과가 모두 성공이고 실행 종료와 감사가 확인된 trace만 자동 회수 대상으로 등록된다.
- MCP는 실행 상태와 승인 기록을 조회한다. 운영자의 승인 발급과 적용 권한은 관리형 CLI에 있다.
