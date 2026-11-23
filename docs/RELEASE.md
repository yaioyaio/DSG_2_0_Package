# DSG 로컬 제품 릴리스

로컬 제품의 사용 범위는 프로젝트 사용량·예산 확인, Docker 집계, 정리 미리보기다. 대시보드와 정리 승인 흐름은 분리돼 있다. 플랫폼별 패키지·서비스·서명 확인은 릴리스 근거에 각각 기록한다. [API 명세](API_SPEC.md#로컬-대시보드) · [검증 기록](HANDOFF.md)

| 대상 | 0.1.0 검증 결과 |
|---|---|
| macOS ARM64 | 로컬 배포 아카이브 설치·실행·제거 및 ad-hoc 서명 확인 |
| Linux ARM64 musl | Ubuntu 게스트 안에서 설치·업그레이드·실행·제거 및 SHA-256 확인 |
| Windows ARM64·x64 | 교차 링크 및 PE 아키텍처·아카이브 재현성 검사. SCM과 Windows에서의 설치 수명 주기는 네이티브 검증 대기 |

## 패키지 만들기

잠긴 의존성으로 대상 OS에 맞는 `dsg`와 `dsgd`를 빌드한다. 생성기는 입력 바이너리, `Cargo.lock`과 출처 고지를 담은 재현 가능한 `tar.gz` 및 SHA-256 파일을 만든다. `--adhoc`은 macOS 복사본에 로컬 서명을 적용하고 검증한다. Linux 패키지는 파일별 SHA-256과 아카이브 해시를 확인한다. 공인 배포 서명은 승인된 서명 환경에서 수행한다.

```sh
cargo build --manifest-path rust-scaffold/Cargo.toml --bins --release --locked --offline --jobs 1
mkdir -p dist
python3 -B scripts/release-package.py build --bin-dir rust-scaffold/target/release \
  --target aarch64-apple-darwin --version 0.1.0 \
  --output dist/dsg-0.1.0-aarch64-apple-darwin.tar.gz --adhoc
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
